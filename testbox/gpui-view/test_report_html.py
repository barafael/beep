"""Tests of report_html.py: the page is built from the aggregation alone, is self-contained, has
a colour for each way, and says what is missing. Run: python3 -I -m unittest discover -s testbox/gpui-view"""

import html.parser
import json
import os
import re
import subprocess
import sys
import tempfile
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
import bench  # noqa: E402
import report_html as rh  # noqa: E402
from test_bench import static_result, write_run  # noqa: E402

VOID = {"meta", "br", "hr", "img", "input", "link", "col", "wbr"}


class Balanced(html.parser.HTMLParser):
    """Whether every opened element is closed in order (svg's self-closed ones included)."""

    def __init__(self):
        super().__init__()
        self.stack, self.errors = [], []

    def handle_starttag(self, tag, attrs):
        if tag not in VOID:
            self.stack.append(tag)

    def handle_startendtag(self, tag, attrs):
        pass

    def handle_endtag(self, tag):
        if tag in VOID:
            return
        if not self.stack or self.stack[-1] != tag:
            self.errors.append(f"unexpected </{tag}> with {self.stack[-3:]}")
        else:
            self.stack.pop()


def live_result(cpu):
    phase = {"wall_s": 30.0, "cpu_s": cpu * 0.3, "cpu_pct": cpu, "gpu_s": 0.0, "wakeups_per_s": cpu * 20,
             "vram_mb": 0.0, "gtt_mb": 0.0, "rss_mb": 100.0, "pss_mb": 90.0, "anon_mb": 50.0, "threads": 20}
    return {"incoming_5_per_s_30s": phase, "soak": {**phase, "rss_slope_mb_per_min": 0.5, "anon_slope_mb_per_min": 0.4}}


class Page(unittest.TestCase):
    def make(self, ways, rounds=3, sync=False):
        d = tempfile.mkdtemp()
        self.addCleanup(lambda: subprocess.run(["rm", "-rf", d]))
        for i, (way, cpu, vram) in enumerate(ways):
            for r in range(1, rounds + 1):
                write_run(d, way, "static", r, static_result(cpu + r, 100 + 10 * i, vram))
                write_run(d, way, "live", r, live_result(cpu * 2 + r))
            if sync:
                write_run(d, way, "sync", 1, {"sync": {"wall_s": 3.0, "cpu_s": 1.0 + i, "cpu_pct": 30.0, "caught_up_s": 3.0 + i,
                                                       "synced_flag_s": 2.9, "rss_mb": 300.0, "unread_total": 1000}})
        with open(os.path.join(d, "environment.json"), "w") as f:
            json.dump({"git": "abc1234", "kernel": "k", "binaries": {w: int(40e6 + 1e6 * i) for i, (w, *_) in enumerate(ways)},
                       "sessions": [{"started": "t", "ways": {}, "skipped": [{"way": "slint-skia", "reason": "binary missing"}],
                                     "max_load": 6, "rounds": rounds}]}, f)
        return d

    FOUR = [("slint-qt", 20, 0), ("slint-femtovg", 15, 60), ("slint-femtovg-patched", 12, 60), ("gpui", 5, 90)]

    def test_self_contained(self):
        page = rh.build(self.make(self.FOUR))
        self.assertNotRegex(page, r"https?://")
        self.assertNotIn("<script", page)
        self.assertNotIn("@import", page)
        self.assertNotRegex(page, r'(src|href)="(?!#)')
        self.assertIn('name="viewport"', page)
        self.assertIn("prefers-color-scheme: dark", page)
        self.assertRegex(page, r"body\{margin:0;background:var\(--bg\)")

    def test_structure_is_balanced(self):
        p = Balanced()
        p.feed(rh.build(self.make(self.FOUR, sync=True)))
        self.assertEqual(p.errors, [])
        self.assertEqual(p.stack, [])

    def test_a_colour_for_each_way_used_in_every_chart(self):
        page = rh.build(self.make(self.FOUR))
        for way in ("slint-qt", "slint-femtovg", "gpui"):
            self.assertEqual(len(re.findall(rf"var\(--c-{way}\)", page)) >= 10, True, way)
        self.assertIn('id="fill-slint-femtovg-patched"', page)  # the hatched variant
        pal = rh.way_palette(["slint-qt", "slint-qt-patched", "gpui"])
        self.assertEqual(pal["slint-qt"][0], pal["slint-qt-patched"][0])  # same hue
        self.assertNotEqual(pal["slint-qt-patched"][0], pal["slint-qt-patched"][1])  # the tint is another colour

    def test_summary_is_derived_from_the_numbers(self):
        data = rh.Data(bench.aggregate(self.make(self.FOUR)))
        lines = rh.summary_lines(data)
        self.assertLessEqual(len(lines), 5)
        self.assertGreaterEqual(len(lines), 4)
        incoming = lines[0]
        # live cpu = 2 * cpu + round (median round 2): gpui 12, slint-qt 42
        self.assertIn("best gpui with 12%", incoming)
        self.assertIn("worst slint-qt with 42%", incoming)
        self.assertIn("3.5 times the best", incoming)
        self.assertTrue(any(l.startswith("Patches, slint-femtovg-patched against slint-femtovg") for l in lines))

    def test_every_number_in_the_headline_table_is_a_median(self):
        d = self.make(self.FOUR)
        agg = bench.aggregate(d)
        data = rh.Data(agg)
        table = rh.headline_table(data, "slint-qt")
        shown = set(re.findall(r"<td[^>]*>([\d,.]+)", table))
        known = set()
        for hid in rh.HEAD:
            for c in rh.head_series(data, hid).values():
                known.add(rh.num(c["median"]))
        self.assertTrue(shown)
        self.assertLessEqual(shown, known)

    def test_two_way_data_and_a_missing_way_still_render(self):
        page = rh.build(self.make([("slint", 20, 0), ("gpui", 5, 90)]))
        self.assertIn("Baseline for ratios: slint.", page)
        d = self.make(self.FOUR)
        # a way that is missing for a scenario shows as no data, not as a zero
        os.remove(os.path.join(d, "gpui-live-1.json"))
        os.remove(os.path.join(d, "gpui-live-2.json"))
        os.remove(os.path.join(d, "gpui-live-3.json"))
        page = rh.build(d)
        self.assertIn("no data", page)
        self.assertIn("Unequal number of live runs", page)

    def test_caveats_from_the_data(self):
        d = self.make(self.FOUR)
        # a way on the GPU that shows none: software fallback
        for r in (1, 2, 3):
            write_run(d, "slint-skia", "static", r, static_result(10, 100, 0.0))
        write_run(d, "slint-skia", "live", 1, live_result(10),
                  {"loadavg": "9 9 9 1/1 1", "load_wait": {"waited_s": 600, "load_first": 9.0, "exceeded": True, "max_load": 6}})
        page = rh.build(d)
        self.assertIn("slint-skia shows no GPU memory and no GPU time", page)
        self.assertNotIn("slint-qt shows no GPU memory", page)  # Qt draws on the CPU by design
        self.assertIn("slint-skia-live-1: waited 600 s from load 9", page)
        self.assertIn("went on above the limit", page)
        self.assertIn("Way slint-skia was skipped", rh.build(self.make(self.FOUR)))
        self.assertIn("No catching-up runs", page)

    def test_bars_start_at_zero_and_scale_linearly(self):
        rows = [("a", {"median": 10.0, "min": 8.0, "max": 12.0, "values": [8, 10, 12], "n": 3}),
                ("b", {"median": 5.0, "min": 5.0, "max": 5.0, "values": [5], "n": 1}), ("c", None)]
        svg = rh.bar_svg(rows, "x")
        paths = re.findall(r'<path d="M([\d.]+),[\d.]+h([\d.]+)', svg)
        self.assertEqual(len(paths), 2)
        self.assertTrue(all(float(x) == rh.PLOT_X for x, _ in paths))  # the same baseline
        self.assertAlmostEqual(float(paths[0][1]) / float(paths[1][1]), 2.0, delta=0.15)  # 10 against 5 (a 4 px rounded end)
        self.assertEqual(svg.count('class="whisk"'), 1)  # only the way with a range
        self.assertIn("no data", svg)

    def test_nice_ceiling(self):
        for v in (0.0, 0.003, 0.9, 1, 7.3, 22, 99, 101, 1959, 45000):
            top, step = rh.nice_ceiling(v)
            self.assertGreaterEqual(top, v)
            self.assertLessEqual(top, max(v * 2.6, 1))
            self.assertGreater(step, 0)

    def test_command_line(self):
        d = self.make(self.FOUR)
        out = os.path.join(d, "out.html")
        r = subprocess.run([sys.executable, "-I", os.path.join(HERE, "report_html.py"), d, out], capture_output=True, text=True)
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertTrue(os.path.getsize(out) > 10000)


if __name__ == "__main__":
    unittest.main()
