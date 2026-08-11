#!/usr/bin/env python3
"""Generate the ThreadStone site from measured result files.

The page is built from `results/*.json` rather than hand-written, so the
published numbers are always exactly what the tool emitted. If a number on the
site is wrong, the result file is wrong, and the result file is committed next
to the page that renders it.

Usage:
    python3 site/build.py [--out site/index.html]

Charts are inline SVG with no external dependencies: the page must render from
a single file with no network access.
"""

from __future__ import annotations

import argparse
import html
import json
import math
import pathlib
import sys
from dataclasses import dataclass

ROOT = pathlib.Path(__file__).resolve().parent.parent
RESULTS = ROOT / "results"

# --- Palette -----------------------------------------------------------------
# Validated with the data-viz palette validator in both modes:
#   light  surface #fcfcfb -> all checks pass
#   dark   surface #1a1a19 -> all checks pass
# Every chart here is single-series, so only slot 1 (blue) carries data.
LIGHT = {
    "page": "#f9f9f7",
    "surface": "#fcfcfb",
    "text": "#0b0b0b",
    "text2": "#52514e",
    "muted": "#898781",
    "grid": "#e1e0d9",
    "axis": "#c3c2b7",
    "series": "#2a78d6",
    "border": "rgba(11,11,11,0.10)",
}
DARK = {
    "page": "#0d0d0d",
    "surface": "#1a1a19",
    "text": "#ffffff",
    "text2": "#c3c2b7",
    "muted": "#898781",
    "grid": "#2c2c2a",
    "axis": "#383835",
    "series": "#3987e5",
    "border": "rgba(255,255,255,0.10)",
}

# Fixed status palette; never reused for a data series.
STATUS = {
    "stable": ("#0ca30c", "stable"),
    "acceptable": ("#0ca30c", "acceptable"),
    "noisy": ("#fab219", "noisy"),
    "unreliable": ("#d03b3b", "unreliable"),
}

UNIT_LABEL = {
    "dhrystones_per_sec": "Dhry/s",
    "gflops": "GFLOP/s",
    "gib_per_sec": "GiB/s",
    "mib_per_sec": "MiB/s",
    "melem_per_sec": "Melem/s",
    "nanoseconds": "ns",
}

LOWER_IS_BETTER = {"nanoseconds"}

# Three ascending bars, URL-encoded for a data: URI. Inline so the page needs no
# second request and works from a local file.
FAVICON = (
    "%3Csvg xmlns='http://www.w3.org/2000/svg' viewBox='0 0 32 32'%3E"
    "%3Crect width='32' height='32' rx='6' fill='%232a78d6'/%3E"
    "%3Crect x='7' y='18' width='4' height='8' rx='1' fill='white'/%3E"
    "%3Crect x='14' y='12' width='4' height='14' rx='1' fill='white'/%3E"
    "%3Crect x='21' y='6' width='4' height='20' rx='1' fill='white'/%3E"
    "%3C/svg%3E"
)

# What each workload is actually for, in the site's voice. Kept here rather than
# taken from the result file's one-line summary so the page can say more.
WORKLOAD_NOTES = {
    "dhrystone": (
        "The 1984 integer benchmark, ported to Rust and verified against the "
        "reference implementation's published final state. Small integers, "
        "dense branching, procedure calls, and 30-byte string copies — a "
        "working set that never leaves L1."
    ),
    "sgemm": (
        "Dense f64 matrix multiply, cache-blocked so three 256×256 matrices sit "
        "in L2. Loop order is i-k-j, which makes the inner loop a contiguous "
        "AXPY that vectorises into FMAs instead of stalling on a dot-product "
        "accumulator."
    ),
    "sha256": (
        "The portable software path, no hardware SHA instructions — those are "
        "an order of magnitude faster and would measure the presence of one "
        "instruction rather than integer throughput. Verified against the NIST "
        "vectors."
    ),
    "sort": (
        "The most realistic workload here. Unpredictable branches at every "
        "comparison and a recursive access pattern no prefetcher models — what "
        "ordinary application code actually does to a CPU."
    ),
    "stream": (
        "McCalpin's Triad over three 64 MiB arrays, partitioned across threads "
        "so the footprint stays constant at every thread count. Counts 24 bytes "
        "per element, following STREAM's convention of ignoring "
        "read-for-ownership traffic."
    ),
    "latency": (
        "A dependent-load pointer chase around one random Hamiltonian cycle: "
        "each step's address is the previous step's value, so nothing can hide "
        "the miss. Measured single-threaded only — splitting the buffer across "
        "threads would fit each slice in cache and report an LLC hit as DRAM "
        "latency."
    ),
}


# --- Helpers -----------------------------------------------------------------


def e(text: object) -> str:
    """HTML-escape a value for text content and attributes."""
    return html.escape(str(text), quote=True)


def si(value: float, digits: int | None = None) -> str:
    """Format with an SI magnitude suffix and three significant figures."""
    if value is None or not math.isfinite(value):
        return "—"
    magnitude = abs(value)
    for threshold, suffix in ((1e12, "T"), (1e9, "G"), (1e6, "M"), (1e3, "k")):
        if magnitude >= threshold:
            scaled, unit = value / threshold, suffix
            break
    else:
        scaled, unit = value, ""
    if digits is None:
        digits = 0 if abs(scaled) >= 100 else 1 if abs(scaled) >= 10 else 2
    return f"{scaled:.{digits}f}{unit}"


def human_bytes(size: int) -> str:
    """Format a byte count in binary units."""
    units = ["B", "KiB", "MiB", "GiB", "TiB"]
    value = float(size)
    index = 0
    while value >= 1024 and index < len(units) - 1:
        value /= 1024
        index += 1
    return f"{value:.0f} {units[index]}"


def log_bounds(low: float, high: float) -> tuple[float, float]:
    """Snap a range outward to the nearest 1-2-5 steps.

    Snapping to whole decades would waste most of the plot: a 0.9 ns to 121 ns
    range would render on a 0.1 to 1000 axis, squeezing the data into the middle
    third.
    """
    steps = [m * 10.0**k for k in range(-3, 7) for m in (1, 2, 5)]
    lo = max((s for s in steps if s <= low), default=low)
    hi = min((s for s in steps if s >= high), default=high)
    return lo, hi


def tick_text(value: float) -> str:
    """Axis ticks read as clean numbers: 0, 5, 10 — never 0.00."""
    if value == 0:
        return "0"
    if abs(value) >= 1000:
        return si(value)
    return f"{value:g}"


def nice_ceiling(value: float) -> float:
    """Round up to a clean axis maximum: 1, 2, 2.5, or 5 times a power of ten."""
    if value <= 0:
        return 1.0
    exponent = math.floor(math.log10(value))
    base = 10**exponent
    for step in (1.0, 2.0, 2.5, 5.0, 10.0):
        if value <= step * base:
            return step * base
    return 10 * base


# --- Data model --------------------------------------------------------------


@dataclass
class Workload:
    """One workload's numbers, flattened for rendering."""

    id: str
    name: str
    unit: str
    unit_label: str
    reference: float
    single: float | None
    multi: float | None
    speedup: float | None
    efficiency: float | None
    cv: float
    stability: str
    samples: int
    outliers: int
    ratio_single: float | None

    @property
    def note(self) -> str:
        return WORKLOAD_NOTES.get(self.id, "")


def load_workloads(report: dict) -> list[Workload]:
    """Flatten a report's workload entries."""
    out: list[Workload] = []
    for w in report["workloads"]:
        unit = w["unit"]
        single = (w.get("single_thread") or {}).get("value")
        multi = (w.get("multi_thread") or {}).get("value")
        passes = [p for p in (w.get("single_thread"), w.get("multi_thread")) if p]

        # Report the weakest evidence behind the row, not the strongest.
        worst = max(passes, key=lambda p: p["stats"]["cv"]) if passes else None
        scaling = w.get("scaling") or {}

        ratio = None
        if single and single > 0:
            ratio = (
                w["reference"] / single
                if unit in LOWER_IS_BETTER
                else single / w["reference"]
            )

        out.append(
            Workload(
                id=w["id"],
                name=w["name"],
                unit=unit,
                unit_label=UNIT_LABEL.get(unit, unit),
                reference=w["reference"],
                single=single,
                multi=multi,
                speedup=scaling.get("speedup"),
                efficiency=scaling.get("efficiency"),
                cv=worst["stats"]["cv"] if worst else float("nan"),
                stability=worst["stats"]["stability"] if worst else "unreliable",
                samples=worst["stats"]["n"] if worst else 0,
                outliers=worst["stats"]["outliers"] if worst else 0,
                ratio_single=ratio,
            )
        )
    return out


# --- Chart primitives --------------------------------------------------------


def svg_open(width: int, height: int, title: str, desc: str) -> str:
    """Open a responsive, labelled SVG element."""
    # No `height` attribute: `height="auto"` is invalid SVG and makes the
    # element fill its container instead of taking the viewBox aspect ratio.
    # CSS `height: auto` on the element does the right thing.
    return (
        f'<svg viewBox="0 0 {width} {height}" width="{width}" '
        f'role="img" preserveAspectRatio="xMinYMin meet" '
        f'aria-label="{e(title)}">'
        f"<title>{e(title)}</title><desc>{e(desc)}</desc>"
    )


def bar_chart(
    rows: list[tuple[str, float, str]],
    *,
    title: str,
    desc: str,
    axis_label: str,
    reference: tuple[float, str] | None = None,
    absent_note: str = "not measured",
) -> str:
    """Horizontal bars, one series.

    `rows` is (label, value, value text). A single series means no legend: the
    heading names what is plotted. Every bar is direct-labelled at the tip, so
    the values are readable without the axis.
    """
    label_width = 132
    right_pad = 78
    top = 26
    row_height = 34
    bar_height = 18  # under the 24px cap
    plot_width = 470
    width = label_width + plot_width + right_pad
    height = top + row_height * len(rows) + 34

    values = [v for _, v, _ in rows if math.isfinite(v)]
    span = nice_ceiling(max(values + ([reference[0]] if reference else []), default=1))

    def x(value: float) -> float:
        return label_width + (value / span) * plot_width

    parts = [svg_open(width, height, title, desc)]

    # Gridlines first, so data sits on top of them.
    ticks = 4
    for i in range(ticks + 1):
        value = span * i / ticks
        gx = x(value)
        parts.append(
            f'<line x1="{gx:.1f}" y1="{top}" x2="{gx:.1f}" '
            f'y2="{top + row_height * len(rows)}" class="grid"/>'
        )
        parts.append(
            f'<text x="{gx:.1f}" y="{top + row_height * len(rows) + 16}" '
            f'class="tick" text-anchor="middle">{tick_text(value)}</text>'
        )

    for index, (label, value, value_text) in enumerate(rows):
        y = top + index * row_height
        centre = y + row_height / 2
        parts.append(
            f'<text x="{label_width - 10}" y="{centre + 4:.1f}" '
            f'class="cat" text-anchor="end">{e(label)}</text>'
        )
        if not math.isfinite(value):
            parts.append(
                f'<text x="{label_width + 6}" y="{centre + 4:.1f}" '
                f'class="tick">{e(absent_note)}</text>'
            )
            continue
        bar_top = centre - bar_height / 2
        bar_width = max(x(value) - label_width, 1.5)
        # 4px rounded data-end, square at the baseline.
        parts.append(
            f'<path class="bar" d="'
            f"M{label_width} {bar_top:.1f} "
            f"H{label_width + bar_width - 4:.1f} "
            f"a4 4 0 0 1 4 4 "
            f"V{bar_top + bar_height - 4:.1f} "
            f"a4 4 0 0 1 -4 4 "
            f"H{label_width} Z"
            f'"><title>{e(label)}: {e(value_text)}</title></path>'
        )
        parts.append(
            f'<text x="{label_width + bar_width + 8:.1f}" y="{centre + 4:.1f}" '
            f'class="value">{e(value_text)}</text>'
        )

    if reference:
        ref_value, ref_label = reference
        rx = x(ref_value)
        parts.append(
            f'<line x1="{rx:.1f}" y1="{top - 4}" x2="{rx:.1f}" '
            f'y2="{top + row_height * len(rows)}" class="refline"/>'
        )
        # Above the plot, not below: the axis title lives at the bottom and the
        # two collided.
        parts.append(
            f'<text x="{rx:.1f}" y="{top - 2}" '
            f'class="reflabel" text-anchor="middle">{e(ref_label)}</text>'
        )

    # Baseline last so it reads as the anchor.
    parts.append(
        f'<line x1="{label_width}" y1="{top}" x2="{label_width}" '
        f'y2="{top + row_height * len(rows)}" class="axis"/>'
    )
    parts.append(
        f'<text x="{label_width + plot_width / 2:.0f}" y="{height - 2}" '
        f'class="axistitle" text-anchor="middle">{e(axis_label)}</text>'
    )
    parts.append("</svg>")
    return "".join(parts)


def sweep_chart(points: list[dict], caches: list[tuple[int, str]]) -> str:
    """Latency against working-set size, on a log x-axis.

    A single series, so no legend. Cache capacities are drawn as labelled
    reference lines, which is what turns the curve into a readable map of the
    hierarchy: each plateau is a level, each step is a boundary.
    """
    left, right, top, bottom = 56, 20, 18, 46
    plot_w, plot_h = 620, 260
    width = left + plot_w + right
    height = top + plot_h + bottom

    xs = [p["bytes"] for p in points]
    ys = [p["latency_ns"] for p in points]
    log_min, log_max = math.log2(min(xs)), math.log2(max(xs))

    # Both axes are log. Latency spans two orders of magnitude here — roughly
    # 1 ns in L1 to 120 ns in DRAM — and on a linear axis the DRAM value flattens
    # every cache plateau below it into the baseline. Log makes each level a
    # visible step, which is the entire point of the chart.
    y_lo, y_hi = log_bounds(min(ys), max(ys))
    log_y_lo, log_y_hi = math.log10(y_lo), math.log10(y_hi)

    def px(byte_count: float) -> float:
        return left + (math.log2(byte_count) - log_min) / (log_max - log_min) * plot_w

    def py(ns: float) -> float:
        span = log_y_hi - log_y_lo
        return top + plot_h - (math.log10(ns) - log_y_lo) / span * plot_h

    parts = [
        svg_open(
            width,
            height,
            "Memory latency against working-set size",
            "Latency per dependent load, measured by pointer chasing over "
            "working sets from 4 KiB to 256 MiB. Plateaus mark cache levels.",
        )
    ]

    # A gridline per decade, plus the 2x and 5x steps inside each, so the eye
    # can read a plateau's value without counting.
    for decade in range(math.floor(log_y_lo), math.ceil(log_y_hi) + 1):
        for multiple in (1, 2, 5):
            value = multiple * 10.0**decade
            if not (y_lo <= value <= y_hi):
                continue
            gy = py(value)
            parts.append(
                f'<line x1="{left}" y1="{gy:.1f}" x2="{left + plot_w}" '
                f'y2="{gy:.1f}" class="grid"/>'
            )
            label = f"{value:g}"
            parts.append(
                f'<text x="{left - 8}" y="{gy + 4:.1f}" class="tick" '
                f'text-anchor="end">{label}</text>'
            )

    for exponent in range(int(log_min), int(log_max) + 1, 2):
        size = 2**exponent
        gx = px(size)
        parts.append(
            f'<text x="{gx:.1f}" y="{top + plot_h + 18}" class="tick" '
            f'text-anchor="middle">{e(human_bytes(size))}</text>'
        )

    # Cache capacities, behind the data line.
    for size, label in caches:
        if not (min(xs) <= size <= max(xs)):
            continue
        cx = px(size)
        parts.append(
            f'<line x1="{cx:.1f}" y1="{top}" x2="{cx:.1f}" '
            f'y2="{top + plot_h}" class="refline"/>'
        )
        parts.append(
            f'<text x="{cx + 5:.1f}" y="{top + 12}" class="reflabel">{e(label)}</text>'
        )

    path = " ".join(
        f"{'M' if i == 0 else 'L'}{px(p['bytes']):.1f} {py(p['latency_ns']):.1f}"
        for i, p in enumerate(points)
    )
    parts.append(f'<path class="line" d="{path}"/>')

    for p in points:
        cx, cy = px(p["bytes"]), py(p["latency_ns"])
        parts.append(
            f'<circle class="dot" cx="{cx:.1f}" cy="{cy:.1f}" r="4">'
            f"<title>{e(human_bytes(p['bytes']))}: "
            f"{p['latency_ns']:.1f} ns</title></circle>"
        )

    # Direct-label only the two ends: the fastest and slowest points are the
    # story, and a number on every point would be unreadable.
    first, last = points[0], points[-1]
    parts.append(
        f'<text x="{px(first["bytes"]) + 10:.1f}" '
        f'y="{py(first["latency_ns"]) - 10:.1f}" class="value">'
        f"{first['latency_ns']:.1f} ns</text>"
    )
    parts.append(
        f'<text x="{px(last["bytes"]) - 8:.1f}" '
        f'y="{py(last["latency_ns"]) - 12:.1f}" class="value" '
        f'text-anchor="end">{last["latency_ns"]:.0f} ns</text>'
    )

    parts.append(
        f'<line x1="{left}" y1="{top + plot_h}" x2="{left + plot_w}" '
        f'y2="{top + plot_h}" class="axis"/>'
    )
    parts.append(
        f'<text x="{left + plot_w / 2:.0f}" y="{height - 6}" class="axistitle" '
        f'text-anchor="middle">Working set</text>'
    )
    parts.append(
        f'<text transform="translate(14 {top + plot_h / 2:.0f}) rotate(-90)" '
        f'class="axistitle" text-anchor="middle">Nanoseconds per access (log)</text>'
    )
    parts.append("</svg>")
    return "".join(parts)


# --- Page --------------------------------------------------------------------


def css() -> str:
    """Page styles. Light values on bare :root; dark redefined under both scopes."""

    def variables(scheme: dict) -> str:
        return "".join(f"--{k}:{v};" for k, v in scheme.items())

    return f"""
:root {{
  color-scheme: light;
  {variables(LIGHT)}
  --font: system-ui, -apple-system, "Segoe UI", sans-serif;
  --mono: ui-monospace, SFMono-Regular, Menlo, Consolas, monospace;
}}
@media (prefers-color-scheme: dark) {{
  :root:not([data-theme="light"]) {{ color-scheme: dark; {variables(DARK)} }}
}}
:root[data-theme="dark"] {{ color-scheme: dark; {variables(DARK)} }}

* {{ box-sizing: border-box; }}
body {{
  margin: 0;
  background: var(--page);
  color: var(--text);
  font-family: var(--font);
  font-size: 16px;
  line-height: 1.6;
  -webkit-font-smoothing: antialiased;
}}
.wrap {{ max-width: 860px; margin: 0 auto; padding: 0 24px; }}
a {{ color: var(--series); text-decoration-thickness: 1px; text-underline-offset: 2px; }}

header {{ padding: 72px 0 40px; }}
h1 {{
  font-size: clamp(2.2rem, 6vw, 3.2rem);
  line-height: 1.05; margin: 0 0 12px; letter-spacing: -0.02em;
}}
.tagline {{ font-size: 1.2rem; color: var(--text2); margin: 0 0 8px; max-width: 44ch; }}
.machine {{ color: var(--muted); font-size: 0.95rem; font-family: var(--mono); }}

h2 {{
  font-size: 1.5rem; margin: 0 0 6px; letter-spacing: -0.01em;
  padding-top: 8px;
}}
h3 {{ font-size: 1.05rem; margin: 0 0 4px; }}
section {{ padding: 40px 0; border-top: 1px solid var(--border); }}
.lede {{ color: var(--text2); margin: 0 0 28px; max-width: 62ch; }}
p {{ max-width: 68ch; }}

/* KPI row -------------------------------------------------------------- */
.kpis {{
  display: grid; gap: 12px; margin: 32px 0 8px;
  grid-template-columns: repeat(auto-fit, minmax(160px, 1fr));
}}
.kpi {{
  background: var(--surface); border: 1px solid var(--border);
  border-radius: 10px; padding: 16px 18px;
}}
.kpi .label {{
  font-size: 0.8rem; color: var(--text2); text-transform: uppercase;
  letter-spacing: 0.06em;
}}
.kpi .value {{ font-size: 2.1rem; font-weight: 600; line-height: 1.15; margin-top: 4px; }}
.sub {{ font-size: 0.85rem; color: var(--muted); }}

/* Charts --------------------------------------------------------------- */
figure {{
  margin: 0 0 8px; background: var(--surface); border: 1px solid var(--border);
  border-radius: 10px; padding: 20px 20px 12px; overflow-x: auto;
}}
figcaption {{ color: var(--text2); font-size: 0.9rem; margin-top: 10px; max-width: 66ch; }}
svg {{ display: block; width: 100%; height: auto; max-width: 100%; min-width: 520px; }}
.grid {{ stroke: var(--grid); stroke-width: 1; }}
.axis {{ stroke: var(--axis); stroke-width: 1; }}
.refline {{ stroke: var(--muted); stroke-width: 1; opacity: 0.65; }}
.reflabel {{ fill: var(--muted); font-size: 11px; font-family: var(--font); }}
.bar {{ fill: var(--series); }}
.line {{ fill: none; stroke: var(--series); stroke-width: 2; stroke-linejoin: round; stroke-linecap: round; }}
.dot {{ fill: var(--series); stroke: var(--surface); stroke-width: 2; }}
.tick {{ fill: var(--muted); font-size: 11px; font-family: var(--font); font-variant-numeric: tabular-nums; }}
.cat {{ fill: var(--text2); font-size: 12.5px; font-family: var(--font); }}
.value {{ fill: var(--text); font-size: 12.5px; font-weight: 600; font-family: var(--font); }}
.axistitle {{ fill: var(--muted); font-size: 11.5px; font-family: var(--font); }}

/* Tables --------------------------------------------------------------- */
.scroll {{ overflow-x: auto; }}
table {{ width: 100%; border-collapse: collapse; font-size: 0.92rem; min-width: 560px; }}
th, td {{ text-align: right; padding: 9px 10px; border-bottom: 1px solid var(--border); }}
th:first-child, td:first-child {{ text-align: left; }}
thead th {{
  color: var(--text2); font-weight: 600; font-size: 0.78rem;
  text-transform: uppercase; letter-spacing: 0.05em;
}}
tbody td {{ font-variant-numeric: tabular-nums; }}
tbody tr:last-child td {{ border-bottom: none; }}
.unit {{ color: var(--muted); font-size: 0.85em; }}

.pill {{
  display: inline-flex; align-items: center; gap: 5px;
  font-size: 0.78rem; color: var(--text2); white-space: nowrap;
}}
.pill .dot-status {{ width: 8px; height: 8px; border-radius: 50%; flex: none; }}

details {{ margin-top: 10px; }}
summary {{ cursor: pointer; color: var(--text2); font-size: 0.88rem; }}

/* Workload cards ------------------------------------------------------- */
.cards {{ display: grid; gap: 14px; grid-template-columns: repeat(auto-fit, minmax(260px, 1fr)); }}
.card {{
  background: var(--surface); border: 1px solid var(--border);
  border-radius: 10px; padding: 18px 20px;
}}
.card .num {{ font-size: 1.5rem; font-weight: 600; margin: 6px 0 2px; }}
.card p {{ font-size: 0.9rem; color: var(--text2); margin: 8px 0 0; }}

.principles {{ display: grid; gap: 18px; grid-template-columns: repeat(auto-fit, minmax(280px, 1fr)); }}
.principle h3 {{ font-size: 1rem; }}
.principle p {{ font-size: 0.92rem; color: var(--text2); margin: 4px 0 0; }}

pre {{
  background: var(--surface); border: 1px solid var(--border); border-radius: 10px;
  padding: 16px 18px; overflow-x: auto; font-family: var(--mono);
  font-size: 0.87rem; line-height: 1.65;
}}
code {{ font-family: var(--mono); font-size: 0.9em; }}
pre code {{ font-size: 1em; }}

footer {{
  border-top: 1px solid var(--border); padding: 32px 0 64px;
  color: var(--muted); font-size: 0.85rem;
}}
footer dl {{ display: grid; grid-template-columns: max-content 1fr; gap: 4px 16px; margin: 12px 0 0; }}
footer dt {{ color: var(--text2); }}
footer dd {{ margin: 0; font-family: var(--mono); font-size: 0.95em; word-break: break-all; }}
"""


def kpi_row(report: dict, workloads: list[Workload]) -> str:
    """Headline numbers as stat tiles.

    A KPI row rather than a single hero figure: the two scores are equally the
    point, and elevating one would misrepresent which matters.
    """
    score = report["score"]
    threads = report["config"]["threads"]
    trustworthy = sum(
        1 for w in workloads if w.stability in ("stable", "acceptable")
    )

    def tile(label: str, value: str, sub: str) -> str:
        return (
            f'<div class="kpi"><div class="label">{e(label)}</div>'
            f'<div class="value">{e(value)}</div>'
            f'<div class="sub">{e(sub)}</div></div>'
        )

    return (
        '<div class="kpis">'
        + tile(
            "Single-core",
            f"{score['single_core']:.0f}",
            "vs. Reference Core = 1000",
        )
        + tile("Multi-core", f"{score['multi_core']:.0f}", f"{threads} threads")
        + tile("Workloads", f"{len(workloads)}", "each measuring something distinct")
        + tile(
            "Stable results",
            f"{trustworthy}/{len(workloads)}",
            "run-to-run variation under 3%",
        )
        + "</div>"
    )


def results_table(workloads: list[Workload], threads: int) -> str:
    """The per-workload numbers. This is the actual result; the score is a summary."""
    rows = []
    for w in workloads:
        colour, name = STATUS.get(w.stability, STATUS["unreliable"])
        cv = "—" if math.isnan(w.cv) else f"{w.cv * 100:.1f}%"
        rows.append(
            "<tr>"
            f"<td>{e(w.name)}<br><span class='unit'>{e(w.unit_label)}</span></td>"
            f"<td>{e(si(w.single))}</td>"
            f"<td>{e(si(w.multi)) if w.multi else '—'}</td>"
            f"<td>{f'{w.speedup:.1f}×' if w.speedup else '—'}</td>"
            f"<td><span class='pill'>"
            f"<span class='dot-status' style='background:{colour}'></span>"
            f"{cv} {e(name)}</span></td>"
            "</tr>"
        )
    return (
        '<div class="scroll"><table><thead><tr>'
        "<th>Workload</th><th>1 thread</th>"
        f"<th>{threads} threads</th><th>Scaling</th><th>Variation</th>"
        "</tr></thead><tbody>" + "".join(rows) + "</tbody></table></div>"
    )


def sweep_table(points: list[dict]) -> str:
    """Table view of the sweep, so the chart is never the only way to read it."""
    rows = "".join(
        f"<tr><td>{e(human_bytes(p['bytes']))}</td>"
        f"<td>{p['latency_ns']:.1f}</td></tr>"
        for p in points
    )
    return (
        "<details><summary>Table view</summary>"
        '<div class="scroll"><table><thead><tr><th>Working set</th>'
        "<th>Latency (ns)</th></tr></thead><tbody>"
        + rows
        + "</tbody></table></div></details>"
    )


def cache_markers(system: dict) -> list[tuple[int, str]]:
    """Cache capacities to annotate on the sweep, from the measured machine."""
    markers = []
    for key, label in (("l1d_bytes", "L1d"), ("l2_bytes", "L2"), ("l3_bytes", "L3")):
        size = system.get(key)
        if size:
            markers.append((size, f"{label} {human_bytes(size)}"))
    return markers


def signature_rows(report: dict) -> str:
    """Footer rows describing the signature, if the result carries one."""
    sig = report.get("signature")
    if not sig:
        return ""
    return (
        f"<dt>Signature</dt><dd>{e(sig['algorithm'])}</dd>"
        f"<dt>Public key</dt><dd>{e(sig['public_key'])}</dd>"
    )


def signature_note(report: dict) -> str:
    """Tell a reader how to check the published numbers for themselves."""
    if not report.get("signature"):
        return ""
    return (
        "<p style=\"margin-top:16px\">These numbers are signed. Download the "
        "<a href=\"https://github.com/romankhadka/ThreadStone/blob/master/"
        "results/apple-m4-pro.json\">result document</a> and run "
        "<code>threadstone verify apple-m4-pro.json --require-signature</code> "
        "to confirm nothing has been edited since it was measured. That proves "
        "integrity, not authority \u2014 it says the file is unmodified, not "
        "that the machine is what it claims.</p>"
    )


def build(report: dict, sweep: list[dict] | None) -> str:
    """Render the whole page."""
    system = report["system"]
    workloads = load_workloads(report)
    threads = report["config"]["threads"]
    cores = (
        f"{system['performance_cores']}P + {system['efficiency_cores']}E"
        if system.get("performance_cores") and system.get("efficiency_cores")
        else f"{system['logical_cores']} cores"
    )
    machine = f"{system.get('cpu_model', 'unknown CPU')} · {cores}"

    scaling_rows = [
        (w.name, w.speedup if w.speedup else float("nan"),
         f"{w.speedup:.1f}×" if w.speedup else "—")
        for w in workloads
    ]
    ratio_rows = [
        (w.name, w.ratio_single if w.ratio_single else float("nan"),
         f"{w.ratio_single:.1f}×" if w.ratio_single else "—")
        for w in workloads
    ]

    cards = "".join(
        f'<div class="card"><h3>{e(w.name)}</h3>'
        f'<div class="num">{e(si(w.single))} '
        f'<span class="unit">{e(w.unit_label)}</span></div>'
        f'<div class="sub">single thread'
        + (
            f" · {e(si(w.multi))} {e(w.unit_label)} at {threads} threads"
            if w.multi
            else " only"
        )
        + f"</div><p>{e(w.note)}</p></div>"
        for w in workloads
    )

    sweep_section = ""
    if sweep:
        sweep_section = f"""
<section id="memory">
  <h2>The memory hierarchy, measured</h2>
  <p class="lede">One dependent load at a time, over working sets from 4&nbsp;KiB
  to 256&nbsp;MiB. Nothing can hide the miss, so each plateau is a cache level
  and each step is a boundary. This is the shape of the machine's memory system.</p>
  <figure>
    {sweep_chart(sweep, cache_markers(system))}
    <figcaption>Latency per access against working-set size, log scale.
    Vertical lines mark this machine's reported cache capacities. The rise from
    {sweep[0]['latency_ns']:.1f}&nbsp;ns to {sweep[-1]['latency_ns']:.0f}&nbsp;ns
    is the whole cost of missing every level of cache.</figcaption>
  </figure>
  {sweep_table(sweep)}
</section>"""

    # A complete document, doctype first: without it browsers fall back to
    # quirks mode, which changes box sizing and breaks the layout.
    description = (
        "Six CPU workloads, each measuring something the others cannot see. "
        "Every result records the machine it ran on, how much it varied, and "
        "whether it should be believed."
    )
    return f"""<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>ThreadStone — a CPU benchmark that shows its work</title>
<meta name="description" content="{e(description)}">
<meta name="color-scheme" content="light dark">
<meta property="og:title" content="ThreadStone">
<meta property="og:description" content="{e(description)}">
<meta property="og:type" content="website">
<link rel="icon" href="data:image/svg+xml,{FAVICON}">
<style>{css()}</style>
</head>
<body>
<div class="wrap">
<header>
  <h1>ThreadStone</h1>
  <p class="tagline">A CPU benchmark suite that shows its work.</p>
  <p class="machine">{e(machine)} · {e(system.get('os_version', system['os']))}</p>
  {kpi_row(report, workloads)}
</header>

<section id="results">
  <h2>Results</h2>
  <p class="lede">Six workloads, each measuring something the others cannot see.
  A CPU that is fast at all six is fast; one that is fast at a single one is fast
  at that one thing. The per-workload numbers are the actual result — the score
  is a convenience that throws away the shape.</p>
  {results_table(workloads, threads)}
  <p class="lede" style="margin-top:20px">Variation is the coefficient of
  variation across {report['config']['samples']} samples, after discarding
  {report['config']['warmup']} warmup rounds and rejecting outliers by median
  absolute deviation. Under 1% is stable; under 3% is usable; above that, the
  measurement is describing the room rather than the CPU.</p>
</section>

<section id="scaling">
  <h2>What the extra cores buy</h2>
  <p class="lede">Speedup from one thread to {threads}. Compute-bound workloads
  approach the core count; STREAM does not, and that is the interesting result —
  a single core already saturates most of this machine's memory controller, so
  there is little headroom left for the others.</p>
  <figure>
    {bar_chart(scaling_rows,
               title=f"Speedup from 1 to {threads} threads",
               desc="Speedup factor per workload, with perfect linear scaling "
                    "marked as a reference line.",
               axis_label="Speedup vs. 1 thread",
               reference=(threads, f"perfect scaling ({threads}×)"),
               absent_note="single-thread only")}
    <figcaption>Memory latency is absent because it is measured
    single-threaded only — see below.</figcaption>
  </figure>
</section>

<section id="reference">
  <h2>Against the reference core</h2>
  <p class="lede">Each workload's single-thread result as a multiple of the
  <em>ThreadStone Reference Core v1</em> — a nominal 3.0&nbsp;GHz out-of-order
  core with 256-bit SIMD and one DDR4-3200 channel. It is a published
  definition, not a machine anyone owns: a reference derived from the author's
  own hardware would score exactly 1000 and make every other machine look like a
  deviation from it.</p>
  <figure>
    {bar_chart(ratio_rows,
               title="Single-thread performance relative to the reference core",
               desc="Each workload's ratio to its published reference value.",
               axis_label="Multiple of the reference core",
               reference=(1.0, "reference"))}
    <figcaption>The score is the geometric mean of these ratios, ×1000. The
    geometric mean is used because the arithmetic mean of ratios depends on
    which machine sits in the denominator — so A could beat B under one
    reference and lose under another.</figcaption>
  </figure>
</section>
{sweep_section}
<section id="workloads">
  <h2>The six workloads</h2>
  <p class="lede">Each covers a dimension of CPU performance the others are
  blind to.</p>
  <div class="cards">{cards}</div>
</section>

<section id="method">
  <h2>Why these numbers can be trusted</h2>
  <p class="lede">Every design decision follows from one idea: a benchmark
  number is a claim, and a claim nobody can check is worthless.</p>
  <div class="principles">
    <div class="principle">
      <h3>Threads start together</h3>
      <p>A work-stealing pool hands out samples as slots free up, so early
      threads run against an idle machine and late ones against a loaded one.
      ThreadStone releases every thread from a barrier, so the window is exactly
      “time for all N threads, having started at the same instant.”</p>
    </div>
    <div class="principle">
      <h3>Iteration counts are calibrated</h3>
      <p>A fixed count that fills 300&nbsp;ms on a laptop fills 3&nbsp;ms on a
      server — close enough to the clock's granularity to be noise. Counts are
      discovered at run time, with every thread active, because a count tuned on
      an idle machine overshoots once memory is contended.</p>
    </div>
    <div class="principle">
      <h3>Nothing unmeasured is in the window</h3>
      <p>Allocation and page-faulting happen before the clock starts. Threads
      are spawned once for the whole workload, not once per sample.</p>
    </div>
    <div class="principle">
      <h3>Every number carries its uncertainty</h3>
      <p>Median rather than mean, because benchmark noise is one-sided — only
      ever making a sample slower. Outliers are rejected by median absolute
      deviation and counted, and each result states whether it should be
      believed.</p>
    </div>
    <div class="principle">
      <h3>Every number carries its provenance</h3>
      <p>CPU topology, cache sizes, OS, compiler version, target triple,
      optimisation flags, and the measured resolution of the clock itself.
      “2,300 Dhrystones/sec” is unfalsifiable; the same number with its machine
      attached is a claim you can refute.</p>
    </div>
    <div class="principle">
      <h3>What can't be measured well isn't reported</h3>
      <p>Memory latency is single-thread only. Splitting a 256&nbsp;MiB chase
      buffer across {threads} threads would give each a slice that fits in
      last-level cache, so the “multi-threaded latency” would be an LLC hit
      time — several times better than reality, and a straightforward lie about
      the machine.</p>
    </div>
  </div>
  <p style="margin-top:28px"><a href="https://github.com/romankhadka/ThreadStone/blob/master/docs/METHODOLOGY.md">Full
  methodology</a>, including the reference values, the statistics, and a
  known-limitations section.</p>
</section>

<section id="run">
  <h2>Run it yourself</h2>
  <pre><code>git clone https://github.com/romankhadka/ThreadStone
cd threadstone
cargo install --path threadstone-cli

threadstone run                  # the full suite, both passes
threadstone sweep                # map your cache hierarchy
threadstone run --out mine.json  # save the full document
threadstone compare theirs.json mine.json</code></pre>
  <p>Requires Rust 1.75 or newer. No C toolchain; three third-party crates in
  the binary. Results can be signed with Ed25519 over canonical JSON — which
  proves integrity, not authority: that a file has not been edited since
  signing, and nothing more.</p>
</section>

<footer>
  <p>ThreadStone {e(report['tool_version'])} ·
  <a href="https://github.com/romankhadka/ThreadStone">source</a> ·
  MIT or Apache-2.0</p>
  <dl>
    <dt>Measured</dt><dd>{e(report['generated_at'])}</dd>
    <dt>Machine</dt><dd>{e(system.get('cpu_model', '—'))}</dd>
    <dt>Target</dt><dd>{e(system['target'])}</dd>
    <dt>Compiler</dt><dd>{e(system.get('rustc_version', '—'))}</dd>
    <dt>Build</dt><dd>opt-level {e(system['build_profile']['opt_level'])}{
      ', ' + e(system['build_profile']['target_features'])
      if system['build_profile'].get('target_features') else ''}</dd>
    <dt>Clock</dt><dd>{e(system['timer']['cycle_source'])},
      {e(system['timer']['resolution_ns'])} ns resolution,
      {system['timer']['overhead_ns']:.1f} ns per read</dd>
    <dt>Run</dt><dd>{report['config']['samples']} samples ×
      {report['config']['window_ms']} ms, {report['config']['warmup']} warmup,
      {report['duration_secs']:.0f} s total</dd>
    <dt>Schema</dt><dd>version {report['schema_version']}</dd>
    {signature_rows(report)}
  </dl>
  {signature_note(report)}
</footer>
</div>
</body>
</html>
"""


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--result",
        type=pathlib.Path,
        default=RESULTS / "apple-m4-pro.json",
        help="benchmark result document to render",
    )
    parser.add_argument(
        "--sweep",
        type=pathlib.Path,
        default=RESULTS / "apple-m4-pro-sweep.json",
        help="cache-hierarchy sweep to render (optional)",
    )
    parser.add_argument(
        "--out", type=pathlib.Path, default=ROOT / "site" / "index.html"
    )
    args = parser.parse_args()

    if not args.result.exists():
        print(f"error: no result file at {args.result}", file=sys.stderr)
        print("run: threadstone run --out results/apple-m4-pro.json", file=sys.stderr)
        return 1

    report = json.loads(args.result.read_text())
    sweep = json.loads(args.sweep.read_text()) if args.sweep.exists() else None

    args.out.parent.mkdir(parents=True, exist_ok=True)
    args.out.write_text(build(report, sweep))
    print(f"wrote {args.out.relative_to(ROOT)} from {args.result.name}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
