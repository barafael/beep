#!/usr/bin/env python3
"""Measures b33p's clients against each other, on the mock, in a display of their own
(testbox/tools `with-display`, in a container) that draws on the host's GPU through its render
node. Nothing of the host's desktop is ever captured or shown to the clients: what is measured
comes from /proc, from the drm fdinfo of the client's own file descriptors, and from the
Wayland requests the client sends.

  bench.py run [--rounds 5] [--soak 120] [--frames-rounds 1] [--scale SPEC] [--out DIR]
               [--uis slint,gpui | --ways LABEL=PATH,... | --ways-dir DIR] [--max-load 6] [--dry-run]
  bench.py report DIR [--baseline slint-qt] [--json]
  report_html.py DIR OUT.html          (beside this file: one self-contained HTML page)

A "way" is one client binary in one graphics stack. The two-way default is `slint` and `gpui`
(BENCH_SLINT, BENCH_GPUI, `--uis`); a multi-way run names its ways, `--ways
slint-qt=PATH,slint-femtovg=PATH,gpui=PATH`, or takes every known name that exists in a
directory (`--ways-dir`: slint-qt, slint-qt-patched, slint-femtovg, slint-femtovg-patched,
slint-skia, slint-skia-patched, gpui). The kind of a way, slint or gpui, is the prefix of its
label. Every way is mounted as `b33p` in a container of its own, so everything below is
the same for all of them.

`run` builds nothing: it needs the binaries built as the `testbox` profile (a release build
that keeps the debug assertions the mock needs), with the harness:

  cargo build --profile testbox -p b33p-slint -p b33p-gpui \\
      --features b33p-slint/harness,b33p-gpui/harness

and the image of Containerfile.bench beside this script (`podman build -t b33p-bench-view -f
testbox/gpui-view/Containerfile.bench testbox/gpui-view`), and the tools of view.sh's header
(`with-display`, built with WITH_DISPLAY_RENDERER support).

Each round runs every way, in a container of their own each; the order rotates from round to
round (round 1: a b c, round 2: b c a, ...) so that a machine that drifts is spread over all of
them. A finished run is a json file in the output directory and is skipped when the same command
is started again with the same --out (resume). Before every run the 1-minute load average is
read; above --max-load the run waits (at most ten minutes) and records the wait. The scenarios:
  static  startup, idle at the list, idle in a long chat, scrolling by the mouse wheel, switching
          between chats, resizing, typing with the keyboard, and the details and media panels
  live    messages arriving in the open chat, then a soak of that, for memory growth
  sync    catching up with a backlog of messages the mock sends in one burst
  frames  the scrolling again, with the Wayland requests logged, for the gaps
          between frames (the logging costs processor time: those runs are not the others')
Every number is a /proc or drm counter read before and after, over the whole phase. The
scenario names are the keys of the json: "static" and "live" runs hold phases named as above
(idle_list_15s, idle_chat_first_10s, idle_chat_after_20s, scroll_up_8s, scroll_down_8s,
switch_100_chats, resize_sweep, type_80_keys, panels_details_media, incoming_5_per_s_30s, soak).
"""

import argparse
import collections
import glob
import json
import math
import os
import datetime
import re
import shlex
import socket
import statistics
import struct
import subprocess
import sys
import threading
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
# BENCH_WITH_DISPLAY: the tool of view.sh's header, when it was built in another checkout.
WITH_DISPLAY = os.environ.get("BENCH_WITH_DISPLAY", f"{ROOT}/target/testbox-workspaces/debug/with-display")
WIDTH, HEIGHT = 1280, 900
GEOMETRY = f"{WIDTH}x{HEIGHT}"
# The names a multi-way run knows, in the order they are listed in reports and charts.
KNOWN_WAYS = ["slint-qt", "slint-qt-patched", "slint-femtovg", "slint-femtovg-patched",
              "slint-skia", "slint-skia-patched", "gpui"]
KINDS = ("slint", "gpui")
# Seconds a run of each mode takes, roughly, for the plan of --dry-run (a soak is added to live).
ESTIMATE_S = {"static": 120, "live": 50, "sync": 15, "frames": 45}


def way_kind(label):
    """The kind of client, slint or gpui, from the prefix of a way's label."""
    for kind in KINDS:
        if label == kind or label.startswith(kind + "-"):
            return kind
    raise ValueError(f"way {label!r}: the label must start with slint or gpui (that is its kind)")


class Way:
    """One client binary in one graphics stack: a label, the binary, its kind, and what the
    container needs on top (environment variables, podman arguments)."""

    def __init__(self, label, path, env=None, args=None):
        self.label, self.path = label, path
        self.kind = way_kind(label)
        self.env, self.args = dict(env or {}), list(args or [])

    def __repr__(self):
        return f"Way({self.label!r}, {self.path!r})"


def way_order(label):
    """Known names in KNOWN_WAYS' order (the 2-way names first), other Slint ones after those,
    GPUI-kind ones last."""
    legacy = ["slint"] + KNOWN_WAYS
    rank = legacy.index(label) if label in legacy else len(legacy)
    return (label == "gpui" or label.startswith("gpui-"), rank, label)


def parse_ways(spec):
    """`label=path,label=path` into [(label, path)]; the path may not contain a comma."""
    out = []
    for item in filter(None, (x.strip() for x in spec.split(","))):
        label, sep, path = item.partition("=")
        if not sep or not label or not path:
            raise ValueError(f"--ways: {item!r} is not LABEL=PATH")
        way_kind(label)
        out.append((label, os.path.abspath(path)))
    return out


def resolve_ways(ways=None, ways_dir=None, uis="slint,gpui", extra_env=(), extra_args=()):
    """The ways to run and the notes on those that cannot: ([Way], [(label, reason)]).
    --ways and --ways-dir are merged (an explicit path wins over the directory's); with neither,
    `uis` names the two-way ones whose binaries are BENCH_SLINT / BENCH_GPUI. A way whose binary
    is missing is skipped with a note."""
    wanted = {}
    if ways_dir:
        for label in KNOWN_WAYS:
            wanted[label] = os.path.join(os.path.abspath(ways_dir), label)
    for label, path in parse_ways(ways) if ways else []:
        wanted[label] = path
    if not wanted:
        for ui in filter(None, uis.split(",")):
            if ui not in BINARIES:
                raise ValueError(f"--uis: {ui!r} is not one of {', '.join(BINARIES)}; use --ways for others")
            wanted[ui] = BINARIES[ui]
    env, args = {}, {}
    for what, items, into in (("--env", extra_env, env), ("--podman-arg", extra_args, args)):
        for item in items:
            label, sep, value = item.partition(":")
            if not sep or not value:
                raise ValueError(f"{what}: {item!r} is not LABEL:VALUE")
            into.setdefault(label, []).append(value)
    for label in list(env) + list(args):
        if label not in wanted:
            raise ValueError(f"--env/--podman-arg name the way {label!r}, which is not among the ways")
    result, skipped = [], []
    for label in sorted(wanted, key=way_order):
        path = wanted[label]
        if not (os.path.isfile(path) and os.access(path, os.X_OK)):
            skipped.append((label, f"binary missing or not executable: {path}"))
            continue
        pairs = [e.partition("=")[::2] for e in env.get(label, [])]
        result.append(Way(label, path, dict(pairs), args.get(label, [])))
    return result, skipped


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


class SysGpu:
    """The whole GPU's load, from the amdgpu driver's sysfs files, sampled five times a second in
    a thread for the whole run: the busy percentage (`gpu_busy_percent`), the package power in
    watts (hwmon `power1_average`; on an APU it holds the CPU cores too) and the shader clock in
    MHz (`freq1_input`). Unlike the drm fdinfo, which counts the client's own GPU time, this
    counts everything that draws on the GPU, the host's desktop included: a run's own idle
    reading (`gpu_idle_no_client`, taken before its client starts) is what to subtract."""

    PERIOD_S = 0.2

    def __init__(self, device):
        self.device = device
        self.hwmon = next(iter(sorted(glob.glob(f"{device}/hwmon/hwmon*"))), None)
        self.samples = []  # (time, busy %, watts, MHz); None for what could not be read
        self.stop_flag = threading.Event()
        self.thread = None

    @staticmethod
    def detect():
        for device in sorted(glob.glob("/sys/class/drm/card[0-9]*/device")):
            if os.path.exists(f"{device}/gpu_busy_percent"):
                return SysGpu(device)
        return None

    def read(self, path, scale):
        try:
            return int(open(path).read().strip()) / scale
        except (OSError, ValueError, TypeError):
            return None

    def sample_once(self):
        hw = self.hwmon
        return (time.time(), self.read(f"{self.device}/gpu_busy_percent", 1),
                self.read(f"{hw}/power1_average", 1e6) if hw else None,
                self.read(f"{hw}/freq1_input", 1e6) if hw else None)

    def start(self):
        def loop():
            while not self.stop_flag.is_set():
                self.samples.append(self.sample_once())
                self.stop_flag.wait(self.PERIOD_S)
        self.thread = threading.Thread(target=loop, daemon=True)
        self.thread.start()

    def window(self, t0, t1):
        """The means (and the busy maximum) over the samples from t0 to t1, as phase metrics."""
        rows = [s for s in list(self.samples) if t0 <= s[0] <= t1]
        out = {}
        for index, key in ((1, "sys_gpu_busy_pct"), (2, "sys_gpu_power_w"), (3, "sys_gpu_sclk_mhz")):
            values = [s[index] for s in rows if s[index] is not None]
            if values:
                out[key] = round(statistics.mean(values), 2)
                if index == 1:
                    out["sys_gpu_busy_max_pct"] = round(max(values), 1)
        return out


SYSGPU = None  # set by `run` when the machine has an amdgpu


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
        **(SYSGPU.window(before["t"], after["t"]) if SYSGPU else {}),
    }


# ---------------------------------------------------------------------------------------
# One client, in a display of its own.


class Run:
    def __init__(self, way, mode, round_, out, scale, frames=False):
        self.way, self.label, self.out, self.scale, self.frames = way, f"{way.label}-{mode}-{round_}", out, scale, frames
        # BENCH_PREFIX: another worktree measuring at the same time needs container names of its own.
        # The name holds the way, the scenario and the round: unique within a run.
        self.name = f"{os.environ.get('BENCH_PREFIX', 'b33p-bench')}-{self.label}"
        self.dir = os.path.join(out, self.label)
        self.view, self.run, self.data = (os.path.join(self.dir, d) for d in ("view", "run", "data"))
        self.n = 0

    def command(self):
        """The `podman run` that starts this client in its display. A frames run logs the
        client's Wayland requests only (the compositor's would be in the same log)."""
        program = ["b33p"]
        if self.frames:
            program = ["sh", "-c", "WAYLAND_DEBUG=client exec b33p"]
        env = {"HOME": "/tmp/home", "B33P_MOCK": "1", "B33P_MOCK_SCALE": self.scale,
               "B33P_AGENT_SOCKET": "/run/b33p/agent.sock", "RUST_LOG": "warn",
               "WITH_DISPLAY_RENDERER": "gles2", "WLR_RENDER_DRM_DEVICE": RENDER_NODE, **self.way.env}
        # BENCH_ENV: more environment for every client, "NAME=value NAME=value" (e.g. RUST_LOG=info).
        env.update(pair.split("=", 1) for pair in os.environ.get("BENCH_ENV", "").split() if "=" in pair)
        return ["podman", "run", "-d", "--rm", "--name", self.name, "--userns=keep-id",
                "--security-opt", "label=disable", "--device", RENDER_NODE, *self.way.args,
                "-v", f"{self.way.path}:/usr/local/bin/b33p:ro",
                "-v", f"{WITH_DISPLAY}:/usr/local/bin/with-display:ro",
                "-v", f"{self.view}:/run/view", "-v", f"{self.run}:/run/b33p", "-v", f"{self.data}:/tmp/home",
                *(x for k, v in env.items() for x in ("-e", f"{k}={v}")),
                IMAGE, "with-display", "/run/view", GEOMETRY, "--", *program]

    def start(self):
        subprocess.run(["podman", "rm", "-f", self.name], capture_output=True)
        subprocess.run(["rm", "-rf", self.dir])
        for d in (self.view, self.run, self.data):
            os.makedirs(d)
        t0 = time.time()
        subprocess.run(self.command(), check=True, capture_output=True, text=True)
        while not os.path.exists(os.path.join(self.view, "interact")):
            time.sleep(0.05)
        self.vnc = Vnc(os.path.join(self.view, "interact"))
        # A keyboard in the compositor's seat, before the window: without one no window is
        # ever active (GPUI takes a keyboard's focus for it), and a window that isn't, isn't
        # a window as a desktop has it. Slint (any backend) does not mind it, so every kind gets it.
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




def read_json(path):
    with open(path) as f:
        return json.load(f)


def read_file(path):
    try:
        return open(path).read().strip()
    except OSError:
        return None


def environment(ways):
    return {
        "kernel": os.uname().release,
        "governor": read_file("/sys/devices/system/cpu/cpu0/cpufreq/scaling_governor"),
        "power_profile": sh("powerprofilesctl", "get").strip() or None,
        "loadavg": read_file("/proc/loadavg"),
        "cpus": os.cpu_count(),
        "gpu": read_file("/sys/class/drm/card1/device/product_name") or read_file("/sys/class/drm/card1/device/device"),
        "git": sh("git", "-C", ROOT, "rev-parse", "--short", "HEAD").strip(),
        "binaries": {w.label: os.path.getsize(w.path) for w in ways},
    }


def machine_state():
    """What is recorded in every run's json, to see a disturbed one afterwards."""
    return {
        "time": datetime.datetime.now().isoformat(timespec="seconds"),
        "loadavg": read_file("/proc/loadavg"),
        "uptime": sh("uptime").strip(),
        "governor": read_file("/sys/devices/system/cpu/cpu0/cpufreq/scaling_governor"),
    }


def load1():
    return float(read_file("/proc/loadavg").split()[0])


def wait_for_load(max_load, limit=600, poll=10, read=load1, sleep=time.sleep, clock=time.time, say=print):
    """Waits until the 1-minute load average is at most `max_load`, for `limit` seconds at most
    (0 or less: do not check). Returns what is recorded with the run: the load when asked, the
    seconds waited, and whether the load was still too high when the wait gave up."""
    first = read()
    if max_load <= 0 or first <= max_load:
        return {"load_first": first, "waited_s": 0, "exceeded": False, "max_load": max_load}
    start = clock()
    say(f"  load {first} is above {max_load}: waiting up to {limit} s", flush=True)
    while read() > max_load and clock() - start < limit:
        sleep(poll)
    waited, now = round(clock() - start), read()
    say(f"  waited {waited} s, load now {now}" + (" (still above: going on anyway)" if now > max_load else ""), flush=True)
    return {"load_first": first, "waited_s": waited, "exceeded": now > max_load, "max_load": max_load}


def rotate(items, k):
    """`items` started at the k-th (mod length): round 1 is the given order, round 2 starts one
    later, so each way is first, last and in between equally often over a multiple of rounds."""
    items = list(items)
    if not items:
        return items
    k %= len(items)
    return items[k:] + items[:k]


def plan_jobs(ways, rounds, sync_rounds, frames_rounds):
    """[(way, mode, round)] in the order they run."""
    jobs = []
    for round_ in range(1, rounds + 1):
        for way in rotate(ways, round_ - 1):
            jobs += [(way, "static", round_), (way, "live", round_)]
    for count, mode in ((sync_rounds, "sync"), (frames_rounds, "frames")):
        for round_ in range(1, count + 1):
            jobs += [(way, mode, round_) for way in rotate(ways, round_ - 1)]
    return jobs


def finished(out, label):
    """Whether a complete json of this run is in `out` (a run that was cut off left none: the
    json is written when the run is over, and atomically)."""
    try:
        record = read_json(os.path.join(out, f"{label}.json"))
    except (OSError, ValueError):
        return False
    return bool(record.get("result"))


def job_scale(args, mode):
    # The live runs have messages arriving, five a second, in the long thread.
    scale = args.scale + (",incoming=5" if mode == "live" else "")
    return scale + (f",backlog={args.backlog}" if mode == "sync" else "")


def write_json(path, value):
    tmp = path + ".part"
    with open(tmp, "w") as f:
        json.dump(value, f, indent=1)
    os.replace(tmp, path)


def print_plan(args, ways, skipped, jobs):
    estimate = 0
    for way, mode, round_ in jobs:
        run = Run(way, mode, round_, args.out, job_scale(args, mode), frames=(mode == "frames"))
        done = finished(args.out, run.label) and not args.force
        seconds = ESTIMATE_S[mode] + (args.soak if mode == "live" else 0)
        estimate += 0 if done else seconds
        print(f"== {run.label} ({way.kind}, ~{seconds} s)" + (" already done: skipped (resume)" if done else ""))
        if not done:
            print("  " + shlex.join(run.command()))
    print(f"\n{len(ways)} ways: {', '.join(w.label for w in ways)}")
    for label, reason in skipped:
        print(f"skipped {label}: {reason}")
    print(f"{len(jobs)} runs, about {estimate / 60:.0f} minutes still to do (a rough estimate, plus waits for load)")


def run_all(args):
    try:
        ways, skipped = resolve_ways(args.ways, args.ways_dir, args.uis, args.env, args.podman_arg)
    except ValueError as error:
        sys.exit(f"bench.py: {error}")
    for label, reason in skipped:
        print(f"note: way {label} skipped: {reason}", flush=True)
    if not ways:
        sys.exit("bench.py: no way to run (no binary found)")
    args.out = os.path.abspath(args.out)
    global SYSGPU
    SYSGPU = SysGpu.detect()
    if SYSGPU:
        SYSGPU.start()
        print(f"whole-GPU sampling: {SYSGPU.device} (hwmon {SYSGPU.hwmon})", flush=True)
    else:
        print("note: no amdgpu with gpu_busy_percent: the whole GPU is not sampled", flush=True)
    jobs = plan_jobs(ways, args.rounds, args.sync_rounds, args.frames_rounds)
    if args.dry_run:
        print_plan(args, ways, skipped, jobs)
        return
    os.makedirs(args.out, exist_ok=True)
    path = os.path.join(args.out, "environment.json")
    try:
        meta = read_json(path)
    except (OSError, ValueError):
        meta = {}
    now = environment(ways)
    session = {"started": machine_state()["time"], "uptime": machine_state()["uptime"], "ways": {w.label: w.path for w in ways},
               "skipped": [{"way": l, "reason": r} for l, r in skipped], "max_load": args.max_load,
               "rounds": args.rounds, "sync_rounds": args.sync_rounds, "frames_rounds": args.frames_rounds,
               "soak": args.soak, "backlog": args.backlog, "scale": args.scale,
               "extra": {w.label: {"env": w.env, "podman_args": w.args} for w in ways if w.env or w.args}}
    meta = {**meta, **now, "binaries": {**meta.get("binaries", {}), **now["binaries"]},
            "sessions": meta.get("sessions", []) + [session]}
    write_json(path, meta)
    print("environment:", json.dumps({k: v for k, v in meta.items() if k != "sessions"}), flush=True)
    failures = []
    for way, mode, round_ in jobs:
        label = f"{way.label}-{mode}-{round_}"
        if finished(args.out, label) and not args.force:
            print(f"== {label}: done in an earlier run, skipped", flush=True)
            continue
        print(f"== {label}", flush=True)
        wait = wait_for_load(args.max_load)
        state = machine_state()
        r = Run(way, mode, round_, args.out, job_scale(args, mode), frames=(mode == "frames"))
        gpu_idle = {}
        if SYSGPU:  # the GPU with no client of ours: the desktop's own load, to subtract
            t_from = time.time()
            time.sleep(6)
            gpu_idle = SYSGPU.window(t_from, time.time())
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
        except Exception as error:  # one broken run must not end an hour of others
            failures.append(label)
            print(f"  FAILED {label}: {error!r}", file=sys.stderr, flush=True)
            with open(os.path.join(args.out, "failures.log"), "a") as f:
                f.write(f"{state['time']} {label}: {error!r}\n{r.logs()[-2000:]}\n")
            continue
        finally:
            r.stop()
        if gpu_idle:
            result["gpu_idle_no_client"] = gpu_idle
        record = {"ui": way.label, "way": way.label, "kind": way.kind, "mode": mode, "round": round_,
                  "scale": args.scale, "result": result,
                  "env": {**state, "load_wait": wait, "loadavg_after": read_file("/proc/loadavg")}}
        write_json(os.path.join(args.out, f"{label}.json"), record)
    print("done:", args.out, f"({len(failures)} failed: {', '.join(failures)})" if failures else "")
    if failures:
        sys.exit(1)


# ---------------------------------------------------------------------------------------
# Aggregation: what `report` and report_html.py both print. Medians over the rounds of each
# (way, scenario, metric); nothing else is computed from the data than what is here.


def derive(mode, result):
    """Numbers that are sums or means of the run's own, added to a copy of its result: the GPU
    memory (video plus shared), and the scrolling as one number (the mean of both directions)."""
    result = {k: dict(v) for k, v in result.items()}
    for metrics in result.values():
        if "vram_mb" in metrics and "gtt_mb" in metrics:
            metrics["gpu_mem_mb"] = round(metrics["vram_mb"] + metrics["gtt_mb"], 1)
    idle = result.get("gpu_idle_no_client", {})
    for name, metrics in result.items():
        if name == "gpu_idle_no_client":
            continue
        for key, base in (("sys_gpu_busy_pct", "sys_gpu_busy_pct"), ("sys_gpu_power_w", "sys_gpu_power_w")):
            if key in metrics and base in idle:
                metrics[key.replace("sys_gpu_", "sys_gpu_over_idle_")] = round(metrics[key] - idle[base], 2)
    up, down = result.get("scroll_up_8s"), result.get("scroll_down_8s")
    if mode == "static" and up and down:
        result["scroll_both_8s_mean"] = {
            key: round((up[key] + down[key]) / 2, 2)
            for key in ("cpu_pct", "cpu_s", "gpu_s", "wakeups_per_s", "sys_gpu_busy_pct", "sys_gpu_power_w",
                        "sys_gpu_over_idle_busy_pct", "sys_gpu_over_idle_power_w")
            if key in up and key in down}
    return result


def load_records(directory):
    records = []
    for name in sorted(os.listdir(directory)):
        if not name.endswith(".json") or name == "environment.json":
            continue
        try:
            rec = read_json(os.path.join(directory, name))
        except ValueError:
            continue
        if isinstance(rec, dict) and "mode" in rec and "result" in rec:
            rec["way"] = rec.get("way") or rec["ui"]
            rec["file"] = name
            records.append(rec)
    return records


def stats(values):
    return {"median": statistics.median(values), "min": min(values), "max": max(values),
            "values": values, "n": len(values)}


def aggregate(directory):
    """{"ways": [...], "kinds": {way: kind}, "environment": {...}, "rounds": {way: {mode: n}},
    "runs": [{way, mode, round, env}], "scenarios": [{"mode", "scenario", "metrics": {metric:
    {way: stats}}}]}. Numbers that are not numbers (None) are left out; records are read in the
    order of their round so that `values` is by round."""
    modes = list(ESTIMATE_S)
    records = sorted(load_records(directory), key=lambda r: (
        modes.index(r["mode"]) if r["mode"] in modes else len(modes), r["mode"], r["way"], r["round"]))
    table = collections.OrderedDict()
    rounds = collections.defaultdict(lambda: collections.defaultdict(int))
    runs = []
    for rec in records:
        rounds[rec["way"]][rec["mode"]] += 1
        runs.append({"way": rec["way"], "mode": rec["mode"], "round": rec["round"], "env": rec.get("env")})
        for scenario, metrics in derive(rec["mode"], rec["result"]).items():
            for metric, value in metrics.items():
                if isinstance(value, (int, float)) and not isinstance(value, bool):
                    table.setdefault((rec["mode"], scenario), collections.OrderedDict()) \
                        .setdefault(metric, collections.defaultdict(list))[rec["way"]].append(value)
    ways = sorted({r["way"] for r in records}, key=way_order)
    env_path = os.path.join(directory, "environment.json")
    env = read_json(env_path) if os.path.exists(env_path) else {}
    return {
        "ways": ways,
        "kinds": {w: way_kind(w) for w in ways},
        "environment": env,
        "rounds": {w: dict(rounds[w]) for w in ways},
        "runs": runs,
        "scenarios": [
            {"mode": mode, "scenario": scenario,
             "metrics": {m: {w: stats(v) for w, v in by_way.items()} for m, by_way in metrics.items()}}
            for (mode, scenario), metrics in table.items()],
    }


def pick_baseline(ways, wanted):
    """The way the ratios are against: `wanted` if it is among `ways`, else slint-qt (the
    shipped one), else the two-way run's slint, else the first way."""
    for candidate in (wanted, "slint-qt", "slint"):
        if candidate in ways:
            return candidate
    return ways[0] if ways else None


# ---------------------------------------------------------------------------------------


def fmt_cell(c):
    if c is None:
        return "-"
    return f"{c['median']:g}" if c["min"] == c["max"] else f"{c['median']:g} ({c['min']:g}-{c['max']:g})"


SHOW = ("cpu_pct", "gpu_s", "sys_gpu_busy_pct", "sys_gpu_busy_max_pct", "sys_gpu_over_idle_busy_pct",
        "sys_gpu_power_w", "sys_gpu_over_idle_power_w", "sys_gpu_sclk_mhz", "wakeups_per_s", "frames_per_s", "gap_ms_p95", "gap_ms_p99", "gap_ms_max",
        "gaps_over_33ms", "rss_mb", "pss_mb", "anon_mb", "vram_mb", "gtt_mb", "gpu_mem_mb", "threads",
        "harness_ready_s", "chat_list_s", "synced_flag_s", "caught_up_s", "snapshot_ms_p95", "snapshot_ms_max",
        "cpu_s", "unread_total", "rss_slope_mb_per_min", "anon_slope_mb_per_min")


def report(directory, baseline=None, as_json=False, out=sys.stdout):
    agg = aggregate(directory)
    if as_json:
        json.dump(agg, out, indent=1)
        out.write("\n")
        return
    ways = agg["ways"]
    base = pick_baseline(ways, baseline)
    if baseline and baseline != base:
        print(f"note: baseline {baseline} has no data here; ratios are against {base}", file=out)
    env = agg["environment"]
    if env:
        print("Measured at", env.get("git"), "on", env.get("kernel"), "| governor", env.get("governor"),
              "| load at start", env.get("loadavg"), file=out)
        print("Binaries:", {k: f"{v / 1e6:.1f} MB" for k, v in env.get("binaries", {}).items()}, file=out)
    print("Rounds:", {w: agg["rounds"][w] for w in ways}, file=out)
    disturbed = [r for r in agg["runs"] if r["env"] and r["env"].get("load_wait", {}).get("waited_s")]
    for r in disturbed:
        w = r["env"]["load_wait"]
        print(f"note: {r['way']}-{r['mode']}-{r['round']} waited {w['waited_s']} s for load "
              f"{w['load_first']} (max {w['max_load']})" + ("; still above" if w.get("exceeded") else ""), file=out)
    print(file=out)
    others = [w for w in ways if w != base]
    print("| phase | metric | " + " | ".join(ways) + " | " + " | ".join(f"{w} / {base}" for w in others) + " |", file=out)
    print("|---|---|" + "---|" * (len(ways) + len(others)), file=out)
    for sc in agg["scenarios"]:
        for metric in SHOW:
            by_way = sc["metrics"].get(metric)
            if not by_way:
                continue
            ratios = []
            for w in others:
                b, c = by_way.get(base), by_way.get(w)
                ratios.append(f"{c['median'] / b['median']:.2f}x" if b and c and b["median"] > 0 else "")
            label = f"{sc['scenario']} (logged)" if sc["mode"] == "frames" else sc["scenario"]
            print(f"| {label} | {metric} | " + " | ".join(fmt_cell(by_way.get(w)) for w in ways) + " | "
                  + " | ".join(ratios) + " |", file=out)


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = parser.add_subparsers(dest="command", required=True)
    run = sub.add_parser("run", help="run the scenarios for every way and write a json per run")
    run.add_argument("--rounds", type=int, default=5)
    run.add_argument("--frames-rounds", type=int, default=1)
    run.add_argument("--uis", default="slint,gpui",
                     help="the two-way names (slint, gpui; binaries BENCH_SLINT, BENCH_GPUI); ignored with --ways or --ways-dir")
    run.add_argument("--ways", help="LABEL=PATH,LABEL=PATH,...: the ways to run, e.g. "
                     "slint-qt=/b/slint-qt,slint-femtovg=/b/slint-femtovg,gpui=/b/gpui. The label starts with "
                     "slint or gpui, which is the kind. Overrides a --ways-dir entry of the same label")
    run.add_argument("--ways-dir", help="DIR: every known name that exists in it is a way (" + ", ".join(KNOWN_WAYS)
                     + "); the missing ones are skipped with a note")
    run.add_argument("--env", action="append", default=[], metavar="LABEL:KEY=VALUE",
                     help="an extra container environment variable for one way (repeatable), e.g. slint-skia:SLINT_BACKEND=winit-skia")
    run.add_argument("--podman-arg", action="append", default=[], metavar="LABEL:ARG",
                     help="an extra argument of `podman run` for one way (repeatable; one word each)")
    run.add_argument("--sync-rounds", type=int, default=3, help="runs of catching up with a backlog")
    run.add_argument("--backlog", type=int, default=20000, help="messages the mock sends before it has synced")
    run.add_argument("--soak", type=int, default=120, help="seconds of arriving messages for the memory slope")
    run.add_argument("--scale", default=SCALE, help="B33P_MOCK_SCALE; incoming= is added to the live runs")
    run.add_argument("--out", default=os.path.join(ROOT, "target/bench"),
                     help="output directory; runs already finished in it are skipped, so a cut-off run is continued by starting it again")
    run.add_argument("--max-load", type=float, default=6,
                     help="before a run, wait (at most 10 minutes) while the 1-minute load average is above this; 0 turns it off. "
                     "The wait is recorded in the run's json")
    run.add_argument("--force", action="store_true", help="run again what --out already holds a finished json of (overwrites it)")
    run.add_argument("--dry-run", action="store_true", help="print the runs and the podman commands, start nothing, write nothing")
    rep = sub.add_parser("report", help="print a markdown table of an output directory")
    rep.add_argument("directory")
    rep.add_argument("--baseline", default="slint-qt",
                     help="the way the ratio columns divide by (default slint-qt, the shipped one; the 2-way "
                     "slint if there is no slint-qt; else the first way)")
    rep.add_argument("--json", action="store_true", dest="as_json",
                     help="print the aggregated numbers (per scenario, per metric, per way: median, min, max, values, n) as json")
    args = parser.parse_args()
    if args.command == "run":
        run_all(args)
    else:
        report(args.directory, args.baseline, args.as_json)


if __name__ == "__main__":
    main()
