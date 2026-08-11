//! Progress reporting during a run.
//!
//! Everything here writes to stderr, so `threadstone run --format json > out`
//! produces a clean document while the user still sees what is happening.
//!
//! A benchmark suite spends a minute or more doing nothing visible, and silence
//! is indistinguishable from a hang. The progress line also serves a second
//! purpose: showing the calibrated iteration count and window makes it obvious
//! when the runner has chosen something absurd.

use std::io::{IsTerminal, Write};
use std::sync::Mutex;

use threadstone_core::runner::{Measurement, Observer};
use threadstone_core::suite::SuiteObserver;

/// Writes single-line progress to stderr.
pub struct Progress {
    /// Suppresses all output.
    quiet: bool,
    /// Whether stderr can handle carriage-return redraws.
    interactive: bool,
    /// Guards interleaved writes; the runner only reports from its coordinating
    /// thread, but the trait does not promise that.
    state: Mutex<Line>,
}

#[derive(Default)]
struct Line {
    /// Columns written by the last redraw, so it can be cleared exactly.
    width: usize,
    /// Label of the pass in progress.
    label: String,
}

impl Progress {
    /// Create a reporter. `quiet` disables all output.
    pub fn new(quiet: bool) -> Progress {
        Progress {
            quiet,
            interactive: std::io::stderr().is_terminal(),
            state: Mutex::new(Line::default()),
        }
    }

    /// Clear any in-progress line. Call once the run is complete.
    pub fn finish(&self) {
        if self.quiet {
            return;
        }
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        self.clear(&mut state);
    }

    fn clear(&self, state: &mut Line) {
        if self.interactive && state.width > 0 {
            let mut err = std::io::stderr();
            let _ = write!(err, "\r{}\r", " ".repeat(state.width));
            let _ = err.flush();
            state.width = 0;
        }
    }

    /// Redraw the transient status line, or print nothing when not interactive.
    fn draw(&self, text: &str) {
        if self.quiet {
            return;
        }
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if !self.interactive {
            return;
        }
        self.clear(&mut state);
        let mut err = std::io::stderr();
        let _ = write!(err, "{text}");
        let _ = err.flush();
        state.width = text.chars().count();
    }

    /// Print a permanent line above the status line.
    fn emit(&self, text: &str) {
        if self.quiet {
            return;
        }
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        self.clear(&mut state);
        let _ = writeln!(std::io::stderr(), "{text}");
    }

    fn label(&self) -> String {
        self.state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .label
            .clone()
    }

    fn set_label(&self, label: String) {
        self.state.lock().unwrap_or_else(|e| e.into_inner()).label = label;
    }
}

impl Observer for Progress {
    fn calibrating(&self, _id: &str, _threads: usize) {
        self.draw(&format!("  {} · calibrating…", self.label()));
    }

    fn calibrated(&self, _id: &str, iters: u64, window_ms: f64) {
        self.draw(&format!(
            "  {} · {} iters/thread, {:.0} ms per round",
            self.label(),
            iters,
            window_ms
        ));
    }

    fn sample(&self, _id: &str, index: u32, total: u32, rate: f64) {
        let filled = (index as usize).min(total as usize);
        let bar: String = "●".repeat(filled) + &"·".repeat(total as usize - filled);
        self.draw(&format!(
            "  {} · {bar} {}",
            self.label(),
            crate::render::si(rate)
        ));
    }

    fn finished(&self, _id: &str, m: &Measurement) {
        self.emit(&format!(
            "  {:<28} {:>10} {}  ±{:.1}%",
            self.label(),
            crate::render::si(m.value()),
            m.unit.label(),
            m.summary.cv * 100.0,
        ));
    }
}

impl SuiteObserver for Progress {
    fn workload_start(&self, _id: &str, name: &str, threads: usize) {
        let suffix = if threads == 1 {
            "1 thread".to_string()
        } else {
            format!("{threads} threads")
        };
        self.set_label(format!("{name} ({suffix})"));
    }

    fn workload_failed(&self, id: &str, error: &str) {
        self.emit(&format!("  {id}: FAILED — {error}"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quiet_progress_produces_no_state_changes() {
        let p = Progress::new(true);
        p.workload_start("x", "X", 4);
        p.calibrating("x", 4);
        p.sample("x", 1, 7, 1.0);
        p.finish();
        // set_label runs regardless; the point is that nothing panics and no
        // output escapes.
        assert_eq!(p.label(), "X (4 threads)");
    }

    #[test]
    fn labels_read_naturally_at_one_thread() {
        let p = Progress::new(true);
        p.workload_start("x", "Dhrystone", 1);
        assert_eq!(p.label(), "Dhrystone (1 thread)");
        p.workload_start("x", "Dhrystone", 14);
        assert_eq!(p.label(), "Dhrystone (14 threads)");
    }

    #[test]
    fn a_poisoned_lock_does_not_propagate_the_panic() {
        // Progress reporting must never be the reason a benchmark run dies.
        let p = std::sync::Arc::new(Progress::new(true));
        let clone = std::sync::Arc::clone(&p);
        let _ = std::thread::spawn(move || {
            let _guard = clone.state.lock().unwrap();
            panic!("poison the mutex");
        })
        .join();
        p.set_label("recovered".into());
        assert_eq!(p.label(), "recovered");
    }
}
