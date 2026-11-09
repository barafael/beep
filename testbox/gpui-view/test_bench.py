"""Tests of bench.py's pure parts: the ways, the order of rounds, resume, the load check, the
aggregation and the report. Run: python3 -I -m unittest discover -s testbox/gpui-view
(or python3 testbox/gpui-view/test_bench.py). Nothing here starts a container."""

import io
import json
import os
import subprocess
import sys
import tempfile
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
import bench  # noqa: E402


def write_run(directory, way, mode, round_, result, env=None):
    record = {"ui": way, "way": way, "kind": bench.way_kind(way), "mode": mode, "round": round_,
              "scale": "s", "result": result}
    if env is not None:
        record["env"] = env
    with open(os.path.join(directory, f"{way}-{mode}-{round_}.json"), "w") as f:
        json.dump(record, f)


def static_result(cpu, rss=100.0, vram=0.0):
    phase = {"wall_s": 8.0, "cpu_s": cpu / 12.5, "cpu_pct": cpu, "gpu_s": 0.0, "wakeups_per_s": cpu * 10,
             "vram_mb": vram, "gtt_mb": 0.0, "rss_mb": rss, "pss_mb": rss - 10, "anon_mb": rss / 2, "threads": 20}
    return {"scroll_up_8s": dict(phase), "scroll_down_8s": {**phase, "cpu_pct": cpu + 2},
            "after": {"rss_mb": rss, "pss_mb": rss - 10, "anon_mb": rss / 2, "vram_mb": vram, "gtt_mb": 1.0,
                      "gpu_s": 0.0, "threads": 20}}


class Ways(unittest.TestCase):
    def test_kind_from_prefix(self):
        self.assertEqual(bench.way_kind("slint-qt-patched"), "slint")
        self.assertEqual(bench.way_kind("slint"), "slint")
        self.assertEqual(bench.way_kind("gpui"), "gpui")
        with self.assertRaises(ValueError):
            bench.way_kind("slinty")
        with self.assertRaises(ValueError):
            bench.way_kind("qt")

    def test_parse_ways(self):
        self.assertEqual(bench.parse_ways("slint-qt=/a/b, gpui=/c"), [("slint-qt", "/a/b"), ("gpui", "/c")])
        with self.assertRaises(ValueError):
            bench.parse_ways("slint-qt")

    def make_dir(self, names):
        d = tempfile.mkdtemp()
        self.addCleanup(lambda: subprocess.run(["rm", "-rf", d]))
        for n in names:
            p = os.path.join(d, n)
            with open(p, "w") as f:
                f.write("#!/bin/sh\n")
            os.chmod(p, 0o755)
        return d

    def test_ways_dir_skips_missing_with_a_note(self):
        d = self.make_dir(["gpui", "slint-femtovg", "slint-skia-patched"])
        ways, skipped = bench.resolve_ways(ways_dir=d)
        self.assertEqual([w.label for w in ways], ["slint-femtovg", "slint-skia-patched", "gpui"])
        self.assertEqual({l for l, _ in skipped}, {"slint-femtovg-patched", "slint-skia"})
        self.assertEqual([w.kind for w in ways], ["slint", "slint", "gpui"])

    def test_explicit_ways_win_and_take_env(self):
        d = self.make_dir(["slint-qt"])
        other = self.make_dir(["mine"])
        ways, skipped = bench.resolve_ways(
            ways=f"slint-qt={other}/mine,gpui={other}/nothere", ways_dir=d,
            extra_env=["slint-qt:A=b=c"], extra_args=["slint-qt:--cpus=2"])
        self.assertEqual([w.path for w in ways], [f"{other}/mine"])
        self.assertEqual(ways[0].env, {"A": "b=c"})
        self.assertEqual(ways[0].args, ["--cpus=2"])
        self.assertIn("gpui", [l for l, _ in skipped])

    def test_env_for_unknown_way_is_an_error(self):
        d = self.make_dir(["slint-qt"])
        with self.assertRaises(ValueError):
            bench.resolve_ways(ways_dir=d, extra_env=["slint-extra:A=b"])

    def test_two_way_default_uses_bench_variables(self):
        ways, skipped = bench.resolve_ways(uis="slint,gpui")
        self.assertEqual({l for l, _ in skipped} | {w.label for w in ways}, {"slint", "gpui"})

    def test_order(self):
        labels = ["gpui", "slint-skia", "slint-qt-patched", "slint-femtovg", "slint-qt", "slint-zzz"]
        self.assertEqual(sorted(labels, key=bench.way_order),
                         ["slint-qt", "slint-qt-patched", "slint-femtovg", "slint-skia", "slint-zzz", "gpui"])


class Rounds(unittest.TestCase):
    def test_rotate_spreads_the_start(self):
        ways = list("abcd")
        firsts = [bench.rotate(ways, r - 1)[0] for r in range(1, 5)]
        self.assertEqual(sorted(firsts), ways)
        self.assertEqual(bench.rotate(ways, 1), list("bcda"))
        self.assertEqual(bench.rotate([], 3), [])

    def test_two_ways_alternate_as_before(self):
        jobs = bench.plan_jobs(["slint", "gpui"], 2, 0, 0)
        self.assertEqual([(w, m, r) for w, m, r in jobs],
                         [("slint", "static", 1), ("slint", "live", 1), ("gpui", "static", 1), ("gpui", "live", 1),
                          ("gpui", "static", 2), ("gpui", "live", 2), ("slint", "static", 2), ("slint", "live", 2)])

    def test_every_way_runs_every_mode_every_round(self):
        ways = list("abc")
        jobs = bench.plan_jobs(ways, 3, 2, 1)
        self.assertEqual(len(jobs), 3 * 3 * 2 + 3 * 2 + 3)
        self.assertEqual(len(set(jobs)), len(jobs))

    def test_container_names_are_unique(self):
        ways = [bench.Way(l, "/x") for l in ("slint-qt", "slint-qt-patched", "gpui")]
        names = set()
        jobs = bench.plan_jobs(ways, 3, 2, 2)
        for w, m, r in jobs:
            names.add(bench.Run(w, m, r, "/o", "s").name)
        self.assertEqual(len(names), len(jobs))
        os.environ["BENCH_PREFIX"] = "zz"
        try:
            self.assertTrue(bench.Run(ways[0], "static", 1, "/o", "s").name.startswith("zz-slint-qt-static-1"))
        finally:
            del os.environ["BENCH_PREFIX"]

    def test_command_has_way_env_and_args(self):
        w = bench.Way("slint-skia", "/bin/x", env={"SLINT_BACKEND": "winit-skia"}, args=["--cpus=4"])
        cmd = bench.Run(w, "frames", 1, "/o", "chats=1", frames=True).command()
        self.assertIn("SLINT_BACKEND=winit-skia", cmd)
        self.assertIn("--cpus=4", cmd)
        self.assertIn("/bin/x:/usr/local/bin/b33p:ro", cmd)
        self.assertIn("WAYLAND_DEBUG=client exec b33p", cmd)
        self.assertIn("B33P_MOCK_SCALE=chats=1", cmd)


class Resume(unittest.TestCase):
    def test_finished(self):
        with tempfile.TemporaryDirectory() as d:
            self.assertFalse(bench.finished(d, "a-static-1"))
            path = os.path.join(d, "a-static-1.json")
            for text, expected in (("{", False), ('{"result": {}}', False), ('{"result": {"x": {"cpu_pct": 1}}}', True)):
                with open(path, "w") as f:
                    f.write(text)  # the first is a run that was cut off
                self.assertEqual(bench.finished(d, "a-static-1"), expected)

    def test_write_json_is_atomic(self):
        with tempfile.TemporaryDirectory() as d:
            bench.write_json(os.path.join(d, "x.json"), {"a": 1})
            self.assertEqual(os.listdir(d), ["x.json"])

    def test_dry_run_shows_what_resume_skips(self):
        with tempfile.TemporaryDirectory() as d, tempfile.TemporaryDirectory() as b:
            for n in ("slint-skia", "gpui"):
                p = os.path.join(b, n)
                with open(p, "w") as f:
                    f.write("#!/bin/sh\n")
                os.chmod(p, 0o755)
            write_run(d, "slint-skia", "static", 1, static_result(10))
            out = subprocess.run([sys.executable, "-I", os.path.join(HERE, "bench.py"), "run", "--ways-dir", b,
                                  "--rounds", "1", "--sync-rounds", "0", "--frames-rounds", "0", "--soak", "30",
                                  "--out", d, "--dry-run"], capture_output=True, text=True)
            self.assertEqual(out.returncode, 0, out.stderr)
            self.assertIn("== slint-skia-static-1 (slint, ~120 s) already done: skipped (resume)", out.stdout)
            self.assertIn("== gpui-static-1 (gpui", out.stdout)
            self.assertEqual(out.stdout.count("podman run"), 3)
            self.assertIn("skipped slint-femtovg", out.stdout)
            self.assertEqual(sorted(os.listdir(d)), ["slint-skia-static-1.json"])  # nothing written


class LoadCheck(unittest.TestCase):
    def test_no_wait_when_quiet_or_off(self):
        self.assertEqual(bench.wait_for_load(6, read=lambda: 2.0)["waited_s"], 0)
        self.assertEqual(bench.wait_for_load(0, read=lambda: 50.0)["waited_s"], 0)

    def test_waits_until_it_drops(self):
        loads = iter([9.0, 9.0, 8.0, 5.0, 5.0])
        clock = {"t": 0.0}
        def sleep(s):
            clock["t"] += s
        notes = io.StringIO()
        r = bench.wait_for_load(6, limit=600, poll=10, read=lambda: next(loads), sleep=sleep, clock=lambda: clock["t"],
                                say=lambda *a, **k: print(*a, file=notes))
        self.assertEqual((r["load_first"], r["waited_s"], r["exceeded"]), (9.0, 20, False))
        self.assertIn("waiting", notes.getvalue())

    def test_gives_up_after_the_limit(self):
        clock = {"t": 0.0}
        def sleep(s):
            clock["t"] += s
        r = bench.wait_for_load(6, limit=600, poll=10, read=lambda: 9.0, sleep=sleep, clock=lambda: clock["t"], say=lambda *a, **k: None)
        self.assertEqual((r["waited_s"], r["exceeded"]), (600, True))


class Aggregation(unittest.TestCase):
    def setUp(self):
        self.dir = tempfile.mkdtemp()
        self.addCleanup(lambda: subprocess.run(["rm", "-rf", self.dir]))
        for rnd, (qt, gp) in enumerate([(30, 10), (20, 12), (40, 11)], 1):
            write_run(self.dir, "slint-qt", "static", rnd, static_result(qt, 100), {"load_wait": {"waited_s": 0, "load_first": 2}})
            write_run(self.dir, "gpui", "static", rnd, static_result(gp, 150, vram=80))
        write_run(self.dir, "slint-femtovg", "static", 1, static_result(15, 120, vram=50),
                  {"loadavg": "9.5 5 5 1/1 1", "load_wait": {"waited_s": 120, "load_first": 9.5, "exceeded": False, "max_load": 6}})
        for name, text in (("environment.json", json.dumps({"git": "abc", "binaries": {"slint-qt": 40e6, "gpui": 50e6}})),
                           ("broken.json", "{"), ("notarun.json", '{"a": 1}')):
            with open(os.path.join(self.dir, name), "w") as f:
                f.write(text)

    def test_medians_ranges_and_counts(self):
        agg = bench.aggregate(self.dir)
        self.assertEqual(agg["ways"], ["slint-qt", "slint-femtovg", "gpui"])
        sc = {(s["mode"], s["scenario"]): s["metrics"] for s in agg["scenarios"]}
        c = sc[("static", "scroll_up_8s")]["cpu_pct"]["slint-qt"]
        self.assertEqual((c["median"], c["min"], c["max"], c["n"], c["values"]), (30, 20, 40, 3, [30, 20, 40]))
        self.assertEqual(sc[("static", "scroll_up_8s")]["cpu_pct"]["slint-femtovg"]["n"], 1)
        self.assertEqual(agg["rounds"]["slint-qt"], {"static": 3})

    def test_derived_numbers(self):
        agg = bench.aggregate(self.dir)
        sc = {(s["mode"], s["scenario"]): s["metrics"] for s in agg["scenarios"]}
        self.assertEqual(sc[("static", "after")]["gpu_mem_mb"]["gpui"]["median"], 81.0)
        # the mean of the up (cpu) and the down (cpu + 2) phases of the same run
        self.assertEqual(sc[("static", "scroll_both_8s_mean")]["cpu_pct"]["gpui"]["values"], [11.0, 13.0, 12.0])

    def test_run_records_are_kept_for_the_load_notes(self):
        agg = bench.aggregate(self.dir)
        waited = [r for r in agg["runs"] if (r["env"] or {}).get("load_wait", {}).get("waited_s")]
        self.assertEqual([r["way"] for r in waited], ["slint-femtovg"])

    def test_old_two_way_records_without_way_field(self):
        d = tempfile.mkdtemp()
        self.addCleanup(lambda: subprocess.run(["rm", "-rf", d]))
        with open(os.path.join(d, "slint-static-1.json"), "w") as f:
            json.dump({"ui": "slint", "mode": "static", "round": 1, "scale": "s", "result": static_result(5)}, f)
        self.assertEqual(bench.aggregate(d)["ways"], ["slint"])

    def test_baseline(self):
        self.assertEqual(bench.pick_baseline(["slint-skia", "gpui"], "slint-skia"), "slint-skia")
        self.assertEqual(bench.pick_baseline(["slint", "gpui"], "slint-skia"), "slint")
        self.assertEqual(bench.pick_baseline(["gpui", "slint-femtovg"], "slint-skia"), "gpui")
        self.assertEqual(bench.pick_baseline(["slint-skia", "gpui"], "gpui"), "gpui")
        # runs from before Qt was retired
        self.assertEqual(bench.pick_baseline(["slint-qt", "gpui"], "slint-skia"), "slint-qt")

    def test_report_table_and_ratios(self):
        out = io.StringIO()
        bench.report(self.dir, "slint-qt", False, out)
        text = out.getvalue()
        self.assertIn("| phase | metric | slint-qt | slint-femtovg | gpui | slint-femtovg / slint-qt | gpui / slint-qt |", text)
        self.assertIn("| scroll_up_8s | cpu_pct | 30 (20-40) | 15 | 11 (10-12) | 0.50x | 0.37x |", text)
        self.assertIn("slint-femtovg-static-1 waited 120 s", text)

    def test_report_json(self):
        out = io.StringIO()
        bench.report(self.dir, "slint-qt", True, out)
        data = json.loads(out.getvalue())
        self.assertEqual(data["ways"], ["slint-qt", "slint-femtovg", "gpui"])
        self.assertEqual(data["environment"]["git"], "abc")

    def test_help_documents_the_options(self):
        for sub, words in (("run", ["--ways", "--ways-dir", "--max-load", "--dry-run", "--env", "--force"]),
                           ("report", ["--baseline", "--json", "slint-qt"])):
            out = subprocess.run([sys.executable, "-I", os.path.join(HERE, "bench.py"), sub, "--help"], capture_output=True, text=True)
            self.assertEqual(out.returncode, 0)
            for w in words:
                self.assertIn(w, out.stdout)


class SystemGpu(unittest.TestCase):
    def test_window_means_the_samples_inside_it(self):
        g = bench.SysGpu("/nonexistent")
        g.samples = [(1.0, 10, 20.0, 700), (2.0, 30, 24.0, 900), (3.0, 50, None, None), (9.0, 99, 99.0, 9999)]
        w = g.window(0.5, 3.5)
        self.assertEqual(w, {"sys_gpu_busy_pct": 30.0, "sys_gpu_busy_max_pct": 50.0,
                             "sys_gpu_power_w": 22.0, "sys_gpu_sclk_mhz": 800.0})
        self.assertEqual(g.window(5, 6), {})

    def test_derive_subtracts_the_runs_own_idle_reading(self):
        result = {"gpu_idle_no_client": {"sys_gpu_busy_pct": 2.0, "sys_gpu_power_w": 17.0},
                  "scroll_up_8s": {"cpu_pct": 1.0, "sys_gpu_busy_pct": 12.0, "sys_gpu_power_w": 25.5}}
        d = bench.derive("static", result)
        self.assertEqual(d["scroll_up_8s"]["sys_gpu_over_idle_busy_pct"], 10.0)
        self.assertEqual(d["scroll_up_8s"]["sys_gpu_over_idle_power_w"], 8.5)
        self.assertNotIn("sys_gpu_over_idle_busy_pct", d["gpu_idle_no_client"])

    def test_no_idle_reading_no_difference(self):
        d = bench.derive("static", {"scroll_up_8s": {"sys_gpu_busy_pct": 12.0}})
        self.assertNotIn("sys_gpu_over_idle_busy_pct", d["scroll_up_8s"])


if __name__ == "__main__":
    unittest.main()
