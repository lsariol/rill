# Reads every process's CPU, memory, I/O and context switches from Windows in one call, the way
# Task Manager does, so Chrome's sandboxed processes, which can't be opened, are counted too.
# CPU comes from the cycle counter: Windows' tick-based CPU time misses work done in short bursts.
# Dot-source it, then call [ProcInfo]::All() and [ProcInfo]::CyclesPerSecond().
Add-Type @'
using System;
using System.Collections.Generic;
using System.Runtime.InteropServices;
public static class ProcInfo {
  [DllImport("ntdll.dll")] static extern int NtQuerySystemInformation(int cls, IntPtr buf, int len, out int needed);
  public class Row { public long Pid, Parent, CpuTicks, Cycles, PrivateWs, Private, Ws, PeakWs, PageFaults, ReadOps, WriteOps, OtherOps, ReadBytes, WriteBytes, OtherBytes, ContextSwitches; public int Threads, Handles; public string Name; }
  [DllImport("kernel32.dll")] static extern bool QueryThreadCycleTime(IntPtr thread, out ulong cycles);
  [DllImport("kernel32.dll")] static extern IntPtr GetCurrentThread();
  // Cycle-counter ticks per second: spin one thread and compare its cycles with the clock.
  public static double CyclesPerSecond() {
    double best = 0;
    for (int i = 0; i < 3; i++) {
      ulong a, b; var sw = System.Diagnostics.Stopwatch.StartNew();
      QueryThreadCycleTime(GetCurrentThread(), out a);
      while (sw.ElapsedMilliseconds < 500) { }
      QueryThreadCycleTime(GetCurrentThread(), out b);
      best = Math.Max(best, (b - a) / sw.Elapsed.TotalSeconds);
    }
    return best;
  }
  public static List<Row> All() {
    int len = 1 << 20; IntPtr buf = IntPtr.Zero;
    while (true) {
      buf = Marshal.AllocHGlobal(len);
      int needed; int st = NtQuerySystemInformation(5, buf, len, out needed);
      if (st == 0) break;
      Marshal.FreeHGlobal(buf);
      if (st != unchecked((int)0xC0000004)) throw new Exception("NtQuerySystemInformation " + st.ToString("X"));
      len = Math.Max(needed, len) * 2;
    }
    var rows = new List<Row>();
    try {
      long off = 0;
      while (true) {
        IntPtr p = new IntPtr(buf.ToInt64() + off);
        int next = Marshal.ReadInt32(p, 0);
        var r = new Row();
        r.Threads = Marshal.ReadInt32(p, 4);
        r.PrivateWs = Marshal.ReadInt64(p, 8);
        r.Cycles = Marshal.ReadInt64(p, 24);
        r.CpuTicks = Marshal.ReadInt64(p, 40) + Marshal.ReadInt64(p, 48);
        ushort nameLen = (ushort)Marshal.ReadInt16(p, 56);
        IntPtr nameBuf = Marshal.ReadIntPtr(p, 64);
        r.Name = nameBuf == IntPtr.Zero ? "" : Marshal.PtrToStringUni(nameBuf, nameLen / 2);
        r.Pid = Marshal.ReadInt64(p, 80);
        r.Parent = Marshal.ReadInt64(p, 88);
        r.Handles = Marshal.ReadInt32(p, 96);
        r.Ws = Marshal.ReadInt64(p, 144);
        r.Private = Marshal.ReadInt64(p, 200);
        r.PeakWs = Marshal.ReadInt64(p, 136);
        r.PageFaults = (uint)Marshal.ReadInt32(p, 128);
        r.ReadOps = Marshal.ReadInt64(p, 208); r.WriteOps = Marshal.ReadInt64(p, 216); r.OtherOps = Marshal.ReadInt64(p, 224);
        r.ReadBytes = Marshal.ReadInt64(p, 232); r.WriteBytes = Marshal.ReadInt64(p, 240); r.OtherBytes = Marshal.ReadInt64(p, 248);
        for (int t = 0; t < r.Threads; t++) r.ContextSwitches += (uint)Marshal.ReadInt32(p, 256 + t * 80 + 64);
        rows.Add(r);
        if (next == 0) break;
        off += next;
      }
    } finally { Marshal.FreeHGlobal(buf); }
    return rows;
  }
}
'@
