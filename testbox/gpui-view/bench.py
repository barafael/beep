#!/usr/bin/env python3
"""Measures b33p-slint and b33p-gpui against each other, on the mock, in a display of their own
(testbox/tools `with-display`, in a container) that draws on the host's GPU through its render
node. Nothing of the host's desktop is ever captured or shown to the clients: what is measured
comes from /proc, from the drm fdinfo of the client's own file descriptors, and from the
Wayland requests the client sends.

  bench.py run [--rounds 5] [--soak 120] [--frames-rounds 1] [--scale SPEC] [--out DIR]
  bench.py report DIR

`run` builds nothing: it needs both binaries built as the `testbox` profile (a release build
that keeps the debug assertions the mock needs), with the harness:

  cargo build --profile testbox -p b33p-slint -p b33p-gpui \\
      --features b33p-slint/harness,b33p-gpui/harness

and the image of Containerfile.bench beside this script (`podman build -t b33p-bench-view -f
testbox/gpui-view/Containerfile.bench testbox/gpui-view`), and the tools of view.sh's header
(`with-display`, built with WITH_DISPLAY_RENDERER support).

Each round runs both clients, in alternating order, in a container of their own each:
  static  startup, idle at the list, idle in a long chat, scrolling by the mouse wheel, switching
          between chats, resizing, typing with the keyboard, and the details and media panels
  live    messages arriving in the open chat, then a soak of that, for memory growth
  sync    catching up with a backlog of messages the mock sends in one burst
  frames  the scrolling again, with the Wayland requests logged, for the gaps
          between frames (the logging costs processor time: those runs are not the others')
Every number is a /proc or drm counter read before and after, over the whole phase.
"""

import argparse
import collections
import json
import math
import os
import re
import socket
import statistics
import struct
import subprocess
import sys
import time

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.abspath(os.path.join(HERE, "../.."))
TICK = os.sysconf("SC_CLK_TCK")
IMAGE = os.environ.get("BENCH_IMAGE", "localhost/b33p-bench-view")
RENDER_NODE = os.environ.get("BENCH_RENDER_NODE", "/dev/dri/renderD128")
# Chats, a long thread, a big group, pictures of a size a camera's thumbnail has.
SCALE = "chats=200,messages=20000,members=500,picture=640"
BINARIES = {
    # BENCH_SLINT, BENCH_GPUI: other binaries, to measure a build against another's.
    "slint": os.environ.get("BENCH_SLINT", os.path.join(ROOT, "target/testbox/b33p")),
    "gpui": os.environ.get("BENCH_GPUI", os.path.join(ROOT, "target/testbox/b33p-gpui")),
}
WIDTH, HEIGHT = 1280, 900
GEOMETRY = f"{WIDTH}x{HEIGHT}"


def sh(*args, **kw):
    return subprocess.run(args, capture_output=True, text=True, **kw).stdout


# ---------------------------------------------------------------------------------------
# Input through the display's VNC "interact" socket: real keys and wheel turns, which both
# clients take from the compositor alike.


class Vnc:
    def __init__(self, path):
        directory, name = os.path.split(path)
        cwd = os.getcwd()
        os.chdir(directory)  # a socket's path may not be long
        try:
            self.s = socket.socket(socket.AF_UNIX)
            self.s.connect(name)
        finally:
            os.chdir(cwd)
        self.s.recv(12)
        self.s.sendall(b"RFB 003.008\n")
        count = self.s.recv(1)[0]
        self.s.recv(count)
        self.s.sendall(bytes([1]))  # no security
        self.s.recv(4)
        self.s.sendall(bytes([1]))  # shared
        header = self.s.recv(24)
        self.s.recv(struct.unpack(">I", header[20:24])[0])
        self.s.settimeout(1)

    def key(self, keysym, down):
        self.s.sendall(struct.pack(">BBHI", 4, down, 0, keysym))

    def tap(self, keysym):
        self.key(keysym, 1)
        self.key(keysym, 0)

    def pointer(self, x, y, mask=0):
        self.s.sendall(struct.pack(">BBHH", 5, mask, x, y))

    def wheel(self, x, y, up):
        bit = 8 if up else 16
        self.pointer(x, y, bit)
        self.pointer(x, y, 0)

    def close(self):
        self.s.close()


# ---------------------------------------------------------------------------------------
# What is read of the client.


class Probe:
    def __init__(self, pid):
        self.pid = pid

    def cpu(self):
        f = open(f"/proc/{self.pid}/stat").read().rsplit(")", 1)[1].split()
        return (int(f[11]) + int(f[12])) / TICK

    def threads(self):
        return len(os.listdir(f"/proc/{self.pid}/task"))

    def switches(self):
        total = 0
        for line in open(f"/proc/{self.pid}/status"):
            if line.startswith(("voluntary_ctxt_switches", "nonvoluntary_ctxt_switches")):
                total += int(line.split()[1])
        return total

    def memory(self):
        r = {}
        for line in open(f"/proc/{self.pid}/smaps_rollup"):
            p = line.split()
            if len(p) >= 2 and p[1].isdigit():
                r[p[0].rstrip(":")] = int(p[1])
        return {"rss_mb": r["Rss"] / 1024, "pss_mb": r["Pss"] / 1024, "anon_mb": r["Anonymous"] / 1024}

    def gpu(self):
        """The GPU time the client has used, in seconds, and the video and shared memory it
        holds in MB, from the drm fdinfo of its descriptors (one per drm client)."""
        clients = {}
        for fd in os.listdir(f"/proc/{self.pid}/fd"):
            try:
                if "/dev/dri/" not in os.readlink(f"/proc/{self.pid}/fd/{fd}"):
                    continue
                info = open(f"/proc/{self.pid}/fdinfo/{fd}").read()
            except OSError:
                continue
            fields = dict(l.split(":", 1) for l in info.splitlines() if l.startswith("drm-"))
            clients[fields.get("drm-client-id", fd).strip()] = fields
        ns = vram = gtt = 0
        for fields in clients.values():
            for key, value in fields.items():
                number = int(value.split()[0]) if value.split() and value.split()[0].isdigit() else 0
                if key.startswith("drm-engine-") and key.endswith(("gfx", "compute", "dma")):
                    ns += number
                elif key == "drm-total-vram":
                    vram += number
                elif key == "drm-total-gtt":
                    gtt += number
        return {"gpu_s": ns / 1e9, "vram_mb": vram / 1024, "gtt_mb": gtt / 1024}

    def sample(self):
        return {"t": time.time(), "cpu": self.cpu(), "sw": self.switches(), **self.gpu()}


def delta(before, after, probe):
    wall = after["t"] - before["t"]
    return {
        "wall_s": round(wall, 2),
        "cpu_s": round(after["cpu"] - before["cpu"], 3),
        "cpu_pct": round(100 * (after["cpu"] - before["cpu"]) / wall, 1),
        "gpu_s": round(after["gpu_s"] - before["gpu_s"], 3),
        "wakeups_per_s": round((after["sw"] - before["sw"]) / wall, 1),
        "vram_mb": round(after["vram_mb"], 1),
        "gtt_mb": round(after["gtt_mb"], 1),
        **{k: round(v, 1) for k, v in probe.memory().items()},
        "threads": probe.threads(),
    }


# ---------------------------------------------------------------------------------------
# One client, in a display of its own.


class Run:
    def __init__(self, ui, label, out, scale, frames=False):
        self.ui, self.label, self.out, self.scale, self.frames = ui, label, out, scale, frames
        # BENCH_PREFIX: another worktree measuring at the same time needs container names of its own.
        self.name = f"{os.environ.get('BENCH_PREFIX', 'b33p-bench')}-{label}"
        self.dir = os.path.join(out, label)
        self.view, self.run, self.data = (os.path.join(self.dir, d) for d in ("view", "run", "data"))
        self.n = 0

    def start(self):
        subprocess.run(["podman", "rm", "-f", self.name], capture_output=True)
        subprocess.run(["rm", "-rf", self.dir])
        for d in (self.view, self.run, self.data):
            os.makedirs(d)
        program = ["b33p"]
        if self.frames:
            # Only the client's requests: the compositor's would be in the same log.
            program = ["sh", "-c", "WAYLAND_DEBUG=client exec b33p"]
        t0 = time.time()
        subprocess.run(
            ["podman", "run", "-d", "--rm", "--name", self.name, "--userns=keep-id",
             "--security-opt", "label=disable", "--device", RENDER_NODE,
             "-v", f"{BINARIES[self.ui]}:/usr/local/bin/b33p:ro",
             "-v", f"{ROOT}/target/testbox-workspaces/debug/with-display:/usr/local/bin/with-display:ro",
             "-v", f"{self.view}:/run/view", "-v", f"{self.run}:/run/b33p", "-v", f"{self.data}:/tmp/home",
             "-e", "HOME=/tmp/home", "-e", "B33P_MOCK=1", "-e", f"B33P_MOCK_SCALE={self.scale}",
             "-e", "B33P_AGENT_SOCKET=/run/b33p/agent.sock", "-e", "RUST_LOG=warn",
             "-e", "WITH_DISPLAY_RENDERER=gles2", "-e", f"WLR_RENDER_DRM_DEVICE={RENDER_NODE}",
             IMAGE, "with-display", "/run/view", GEOMETRY, "--", *program],
            check=True, capture_output=True, text=True)
        while not os.path.exists(os.path.join(self.view, "interact")):
            time.sleep(0.05)
        self.vnc = Vnc(os.path.join(self.view, "interact"))
        # A keyboard in the compositor's seat, before the window: without one no window is
        # ever active (GPUI takes a keyboard's focus for it), and a window that isn't, isn't
        # a window as a desktop has it.
        self.vnc.tap(0xFFE1)
        time.sleep(0.5)
        sock = os.path.join(self.run, "agent.sock")
        while not os.path.exists(sock):
            time.sleep(0.05)
        cwd = os.getcwd()
        os.chdir(self.run)
        try:
            self.s = socket.socket(socket.AF_UNIX)
            self.s.connect("agent.sock")
        finally:
            os.chdir(cwd)
        self.f = self.s.makefile("rw")
        self.t0 = t0
        self.call("snapshot")
        self.harness_ready_s = time.time() - t0
        rows = {r.split()[1]: int(r.split()[0]) for r in sh("podman", "top", self.name, "hpid", "comm").split("\n")[1:] if r.strip()}
        self.pid = rows["b33p"]
        self.probe = Probe(self.pid)

    def call(self, method, **kw):
        self.n += 1
        self.f.write(json.dumps({"id": self.n, "method": method, **kw}) + "\n")
        self.f.flush()
        return json.loads(self.f.readline())

    def act(self, **action):
        answer = self.call("act", action=action)
        if "error" in answer:
            print(f"  action {action.get('type')} refused: {answer['error']}", file=sys.stderr)

    def snapshot(self):
        return self.call("snapshot").get("result", {})

    def wait_for_list(self):
        """Until the chat list has its conversations; the mock links after two seconds."""
        while True:
            if self.snapshot().get("conversations"):
                return time.time() - self.t0
            time.sleep(0.05)

    def phase(self, name, body):
        before = self.probe.sample()
        t_from = time.time()
        body()
        after = self.probe.sample()
        self.windows[name] = (t_from, time.time())
        self.result[name] = delta(before, after, self.probe)

    def stop(self):
        try:
            self.vnc.close()
        except (AttributeError, OSError):
            pass
        subprocess.run(["podman", "rm", "-f", self.name], capture_output=True)

    def logs(self):
        r = subprocess.run(["podman", "logs", self.name], capture_output=True, text=True)
        return r.stdout + r.stderr


def idle(seconds):
    return lambda: time.sleep(seconds)


# The chats the switching goes through: contacts and groups of the added ones, the long thread,
# the big group, and the mock's own.
SWITCH = ["c:long", "g:group-crowd", "c:chat-1", "g:group-0", "c:bob", "g:rust", "c:chat-3", "c:alice", "g:group-2", "c:chat-5"]


def scenario_static(r):
    r.result, r.windows = {}, {}
    r.result["startup"] = {"harness_ready_s": round(r.harness_ready_s, 2), "chat_list_s": round(r.wait_for_list(), 2)}
    time.sleep(5)
    r.result["settled"] = {**r.probe.memory(), **r.probe.gpu(), "threads": r.probe.threads()}
    r.phase("idle_list_15s", idle(15))

    r.act(type="select", conversation="c:long")
    time.sleep(1.5)
    r.phase("idle_chat_first_10s", idle(10))
    time.sleep(10)
    r.phase("idle_chat_after_20s", idle(10))

    def scroll(up, seconds):
        def body():
            x, y = 800, 450
            r.vnc.pointer(x, y)
            end = time.time() + seconds
            while time.time() < end:
                r.vnc.wheel(x, y, up)
                time.sleep(0.016)
        return body
    r.phase("scroll_up_8s", scroll(True, 8))
    time.sleep(1)
    r.phase("scroll_down_8s", scroll(False, 8))
    time.sleep(1)

    def switch():
        for i in range(100):
            r.act(type="select", conversation=SWITCH[i % len(SWITCH)])
            time.sleep(0.08)
    r.phase("switch_100_chats", switch)
    time.sleep(2)

    def resize():
        for w, h in ((900, 700), (1600, 1000), (1280, 900), (700, 800), (1280, 900), (1000, 600)):
            r.act(type="resize", width=w, height=h)
            time.sleep(0.5)
    r.phase("resize_sweep", resize)

    r.act(type="select", conversation="c:long")
    time.sleep(2)

    def typing():
        for c in "the meeting was moved to friday so see you there and call me when you land ok":
            r.vnc.tap(ord(c))
            time.sleep(0.066)
    r.phase("type_80_keys", typing)
    r.act(type="type", text="")

    def panels():
        r.act(type="select", conversation="g:group-crowd")
        time.sleep(1.5)
        r.act(type="open_details")
        time.sleep(3)
        r.act(type="close_details")
        r.act(type="select", conversation="c:long")
        time.sleep(1.5)
        for _ in range(3):
            r.act(type="open_media")
            time.sleep(2)
            r.act(type="close_media")
            time.sleep(0.5)
    r.phase("panels_details_media", panels)
    time.sleep(3)
    r.result["after"] = {**r.probe.memory(), **r.probe.gpu(), "threads": r.probe.threads()}
    return r.result


def scenario_live(r, soak):
    r.result, r.windows = {}, {}
    r.wait_for_list()
    r.act(type="select", conversation="c:long")
    time.sleep(3)
    r.phase("incoming_5_per_s_30s", idle(30))
    series = []
    end = time.time() + soak
    first = r.probe.sample()
    while time.time() < end:
        time.sleep(10)
        m = r.probe.memory()
        series.append((time.time() - first["t"], m["rss_mb"], m["anon_mb"], r.probe.gpu()["vram_mb"]))
    last = r.probe.sample()
    r.result["soak"] = {**delta(first, last, r.probe),
                        "rss_slope_mb_per_min": slope([(t, v) for t, v, _, _ in series[len(series) // 2:]]),
                        "anon_slope_mb_per_min": slope([(t, v) for t, _, v, _ in series[len(series) // 2:]]),
                        "messages_in_chat": len(r.snapshot().get("messages", []))}
    return r.result


def slope(points):
    """Megabytes a minute, by least squares over (seconds, megabytes)."""
    if len(points) < 3:
        return None
    xs, ys = [p[0] for p in points], [p[1] for p in points]
    mx, my = statistics.mean(xs), statistics.mean(ys)
    den = sum((x - mx) ** 2 for x in xs)
    return round(60 * sum((x - mx) * (y - my) for x, y in points) / den, 2) if den else None


def scenario_sync(r):
    """Catching up: the mock sends the backlog in one burst before it says it has synced, as the
    backend does for what came while the client was away. The time is from the process's start
    (the mock itself waits two seconds to link, which both clients wait alike) until the state
    has synced and its unread count has stopped rising; the polling is itself the probe of the
    window: a snapshot is answered on the window's own thread, so its time is how long the
    window was not responding."""
    r.result, r.windows = {}, {}
    before = r.probe.sample()
    latencies, last, since, synced_at = [], -1, None, None
    while True:
        t = time.time()
        snap = r.snapshot()
        latencies.append(time.time() - t)
        unread = snap.get("unread_total", 0)
        if snap.get("synced") and synced_at is None:
            synced_at = time.time() - r.t0
        if snap.get("synced") and unread == last and unread > 0:
            if since is None:
                since = t
            elif t - since > 1.5:
                break
        else:
            since, last = None, unread
        if time.time() - r.t0 > 300:
            break
        time.sleep(0.05)
    done = time.time() - r.t0 - 1.5
    latencies.sort()
    after = r.probe.sample()
    r.result["sync"] = {
        **delta(before, after, r.probe),
        "synced_flag_s": round(synced_at or 0, 2),
        "caught_up_s": round(done, 2),
        "unread_total": unread,
        "snapshot_ms_p50": round(1000 * percentile(latencies, 50), 1),
        "snapshot_ms_p95": round(1000 * percentile(latencies, 95), 1),
        "snapshot_ms_max": round(1000 * latencies[-1], 1),
    }
    r.result["sync"]["cpu_total_s"] = round(after["cpu"], 2)
    return r.result


def scenario_frames(r):
    """The scrolling and the arriving of the other scenarios, with the requests logged."""
    r.result, r.windows = {}, {}
    r.wait_for_list()
    r.act(type="select", conversation="c:long")
    time.sleep(3)

    def scroll():
        x, y = 800, 450
        r.vnc.pointer(x, y)
        end = time.time() + 8
        while time.time() < end:
            r.vnc.wheel(x, y, True)
            time.sleep(0.016)
    r.phase("scroll_up_8s", scroll)
    time.sleep(1)
    r.phase("idle_chat_10s", idle(10))
    stamps = collections.defaultdict(list)
    for line in r.logs().splitlines():
        if line.startswith("[") and "-> wl_surface" in line and ".commit" in line:
            h, m, x = line[1:line.index("]")].split(":")
            stamps["all"].append(int(h) * 3600 + int(m) * 60 + float(x))
    out = {}
    for name, (a, b) in r.windows.items():
        lo, hi = a % 86400, b % 86400
        ts = [t for t in stamps["all"] if lo <= t <= hi]
        gaps = sorted(1000 * (y - x) for x, y in zip(ts, ts[1:]) if y - x < 0.5)
        out[name] = {
            "frames": len(ts),
            "frames_per_s": round(len(ts) / (b - a), 1),
            "gap_ms_p50": round(percentile(gaps, 50), 1),
            "gap_ms_p95": round(percentile(gaps, 95), 1),
            "gap_ms_p99": round(percentile(gaps, 99), 1),
            "gap_ms_max": round(gaps[-1], 1) if gaps else 0,
            "gaps_over_33ms": sum(g > 33.4 for g in gaps),
            "gaps_over_50ms": sum(g > 50 for g in gaps),
            **r.result[name],
        }
    return out


def percentile(sorted_values, p):
    if not sorted_values:
        return 0.0
    index = min(len(sorted_values) - 1, max(0, math.ceil(p / 100 * len(sorted_values)) - 1))
    return sorted_values[index]


# ---------------------------------------------------------------------------------------


def environment():
    def read(path):
        try:
            return open(path).read().strip()
        except OSError:
            return None
    return {
        "kernel": os.uname().release,
        "governor": read("/sys/devices/system/cpu/cpu0/cpufreq/scaling_governor"),
        "power_profile": sh("powerprofilesctl", "get").strip() or None,
        "loadavg": read("/proc/loadavg"),
        "cpus": os.cpu_count(),
        "gpu": read("/sys/class/drm/card1/device/product_name") or read("/sys/class/drm/card1/device/device"),
        "git": sh("git", "-C", ROOT, "rev-parse", "--short", "HEAD").strip(),
        "binaries": {ui: os.path.getsize(p) for ui, p in BINARIES.items()},
    }


def run_all(args):
    args.out = os.path.abspath(args.out)
    os.makedirs(args.out, exist_ok=True)
    meta = environment()
    json.dump(meta, open(os.path.join(args.out, "environment.json"), "w"), indent=1)
    print("environment:", json.dumps(meta), flush=True)
    order = [ui for ui in ("slint", "gpui") if ui in args.uis.split(",")]
    jobs = []
    for round_ in range(1, args.rounds + 1):
        for ui in (order if round_ % 2 else order[::-1]):
            jobs.append((ui, "static", round_))
            jobs.append((ui, "live", round_))
    for round_ in range(1, args.sync_rounds + 1):
        for ui in (order if round_ % 2 else order[::-1]):
            jobs.append((ui, "sync", round_))
    for round_ in range(1, args.frames_rounds + 1):
        for ui in (order if round_ % 2 else order[::-1]):
            jobs.append((ui, "frames", round_))
    for ui, mode, round_ in jobs:
        label = f"{ui}-{mode}-{round_}"
        print(f"== {label}", flush=True)
        # The live runs have messages arriving, five a second, in the long thread.
        scale = args.scale + (",incoming=5" if mode == "live" else "")
        scale += f",backlog={args.backlog}" if mode == "sync" else ""
        r = Run(ui, label, args.out, scale, frames=(mode == "frames"))
        try:
            r.start()
            if mode == "static":
                result = scenario_static(r)
            elif mode == "live":
                result = scenario_live(r, args.soak)
            elif mode == "sync":
                result = scenario_sync(r)
            else:
                result = scenario_frames(r)
        finally:
            r.stop()
        record = {"ui": ui, "mode": mode, "round": round_, "scale": args.scale, "result": result}
        json.dump(record, open(os.path.join(args.out, f"{label}.json"), "w"), indent=1)
    print("done:", args.out)


# ---------------------------------------------------------------------------------------


def report(directory):
    records = []
    for name in sorted(os.listdir(directory)):
        if name.endswith(".json") and name != "environment.json":
            records.append(json.load(open(os.path.join(directory, name))))
    table = collections.defaultdict(lambda: collections.defaultdict(lambda: collections.defaultdict(list)))
    for rec in records:
        for scenario, metrics in rec["result"].items():
            for metric, value in metrics.items():
                if isinstance(value, (int, float)):
                    table[(rec["mode"], scenario)][metric][rec["ui"]].append(value)
    env = os.path.join(directory, "environment.json")
    if os.path.exists(env):
        meta = json.load(open(env))
        print("Measured at", meta.get("git"), "on", meta.get("kernel"), "| governor", meta.get("governor"),
              "| load at start", meta.get("loadavg"))
        print("Binaries:", {k: f"{v / 1e6:.1f} MB" for k, v in meta.get("binaries", {}).items()})
        print()
    show = ("cpu_pct", "gpu_s", "wakeups_per_s", "frames_per_s", "gap_ms_p95", "gap_ms_p99", "gap_ms_max",
            "gaps_over_33ms", "rss_mb", "pss_mb", "anon_mb", "vram_mb", "gtt_mb", "threads",
            "harness_ready_s", "chat_list_s", "synced_flag_s", "caught_up_s", "snapshot_ms_p95", "snapshot_ms_max",
            "cpu_s", "unread_total", "rss_slope_mb_per_min", "anon_slope_mb_per_min")
    print("| phase | metric | slint | gpui | gpui / slint |")
    print("|---|---|---|---|---|")
    for (mode, scenario), metrics in table.items():
        for metric in show:
            if metric not in metrics:
                continue
            cells = {}
            for ui in ("slint", "gpui"):
                values = metrics[metric].get(ui)
                cells[ui] = (statistics.median(values), min(values), max(values), len(values)) if values else None
            def fmt(c):
                if c is None:
                    return "-"
                return f"{c[0]:g}" if c[1] == c[2] else f"{c[0]:g} ({c[1]:g}-{c[2]:g})"
            ratio = ""
            if cells["slint"] and cells["gpui"] and cells["slint"][0] > 0:
                ratio = f"{cells['gpui'][0] / cells['slint'][0]:.2f}x"
            label = f"{scenario} (logged)" if mode == "frames" else scenario
            print(f"| {label} | {metric} | {fmt(cells['slint'])} | {fmt(cells['gpui'])} | {ratio} |")


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = parser.add_subparsers(dest="command", required=True)
    run = sub.add_parser("run")
    run.add_argument("--rounds", type=int, default=5)
    run.add_argument("--frames-rounds", type=int, default=1)
    run.add_argument("--uis", default="slint,gpui", help="which clients to run")
    run.add_argument("--sync-rounds", type=int, default=3, help="runs of catching up with a backlog")
    run.add_argument("--backlog", type=int, default=20000, help="messages the mock sends before it has synced")
    run.add_argument("--soak", type=int, default=120, help="seconds of arriving messages for the memory slope")
    run.add_argument("--scale", default=SCALE, help="B33P_MOCK_SCALE; incoming= is added to the live runs")
    run.add_argument("--out", default=os.path.join(ROOT, "target/bench"))
    rep = sub.add_parser("report")
    rep.add_argument("directory")
    args = parser.parse_args()
    if args.command == "run":
        run_all(args)
    else:
        report(args.directory)


if __name__ == "__main__":
    main()
