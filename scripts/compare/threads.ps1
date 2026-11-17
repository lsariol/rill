# Records each thread's CPU and the process's memory every few seconds; when a thread stays busy
# (above -Busy percent of a core twice running), samples its stacks and names the functions,
# using the symbols next to the program. Writes threads.csv and hot-<thread>.txt to -Out.
# powershell -File threads.ps1 -ProcessId <pid> -Out <dir> [-Seconds 1500] [-Busy 60]
param([int]$ProcessId, [string]$Out, [int]$Seconds = 1500, [double]$Busy = 60, [int]$Every = 5)
$ErrorActionPreference = 'Stop'
New-Item -ItemType Directory -Force $Out | Out-Null
Add-Type @'
using System;
using System.Collections.Generic;
using System.Diagnostics;
using System.Runtime.InteropServices;
using System.Text;
public static class Hot {
  [DllImport("kernel32.dll")] static extern IntPtr OpenThread(uint access, bool inherit, uint id);
  [DllImport("kernel32.dll")] static extern IntPtr OpenProcess(uint access, bool inherit, uint id);
  [DllImport("kernel32.dll")] static extern bool CloseHandle(IntPtr h);
  [DllImport("kernel32.dll")] static extern bool QueryThreadCycleTime(IntPtr t, out ulong cycles);
  [DllImport("kernel32.dll")] static extern int GetThreadDescription(IntPtr t, out IntPtr desc);
  [DllImport("kernel32.dll")] static extern IntPtr LocalFree(IntPtr p);
  [DllImport("kernel32.dll")] static extern uint SuspendThread(IntPtr t);
  [DllImport("kernel32.dll")] static extern uint ResumeThread(IntPtr t);
  [DllImport("kernel32.dll")] static extern bool GetThreadContext(IntPtr t, IntPtr ctx);
  [DllImport("kernel32.dll")] static extern IntPtr LoadLibrary(string name);
  [DllImport("kernel32.dll")] static extern IntPtr GetProcAddress(IntPtr mod, string name);
  [DllImport("dbghelp.dll")] static extern uint SymSetOptions(uint o);
  [DllImport("dbghelp.dll", CharSet = CharSet.Ansi)] static extern bool SymInitialize(IntPtr p, string path, bool invade);
  [DllImport("dbghelp.dll")] static extern bool SymFromAddr(IntPtr p, ulong addr, out ulong disp, IntPtr sym);
  [DllImport("dbghelp.dll")] static extern bool StackWalk64(uint machine, IntPtr p, IntPtr t, IntPtr frame, IntPtr ctx, IntPtr read, IntPtr fta, IntPtr gmb, IntPtr tr);
  const uint THREAD_ALL = 0x1F03FF, PROCESS_ALL = 0x1F0FFF;

  public static Dictionary<uint, ulong> Cycles(int pid) {
    var d = new Dictionary<uint, ulong>();
    foreach (ProcessThread th in Process.GetProcessById(pid).Threads) {
      IntPtr h = OpenThread(0x0800, false, (uint)th.Id); // THREAD_QUERY_LIMITED_INFORMATION
      if (h == IntPtr.Zero) continue;
      ulong c; if (QueryThreadCycleTime(h, out c)) d[(uint)th.Id] = c;
      CloseHandle(h);
    }
    return d;
  }

  public static string Name(uint tid) {
    IntPtr h = OpenThread(0x0800, false, tid);
    if (h == IntPtr.Zero) return "?";
    IntPtr s; string name = "";
    if (GetThreadDescription(h, out s) >= 0 && s != IntPtr.Zero) { name = Marshal.PtrToStringUni(s); LocalFree(s); }
    CloseHandle(h);
    return name;
  }

  static IntPtr proc = IntPtr.Zero, fta, gmb;
  public static void Init(int pid, string symPath) {
    proc = OpenProcess(PROCESS_ALL, false, (uint)pid);
    SymSetOptions(0x2 | 0x4 | 0x10); // undecorate, deferred loads, line numbers
    if (!SymInitialize(proc, symPath, true)) throw new Exception("SymInitialize failed");
    IntPtr dbg = LoadLibrary("dbghelp.dll");
    fta = GetProcAddress(dbg, "SymFunctionTableAccess64");
    gmb = GetProcAddress(dbg, "SymGetModuleBase64");
  }

  static string Sym(ulong addr) {
    IntPtr buf = Marshal.AllocHGlobal(88 + 1024);
    try {
      for (int i = 0; i < 88 + 1024; i++) Marshal.WriteByte(buf, i, 0);
      Marshal.WriteInt32(buf, 0, 88);
      Marshal.WriteInt32(buf, 80, 1024);
      ulong disp;
      if (!SymFromAddr(proc, addr, out disp, buf)) return "0x" + addr.ToString("X");
      return Marshal.PtrToStringAnsi(new IntPtr(buf.ToInt64() + 84));
    } finally { Marshal.FreeHGlobal(buf); }
  }

  // One stack of the thread, innermost first, up to `depth` frames.
  public static List<string> Stack(uint tid, int depth) {
    var frames = new List<string>();
    IntPtr t = OpenThread(THREAD_ALL, false, tid);
    if (t == IntPtr.Zero) return frames;
    IntPtr raw = Marshal.AllocHGlobal(1232 + 16);
    IntPtr ctx = new IntPtr((raw.ToInt64() + 15) & ~15L);
    IntPtr sf = Marshal.AllocHGlobal(0x200);
    try {
      for (int i = 0; i < 1232; i++) Marshal.WriteByte(ctx, i, 0);
      for (int i = 0; i < 0x200; i++) Marshal.WriteByte(sf, i, 0);
      Marshal.WriteInt32(ctx, 0x30, 0x10000B); // CONTEXT_FULL (AMD64)
      SuspendThread(t);
      try {
        if (!GetThreadContext(t, ctx)) return frames;
        ulong rip = (ulong)Marshal.ReadInt64(ctx, 0xF8), rsp = (ulong)Marshal.ReadInt64(ctx, 0x98), rbp = (ulong)Marshal.ReadInt64(ctx, 0xA0);
        Marshal.WriteInt64(sf, 0, (long)rip); Marshal.WriteInt32(sf, 12, 3);
        Marshal.WriteInt64(sf, 32, (long)rbp); Marshal.WriteInt32(sf, 44, 3);
        Marshal.WriteInt64(sf, 48, (long)rsp); Marshal.WriteInt32(sf, 60, 3);
        var addrs = new List<ulong>();
        while (addrs.Count < depth && StackWalk64(0x8664, proc, t, sf, ctx, IntPtr.Zero, fta, gmb, IntPtr.Zero)) {
          ulong pc = (ulong)Marshal.ReadInt64(sf, 0);
          if (pc == 0) break;
          addrs.Add(pc);
        }
        ResumeThread(t);
        foreach (var a in addrs) frames.Add(Sym(a));
        return frames;
      } finally { ResumeThread(t); }
    } finally { Marshal.FreeHGlobal(raw); Marshal.FreeHGlobal(sf); CloseHandle(t); }
  }
}
'@
$exe = (Get-Process -Id $ProcessId).Path
[Hot]::Init($ProcessId, (Split-Path $exe))
. (Join-Path $PSScriptRoot 'procinfo.ps1')
$hz = [ProcInfo]::CyclesPerSecond()
$timeline = Join-Path $Out 'threads.csv'
't_s,tid,name,cpu_pct' | Set-Content -Encoding utf8 $timeline
$prev = [Hot]::Cycles($ProcessId)
$hotCount = @{}
$sampled = @{}
$clock = [Diagnostics.Stopwatch]::StartNew()
$last = 0.0
while ($clock.Elapsed.TotalSeconds -lt $Seconds) {
    Start-Sleep -Seconds $Every
    if (-not (Get-Process -Id $ProcessId -ErrorAction SilentlyContinue)) { break }
    $now = [Hot]::Cycles($ProcessId)
    $dt = $clock.Elapsed.TotalSeconds - $last; $last = $clock.Elapsed.TotalSeconds
    $mem = [ProcInfo]::All() | Where-Object Pid -eq $ProcessId
    if ($mem) { '{0:F0},0,memory MB,{1:F1}' -f $last, ($mem.PrivateWs / 1MB) | Add-Content -Encoding utf8 $timeline }
    foreach ($tid in $now.Keys) {
        if (-not $prev.ContainsKey($tid)) { continue }
        $pct = ($now[$tid] - $prev[$tid]) / $hz / $dt * 100
        if ($pct -ge 1) { '{0:F0},{1},{2},{3:F1}' -f $last, $tid, ([Hot]::Name($tid)), $pct | Add-Content -Encoding utf8 $timeline }
        if ($pct -ge $Busy) { $hotCount[$tid] = 1 + [int]$hotCount[$tid] } else { $hotCount[$tid] = 0 }
        if ($hotCount[$tid] -ge 2 -and -not $sampled[$tid]) {
            $sampled[$tid] = $true
            $name = [Hot]::Name($tid)
            "{0:F0}s thread {1} '{2}' at {3:F0}% - sampling" -f $last, $tid, $name, $pct
            $counts = @{}
            foreach ($k in 1..300) {
                $frames = [Hot]::Stack([uint32]$tid, 40)
                if ($frames.Count -gt 0) { $key = ($frames | Select-Object -First 25) -join "`n  <- "; $counts[$key] = 1 + [int]$counts[$key] }
                Start-Sleep -Milliseconds 20
            }
            $report = Join-Path $Out "hot-$tid.txt"
            "Thread $tid '$name' at $([int]$pct)% after $([int]$last) s; $((($counts.Values | Measure-Object -Sum).Sum)) samples" | Set-Content -Encoding utf8 $report
            $counts.GetEnumerator() | Sort-Object Value -Descending | Select-Object -First 8 | ForEach-Object { "`n=== $($_.Value) samples`n  $($_.Key)" } | Add-Content -Encoding utf8 $report
            "  wrote $report"
        }
    }
    $prev = $now
}
'done'
