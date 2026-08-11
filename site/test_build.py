#!/usr/bin/env python3
"""Tests for the site generator.

Run with `python3 -m unittest discover -s site` from the repository root.

The generator does real work — log-axis bounds, unit scaling, SVG geometry — and
a silent mistake there publishes a wrong chart rather than crashing. These
tests cover the arithmetic and assert that the emitted document is well formed.
"""

from __future__ import annotations

import json
import math
import pathlib
import re
import sys
import unittest

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent))

import build  # noqa: E402


class SiFormatting(unittest.TestCase):
    def test_scales_across_the_whole_range(self):
        self.assertEqual(build.si(0), "0.00")
        self.assertEqual(build.si(42.5), "42.5")
        self.assertEqual(build.si(999), "999")
        self.assertEqual(build.si(1234), "1.23k")
        self.assertEqual(build.si(45_600_000), "45.6M")
        self.assertEqual(build.si(2_500_000_000), "2.50G")

    def test_non_finite_renders_as_a_dash(self):
        self.assertEqual(build.si(float("nan")), "—")
        self.assertEqual(build.si(float("inf")), "—")
        self.assertEqual(build.si(None), "—")


class ByteFormatting(unittest.TestCase):
    def test_binary_units(self):
        self.assertEqual(build.human_bytes(512), "512 B")
        self.assertEqual(build.human_bytes(4096), "4 KiB")
        self.assertEqual(build.human_bytes(1 << 20), "1 MiB")
        self.assertEqual(build.human_bytes(256 << 20), "256 MiB")


class AxisBounds(unittest.TestCase):
    def test_log_bounds_snap_outward_to_one_two_five(self):
        # The real case: 0.9 ns in L1 to 121 ns in DRAM.
        self.assertEqual(build.log_bounds(0.9, 121.0), (0.5, 200.0))
        self.assertEqual(build.log_bounds(3.0, 7500.0), (2.0, 10000.0))

    def test_log_bounds_always_contain_the_data(self):
        for low, high in [(0.4, 3.0), (1.0, 1.0), (12.0, 999.0), (0.02, 55.0)]:
            lo, hi = build.log_bounds(low, high)
            self.assertLessEqual(lo, low, f"{lo} must not clip {low}")
            self.assertGreaterEqual(hi, high, f"{hi} must not clip {high}")

    def test_linear_ceiling_is_clean_and_never_clips(self):
        self.assertEqual(build.nice_ceiling(14), 20)
        self.assertEqual(build.nice_ceiling(9.5), 10)
        self.assertEqual(build.nice_ceiling(0.7), 1.0)
        for value in (0.3, 1.0, 3.7, 14.0, 210.0, 9_999.0):
            self.assertGreaterEqual(build.nice_ceiling(value), value)

    def test_ticks_read_as_clean_numbers(self):
        self.assertEqual(build.tick_text(0), "0")
        self.assertEqual(build.tick_text(5), "5")
        self.assertEqual(build.tick_text(2.5), "2.5")
        self.assertEqual(build.tick_text(15000), "15.0k")


class Charts(unittest.TestCase):
    def test_bar_chart_is_balanced_svg_with_a_label_per_row(self):
        rows = [("Alpha", 3.0, "3.0×"), ("Beta", 9.0, "9.0×")]
        svg = build.bar_chart(
            rows, title="t", desc="d", axis_label="x", reference=(14, "linear")
        )
        self.assertEqual(svg.count("<svg"), 1)
        self.assertTrue(svg.endswith("</svg>"))
        self.assertIn("<title>t</title>", svg)
        for label, _, text in rows:
            self.assertIn(label, svg)
            self.assertIn(text, svg)
        self.assertIn("linear", svg)

    def test_bar_chart_marks_absent_values_without_drawing_a_bar(self):
        rows = [("Present", 2.0, "2.0×"), ("Absent", float("nan"), "—")]
        svg = build.bar_chart(
            rows, title="t", desc="d", axis_label="x", absent_note="not applicable"
        )
        self.assertIn("not applicable", svg)
        self.assertEqual(svg.count('class="bar"'), 1, "only one bar should be drawn")

    def test_bar_chart_never_emits_non_finite_coordinates(self):
        rows = [("A", float("nan"), "—"), ("B", 0.0, "0")]
        svg = build.bar_chart(rows, title="t", desc="d", axis_label="x")
        self.assertNotIn("nan", svg.lower())
        self.assertNotIn("inf", svg.lower())

    def test_sweep_chart_plots_every_point(self):
        points = [
            {"bytes": 4096 << i, "latency_ns": 1.0 + i * 8} for i in range(10)
        ]
        svg = build.sweep_chart(points, [(65536, "L1d 64 KiB")])
        self.assertEqual(svg.count('class="dot"'), len(points))
        self.assertIn("L1d 64 KiB", svg)
        self.assertEqual(svg.count("<svg"), 1)

    def test_sweep_chart_coordinates_stay_inside_the_viewbox(self):
        points = [{"bytes": 4096 << i, "latency_ns": 0.9 * (1.7**i)} for i in range(17)]
        svg = build.sweep_chart(points, [])
        viewbox = re.search(r'viewBox="0 0 (\d+) (\d+)"', svg)
        width, height = int(viewbox.group(1)), int(viewbox.group(2))
        for cx, cy in re.findall(r'<circle class="dot" cx="([\d.]+)" cy="([\d.]+)"', svg):
            self.assertTrue(0 <= float(cx) <= width, f"cx {cx} outside 0..{width}")
            self.assertTrue(0 <= float(cy) <= height, f"cy {cy} outside 0..{height}")


class ReportLoading(unittest.TestCase):
    """Exercised against the committed result, so the site and data stay in step."""

    @classmethod
    def setUpClass(cls):
        path = build.RESULTS / "apple-m4-pro.json"
        if not path.exists():
            raise unittest.SkipTest(f"no committed result at {path}")
        cls.report = json.loads(path.read_text())

    def test_every_unit_in_the_report_has_a_label(self):
        for w in self.report["workloads"]:
            self.assertIn(
                w["unit"],
                build.UNIT_LABEL,
                f"{w['id']} uses unit {w['unit']!r} with no display label",
            )

    def test_every_workload_has_a_written_note(self):
        for w in self.report["workloads"]:
            self.assertIn(
                w["id"],
                build.WORKLOAD_NOTES,
                f"{w['id']} would render with an empty description",
            )

    def test_ratios_are_direction_corrected(self):
        for w in build.load_workloads(self.report):
            if w.ratio_single is None:
                continue
            self.assertGreater(w.ratio_single, 0)
            if w.unit in build.LOWER_IS_BETTER:
                # Lower is better, so a measurement below the reference must
                # produce a ratio above 1.
                expected = w.reference / w.single
            else:
                expected = w.single / w.reference
            self.assertAlmostEqual(w.ratio_single, expected, places=9)

    def test_page_is_a_well_formed_document(self):
        html = build.build(self.report, None)
        self.assertTrue(html.startswith("<!DOCTYPE html>"))
        self.assertEqual(html.count("<html"), 1)
        self.assertEqual(html.count("</html>"), 1)
        self.assertLess(html.index("</head>"), html.index("<body>"))
        self.assertIn("<title>", html)
        self.assertEqual(html.count("<svg"), html.count("</svg>"))

    def test_page_references_no_external_assets(self):
        # The page must render from one file: no CDN, no remote fonts.
        html = build.build(self.report, None)
        assets = re.findall(r'(?:src|href)="(https?://[^"]+)"', html)
        for url in assets:
            self.assertTrue(
                url.startswith("https://github.com/"),
                f"unexpected external asset: {url}",
            )

    def test_page_shows_every_workload_and_both_scores(self):
        html = build.build(self.report, None)
        for w in self.report["workloads"]:
            self.assertIn(w["name"], html)
        self.assertIn(f"{self.report['score']['single_core']:.0f}", html)
        self.assertIn(f"{self.report['score']['multi_core']:.0f}", html)

    def test_page_renders_with_a_sweep(self):
        path = build.RESULTS / "apple-m4-pro-sweep.json"
        if not path.exists():
            self.skipTest("no committed sweep")
        sweep = json.loads(path.read_text())
        html = build.build(self.report, sweep)
        self.assertIn("memory hierarchy", html)
        self.assertEqual(html.count("<svg"), 3)


class StabilityRendering(unittest.TestCase):
    def test_every_stability_verdict_has_a_colour(self):
        # A verdict the site does not know would silently render as "unreliable".
        for verdict in ("stable", "acceptable", "noisy", "unreliable"):
            self.assertIn(verdict, build.STATUS)

    def test_status_colours_are_not_reused_as_the_series_colour(self):
        series = {build.LIGHT["series"], build.DARK["series"]}
        for colour, _ in build.STATUS.values():
            self.assertNotIn(colour, series)


if __name__ == "__main__":
    unittest.main()
