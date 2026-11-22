#!/usr/bin/env python3
"""Runs a client and samples its whole process tree (the WebKit helpers included): CPU seconds,
RSS and PSS, every 0.5 s. Prints one JSON object; the phases are marked by the caller's
timeline (seconds from start) so that idle and busy stretches can be told apart.

usage: measure.py <seconds> <label> -- <command...>   (environment passed through)
"""
import json
import os
import signal
import subprocess
import sys
import time

TICK = os.sysconf("SC_CLK_TCK")


def tree(root):
    pids, frontier = {root}, [root]
    children = {}
    for entry in os.listdir("/proc"):
        if entry.isdigit():
            try:
                with open(f"/proc/{entry}/stat") as f:
                    ppid = int(f.read().rsplit(")", 1)[1].split()[1])
                children.setdefault(ppid, []).append(int(entry))
            except OSError:
                pass
    while frontier:
        for child in children.get(frontier.pop(), []):
            if child not in pids:
                pids.add(child)
                frontier.append(child)
    return pids


def sample(pids):
    cpu = rss = pss = 0
    names = []
    by_proc = {}
    for pid in pids:
        try:
            with open(f"/proc/{pid}/stat") as f:
                fields = f.read().rsplit(")", 1)[1].split()
            own = (int(fields[11]) + int(fields[12])) / TICK
            cpu += own
            with open(f"/proc/{pid}/smaps_rollup") as f:
                for line in f:
                    if line.startswith("Rss:"):
                        rss += int(line.split()[1])
                    elif line.startswith("Pss:"):
                        pss += int(line.split()[1])
            with open(f"/proc/{pid}/comm") as f:
                name = f.read().strip()
            names.append(name)
            by_proc[name] = round(by_proc.get(name, 0) + own, 2)
        except OSError:
            pass
    return cpu, rss // 1024, pss // 1024, sorted(names), by_proc


def main():
    seconds, label = float(sys.argv[1]), sys.argv[2]
    command = sys.argv[sys.argv.index("--") + 1 :]
    start = time.monotonic()
    proc = subprocess.Popen(command, stdout=subprocess.DEVNULL, stderr=open(f"{label}.stderr", "w"),
                            start_new_session=True)
    samples = []
    while time.monotonic() - start < seconds and proc.poll() is None:
        t = time.monotonic() - start
        cpu, rss, pss, names, by_proc = sample(tree(proc.pid))
        samples.append({"t": round(t, 2), "cpu": round(cpu, 2), "rss_mb": rss, "pss_mb": pss,
                        "procs": len(names), "by_proc": by_proc})
        time.sleep(0.5)
    final_names = sample(tree(proc.pid))[3]
    os.killpg(proc.pid, signal.SIGTERM)
    try:
        proc.wait(5)
    except subprocess.TimeoutExpired:
        os.killpg(proc.pid, signal.SIGKILL)
    print(json.dumps({"label": label, "processes": final_names, "samples": samples}))


main()
