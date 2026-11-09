#!/usr/bin/env python3
"""One self-contained HTML page from a bench.py output directory, for any number of ways.

  report_html.py DIR OUT.html [--baseline slint-skia] [--title TEXT]

No script, font or image is loaded from anywhere: the CSS is inline and the charts are inline
SVG drawn here. Every number on the page comes from `bench.aggregate(DIR)` (the medians,
minima and maxima over the rounds, and the values of the rounds) or from environment.json;
the sentences of the summary and of the caveats are made from those numbers, not written by
hand. The page follows prefers-color-scheme, has an explicit background in both, and works
at the width of a phone.
"""

import argparse
import html
import os
import re
import statistics
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
import bench  # noqa: E402

# ---------------------------------------------------------------------------------------
# Colours. One hue per graphics stack, the same in every chart: the three Slint stacks are the
# cool family (blue, aqua, violet; patched variants are a lighter tint of their stack's hue,
# hatched), GPUI is orange, the odd one out. The base colours are slots 1, 3, 7 and 2 of the
# reference palette of the dataviz skill, in the order the rows are listed (qt, femtovg, skia,
# gpui; Qt is retired, its colour stays for the results of runs that had it), which validates in both modes on the adjacent pairlist; the rows are also labelled
# with their names, and the table at the bottom has every number.

STACK_COLORS = {  # stack: (light, dark)
    "slint-qt": ("#2a78d6", "#3987e5"),
    "slint-femtovg": ("#1baf7a", "#199e70"),
    "slint-skia": ("#6250d6", "#9085e9"),
    "gpui": ("#eb6834", "#d95926"),
}
# A way the page does not know (another stack): the remaining slots of the reference palette.
EXTRA_COLORS = [("#e87ba4", "#d55181"), ("#008300", "#008300"), ("#eda100", "#c98500"), ("#e34948", "#e66767")]
SURFACE = ("#fcfcfb", "#1a1a19")
INK = ("#0b0b0b", "#f0efec")
# Segments of the memory stacks (not ways): anonymous and file-backed resident memory in grays,
# video and shared GPU memory in magenta.
SEGMENTS = [("anon", "Anonymous (heap)", ("#52514e", "#b4b3a9")),
            ("file", "File-backed (shared libraries, mapped files)", ("#a9a79f", "#6b6a64")),
            ("vram", "GPU video memory (VRAM)", ("#e87ba4", "#d55181")),
            ("gtt", "GPU shared memory (GTT)", ("#f4b9ce", "#e8a0bb"))]


def mix(a, b, t):
    """The hex colour a moved by t towards b."""
    pa = [int(a[i:i + 2], 16) for i in (1, 3, 5)]
    pb = [int(b[i:i + 2], 16) for i in (1, 3, 5)]
    return "#" + "".join(f"{round(x + (y - x) * t):02x}" for x, y in zip(pa, pb))


def slug(label):
    return re.sub(r"[^a-zA-Z0-9]+", "-", label)


def stack_of(label):
    return label[:-len("-patched")] if label.endswith("-patched") else label


def is_patched(label):
    return label.endswith("-patched")


def way_palette(ways):
    """{way: (base, tint)} each as (light, dark): the same stack always has the same hue."""
    extra = iter(EXTRA_COLORS)
    colors = dict(STACK_COLORS)
    out = {}
    for way in ways:
        stack = stack_of(way)
        if stack == "slint" and "slint" not in colors:
            colors["slint"] = STACK_COLORS["slint-skia"]  # the two-way run's Slint is the shipped one
        if stack not in colors:
            colors[stack] = next(extra, ("#898781", "#898781"))
        light, dark = colors[stack]
        out[way] = ((light, dark), (mix(light, SURFACE[0], 0.5), mix(dark, INK[1], 0.4)))
    return out


# ---------------------------------------------------------------------------------------
# Numbers and text.


def esc(x):
    return html.escape(str(x), quote=True)


def num(v):
    """A number at the precision it deserves: 0.04, 1.5, 22, 1,234."""
    if v is None:
        return "-"
    a = abs(v)
    if a >= 1000:
        return f"{v:,.0f}"
    if a >= 100:
        return f"{v:.0f}"
    if a >= 10:
        return f"{v:.1f}".rstrip("0").rstrip(".")
    if a >= 1:
        return f"{v:.2f}".rstrip("0").rstrip(".")
    return f"{v:.3f}".rstrip("0").rstrip(".") or "0"


def nice_ceiling(v):
    """The smallest of 1, 2, 2.5, 5, 10 times a power of ten that is at least v, and its step."""
    if v <= 0:
        return 1.0, 0.25
    exp = 10 ** (len(str(int(v))) - 1) if v >= 1 else 10 ** (-(len(str(int(1 / v))) + 0))
    for mult in (1, 2, 2.5, 3, 4, 5, 6, 8, 10, 20, 25, 50, 100):
        top = exp * mult
        if top >= v:
            break
    for divisions in (5, 4, 3, 2):
        step = top / divisions
        if abs(step * divisions - top) < 1e-9 and len(f"{step:g}") <= 6:
            return top, step
    return top, top / 2


class Data:
    """Read access to the aggregation, by (mode, scenario, metric, way)."""

    def __init__(self, agg):
        self.agg = agg
        self.ways = agg["ways"]
        self.index = {(s["mode"], s["scenario"]): s["metrics"] for s in agg["scenarios"]}
        self.env = agg["environment"]
        sizes = self.env.get("binaries", {})
        self.binary = {w: {"median": sizes[w] / 1e6, "min": sizes[w] / 1e6, "max": sizes[w] / 1e6,
                           "values": [sizes[w] / 1e6], "n": 1} for w in self.ways if w in sizes}

    def get(self, mode, scenario, metric, way):
        return self.index.get((mode, scenario), {}).get(metric, {}).get(way)

    def has(self, mode, scenario, metric):
        return bool(self.index.get((mode, scenario), {}).get(metric))

    def scenario_metrics(self, mode, scenario):
        return self.index.get((mode, scenario), {})


# What the headline table and the summary use: (id, label, unit, source, better).
# `source` is (mode, scenario, metric), or "binary" for the size from environment.json.
HEADLINE = [
    ("incoming", "CPU, 5 messages a second arriving", "%", ("live", "incoming_5_per_s_30s", "cpu_pct"), "low"),
    ("idle_chat", "CPU, idle in a chat after the caret stops", "%", ("static", "idle_chat_after_20s", "cpu_pct"), "low"),
    ("scroll", "CPU, scrolling (mean of up and down)", "%", ("static", "scroll_both_8s_mean", "cpu_pct"), "low"),
    ("switch", "CPU, switching 100 chats", "%", ("static", "switch_100_chats", "cpu_pct"), "low"),
    ("typing", "CPU, typing 80 keys", "%", ("static", "type_80_keys", "cpu_pct"), "low"),
    ("wakeups", "Wakeups a second while scrolling", "/s", ("static", "scroll_both_8s_mean", "wakeups_per_s"), "low"),
    ("rss_chat", "RSS in an open chat", "MB", ("static", "idle_chat_after_20s", "rss_mb"), "low"),
    ("rss_after", "RSS after all phases", "MB", ("static", "after", "rss_mb"), "low"),
    ("gpu_mem", "GPU memory (VRAM + GTT) after all phases", "MB", ("static", "after", "gpu_mem_mb"), "low"),
    ("threads", "Threads", "", ("static", "settled", "threads"), "low"),
    ("binary", "Binary size", "MB", "binary", "low"),
    ("startup", "Startup to the chat list", "s", ("static", "startup", "chat_list_s"), "low"),
    ("sync_cpu", "Catching up with the backlog: CPU seconds", "s", ("sync", "sync", "cpu_s"), "low"),
    ("sync_wall", "Catching up with the backlog: wall seconds", "s", ("sync", "sync", "caught_up_s"), "low"),
    ("soak", "Soak: RSS growth a minute", "MB/min", ("live", "soak", "rss_slope_mb_per_min"), "low"),
]
HEAD = {h[0]: h for h in HEADLINE}


def head_cell(data, hid, way):
    source = HEAD[hid][3]
    return data.binary.get(way) if source == "binary" else data.get(*source, way)


def head_series(data, hid):
    """{way: stats} of a headline metric, for the ways that have it."""
    return {w: c for w in data.ways if (c := head_cell(data, hid, w))}


def with_unit(v, unit):
    return f"{num(v)}{' ' if unit and not unit.startswith('%') else ''}{unit}".strip()


def best_worst(series):
    ranked = sorted(series.items(), key=lambda kv: kv[1]["median"])
    return ranked[0], ranked[-1]


def summary_lines(data):
    """Five sentences made from the numbers: best and worst way per headline metric, then what
    the patches did where both a way and its patched variant were measured."""
    lines = []

    def extreme(hid):
        series = head_series(data, hid)
        label, unit = HEAD[hid][1], HEAD[hid][2]
        if len(series) < 2:
            return None
        (bw, bs), (ww, ws) = best_worst(series)
        if bs["median"] == ws["median"]:
            return f"{label}: all ways alike, {with_unit(bs['median'], unit)}."
        ratio = f", {ws['median'] / bs['median']:.1f} times the best" if bs["median"] > 0 else ""
        return (f"{label}: best {bw} with {with_unit(bs['median'], unit)}, worst {ww} with "
                f"{with_unit(ws['median'], unit)}{ratio}.")

    SHORT = {"incoming": "incoming", "scroll": "scrolling", "switch": "switching"}

    def patches():
        parts = []
        for way in data.ways:
            if not is_patched(way) or stack_of(way) not in data.ways:
                continue
            deltas = []
            for hid in ("incoming", "scroll", "switch"):
                a, b = head_cell(data, hid, stack_of(way)), head_cell(data, hid, way)
                if a and b and a["median"] > 0:
                    deltas.append(f"{SHORT[hid]} {num(a['median'])} to {num(b['median'])}% "
                                  f"({(b['median'] - a['median']) / a['median'] * 100:+.0f}%)")
            if deltas:
                parts.append(f"{way} against {stack_of(way)}, CPU: " + "; ".join(deltas))
        return ("Patches, " + " | ".join(parts) + ".") if parts else None

    for hid in ("incoming", "scroll", "switch", "rss_after"):
        if (line := extreme(hid)):
            lines.append(line)
    if (line := patches()):
        lines.append(line)
    for hid in ("idle_chat", "typing", "wakeups", "soak", "startup", "sync_cpu", "threads", "binary"):
        if len(lines) >= 5:
            break
        if (line := extreme(hid)):
            lines.append(line)
    return lines[:5]


# ---------------------------------------------------------------------------------------
# SVG charts. Geometry in viewBox units; the svg is scaled to its column (about 1 to 1).

LOW, HIGH = "lower is better", "higher is better"
W, LABEL_X, PLOT_X, PLOT_W, ROW, BAR = 380, 22, 152, 168, 26, 14


def swatch_svg(way, x, cy, size=10):
    fill = f"url(#fill-{slug(way)})" if is_patched(way) else f"var(--c-{slug(way)})"
    return (f'<rect x="{x}" y="{cy - size / 2}" width="{size}" height="{size}" rx="2" '
            f'style="fill:{fill}"/>')


def bar_path(x, y, w, h, r=4):
    """A bar with a rounded data end and a square foot at the baseline x."""
    if w < 2 * r:
        return f"M{x:.1f},{y:.1f}h{max(w, 0.5):.1f}v{h:.1f}h{-max(w, 0.5):.1f}z"
    return (f"M{x:.1f},{y:.1f}h{w - r:.1f}a{r},{r} 0 0 1 {r},{r}v{h - 2 * r:.1f}a{r},{r} 0 0 1 {-r},{r}"
            f"h{-(w - r):.1f}z")


def tooltip(way, c, unit):
    if c is None:
        return f"{way}: no data"
    if c["n"] == 1:
        return f"{way}: {num(c['median'])} {unit}".strip()
    values = ", ".join(num(v) for v in c["values"])
    return (f"{way}: median {num(c['median'])} {unit}, min {num(c['min'])}, max {num(c['max'])}, "
            f"{c['n']} rounds ({values})").replace("  ", " ")


def axis_for(cells):
    top = max((c["max"] for c in cells if c), default=0)
    return nice_ceiling(top)


def bar_svg(rows, unit, best=None):
    """Horizontal bars from zero, one row a way, whiskers min to max over the rounds, the value
    at the tip. `rows` is [(way, stats or None)]."""
    top, step = axis_for([c for _, c in rows])
    height = 6 + ROW * len(rows) + 22

    def sx(v):
        return PLOT_X + PLOT_W * v / top

    out = [f'<svg class="chart" viewBox="0 0 {W} {height}" role="img" aria-label="bar chart">']
    ticks = []
    t = 0.0
    while t <= top + 1e-9:
        ticks.append(t)
        t += step
    for t in ticks:
        out.append(f'<line class="grid" x1="{sx(t):.1f}" y1="4" x2="{sx(t):.1f}" y2="{height - 20}"/>')
        out.append(f'<text class="tick" x="{sx(t):.1f}" y="{height - 6}" text-anchor="middle">{num(t)}</text>')
    out.append(f'<line class="axis" x1="{PLOT_X}" y1="4" x2="{PLOT_X}" y2="{height - 20}"/>')
    for i, (way, c) in enumerate(rows):
        cy = 6 + ROW * i + ROW / 2
        out.append(f'<g><title>{esc(tooltip(way, c, unit))}</title>')
        out.append(swatch_svg(way, 4, cy))
        out.append(f'<text class="lab" x="{LABEL_X}" y="{cy + 4:.1f}">{esc(way)}</text>')
        if c is None:
            out.append(f'<text class="muted" x="{PLOT_X + 8}" y="{cy + 4:.1f}">no data</text></g>')
            continue
        fill = f"url(#fill-{slug(way)})" if is_patched(way) else f"var(--c-{slug(way)})"
        w = max(0.0, sx(c["median"]) - PLOT_X)
        if w > 0:
            out.append(f'<path d="{bar_path(PLOT_X, cy - BAR / 2, w, BAR)}" style="fill:{fill}"/>')
        end = sx(c["median"])
        if c["n"] > 1 and c["min"] < c["max"]:
            x1, x2 = sx(c["min"]), sx(c["max"])
            for cls in ("whisk-halo", "whisk"):
                out.append(f'<path class="{cls}" d="M{x1:.1f},{cy:.1f}H{x2:.1f}M{x1:.1f},{cy - 4:.1f}v8M{x2:.1f},{cy - 4:.1f}v8"/>')
            end = max(end, x2)
        weight = " best" if best is not None and c["median"] == best else ""
        out.append(f'<text class="val{weight}" x="{end + 6:.1f}" y="{cy + 4:.1f}">{num(c["median"])}</text></g>')
    out.append("</svg>")
    return "".join(out)


def stack_svg(rows):
    """Stacked horizontal bars: rows is [(way, [(segment key, value)] or None)]."""
    colors = {k: f"var(--s-{k})" for k, _, _ in SEGMENTS}
    totals = [sum(v for _, v in parts) for _, parts in rows if parts]
    top, step = nice_ceiling(max(totals, default=0))
    height = 6 + ROW * len(rows) + 22

    def sx(v):
        return PLOT_X + PLOT_W * v / top

    out = [f'<svg class="chart" viewBox="0 0 {W} {height}" role="img" aria-label="stacked bar chart">']
    t = 0.0
    while t <= top + 1e-9:
        out.append(f'<line class="grid" x1="{sx(t):.1f}" y1="4" x2="{sx(t):.1f}" y2="{height - 20}"/>')
        out.append(f'<text class="tick" x="{sx(t):.1f}" y="{height - 6}" text-anchor="middle">{num(t)}</text>')
        t += step
    out.append(f'<line class="axis" x1="{PLOT_X}" y1="4" x2="{PLOT_X}" y2="{height - 20}"/>')
    names = {k: n for k, n, _ in SEGMENTS}
    for i, (way, parts) in enumerate(rows):
        cy = 6 + ROW * i + ROW / 2
        tip = f"{way}: no data" if not parts else f"{way}: " + ", ".join(f"{names[k]} {num(v)} MB" for k, v in parts) + f", total {num(sum(v for _, v in parts))} MB"
        out.append(f'<g><title>{esc(tip)}</title>{swatch_svg(way, 4, cy)}')
        out.append(f'<text class="lab" x="{LABEL_X}" y="{cy + 4:.1f}">{esc(way)}</text>')
        if not parts:
            out.append(f'<text class="muted" x="{PLOT_X + 8}" y="{cy + 4:.1f}">no data</text></g>')
            continue
        x = PLOT_X
        live = [(k, v) for k, v in parts if v > 0]
        for j, (k, v) in enumerate(live):
            w = sx(v) - PLOT_X
            gap = 2 if j < len(live) - 1 else 0  # a 2 px surface gap between segments
            seg = max(w - gap, 0.5)
            if j == len(live) - 1:
                out.append(f'<path d="{bar_path(x, cy - BAR / 2, seg, BAR)}" style="fill:{colors[k]}"/>')
            else:
                out.append(f'<rect x="{x:.1f}" y="{cy - BAR / 2}" width="{seg:.1f}" height="{BAR}" style="fill:{colors[k]}"/>')
            x += w
        out.append(f'<text class="val" x="{x + 6:.1f}" y="{cy + 4:.1f}">{num(sum(v for _, v in parts))}</text></g>')
    out.append("</svg>")
    return "".join(out)


def panel(title, note, svg):
    return f'<figure class="panel"><figcaption><b>{esc(title)}</b><span>{esc(note)}</span></figcaption>{svg}</figure>'


def best_value(medians, lower):
    """The value to mark as best in a chart, or None when there is one way or all are alike."""
    if len(medians) < 2 or min(medians) == max(medians):
        return None
    return min(medians) if lower else max(medians)


def metric_panel(data, mode, scenario, metric, title, unit, better=LOW, extra_note=""):
    rows = [(w, data.get(mode, scenario, metric, w)) for w in data.ways]
    if not any(c for _, c in rows):
        return ""
    present = [(w, c) for w, c in rows if c]
    best = best_value([c["median"] for _, c in present], better == LOW)
    n = sorted({c["n"] for _, c in present})
    rounds = f"{n[0]} rounds" if len(n) == 1 and n[0] > 1 else (f"{n[0]}-{n[-1]} rounds" if len(n) > 1 else "")
    note = " · ".join(x for x in (unit, better, rounds, extra_note) if x)
    return panel(title, note, bar_svg(rows, unit, best))


def series_panel(data, title, series, unit, better=LOW):
    rows = [(w, series.get(w)) for w in data.ways]
    if not any(c for _, c in rows):
        return ""
    present = [(w, c) for w, c in rows if c]
    best = best_value([c["median"] for _, c in present], better == LOW)
    return panel(title, " · ".join(x for x in (unit, better) if x), bar_svg(rows, unit, best))


# ---------------------------------------------------------------------------------------
# Sections: (id, title, intro, [(mode, scenario, metric, title, unit, better)]).

CPU = ("cpu_pct", "CPU", "% of one core")
WAKE = ("wakeups_per_s", "Wakeups", "context switches a second")
GPU_S = ("gpu_s", "GPU time", "seconds of GPU engine time in the phase")
SECTIONS = [
    ("idle", "Idle", "Nothing happens; what a window costs while it sits there. The caret blinks in the first seconds "
     "of a chat; the last window is after it has stopped.",
     [("static", s, m, f"{t}, {name}", u, LOW) for s, name in (("idle_list_15s", "idle at the chat list"),
                                                                 ("idle_chat_first_10s", "first 10 s in a chat"),
                                                                 ("idle_chat_after_20s", "idle in a chat, caret stopped"))
      for m, t, u in (CPU, WAKE)]),
    ("incoming", "Incoming messages", "Five messages a second arrive in the open chat of a 20,000-message thread.",
     [("live", "incoming_5_per_s_30s", m, f"{t}, messages arriving", u, LOW) for m, t, u in (CPU, WAKE, GPU_S)]
     + [("live", "incoming_5_per_s_30s", "rss_mb", "RSS, messages arriving", "MB", LOW)]),
    ("scrolling", "Scrolling", "Mouse-wheel turns every 16 ms for 8 s up and 8 s down in the long thread.",
     [("static", "scroll_both_8s_mean", "cpu_pct", "CPU, scrolling (mean of both directions)", "% of one core", LOW),
      ("static", "scroll_both_8s_mean", "wakeups_per_s", "Wakeups, scrolling (mean of both directions)", "context switches a second", LOW),
      ("static", "scroll_up_8s", "cpu_pct", "CPU, scrolling up", "% of one core", LOW),
      ("static", "scroll_down_8s", "cpu_pct", "CPU, scrolling down", "% of one core", LOW),
      ("static", "scroll_up_8s", "gpu_s", "GPU time, scrolling up", "seconds in the phase", LOW),
      ("static", "scroll_down_8s", "gpu_s", "GPU time, scrolling down", "seconds in the phase", LOW)]),
    ("switching", "Switching chats", "100 selections of ten different chats, 80 ms apart.",
     [("static", "switch_100_chats", m, f"{t}, switching chats", u, LOW) for m, t, u in (CPU, WAKE, GPU_S)]),
    ("gpu_system", "Whole GPU", "Everything the GPU does while the phase runs, from the amdgpu sysfs files (busy percentage, "
     "package power, shader clock), sampled five times a second. Unlike the GPU time above, which counts only the client's "
     "own engine time, this includes the desktop of the host; each run took its own reading with no client started "
     "(`gpu_idle_no_client`) and the bars are the phase minus that. The power is the package's: on this APU it holds the CPU cores "
     "too, so a CPU-heavy build shows watts here that are not the GPU's.",
     [(mode, scen, m, f"{t}, {name}", u, LOW)
      for mode, scen, name in (("static", "scroll_both_8s_mean", "scrolling"), ("static", "switch_100_chats", "switching chats"),
                               ("live", "incoming_5_per_s_30s", "messages arriving"), ("live", "soak", "soak"))
      for m, t, u in (("sys_gpu_over_idle_busy_pct", "GPU busy over idle", "percentage points"),
                      ("sys_gpu_over_idle_power_w", "Package power over idle", "watts"))
      ]),
    ("typing", "Typing", "80 real key presses through the display, 66 ms apart, into the message field.",
     [("static", "type_80_keys", m, f"{t}, typing", u, LOW) for m, t, u in (CPU, WAKE, GPU_S)]),
    ("panels", "Panels", "The details panel of a 500-member group, then the media panel of the long chat, three times.",
     [("static", "panels_details_media", m, f"{t}, panels", u, LOW) for m, t, u in (CPU, WAKE)]
     + [("static", "panels_details_media", "rss_mb", "RSS after the panels", "MB", LOW)]),
    ("resize", "Resize", "Six window sizes in a row, 0.5 s apart.",
     [("static", "resize_sweep", m, f"{t}, resizing", u, LOW) for m, t, u in (CPU, WAKE, GPU_S)]),
    ("startup", "Startup", "From starting the process to the harness answering, and to the chat list having its conversations "
     "(the mock itself waits two seconds to link, alike for all ways).",
     [("static", "startup", "harness_ready_s", "Time to the harness", "s", LOW),
      ("static", "startup", "chat_list_s", "Time to the chat list", "s", LOW)]),
    ("sync", "Catching up with a backlog", "The mock sends a backlog of messages in one burst before it says it has synced. "
     "The snapshot times are how long the window did not respond.",
     [("sync", "sync", "cpu_s", "CPU seconds while catching up", "s", LOW),
      ("sync", "sync", "caught_up_s", "Time until caught up", "s from the process start", LOW),
      ("sync", "sync", "synced_flag_s", "Time until the synced flag", "s from the process start", LOW),
      ("sync", "sync", "rss_mb", "RSS after catching up", "MB", LOW),
      ("sync", "sync", "snapshot_ms_p95", "Window unresponsive, 95th percentile", "ms", LOW),
      ("sync", "sync", "snapshot_ms_max", "Window unresponsive, longest", "ms", LOW)]),
    ("soak", "Soak", "Messages keep arriving for the soak time; the slope is a least-squares fit over the second half of the "
     "memory samples.",
     [("live", "soak", "rss_slope_mb_per_min", "RSS growth", "MB a minute", LOW),
      ("live", "soak", "anon_slope_mb_per_min", "Anonymous memory growth", "MB a minute", LOW),
      ("live", "soak", "cpu_pct", "CPU during the soak", "% of one core", LOW),
      ("live", "soak", "rss_mb", "RSS at the end of the soak", "MB", LOW)]),
    ("frames", "Frame pacing", "The scrolling and an idle chat again, in separate runs with the Wayland requests logged "
     "(the logging costs processor time: these CPU numbers are not the others'). Gaps are between buffer commits of the window.",
     [("frames", "scroll_up_8s", "frames_per_s", "Frames a second, scrolling", "frames/s", HIGH),
      ("frames", "scroll_up_8s", "gap_ms_p50", "Gap between frames, median", "ms", LOW),
      ("frames", "scroll_up_8s", "gap_ms_p95", "Gap between frames, 95th percentile", "ms", LOW),
      ("frames", "scroll_up_8s", "gap_ms_p99", "Gap between frames, 99th percentile", "ms", LOW),
      ("frames", "scroll_up_8s", "gap_ms_max", "Gap between frames, longest", "ms", LOW),
      ("frames", "scroll_up_8s", "gaps_over_33ms", "Gaps over 33 ms, scrolling", "gaps", LOW),
      ("frames", "scroll_up_8s", "gaps_over_50ms", "Gaps over 50 ms, scrolling", "gaps", LOW),
      ("frames", "idle_chat_10s", "frames_per_s", "Frames a second, idle chat", "frames/s", LOW)]),
]


def section(sid, title, intro, body):
    return f'<section id="{sid}"><h2>{esc(title)}</h2><p class="intro">{esc(intro)}</p><div class="grid-panels">{body}</div></section>'


def legend(data, pal):
    items = []
    for way in data.ways:
        cls = "sw patched" if is_patched(way) else "sw"
        items.append(f'<li><span class="{cls}" style="--c:var(--c-{slug(way)});--t:var(--t-{slug(way)})"></span>{esc(way)}</li>')
    return f'<ul class="legend" aria-label="legend">{"".join(items)}</ul>'


# ---------------------------------------------------------------------------------------
# The headline table.


def headline_table(data, baseline):
    head = "".join(f'<th scope="col"><span class="sw{" patched" if is_patched(w) else ""}" '
                   f'style="--c:var(--c-{slug(w)});--t:var(--t-{slug(w)})"></span> {esc(w).replace("-", "-<wbr>")}</th>'
                   for w in data.ways)
    rows = []
    for hid, label, unit, _, better in HEADLINE:
        series = head_series(data, hid)
        if not series:
            continue
        low = min(c["median"] for c in series.values())
        best = {w for w, c in series.items() if c["median"] == low} if len(series) > 1 else set()
        base = series.get(baseline)
        cells = []
        for w in data.ways:
            c = series.get(w)
            if not c:
                cells.append('<td class="na">-</td>')
                continue
            ratio = ""
            if base and w != baseline and base["median"] > 0:
                ratio = f'<small>{c["median"] / base["median"]:.2f}x</small>'
            cls = ' class="best"' if w in best and len(best) < len(series) else ""
            cells.append(f'<td{cls} title="{esc(tooltip(w, c, unit))}">{num(c["median"])}{ratio}</td>')
        rows.append(f'<tr><th scope="row">{esc(label)} <span class="unit">{esc(unit)}</span></th>{"".join(cells)}</tr>')
    note = (f'Median over the rounds; the best way of each row is marked. Under each value of the other ways, the ratio to '
            f'{baseline}, the baseline.' if baseline in data.ways else "Median over the rounds; the best way of each row is marked.")
    return f'<div class="scroll"><table class="head"><thead><tr><th></th>{head}</tr></thead><tbody>{"".join(rows)}</tbody></table></div><p class="note">{esc(note)}</p>'


# ---------------------------------------------------------------------------------------
# Memory, threads and binary size.


def memory_stack(data, scenario):
    rows = []
    for w in data.ways:
        g = lambda m: data.get("static", scenario, m, w)  # noqa: E731
        rss, anon = g("rss_mb"), g("anon_mb")
        if not rss or not anon:
            rows.append((w, None))
            continue
        vram, gtt = g("vram_mb"), g("gtt_mb")
        rows.append((w, [("anon", anon["median"]), ("file", max(rss["median"] - anon["median"], 0)),
                         ("vram", vram["median"] if vram else 0), ("gtt", gtt["median"] if gtt else 0)]))
    return rows


def memory_section(data):
    out = []
    for scenario, title in (("settled", "Memory at the chat list (5 s after the list appears)"),
                            ("after", "Memory after all phases")):
        rows = memory_stack(data, scenario)
        if any(p for _, p in rows):
            out.append(panel(title, "MB · RSS split into anonymous and file-backed, plus GPU memory",
                             stack_svg(rows)))
    for mode, scenario, metric, title, unit in (
            ("static", "idle_chat_after_20s", "rss_mb", "RSS in an open chat", "MB"),
            ("static", "after", "rss_mb", "RSS after all phases", "MB"),
            ("static", "after", "anon_mb", "Anonymous memory after all phases", "MB"),
            ("static", "after", "pss_mb", "PSS after all phases", "MB"),
            ("static", "after", "gpu_mem_mb", "GPU memory after all phases (VRAM + GTT)", "MB")):
        out.append(metric_panel(data, mode, scenario, metric, title, unit))
    segs = "".join(f'<li><span class="sw seg" style="--c:var(--s-{k})"></span>{esc(n)}</li>' for k, n, _ in SEGMENTS)
    intro = ("RSS is what the process holds in RAM (shared libraries included, which is why a stack with large libraries has a large file-backed part); "
             "anonymous is its heap. VRAM and GTT are read from the drm client's fdinfo. The stacks use the medians of "
             "each part, so a stack's total is the median RSS plus the median GPU memory.")
    return (f'<section id="memory"><h2>Memory</h2><p class="intro">{esc(intro)}</p><ul class="legend segs">{segs}</ul>'
            f'<div class="grid-panels">{"".join(out)}</div></section>')


def threads_section(data):
    out = [metric_panel(data, "static", "settled", "threads", "Threads at the chat list", "threads")]
    out.append(series_panel(data, "Binary size", {w: c for w, c in data.binary.items()}, "MB"))
    body = "".join(out)
    if not body:
        return ""
    return ('<section id="threads"><h2>Threads and binary size</h2><p class="intro">Threads are counted in /proc once the '
            'chat list is up. The binary is the file that was mounted into the container (a testbox-profile build with '
            'the harness, not the stripped release).</p><div class="grid-panels">' + body + "</div></section>")


# ---------------------------------------------------------------------------------------
# How it was measured, and the caveats.

SCENARIO_TEXT = [
    ("startup", "static", "From starting the container to the harness answering and to the chat list having its conversations."),
    ("idle_list_15s", "static", "Nothing happens at the chat list."),
    ("idle_chat_first_10s", "static", "The long chat is selected; the first seconds, the caret blinks."),
    ("idle_chat_after_20s", "static", "The same chat 20 s later."),
    ("scroll_up_8s", "static", "Wheel turns every 16 ms, up, through the display's VNC socket."),
    ("scroll_down_8s", "static", "The same, down."),
    ("switch_100_chats", "static", "100 select actions over ten chats, 80 ms apart."),
    ("resize_sweep", "static", "Six window sizes, 0.5 s apart."),
    ("type_80_keys", "static", "80 key presses 66 ms apart."),
    ("panels_details_media", "static", "Details panel of the big group, media panel of the long chat three times."),
    ("incoming_5_per_s_30s", "live", "Five messages a second arrive in the open long chat."),
    ("soak", "live", "The same, for the soak time; memory growth is a fit over the second half of the samples."),
    ("sync", "sync", "A backlog arrives in one burst before the state reports synced."),
    ("scroll_up_8s", "frames", "The scrolling again with the Wayland requests logged, for the gaps between frames."),
]


def median_wall(data, mode, scenario):
    vals = [c["median"] for c in (data.get(mode, scenario, "wall_s", w) for w in data.ways) if c]
    return statistics.median(vals) if vals else None


def disturbed_runs(agg):
    """Runs that waited for a quiet machine, or went on while it was not, from the per-run record."""
    out = []
    for r in agg["runs"]:
        env = r.get("env") or {}
        wait = env.get("load_wait") or {}
        if wait.get("waited_s") or wait.get("exceeded"):
            out.append((r, wait))
    return out


def caveats(data):
    agg, env = data.agg, data.env
    lines = []
    sessions = env.get("sessions", [])
    skipped = {}
    for s in sessions:
        for item in s.get("skipped", []):
            skipped[item["way"]] = item["reason"]
    for way, reason in skipped.items():
        if way not in data.ways:
            lines.append(f"Way {way} was skipped and has no numbers here: {reason}.")
    absent = [w for w in bench.KNOWN_WAYS if w not in data.ways and w not in skipped]
    if absent and len(data.ways) > 2:
        lines.append(f"Known ways with no data in this directory: {', '.join(absent)}.")
    # rounds that differ between ways
    for mode in ("static", "live", "sync", "frames"):
        counts = {w: agg["rounds"][w].get(mode, 0) for w in data.ways}
        if len(set(counts.values())) > 1:
            lines.append(f"Unequal number of {mode} runs: " + ", ".join(f"{w} {n}" for w, n in counts.items()) + ".")
        elif counts and 0 < max(counts.values()) < 3:
            lines.append(f"Only {max(counts.values())} {mode} run(s) per way: medians of so few values are weak; the whiskers are all the spread there is.")
    # disturbed load
    disturbed = disturbed_runs(agg)
    if disturbed:
        waits = "; ".join(f"{r['way']}-{r['mode']}-{r['round']}: waited {w['waited_s']} s from load {w['load_first']}"
                          + (" and went on above the limit" if w.get("exceeded") else "") for r, w in disturbed)
        lines.append(f"Runs that had to wait for the machine to calm down (limit {disturbed[0][1].get('max_load')}): {waits}.")
    with_env = [r for r in agg["runs"] if r.get("env")]
    if not with_env:
        lines.append("These runs carry no per-run load record (they predate it): only the load at the start of the session is known.")
    else:
        busy = [r for r in with_env if r["env"].get("loadavg") and float(r["env"]["loadavg"].split()[0]) > 6
                and not (r["env"].get("load_wait") or {}).get("waited_s")]
        if busy:
            lines.append(f"{len(busy)} run(s) started with a 1-minute load above 6 without a recorded wait (the check was off or set higher).")
    # software fallback
    for way in data.ways:
        if "qt" in way:
            continue
        vram = data.get("static", "after", "vram_mb", way)
        gpu = data.get("static", "after", "gpu_s", way)
        if vram is not None and vram["max"] == 0 and (gpu is None or gpu["max"] == 0):
            lines.append(f"{way} shows no GPU memory and no GPU time in any run: it may have fallen back to software rendering.")
    # noisy headline cells
    noisy = []
    for hid, label, unit, _, _ in HEADLINE:
        for w, c in head_series(data, hid).items():
            if c["n"] >= 2 and c["median"] > 0 and (c["max"] - c["min"]) / c["median"] > 0.5 and c["max"] - c["min"] >= 0.5:
                noisy.append(((c["max"] - c["min"]) / c["median"], f"{w}, {label.lower()}: {num(c['min'])} to {num(c['max'])}"))
    if noisy:
        noisy.sort(reverse=True)
        lines.append("Wide spread between rounds (max minus min over half the median): " + "; ".join(t for _, t in noisy[:5]) + ".")
    if not data.has("sync", "sync", "cpu_s"):
        lines.append("No catching-up runs in this directory.")
    if not data.has("frames", "scroll_up_8s", "frames_per_s"):
        lines.append("No frame-pacing runs in this directory.")
    return lines


def measured_section(data):
    env, agg = data.env, data.agg
    parts = ['<section id="method"><h2>How it was measured</h2>']
    parts.append("<p>Each client is built as the testbox profile and started on the mock backend in a container of its own, in a "
                 "private display (the cage compositor with a VNC output) that draws on the host's GPU through a render node. "
                 "Nothing of the host's desktop is shown to or captured from the clients. The mock holds 200 chats, a 20,000-message "
                 "thread, a 500-member group and 640-pixel pictures. Input is real: wheel turns and key presses through the display's "
                 "VNC socket. Every number is a /proc or drm counter read before and after the phase (CPU time of the process, "
                 "context switches, memory from smaps_rollup, GPU engine time and memory from the drm fdinfo of the client's "
                 "descriptors). The order of the ways rotates from round to round so that a drifting machine is spread over all.</p>")
    machine = []
    for key, label in (("kernel", "Kernel"), ("cpus", "CPUs"), ("gpu", "GPU id"), ("governor", "CPU governor"),
                       ("power_profile", "Power profile"), ("git", "Source commit"), ("loadavg", "Load at the start of the first session")):
        if env.get(key) not in (None, ""):
            machine.append(f"<dt>{label}</dt><dd>{esc(env[key])}</dd>")
    sessions = env.get("sessions", [])
    if sessions:
        s = sessions[0]
        for key, label in (("started", "Started"), ("rounds", "Rounds (static and live)"), ("sync_rounds", "Catching-up rounds"),
                           ("frames_rounds", "Frame-pacing rounds"), ("soak", "Soak seconds"), ("backlog", "Backlog messages"),
                           ("max_load", "Load limit before a run")):
            if s.get(key) is not None:
                machine.append(f"<dt>{label}</dt><dd>{esc(s[key])}</dd>")
        loads = [float(r["env"]["loadavg"].split()[0]) for r in agg["runs"] if (r.get("env") or {}).get("loadavg")]
        if loads:
            machine.append(f"<dt>1-minute load at the start of a run</dt><dd>lowest {num(min(loads))}, highest {num(max(loads))}, "
                           f"over {len(loads)} runs that recorded it</dd>")
        if len(sessions) > 1:
            machine.append(f"<dt>Sessions</dt><dd>{len(sessions)} (the run was continued; ways: "
                           f"{esc('; '.join(', '.join(x.get('ways', {})) for x in sessions))})</dd>")
        extra = {w: v for x in sessions for w, v in x.get("extra", {}).items()}
        for w, v in extra.items():
            machine.append(f"<dt>Extra for {esc(w)}</dt><dd>{esc(v)}</dd>")
    parts.append(f'<dl class="kv">{"".join(machine)}</dl>')
    rows = "".join(f"<tr><th scope='row'>{esc(w)}</th>" + "".join(f"<td>{agg['rounds'][w].get(m, 0)}</td>" for m in ("static", "live", "sync", "frames")) + "</tr>"
                   for w in data.ways)
    parts.append("<h3>Runs that finished, per way</h3><div class='scroll'><table class='plain'><thead><tr><th></th><th>static</th>"
                 f"<th>live</th><th>sync</th><th>frames</th></tr></thead><tbody>{rows}</tbody></table></div>")
    srows = []
    for scenario, mode, text in SCENARIO_TEXT:
        wall = median_wall(data, mode, scenario)
        srows.append(f"<tr><th scope='row'>{esc(scenario)}</th><td>{esc(mode)}</td><td>{esc(text)}</td>"
                     f"<td>{num(wall) + ' s' if wall else '-'}</td></tr>")
    parts.append("<h3>The scenarios</h3><div class='scroll'><table class='plain'><thead><tr><th>Scenario</th><th>Run</th><th>What happens</th>"
                 f"<th>Window (median)</th></tr></thead><tbody>{''.join(srows)}</tbody></table></div>")
    parts.append("<h3>What the numbers cannot say</h3><ul>"
                 "<li>One machine, one GPU, one driver, one compositor: another setup can order the ways differently.</li>"
                 "<li>The mock has no network, no real Signal data and no real pictures from a camera; it measures the clients, not the service.</li>"
                 "<li>CPU is the whole process, all threads, in percent of one core; a way that moves work to the GPU shows it in GPU time and memory, not in CPU.</li>"
                 "<li>Wakeups are context switches of the process (voluntary and not), not hardware wakeups or power.</li>"
                 + ("<li>Qt on the CPU draws without the GPU, so its GPU numbers are zero by design; the file-backed part of its RSS includes the Qt libraries.</li>"
                    if any("qt" in w for w in data.ways) else "") +
                 "<li>A few rounds give a median and a range, not a confidence interval. The whiskers are the smallest and the largest round.</li>"
                 "<li>Frame gaps come from separate runs with the request log on, which costs processor time; they say how evenly frames were committed, not how they looked.</li>"
                 "<li>Nothing here measures input latency, text quality or visual differences.</li></ul>")
    parts.append("</section>")
    return "".join(parts)


def caveat_section(data):
    lines = caveats(data)
    body = "".join(f"<li>{esc(x)}</li>" for x in lines) if lines else "<li>Nothing to note: all ways have the same rounds, none waited for the machine, none shows a software fallback.</li>"
    return f'<section id="caveats"><h2>Caveats</h2><ul>{body}</ul></section>'


def raw_table(data):
    head = "".join(f"<th>{esc(w)}</th>" for w in data.ways)
    rows = []
    for sc in data.agg["scenarios"]:
        for metric, by_way in sc["metrics"].items():
            cells = []
            for w in data.ways:
                c = by_way.get(w)
                cells.append(f'<td title="{esc(", ".join(num(v) for v in c["values"]))}">{num(c["median"])} <small>({num(c["min"])}-{num(c["max"])}, n={c["n"]})</small></td>'
                             if c else '<td class="na">-</td>')
            rows.append(f"<tr><th scope='row'>{esc(sc['mode'])}/{esc(sc['scenario'])}</th><td>{esc(metric)}</td>{''.join(cells)}</tr>")
    return ("<section id='raw'><details><summary>Raw numbers, every way, every metric</summary>"
            "<p class='note'>Median (min-max, rounds) per way; hover a cell for the value of each round. Derived rows: "
            "gpu_mem_mb is vram_mb + gtt_mb; scroll_both_8s_mean is the mean of the up and down phases of the same run.</p>"
            f"<div class='scroll tall'><table class='raw'><thead><tr><th>Scenario</th><th>Metric</th>{head}</tr></thead>"
            f"<tbody>{''.join(rows)}</tbody></table></div></details></section>")


# ---------------------------------------------------------------------------------------
# The page.


def css(ways, pal):
    def block(mode):
        i = 0 if mode == "light" else 1
        lines = [f"--bg:{('#f9f9f7', '#0d0d0d')[i]};--surface:{SURFACE[i]};--ink:{INK[i]};--ink2:{('#52514e', '#c3c2b7')[i]};"
                 f"--muted:{('#6e6c66', '#898781')[i]};--grid:{('#e1e0d9', '#2c2c2a')[i]};--axis:{('#c3c2b7', '#4a4a46')[i]};"
                 f"--hair:{('#e1e0d9', '#2c2c2a')[i]};--best:{('#e8f1fb', '#1d2a3a')[i]};--accent:{('#2a78d6', '#3987e5')[i]};"]
        for key, _, colors in SEGMENTS:
            lines.append(f"--s-{key}:{colors[i]};")
        for w in ways:
            base, tint = pal[w]
            lines.append(f"--c-{slug(w)}:{base[i]};--t-{slug(w)}:{tint[i]};")
        return "".join(lines)
    light, dark = block("light"), block("dark")
    return f"""
:root{{color-scheme:light dark;{light}}}
@media (prefers-color-scheme: dark){{:root:not([data-theme="light"]){{color-scheme:dark;{dark}}}}}
:root[data-theme="dark"]{{color-scheme:dark;{dark}}}
*{{box-sizing:border-box}}
html{{background:var(--bg)}}
body{{margin:0;background:var(--bg);color:var(--ink);font:15px/1.5 system-ui,-apple-system,"Segoe UI",sans-serif;-webkit-text-size-adjust:100%}}
main{{max-width:1120px;margin:0 auto;padding:20px 16px 64px}}
h1{{font-size:26px;line-height:1.2;margin:8px 0 6px;font-weight:650}}
h2{{font-size:20px;margin:40px 0 6px;font-weight:650;border-top:1px solid var(--hair);padding-top:20px}}
h3{{font-size:16px;margin:24px 0 8px}}
p,li{{max-width:75ch}}
.sub{{color:var(--ink2);margin:0 0 16px}}
.intro,.note{{color:var(--ink2);margin:0 0 14px}}
.note{{font-size:13px;margin:8px 0 0}}
.summary{{background:var(--surface);border:1px solid var(--hair);border-radius:10px;padding:12px 16px 12px 36px;margin:12px 0 18px}}
.summary li{{margin:5px 0;max-width:none}}
.legend{{list-style:none;display:flex;flex-wrap:wrap;gap:6px 16px;padding:0;margin:8px 0 18px}}
.legend li{{display:flex;align-items:center;gap:6px;font-size:13px;max-width:none}}
.sw{{display:inline-block;width:12px;height:12px;border-radius:2px;background:var(--c);flex:none;vertical-align:-1px}}
.sw.patched{{background:repeating-linear-gradient(45deg,var(--c) 0 2px,var(--t) 2px 5px)}}
.sw.seg{{background:var(--c)}}
.scroll{{overflow-x:auto;-webkit-overflow-scrolling:touch}}
.scroll.tall{{max-height:70vh;overflow:auto}}
table{{border-collapse:collapse;font-size:13px;font-variant-numeric:tabular-nums}}
th,td{{padding:5px 8px;border-bottom:1px solid var(--hair);text-align:right;vertical-align:top}}
thead th{{font-weight:600;color:var(--ink2);vertical-align:bottom;min-width:84px}}
tbody th{{text-align:left;font-weight:500}}
table.head th:first-child{{position:sticky;left:0;background:var(--bg);min-width:180px;max-width:220px;z-index:1}}
table.head tbody th{{white-space:normal}}
table.head td{{white-space:nowrap}}
td.best{{background:var(--best);font-weight:650}}
td small{{display:block;color:var(--muted);font-weight:400;font-size:11px}}
.raw td small{{display:inline}}
td.na{{color:var(--muted)}}
.unit{{color:var(--muted);font-weight:400}}
table.plain td,table.plain th{{text-align:left}}
table.plain thead th{{min-width:0}}
table.plain td:nth-child(3){{min-width:240px}}
table.plain td:first-child{{white-space:nowrap}}
.grid-panels{{display:grid;grid-template-columns:repeat(auto-fill,minmax(340px,1fr));gap:18px 20px}}
.panel{{margin:0;background:var(--surface);border:1px solid var(--hair);border-radius:10px;padding:10px 12px 8px;min-width:0}}
.panel figcaption{{display:flex;flex-direction:column;margin-bottom:4px;gap:1px}}
.panel figcaption b{{font-size:14px;font-weight:600}}
.panel figcaption span{{font-size:12px;color:var(--muted)}}
svg.chart{{display:block;width:100%;max-width:460px;height:auto}}
.chart text{{font:11px system-ui,-apple-system,"Segoe UI",sans-serif;fill:var(--ink2)}}
.chart .lab{{fill:var(--ink)}}
.chart .val{{fill:var(--ink);font-variant-numeric:tabular-nums}}
.chart .val.best{{font-weight:700}}
.chart .tick{{fill:var(--muted);font-size:10px;font-variant-numeric:tabular-nums}}
.chart .muted{{fill:var(--muted);font-style:italic}}
.chart .grid{{stroke:var(--grid);stroke-width:1}}
.chart .axis{{stroke:var(--axis);stroke-width:1}}
.chart .whisk{{stroke:var(--ink);stroke-width:1.5;fill:none}}
.chart .whisk-halo{{stroke:var(--surface);stroke-width:4;fill:none}}
dl.kv{{display:grid;grid-template-columns:max-content 1fr;gap:4px 16px;margin:12px 0;font-size:14px}}
dl.kv dt{{color:var(--ink2)}}dl.kv dd{{margin:0;overflow-wrap:anywhere}}
details summary{{cursor:pointer;font-weight:600;padding:8px 0}}
.raw th:first-child,.raw td:nth-child(2){{text-align:left}}
.raw td:nth-child(2){{color:var(--ink2)}}
nav.toc{{display:flex;flex-wrap:wrap;gap:4px 14px;font-size:13px;margin:6px 0 4px}}
nav.toc a{{color:var(--accent);text-decoration:none}}
@media (max-width:480px){{dl.kv{{grid-template-columns:1fr;gap:0}}dl.kv dd{{margin:0 0 8px}}h1{{font-size:22px}}.grid-panels{{grid-template-columns:minmax(0,1fr)}}main{{padding:14px 16px 48px}}}}
"""


def defs(ways, pal):
    """Hatched fills of the patched ways: the tint with diagonal lines of the stack's colour."""
    out = ['<svg width="0" height="0" style="position:absolute" aria-hidden="true"><defs>']
    for w in ways:
        if is_patched(w):
            s = slug(w)
            out.append(f'<pattern id="fill-{s}" width="6" height="6" patternUnits="userSpaceOnUse" patternTransform="rotate(45)">'
                       f'<rect width="6" height="6" style="fill:var(--t-{s})"/><rect width="2.5" height="6" style="fill:var(--c-{s})"/></pattern>')
    out.append("</defs></svg>")
    return "".join(out)


def build(directory, baseline="slint-skia", title="b33p graphics stacks"):
    agg = bench.aggregate(directory)
    data = Data(agg)
    if not data.ways:
        raise SystemExit(f"report_html.py: no runs in {directory}")
    pal = way_palette(data.ways)
    base = bench.pick_baseline(data.ways, baseline)
    env = data.env
    n_runs = len(agg["runs"])
    sub = (f"{len(data.ways)} ways, {n_runs} runs, on the mock backend at {esc(env.get('git') or '?')}"
           f"{', ' + esc(env.get('kernel')) if env.get('kernel') else ''}. Baseline for ratios: {esc(base)}.")
    summary = "".join(f"<li>{esc(x)}</li>" for x in summary_lines(data)) or "<li>Too little data for a summary.</li>"
    sections = ['<section id="headline"><h2>Headline numbers</h2>' + headline_table(data, base) + "</section>"]
    for sid, stitle, intro, specs in SECTIONS:
        panels = "".join(metric_panel(data, mode, scenario, metric, ptitle, unit, better)
                         for mode, scenario, metric, ptitle, unit, better in specs)
        if panels:
            sections.append(section(sid, stitle, intro, panels))
    mem, thr = memory_section(data), threads_section(data)
    toc_items = [("headline", "Headline")] + [(sid, st) for sid, st, _, _ in SECTIONS if f'id="{sid}"' in "".join(sections)]
    toc_items += [("memory", "Memory"), ("threads", "Threads and size"), ("method", "How it was measured"), ("caveats", "Caveats"), ("raw", "Raw numbers")]
    toc = '<nav class="toc">' + "".join(f'<a href="#{i}">{esc(t)}</a>' for i, t in toc_items) + "</nav>"
    body = "".join(sections) + mem + thr + measured_section(data) + caveat_section(data) + raw_table(data)
    return f"""<!doctype html>
<html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width, initial-scale=1">
<meta name="color-scheme" content="light dark"><title>{esc(title)}</title><style>{css(data.ways, pal)}</style></head>
<body>{defs(data.ways, pal)}<main>
<h1>{esc(title)}</h1><p class="sub">{sub}</p>
{legend(data, pal)}
<ul class="summary" style="list-style:decimal">{summary}</ul>
{toc}
{body}
</main></body></html>
"""


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("directory", help="a bench.py output directory")
    parser.add_argument("out", help="the html file to write")
    parser.add_argument("--baseline", default="slint-skia", help="the way the ratios in the headline table are against (default slint-skia)")
    parser.add_argument("--title", default="b33p graphics stacks")
    args = parser.parse_args()
    page = build(args.directory, args.baseline, args.title)
    with open(args.out, "w") as f:
        f.write(page)
    print(f"wrote {args.out} ({len(page) / 1024:.0f} KB)")


if __name__ == "__main__":
    main()
