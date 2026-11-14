"""Test T7, first part: what one heddle hook run costs.

  python3 -I day0/cost.py HEDDLE_BINARY [RUNS]

Runs each hook line RUNS times (default 1000) on the recorded payloads in
adapters/claude-code/payloads/, plus PostToolUse with Bash results of 32 KiB
(about Claude Code's own cap on Bash output), 256 KiB and 1 MiB, against a
scratch state folder. Prints the 50th and 95th percentile and the worst run,
in milliseconds, wall clock, process start included. The gate is 30 ms at the
95th percentile over the recorded payloads; the larger results show how the
cost grows with size. No model is called.
"""

import json
import os
import subprocess
import sys
import tempfile
import time

here = os.path.dirname(os.path.abspath(__file__))
payloads = os.path.join(here, "..", "adapters", "claude-code", "payloads")


def measure(binary, event, payload, runs, root, state):
    args = [binary, "hook", "claude-code", event, "--root", root, "--state", state]
    times = []
    for _ in range(runs):
        t0 = time.perf_counter_ns()
        p = subprocess.run(args, input=payload, capture_output=True)
        times.append((time.perf_counter_ns() - t0) / 1e6)
        if p.returncode != 0:
            sys.exit(f"{event}: exit {p.returncode}")
    times.sort()
    pick = lambda q: times[min(len(times) - 1, int(q * len(times)))]
    return pick(0.50), pick(0.95), times[-1], p.stdout


def main():
    binary = os.path.abspath(sys.argv[1])
    runs = int(sys.argv[2]) if len(sys.argv) > 2 else 1000
    with tempfile.TemporaryDirectory() as root, tempfile.TemporaryDirectory() as state:
        cases = []
        for name in sorted(os.listdir(payloads)):
            event = name.split(".")[0].split("-")[0]
            with open(os.path.join(payloads, name), "rb") as f:
                cases.append((name, event, f.read()))
        recorded = len(cases)
        with open(os.path.join(payloads, "PostToolUse.json"), "rb") as f:
            post = json.load(f)
        line = "lorem ipsum dolor sit amet, 42 items\n"
        for kib in (32, 256, 1024):
            big = dict(post)
            big["tool_response"] = dict(post["tool_response"], stdout=line * (kib * 1024 // len(line)))
            cases.append((f"PostToolUse, {kib} KiB result", "PostToolUse", json.dumps(big).encode()))
        print(f"{'case':<34} {'p50':>7} {'p95':>7} {'worst':>7}  printed")
        worst95 = 0.0
        for i, (name, event, payload) in enumerate(cases):
            p50, p95, worst, out = measure(binary, event, payload, runs, root, state)
            if i < recorded:
                worst95 = max(worst95, p95)
            print(f"{name:<34} {p50:7.2f} {p95:7.2f} {worst:7.2f}  {'nothing' if not out else out[:40]}")
        verdict = "PASS" if worst95 <= 30 else "FAIL"
        print(f"gate, recorded payloads (95th percentile <= 30 ms): {verdict} (highest p95 {worst95:.2f} ms)")


main()
