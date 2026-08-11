//! Human-readable rendering of a [`Report`].
//!
//! Two audiences, two formats: a terminal table for someone who just ran the
//! benchmark, and Markdown for pasting into an issue or a README. Both show the
//! same facts, including the ones a benchmark tool is tempted to hide — how
//! variable each measurement was, and which numbers are not to be trusted.

use threadstone_core::report::{Pass, Report, WorkloadReport};
use threadstone_core::stats::Stability;

/// Whether to emit ANSI colour.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Color {
    /// Emit escape sequences.
    Always,
    /// Emit plain text.
    Never,
}

impl Color {
    /// Choose based on whether stdout is a terminal, honouring `NO_COLOR`.
    ///
    /// See <https://no-color.org>: any non-empty value disables colour.
    pub fn detect() -> Color {
        use std::io::IsTerminal;
        let disabled = std::env::var_os("NO_COLOR").is_some_and(|v| !v.is_empty());
        if !disabled && std::io::stdout().is_terminal() {
            Color::Always
        } else {
            Color::Never
        }
    }

    fn wrap(self, code: &str, text: &str) -> String {
        match self {
            Color::Always => format!("\x1b[{code}m{text}\x1b[0m"),
            Color::Never => text.to_string(),
        }
    }

    fn dim(self, text: &str) -> String {
        self.wrap("2", text)
    }

    fn bold(self, text: &str) -> String {
        self.wrap("1", text)
    }

    fn for_stability(self, s: Stability) -> String {
        let code = match s {
            Stability::Stable => "32",     // green
            Stability::Acceptable => "36", // cyan
            Stability::Noisy => "33",      // yellow
            Stability::Unreliable => "31", // red
        };
        self.wrap(code, &s.glyph().to_string())
    }
}

/// Format a value with an SI-style magnitude suffix and three significant
/// figures.
///
/// Benchmark values span from 40 (nanoseconds) to 500,000,000 (Dhrystones per
/// second) within a single table, and neither raw digits nor scientific
/// notation is readable at a glance across that range.
pub fn si(value: f64) -> String {
    if !value.is_finite() {
        return "—".to_string();
    }
    let magnitude = value.abs();
    let (scaled, suffix) = match magnitude {
        m if m >= 1e12 => (value / 1e12, "T"),
        m if m >= 1e9 => (value / 1e9, "G"),
        m if m >= 1e6 => (value / 1e6, "M"),
        m if m >= 1e3 => (value / 1e3, "k"),
        _ => (value, ""),
    };
    let decimals = match scaled.abs() {
        s if s >= 100.0 => 0,
        s if s >= 10.0 => 1,
        _ => 2,
    };
    format!("{scaled:.decimals$}{suffix}")
}

/// Pad `text` to `width` display columns, ignoring ANSI escape sequences.
fn pad(text: &str, width: usize) -> String {
    let visible = visible_len(text);
    if visible >= width {
        text.to_string()
    } else {
        format!("{text}{}", " ".repeat(width - visible))
    }
}

/// Length of `text` in display columns, skipping ANSI escapes.
fn visible_len(text: &str) -> usize {
    let mut count = 0;
    let mut in_escape = false;
    for ch in text.chars() {
        if in_escape {
            if ch == 'm' {
                in_escape = false;
            }
        } else if ch == '\x1b' {
            in_escape = true;
        } else {
            count += 1;
        }
    }
    count
}

/// Right-align `text` within `width` display columns.
fn rpad(text: &str, width: usize) -> String {
    let visible = visible_len(text);
    if visible >= width {
        text.to_string()
    } else {
        format!("{}{text}", " ".repeat(width - visible))
    }
}

/// Render a report as a terminal table.
pub fn table(report: &Report, color: Color) -> String {
    let mut out = String::new();
    let threads = report.config.threads;

    out.push_str(&color.bold(&format!(
        "ThreadStone {} · {}\n",
        report.tool_version,
        report.system.describe()
    )));
    out.push_str(&color.dim(&format!(
        "{} · {} samples of {} ms after {} warmup · {:.1}s total\n\n",
        report.generated_at,
        report.config.samples,
        report.config.window_ms,
        report.config.warmup,
        report.duration_secs,
    )));

    const W_NAME: usize = 20;
    const W_UNIT: usize = 9;
    const W_VALUE: usize = 12;
    const W_SCALE: usize = 9;

    out.push_str(&color.dim(&format!(
        "{}{}{}{}{}{}\n",
        pad("Workload", W_NAME),
        pad("Unit", W_UNIT),
        rpad("1 thread", W_VALUE),
        rpad(&format!("{threads} threads"), W_VALUE),
        rpad("scaling", W_SCALE),
        "  cv",
    )));
    out.push_str(&color.dim(&format!("{}\n", "─".repeat(68))));

    for w in &report.workloads {
        out.push_str(&workload_row(w, color, W_NAME, W_UNIT, W_VALUE, W_SCALE));
    }

    out.push_str(&color.dim(&format!("{}\n", "─".repeat(68))));
    out.push_str(&score_line(report, color));
    out.push_str(&caveats(report, color));
    out
}

fn workload_row(
    w: &WorkloadReport,
    color: Color,
    w_name: usize,
    w_unit: usize,
    w_value: usize,
    w_scale: usize,
) -> String {
    let value_of = |p: &Option<Pass>| p.as_ref().map_or("—".to_string(), |p| si(p.value));
    let scaling = w
        .scaling
        .as_ref()
        .map_or_else(|| "—".to_string(), |s| format!("{:.1}x", s.speedup));

    // Show the worse of the two passes' stability: a table should surface the
    // weakest evidence behind a row, not the strongest.
    let worst = [&w.single_thread, &w.multi_thread]
        .into_iter()
        .flatten()
        .map(|p| (p.stats.stability, p.stats.cv))
        .max_by(|a, b| a.1.total_cmp(&b.1));

    let (glyph, cv) = match worst {
        Some((stability, cv)) => (
            color.for_stability(stability),
            format!("{:.1}%", cv * 100.0),
        ),
        None => ("—".to_string(), String::new()),
    };

    let mut row = format!(
        "{}{}{}{}{}  {} {}\n",
        pad(&w.name, w_name),
        color.dim(&pad(w.unit.label(), w_unit)),
        rpad(&value_of(&w.single_thread), w_value),
        rpad(&value_of(&w.multi_thread), w_value),
        rpad(&scaling, w_scale),
        glyph,
        color.dim(&cv),
    );

    if let Some(err) = &w.error {
        row.push_str(&color.wrap("31", &format!("  ! {err}\n")));
    }
    row
}

fn score_line(report: &Report, color: Color) -> String {
    let fmt = |v: Option<f64>| v.map_or("—".to_string(), |v| format!("{v:.0}"));
    format!(
        "{}  single-core {}   multi-core {}\n",
        color.bold("ThreadStone Score"),
        color.bold(&fmt(report.score.single_core)),
        color.bold(&fmt(report.score.multi_core)),
    )
}

/// Warnings a reader needs in order to interpret the numbers correctly.
///
/// Printed unconditionally when they apply. A benchmark that quietly reports an
/// unreliable number as though it were solid is worse than one that reports
/// nothing.
fn caveats(report: &Report, color: Color) -> String {
    let mut notes: Vec<String> = Vec::new();

    if report.system.build_profile.debug_assertions {
        notes.push(
            "built with debug assertions: these numbers do not describe an \
             optimised build"
                .to_string(),
        );
    }

    let shaky: Vec<&str> = report
        .workloads
        .iter()
        .filter(|w| {
            [&w.single_thread, &w.multi_thread]
                .into_iter()
                .flatten()
                .any(|p| !p.stats.stability.is_trustworthy())
        })
        .map(|w| w.id.as_str())
        .collect();
    if !shaky.is_empty() {
        notes.push(format!(
            "high run-to-run variance in {}: close other applications and re-run",
            shaky.join(", ")
        ));
    }

    let short: Vec<&str> = report
        .workloads
        .iter()
        .filter(|w| {
            [&w.single_thread, &w.multi_thread]
                .into_iter()
                .flatten()
                .any(|p| p.window_too_short)
        })
        .map(|w| w.id.as_str())
        .collect();
    if !short.is_empty() {
        notes.push(format!(
            "measurement windows too short in {}: raise --window-ms",
            short.join(", ")
        ));
    }

    let excluded: Vec<&str> = report
        .workloads
        .iter()
        .filter(|w| w.excluded_from_multi_core.is_some())
        .map(|w| w.id.as_str())
        .collect();
    if !excluded.is_empty() {
        notes.push(format!(
            "{} measured single-threaded only, and excluded from the multi-core score",
            excluded.join(", ")
        ));
    }

    if notes.is_empty() {
        return String::new();
    }
    let mut out = String::from("\n");
    for note in notes {
        out.push_str(&color.dim(&format!("note: {note}\n")));
    }
    out
}

/// Render a report as a Markdown table, for pasting into issues and READMEs.
pub fn markdown(report: &Report) -> String {
    let threads = report.config.threads;
    let mut out = String::new();

    out.push_str(&format!("## ThreadStone {}\n\n", report.tool_version));
    out.push_str(&format!("**{}**\n\n", report.system.describe()));
    if let (Some(single), Some(multi)) = (report.score.single_core, report.score.multi_core) {
        out.push_str(&format!(
            "**Score:** {single:.0} single-core · {multi:.0} multi-core \
             (vs. {})\n\n",
            report.score.reference
        ));
    }

    out.push_str(&format!(
        "| Workload | Unit | 1 thread | {threads} threads | Scaling | CV |\n"
    ));
    out.push_str("|---|---|---:|---:|---:|---:|\n");

    for w in &report.workloads {
        let value_of = |p: &Option<Pass>| p.as_ref().map_or("—".to_string(), |p| si(p.value));
        let scaling = w
            .scaling
            .as_ref()
            .map_or_else(|| "—".to_string(), |s| format!("{:.1}×", s.speedup));
        let cv = [&w.single_thread, &w.multi_thread]
            .into_iter()
            .flatten()
            .map(|p| p.stats.cv)
            .fold(f64::NEG_INFINITY, f64::max);
        let cv = if cv.is_finite() {
            format!("{:.1}%", cv * 100.0)
        } else {
            "—".to_string()
        };
        out.push_str(&format!(
            "| {} | {} | {} | {} | {} | {} |\n",
            w.name,
            w.unit.label(),
            value_of(&w.single_thread),
            value_of(&w.multi_thread),
            scaling,
            cv,
        ));
    }

    out.push_str(&format!(
        "\n<sub>{} · {} samples of {} ms · generated {}</sub>\n",
        report.system.target, report.config.samples, report.config.window_ms, report.generated_at,
    ));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn si_scales_across_the_whole_range() {
        assert_eq!(si(0.0), "0.00");
        assert_eq!(si(42.5), "42.5");
        assert_eq!(si(999.0), "999");
        assert_eq!(si(1_234.0), "1.23k");
        assert_eq!(si(45_600_000.0), "45.6M");
        assert_eq!(si(2_500_000_000.0), "2.50G");
        assert_eq!(si(1.5e12), "1.50T");
    }

    #[test]
    fn si_handles_negatives_and_non_finite() {
        assert_eq!(si(-1_500.0), "-1.50k");
        assert_eq!(si(f64::NAN), "—");
        assert_eq!(si(f64::INFINITY), "—");
    }

    #[test]
    fn visible_len_skips_ansi_escapes() {
        assert_eq!(visible_len("abc"), 3);
        assert_eq!(visible_len("\x1b[1mabc\x1b[0m"), 3);
        assert_eq!(visible_len("\x1b[31m=\x1b[0m"), 1);
    }

    #[test]
    fn padding_aligns_coloured_and_plain_text_identically() {
        // Without escape-aware padding, coloured columns drift out of line.
        let plain = pad("abc", 10);
        let coloured = pad("\x1b[1mabc\x1b[0m", 10);
        assert_eq!(visible_len(&plain), visible_len(&coloured));
        assert_eq!(visible_len(&rpad("42", 8)), 8);
    }

    #[test]
    fn padding_does_not_truncate_oversized_text() {
        assert_eq!(pad("abcdefgh", 3), "abcdefgh");
        assert_eq!(rpad("abcdefgh", 3), "abcdefgh");
    }

    #[test]
    fn color_never_emits_no_escapes() {
        let text = Color::Never.bold("x");
        assert_eq!(text, "x");
        assert!(!Color::Never
            .for_stability(Stability::Noisy)
            .contains('\x1b'));
    }

    #[test]
    fn color_always_emits_escapes() {
        assert!(Color::Always.bold("x").contains('\x1b'));
    }
}
