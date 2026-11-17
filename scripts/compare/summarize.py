"""Summarizes a compare.ps1 run: Rill against the three pop-out chats, as tables.

python summarize.py <run folder>

Writes summary.json and summary.md into the run folder and prints the Markdown.
CPU comes from the cycle counter; percentages are of one thread (100 = one core busy) unless
they say "of the PC".
"""
import csv
import json
import os
import re
import statistics as st
import sys
from collections import defaultdict

run = sys.argv[1]
THREADS = os.cpu_count()


def path(name):
    return os.path.join(run, name)


def load_json(name, default=None):
    try:
        with open(path(name), encoding='utf-8-sig') as f:
            return json.load(f)
    except FileNotFoundError:
        return default


def pct(values, q):
    values = sorted(values)
    if not values:
        return 0.0
    k = (len(values) - 1) * q
    lo, hi = int(k), min(int(k) + 1, len(values) - 1)
    return values[lo] + (values[hi] - values[lo]) * (k - lo)


setup = load_json('setup.json')
read = load_json('read.json', {})
info = load_json('run.json', {})
procs_start = load_json('chrome-procs-start.json')
procs_end = load_json('chrome-procs-end.json')
HZ = float(open(path('cycles-per-second.txt'), encoding='utf-8-sig').read())
ptype = {p['id']: p['type'] for p in setup['processes'] + procs_start['processes'] + procs_end['processes']}
renderer = {v['pid']: k for k, v in setup['renderer'].items()}

# Samples per pid: time and every counter.
FIELDS = ['cycles', 'cpu_ms', 'private_ws_mb', 'private_mb', 'ws_mb', 'threads', 'handles', 'peak_ws_mb',
          'page_faults', 'read_ops', 'write_ops', 'other_ops', 'read_bytes', 'write_bytes', 'other_bytes',
          'context_switches']
by_pid = defaultdict(dict)
who = {}
with open(path('procs.csv'), encoding='utf-8-sig') as f:
    for r in csv.DictReader(f):
        pid, t = int(r['pid']), float(r['t_ms'])
        who[pid] = r['who']
        row = {k: float(r[k]) for k in FIELDS if k in r and r[k] != ''}
        row['cpu_cycles_ms'] = row['cycles'] / HZ * 1000
        by_pid[pid][t] = row
times = sorted({t for v in by_pid.values() for t in v})
span_s = (times[-1] - times[0]) / 1000


def label(pid):
    if who[pid] == 'rill':
        return 'Rill'
    if pid in renderer:
        return 'Chrome: ' + {'twitch': 'Twitch page', 'youtube': 'YouTube page', 'kick': 'Kick page'}[renderer[pid]]
    t = ptype.get(pid, 'other')
    return {'browser': 'Chrome: browser process', 'GPU': 'Chrome: GPU process', 'renderer': 'Chrome: other pages',
            'network.mojom.NetworkService': 'Chrome: network service'}.get(t, 'Chrome: other services')


groups = defaultdict(set)
for pid in by_pid:
    groups[label(pid)].add(pid)
    if who[pid] == 'chrome':
        groups['Chrome: all three pop-outs'].add(pid)
        if pid in renderer:
            groups['Chrome: the three pages'].add(pid)
        else:
            groups['Chrome: shared processes'].add(pid)


def level(pids, field):
    """The sum over pids of a level (memory, threads) at each sample time."""
    return [sum(by_pid[p][t].get(field, 0) for p in pids if t in by_pid[p]) for t in times]


def rate(pids, field, scale=1.0):
    """Per-interval rate of a cumulative counter, per second (CPU: percent of a thread)."""
    out = []
    for a, b in zip(times, times[1:]):
        used = 0.0
        for p in pids:
            s = by_pid[p]
            if a in s and b in s:
                used += s[b].get(field, 0) - s[a].get(field, 0)
            elif b in s:
                used += s[b].get(field, 0)  # started during the interval
        out.append(used / ((b - a) / 1000) * scale)
    return out


# GPU and whole-PC counters (counters.csv; gpu.csv in older runs).
counter_file = path('counters.csv') if os.path.exists(path('counters.csv')) else path('gpu.csv')
gpu_util = defaultdict(lambda: defaultdict(float))
gpu_3d = defaultdict(lambda: defaultdict(float))
gpu_ded = defaultdict(lambda: defaultdict(float))
gpu_shared = defaultdict(lambda: defaultdict(float))
system_cpu, system_avail = {}, {}
ctimes = set()
with open(counter_file, encoding='utf-8-sig') as f:
    for line in f:
        parts = line.strip().split('|')
        if len(parts) != 3:
            continue
        t, p, v = parts
        v = float(v)
        ctimes.add(t)
        if '\\processor(_total)\\' in p:
            system_cpu[t] = v
            continue
        if '\\memory\\available mbytes' in p:
            system_avail[t] = v
            continue
        m = re.search(r'pid_(\d+)_', p)
        if not m:
            continue
        pid = int(m.group(1))
        if 'utilization percentage' in p:
            gpu_util[pid][t] += v
            if 'engtype_3d' in p:
                gpu_3d[pid][t] += v
        elif 'dedicated usage' in p:
            gpu_ded[pid][t] += v / 1048576
        elif 'shared usage' in p:
            gpu_shared[pid][t] += v / 1048576
ctimes = sorted(ctimes)


def gpu(table, pids):
    per_t = [sum(table[p].get(t, 0) for p in pids) for t in ctimes]
    return {'avg': st.mean(per_t) if per_t else 0, 'peak': max(per_t) if per_t else 0}


ORDER = ['Rill', 'Chrome: all three pop-outs', 'Chrome: Twitch page', 'Chrome: YouTube page', 'Chrome: Kick page',
         'Chrome: the three pages', 'Chrome: shared processes', 'Chrome: GPU process', 'Chrome: browser process',
         'Chrome: network service', 'Chrome: other pages', 'Chrome: other services']
rows = []
for g in ORDER + sorted(set(groups) - set(ORDER)):
    pids = groups.get(g)
    if not pids:
        continue
    cpu = rate(pids, 'cpu_cycles_ms', 0.1)  # ms per s -> percent of a thread
    ticks = rate(pids, 'cpu_ms', 0.1)
    mem = level(pids, 'private_ws_mb')
    rows.append({
        'group': g, 'processes': len(pids),
        'cpu_avg': st.mean(cpu), 'cpu_median': st.median(cpu), 'cpu_p95': pct(cpu, 0.95), 'cpu_peak': max(cpu),
        'cpu_avg_of_pc': st.mean(cpu) / THREADS, 'cpu_seconds': st.mean(cpu) / 100 * span_s,
        'cpu_avg_tick_based': st.mean(ticks),
        'mem_start': mem[0], 'mem_end': mem[-1], 'mem_peak': max(mem), 'mem_avg': st.mean(mem),
        'private_bytes_end': level(pids, 'private_mb')[-1],
        'peak_working_set': max(level(pids, 'peak_ws_mb')),
        'threads_end': level(pids, 'threads')[-1], 'handles_end': level(pids, 'handles')[-1],
        'page_faults_per_s': st.mean(rate(pids, 'page_faults')),
        'context_switches_per_s': st.mean(rate(pids, 'context_switches')),
        'read_kb_per_s': st.mean(rate(pids, 'read_bytes')) / 1024,
        'write_kb_per_s': st.mean(rate(pids, 'write_bytes')) / 1024,
        'other_io_kb_per_s': st.mean(rate(pids, 'other_bytes')) / 1024,
        'io_ops_per_s': st.mean(rate(pids, 'read_ops')) + st.mean(rate(pids, 'write_ops')) + st.mean(rate(pids, 'other_ops')),
        'gpu_avg': gpu(gpu_util, pids)['avg'], 'gpu_peak': gpu(gpu_util, pids)['peak'],
        'gpu_3d_avg': gpu(gpu_3d, pids)['avg'],
        'gpu_dedicated_mb': gpu(gpu_ded, pids)['avg'], 'gpu_dedicated_peak_mb': gpu(gpu_ded, pids)['peak'],
        'gpu_shared_mb': gpu(gpu_shared, pids)['avg'],
    })
by_group = {r['group']: r for r in rows}

# Minute by minute: CPU and memory for Rill and the pop-outs.
timeline = []
per_min = max(1, int(round(60000 / (times[1] - times[0]))))
rill_cpu = rate(groups['Rill'], 'cpu_cycles_ms', 0.1)
chrome_cpu = rate(groups['Chrome: all three pop-outs'], 'cpu_cycles_ms', 0.1)
rill_mem = level(groups['Rill'], 'private_ws_mb')
chrome_mem = level(groups['Chrome: all three pop-outs'], 'private_ws_mb')
for m in range(0, len(rill_cpu), per_min):
    timeline.append({'minute': m // per_min + 1,
                     'rill_cpu': st.mean(rill_cpu[m:m + per_min]), 'chrome_cpu': st.mean(chrome_cpu[m:m + per_min]),
                     'rill_mem': rill_mem[min(m + per_min, len(rill_mem) - 1)],
                     'chrome_mem': chrome_mem[min(m + per_min, len(chrome_mem) - 1)]})

# Pages over time: JavaScript memory and page size.
pages = []
if os.path.exists(path('pages.jsonl')):
    with open(path('pages.jsonl'), encoding='utf-8-sig') as f:
        for line in f:
            line = line.strip()
            if line:
                pages.append(json.loads(line))
page_trend = {}
for name in ('twitch', 'youtube', 'kick'):
    seen = [(p['t_ms'], p['pages'].get(name)) for p in pages if p['pages'].get(name)]
    if seen:
        page_trend[name] = {'js_heap_start_mb': seen[0][1]['jsHeapMB'], 'js_heap_end_mb': seen[-1][1]['jsHeapMB'],
                            'dom_nodes_start': seen[0][1]['domNodes'], 'dom_nodes_end': seen[-1][1]['domNodes']}

# Rill's threads: average CPU over the run, busiest first.
threads = []
tfile = path('rill-threads/threads.csv')
if os.path.exists(tfile):
    acc = defaultdict(list)
    names = {}
    with open(tfile, encoding='utf-8-sig') as f:
        for r in csv.DictReader(f):
            if r['tid'] == '0':
                continue
            acc[r['tid']].append(float(r['cpu_pct']))
            names[r['tid']] = r['name'] or '(unnamed)'
    samples = max((len(v) for v in acc.values()), default=1)
    for tid, v in acc.items():
        threads.append({'thread': names[tid], 'id': tid, 'avg': sum(v) / samples, 'peak': max(v)})
    threads.sort(key=lambda x: -x['avg'])
hot = sorted(f for f in os.listdir(path('rill-threads'))) if os.path.isdir(path('rill-threads')) else []

# Cross-check: Chrome's own CPU accounting over the same window.
start = {p['id']: p['cpuTime'] for p in procs_start['processes']}
end = {p['id']: p['cpuTime'] for p in procs_end['processes']}
chrome_own = sum(end[i] - start.get(i, 0) for i in end) / ((procs_end['at'] - procs_start['at']) / 1000) * 100

stats = {}
if os.path.exists(path('rill-stats.txt')):
    for line in open(path('rill-stats.txt'), encoding='utf-8-sig'):
        k, _, v = line.strip().partition('=')
        if k:
            stats[k] = float(v)
irc = open(path('twitch-irc.txt'), encoding='utf-8-sig').read().strip() if os.path.exists(path('twitch-irc.txt')) else ''
cut_short = 0
for name in os.listdir(run):
    if name.endswith('.log'):
        cut_short += sum('animated image cut short' in line for line in open(path(name), encoding='utf-8', errors='replace'))

summary = {
    'run': info, 'seconds': span_s, 'samples': len(times), 'interval_s': (times[1] - times[0]) / 1000,
    'logical_processors': THREADS, 'cycles_per_second': HZ, 'groups': rows, 'timeline': timeline,
    'messages': {k: {'count': v['messages'], 'per_second': v['messages'] / v['seconds']} for k, v in read.items()},
    'youtube_mode': setup.get('youtubeMode'), 'twitch_irc': irc, 'rill_stats': stats, 'pages': page_trend,
    'rill_threads': threads[:8], 'rill_hot_thread_reports': [h for h in hot if h.startswith('hot-')],
    'animated_images_cut_short': cut_short,
    'system': {'cpu_avg_pct': st.mean(system_cpu.values()) if system_cpu else None,
               'cpu_peak_pct': max(system_cpu.values()) if system_cpu else None,
               'available_memory_min_mb': min(system_avail.values()) if system_avail else None},
    'cross_check': {'chrome_own_cpu_avg': chrome_own,
                    'chrome_cycles_cpu_avg': by_group['Chrome: all three pop-outs']['cpu_avg']},
}
json.dump(summary, open(path('summary.json'), 'w', encoding='utf-8'), indent=2)

# Markdown.
R, C = by_group['Rill'], by_group['Chrome: all three pop-outs']
out = []
w = out.append
w(f"# Rill vs three pop-out chats\n")
w(f"{span_s / 60:.0f} minutes, sampled every {summary['interval_s']:.0f} s ({len(times)} samples), {THREADS} logical processors.")
if info:
    w(f"Twitch {info.get('twitch')}, YouTube video {info.get('youtube_video')}, Kick {info.get('kick')}; Chrome {info.get('chrome')}; windows {info.get('window')}.\n")
w("## Rill vs the three pop-outs\n")
w("| | Rill | 3 pop-outs | Ratio |\n|---|---:|---:|---:|")
def line(name, key, fmt, ratio=True):
    a, b = R[key], C[key]
    r = f"{b / a:.1f}x" if ratio and a >= 0.01 and b >= 0.01 else ''
    w(f"| {name} | {fmt.format(a)} | {fmt.format(b)} | {r} |")
line('CPU, % of a thread, average', 'cpu_avg', '{:.1f}%')
line('CPU, median', 'cpu_median', '{:.1f}%')
line('CPU, 95th percentile', 'cpu_p95', '{:.1f}%')
line('CPU, peak (one sample)', 'cpu_peak', '{:.1f}%')
line('CPU, % of the whole PC', 'cpu_avg_of_pc', '{:.2f}%')
line('CPU time used', 'cpu_seconds', '{:.0f} s')
line('Memory at start', 'mem_start', '{:.0f} MB')
line('Memory at end', 'mem_end', '{:.0f} MB')
line('Memory, peak', 'mem_peak', '{:.0f} MB')
line('Memory, average', 'mem_avg', '{:.0f} MB')
line('Committed memory at end', 'private_bytes_end', '{:.0f} MB')
line('GPU use, average', 'gpu_avg', '{:.2f}%')
line('GPU use, peak', 'gpu_peak', '{:.2f}%')
line('Graphics memory (dedicated)', 'gpu_dedicated_mb', '{:.0f} MB')
line('Graphics memory (shared)', 'gpu_shared_mb', '{:.0f} MB')
line('Processes', 'processes', '{:.0f}')
line('Threads at end', 'threads_end', '{:.0f}')
line('Handles at end', 'handles_end', '{:.0f}')
line('Context switches per second', 'context_switches_per_s', '{:.0f}')
line('Page faults per second', 'page_faults_per_s', '{:.0f}')
line('Data read, KB/s', 'read_kb_per_s', '{:.1f}')
line('Data written, KB/s', 'write_kb_per_s', '{:.1f}')
line('Other I/O, KB/s', 'other_io_kb_per_s', '{:.1f}')
w("\nMemory is the private working set (Task Manager's Memory column). CPU from the cycle counter.")
w("Reads and writes count files and pipes, which includes Chrome's pages sending their drawing to")
w("its GPU process; network traffic isn't counted.\n")

w("## Every process group\n")
w("| Group | Procs | CPU avg | CPU p95 | CPU peak | Mem start | Mem end | Mem peak | GPU | GPU MB | Threads | Ctx sw/s |\n|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|")
for r in rows:
    w(f"| {r['group']} | {r['processes']} | {r['cpu_avg']:.1f}% | {r['cpu_p95']:.1f}% | {r['cpu_peak']:.1f}% | {r['mem_start']:.0f} | {r['mem_end']:.0f} | {r['mem_peak']:.0f} | {r['gpu_avg']:.2f}% | {r['gpu_dedicated_mb']:.0f} | {r['threads_end']:.0f} | {r['context_switches_per_s']:.0f} |")

w("\n## Minute by minute\n")
w("| Minute | Rill CPU | Pop-outs CPU | Rill memory | Pop-outs memory |\n|---:|---:|---:|---:|---:|")
for m in timeline:
    w(f"| {m['minute']} | {m['rill_cpu']:.1f}% | {m['chrome_cpu']:.1f}% | {m['rill_mem']:.0f} MB | {m['chrome_mem']:.0f} MB |")

w("\n## Chat\n")
for k, v in summary['messages'].items():
    w(f"- {k}: {v['count']} messages shown by the pop-out, {v['per_second']:.2f} a second")
if irc:
    w(f"- Twitch, counted from Twitch's chat server: {irc}")
w(f"- YouTube pop-out mode: {summary['youtube_mode']}")
if stats:
    w(f"- Rill over its whole run ({stats.get('seconds', 0):.0f} s): {stats.get('events', 0):.0f} events, {stats.get('renders', 0):.0f} redraws ({stats.get('renders', 0) / max(stats.get('seconds', 1), 1):.1f} a second), {stats.get('feed', 0):.0f} lines kept, {stats.get('images', 0):.0f} images")
w(f"- Animated images Rill cut short (broken files): {cut_short}")

if page_trend:
    w("\n## Pop-out pages\n")
    w("| Page | JS memory start | JS memory end | Page elements start | Page elements end |\n|---|---:|---:|---:|---:|")
    for k, v in page_trend.items():
        w(f"| {k} | {v['js_heap_start_mb']} MB | {v['js_heap_end_mb']} MB | {v['dom_nodes_start']} | {v['dom_nodes_end']} |")

if threads:
    w("\n## Rill's busiest threads\n")
    w("| Thread | Average | Peak |\n|---|---:|---:|")
    for t in threads[:6]:
        w(f"| {t['thread']} ({t['id']}) | {t['avg']:.1f}% | {t['peak']:.1f}% |")
    if summary['rill_hot_thread_reports']:
        w(f"\nStack samples of busy threads: {', '.join(summary['rill_hot_thread_reports'])}")

w("\n## The PC and cross-checks\n")
sysi = summary['system']
if sysi['cpu_avg_pct'] is not None:
    w(f"- Whole PC: {sysi['cpu_avg_pct']:.1f}% CPU on average, {sysi['cpu_peak_pct']:.1f}% at most; at least {sysi['available_memory_min_mb']:.0f} MB of memory free")
w(f"- Chrome's own CPU accounting: {chrome_own:.1f}% of a thread; cycle counts: {C['cpu_avg']:.1f}%")
w(f"- Windows' tick-based CPU time (not used): Rill {R['cpu_avg_tick_based']:.2f}%, pop-outs {C['cpu_avg_tick_based']:.1f}%")

md = '\n'.join(out) + '\n'
open(path('summary.md'), 'w', encoding='utf-8').write(md)
print(md)
