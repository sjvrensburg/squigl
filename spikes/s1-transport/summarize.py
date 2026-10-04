#!/usr/bin/env python3
"""Prints spike S1's results as a table, marking each run against the bar:
at 30 fps, a 3 MB frame (or larger) must keep 30 fps with p95 latency <= 50 ms on
under one core in total."""
import json
import sys

rows = [json.loads(l) for path in sys.argv[1:] for l in open(path) if l.strip()]
print(f"{'backend':8} {'transport':9} {'MB':>5} {'rate':>5} {'fps':>7} {'p50':>6} {'p95':>6} "
      f"{'max':>6} {'cores':>5} {'corrupt':>7} {'reordered':>9}  bar")
for r in rows:
    if "error" in r and "fps" not in r:
        print(f"{r.get('backend', '?'):8} {r.get('transport', '?'):9} "
              f"{r.get('bytes', 0) / 1e6:5.1f}  ERROR {r['error']}")
        continue
    paced = r.get("target_fps", 0) > 0
    bar = ""
    if paced:
        ok = (r["fps"] >= 0.97 * r["target_fps"] and r["latency_ms_p95"] <= 50
              and r.get("cpu_cores", 0) < 1.0)
        bar = "pass" if ok else "FAIL"
    print(f"{r['backend']:8} {r['transport']:9} {r['mb']:5.1f} "
          f"{('%d' % r['target_fps']) if paced else 'max':>5} {r['fps']:7.1f} "
          f"{r['latency_ms_p50']:6.1f} {r['latency_ms_p95']:6.1f} {r['latency_ms_max']:6.1f} "
          f"{r.get('cpu_cores', float('nan')):5.2f} {r['mismatched']:7} {r['out_of_order']:9}  {bar}")
gpus = sorted({r.get("gpu", "") for r in rows})
print("\nGPU:", "; ".join(g for g in gpus if g) or "unknown")
