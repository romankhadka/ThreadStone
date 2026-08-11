//! Checking a result file.
//!
//! Verification asks three separate questions, and reports each one's answer
//! rather than collapsing them into a single pass/fail:
//!
//! 1. **Does it parse?** Serde enforces the document's shape strictly, so a
//!    file that deserialises has every required field at the right type.
//! 2. **Is it internally coherent?** A file can parse and still be nonsense —
//!    a negative sample, a median outside its own min/max, a schema version
//!    from the future. These are the checks a JSON Schema cannot express.
//! 3. **Is it unmodified?** The Ed25519 signature over the canonical form.
//!
//! The previous implementation validated a report against a schema generated
//! from the very type it had just deserialised into, which by construction can
//! never fail. The useful checks are the semantic ones.

use std::path::Path;

use threadstone_core::report::{Report, SCHEMA_VERSION};

use crate::signing;

/// Result of checking one file.
#[derive(Debug)]
pub struct Outcome {
    /// Whether the document deserialised, and its parse error if not.
    pub parse_error: Option<String>,
    /// Semantic problems found. Empty means the document is coherent.
    pub problems: Vec<String>,
    /// Non-fatal observations worth printing.
    pub notes: Vec<String>,
    /// Signature check result: `None` if the file carried no signature.
    pub signature: Option<Result<(), String>>,
    /// Whether a missing signature should be treated as a failure.
    pub signature_required: bool,
}

impl Outcome {
    /// Whether the file passed every check that was applied.
    pub fn is_ok(&self) -> bool {
        if self.parse_error.is_some() || !self.problems.is_empty() {
            return false;
        }
        match &self.signature {
            Some(Ok(())) => true,
            Some(Err(_)) => false,
            None => !self.signature_required,
        }
    }
}

/// Check the contents of a result file.
pub fn check(text: &str, signature_required: bool) -> Outcome {
    let mut outcome = Outcome {
        parse_error: None,
        problems: Vec::new(),
        notes: Vec::new(),
        signature: None,
        signature_required,
    };

    let report: Report = match serde_json::from_str(text) {
        Ok(r) => r,
        Err(e) => {
            outcome.parse_error = Some(e.to_string());
            return outcome;
        }
    };

    outcome.problems.extend(semantic_problems(&report));
    outcome.notes.extend(observations(&report));

    if let Some(sig) = &report.signature {
        outcome.signature = Some(match report.signing_bytes() {
            Ok(message) => signing::verify(&message, sig).map_err(|e| e.to_string()),
            Err(e) => Err(format!("cannot re-serialise for verification: {e}")),
        });
    }

    outcome
}

/// Checks that a schema cannot express: relationships between fields.
fn semantic_problems(report: &Report) -> Vec<String> {
    let mut problems = Vec::new();

    if report.schema_version > SCHEMA_VERSION {
        problems.push(format!(
            "schema version {} is newer than this build understands ({SCHEMA_VERSION})",
            report.schema_version
        ));
    }
    if report.system.logical_cores == 0 {
        problems.push("system reports zero logical cores".to_string());
    }
    if report.config.samples == 0 {
        problems.push("configuration records zero samples".to_string());
    }
    if !report.duration_secs.is_finite() || report.duration_secs < 0.0 {
        problems.push(format!("implausible duration {}", report.duration_secs));
    }

    for w in &report.workloads {
        for (label, pass) in [
            ("single-thread", &w.single_thread),
            ("multi-thread", &w.multi_thread),
        ] {
            let Some(pass) = pass else { continue };
            let where_ = format!("{} {label}", w.id);

            if !pass.value.is_finite() || pass.value <= 0.0 {
                problems.push(format!("{where_}: non-positive value {}", pass.value));
            }
            if pass.threads == 0 {
                problems.push(format!("{where_}: zero threads"));
            }
            if pass.samples.is_empty() {
                problems.push(format!("{where_}: no samples recorded"));
            }
            if pass.samples.iter().any(|v| !v.is_finite()) {
                problems.push(format!("{where_}: contains a non-finite sample"));
            }
            // The headline value must be the median of the retained samples,
            // and a median always lies within the retained range.
            if pass.stats.n > 0 && (pass.value < pass.stats.min || pass.value > pass.stats.max) {
                problems.push(format!(
                    "{where_}: value {} outside its own range [{}, {}]",
                    pass.value, pass.stats.min, pass.stats.max
                ));
            }
            if pass.stats.min > pass.stats.max {
                problems.push(format!("{where_}: min exceeds max"));
            }
            if pass.stats.n + pass.stats.outliers > pass.samples.len() {
                problems.push(format!(
                    "{where_}: statistics count {} samples but only {} are recorded",
                    pass.stats.n + pass.stats.outliers,
                    pass.samples.len()
                ));
            }
        }

        if w.single_thread.is_none() && w.multi_thread.is_none() && w.error.is_none() {
            problems.push(format!("{}: no passes and no error explaining why", w.id));
        }
    }

    problems
}

/// Things a reader should know that are not defects.
fn observations(report: &Report) -> Vec<String> {
    let mut notes = Vec::new();

    if report.system.build_profile.debug_assertions {
        notes.push(
            "produced by a build with debug assertions; the numbers do not \
             describe an optimised binary"
                .to_string(),
        );
    }
    if report.schema_version < SCHEMA_VERSION {
        notes.push(format!(
            "written against schema version {}, current is {SCHEMA_VERSION}",
            report.schema_version
        ));
    }

    let unreliable: Vec<&str> = report
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
    if !unreliable.is_empty() {
        notes.push(format!(
            "high run-to-run variance in {}",
            unreliable.join(", ")
        ));
    }

    let failed: Vec<&str> = report
        .workloads
        .iter()
        .filter(|w| w.error.is_some())
        .map(|w| w.id.as_str())
        .collect();
    if !failed.is_empty() {
        notes.push(format!(
            "workloads that failed to run: {}",
            failed.join(", ")
        ));
    }

    notes
}

/// Render an outcome for the terminal.
pub fn render(outcome: &Outcome, path: &Path) -> String {
    let name = path.display();
    let mut out = String::new();

    if let Some(error) = &outcome.parse_error {
        return format!("FAIL {name}\n  not a valid result document: {error}\n");
    }

    let verdict = if outcome.is_ok() { "OK  " } else { "FAIL" };
    out.push_str(&format!("{verdict} {name}\n"));
    out.push_str("  structure   valid\n");

    if outcome.problems.is_empty() {
        out.push_str("  contents    coherent\n");
    } else {
        out.push_str("  contents    INVALID\n");
        for problem in &outcome.problems {
            out.push_str(&format!("    - {problem}\n"));
        }
    }

    match &outcome.signature {
        Some(Ok(())) => out.push_str("  signature   verified\n"),
        Some(Err(e)) => out.push_str(&format!("  signature   INVALID: {e}\n")),
        None if outcome.signature_required => {
            out.push_str("  signature   MISSING (required by --require-signature)\n");
        }
        None => out.push_str("  signature   absent (nothing to check)\n"),
    }

    for note in &outcome.notes {
        out.push_str(&format!("  note: {note}\n"));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use threadstone_core::kernel::Unit;
    use threadstone_core::report::{Pass, RunSettings, WorkloadReport};
    use threadstone_core::score::ScoreCard;
    use threadstone_core::stats::Summary;
    use threadstone_core::sysinfo::SystemInfo;

    fn valid_report() -> Report {
        let samples = vec![100.0, 101.0, 99.0, 100.5, 99.5];
        let stats = Summary::new(&samples).unwrap();
        Report {
            schema_version: SCHEMA_VERSION,
            tool_version: "2.0.0".into(),
            generated_at: "2026-08-10T12:00:00Z".into(),
            duration_secs: 12.5,
            system: SystemInfo::detect(),
            config: RunSettings {
                threads: 4,
                samples: 5,
                warmup: 2,
                window_ms: 250,
            },
            workloads: vec![WorkloadReport {
                id: "sgemm".into(),
                name: "SGEMM".into(),
                summary: "test".into(),
                unit: Unit::Gflops,
                reference: 30.0,
                single_thread: Some(Pass {
                    threads: 1,
                    iters_per_thread: 100,
                    value: stats.median,
                    samples,
                    stats,
                    window_ms: 250.0,
                    window_too_short: false,
                }),
                multi_thread: None,
                scaling: None,
                excluded_from_multi_core: None,
                error: None,
            }],
            score: ScoreCard::new(vec![], vec![]),
            signature: None,
        }
    }

    fn json_of(report: &Report) -> String {
        serde_json::to_string(report).unwrap()
    }

    #[test]
    fn a_well_formed_unsigned_report_passes() {
        let outcome = check(&json_of(&valid_report()), false);
        assert!(outcome.is_ok(), "{:?}", outcome.problems);
        assert!(outcome.problems.is_empty());
        assert!(outcome.signature.is_none());
    }

    #[test]
    fn a_missing_signature_fails_only_when_required() {
        let json = json_of(&valid_report());
        assert!(check(&json, false).is_ok());
        assert!(!check(&json, true).is_ok());
    }

    #[test]
    fn malformed_json_is_reported_as_a_parse_error() {
        let outcome = check("{ not json", false);
        assert!(outcome.parse_error.is_some());
        assert!(!outcome.is_ok());
        let text = render(&outcome, Path::new("x.json"));
        assert!(text.starts_with("FAIL"));
    }

    #[test]
    fn a_document_missing_required_fields_fails_to_parse() {
        let outcome = check(r#"{"schema_version": 2}"#, false);
        assert!(outcome.parse_error.is_some());
    }

    #[test]
    fn a_future_schema_version_is_refused() {
        let mut report = valid_report();
        report.schema_version = SCHEMA_VERSION + 1;
        let outcome = check(&json_of(&report), false);
        assert!(!outcome.is_ok());
        assert!(outcome.problems.iter().any(|p| p.contains("newer")));
    }

    #[test]
    fn a_negative_value_is_caught() {
        let mut report = valid_report();
        report.workloads[0].single_thread.as_mut().unwrap().value = -5.0;
        let outcome = check(&json_of(&report), false);
        assert!(!outcome.is_ok());
        assert!(outcome.problems.iter().any(|p| p.contains("non-positive")));
    }

    #[test]
    fn a_value_outside_its_own_range_is_caught() {
        // The kind of tampering a schema check cannot see: every field is the
        // right type, but the median no longer lies inside the sample range.
        let mut report = valid_report();
        report.workloads[0].single_thread.as_mut().unwrap().value = 9_999.0;
        let outcome = check(&json_of(&report), false);
        assert!(!outcome.is_ok());
        assert!(
            outcome
                .problems
                .iter()
                .any(|p| p.contains("outside its own range")),
            "{:?}",
            outcome.problems
        );
    }

    #[test]
    fn a_workload_with_no_passes_needs_an_explanation() {
        let mut report = valid_report();
        report.workloads[0].single_thread = None;
        let outcome = check(&json_of(&report), false);
        assert!(!outcome.is_ok());

        report.workloads[0].error = Some("calibration failed".into());
        assert!(check(&json_of(&report), false).is_ok());
    }

    #[test]
    fn a_valid_signature_verifies() {
        let key = signing::generate().unwrap();
        let mut report = valid_report();
        let message = report.signing_bytes().unwrap();
        report.signature = Some(signing::sign(&message, &key.pkcs8).unwrap());

        let outcome = check(&json_of(&report), true);
        assert!(outcome.is_ok(), "{outcome:?}");
        assert!(matches!(outcome.signature, Some(Ok(()))));
    }

    #[test]
    fn editing_a_signed_report_invalidates_it() {
        // The end-to-end property the whole signing path exists for.
        let key = signing::generate().unwrap();
        let mut report = valid_report();
        let message = report.signing_bytes().unwrap();
        report.signature = Some(signing::sign(&message, &key.pkcs8).unwrap());

        // Falsify the result after signing.
        let mut tampered: serde_json::Value = serde_json::from_str(&json_of(&report)).unwrap();
        tampered["workloads"][0]["single_thread"]["value"] = serde_json::json!(100.4);
        let outcome = check(&tampered.to_string(), false);

        assert!(!outcome.is_ok());
        assert!(
            matches!(&outcome.signature, Some(Err(e)) if e.contains("does not match")),
            "{:?}",
            outcome.signature
        );
    }

    #[test]
    fn reformatting_a_signed_report_keeps_it_valid() {
        // Canonicalisation must make the signature survive re-serialisation
        // with different whitespace and key order.
        let key = signing::generate().unwrap();
        let mut report = valid_report();
        let message = report.signing_bytes().unwrap();
        report.signature = Some(signing::sign(&message, &key.pkcs8).unwrap());

        let pretty = serde_json::to_string_pretty(&report).unwrap();
        let reparsed: serde_json::Value = serde_json::from_str(&pretty).unwrap();
        let compact = serde_json::to_string(&reparsed).unwrap();

        assert!(check(&pretty, true).is_ok());
        assert!(check(&compact, true).is_ok());
    }

    #[test]
    fn rendering_names_each_check() {
        let text = render(
            &check(&json_of(&valid_report()), false),
            Path::new("r.json"),
        );
        assert!(text.contains("structure"));
        assert!(text.contains("contents"));
        assert!(text.contains("signature"));
        assert!(text.starts_with("OK"));
    }
}
