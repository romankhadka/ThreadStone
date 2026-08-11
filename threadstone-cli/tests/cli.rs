//! End-to-end tests driving the built binary.
//!
//! Unit tests cover the pieces; these cover the seams — that a run writes a
//! file the verifier accepts, that signing survives the trip through disk, and
//! that the exit codes mean what a shell script would assume they mean.
//!
//! Every invocation uses the smallest configuration that still exercises the
//! real path: one workload, one sample, and a 15 ms window. A full run takes
//! half a minute and belongs in `threadstone run`, not in CI.

use std::path::Path;
use std::process::Command;

use assert_cmd::prelude::*;
use predicates::prelude::*;
use tempfile::TempDir;

/// The binary under test.
fn threadstone() -> Command {
    Command::cargo_bin("threadstone").expect("binary should be built")
}

/// Arguments for the quickest possible real run.
fn quick_run(workload: &str) -> Vec<String> {
    [
        "run",
        "--workload",
        workload,
        "--samples",
        "1",
        "--warmup",
        "0",
        "--window-ms",
        "15",
        "--threads",
        "2",
        "--quiet",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect()
}

#[test]
fn list_describes_every_workload() {
    threadstone()
        .arg("list")
        .assert()
        .success()
        .stdout(predicate::str::contains("dhrystone"))
        .stdout(predicate::str::contains("sgemm"))
        .stdout(predicate::str::contains("sha256"))
        .stdout(predicate::str::contains("sort"))
        .stdout(predicate::str::contains("stream"))
        .stdout(predicate::str::contains("latency"))
        .stdout(predicate::str::contains("ThreadStone Reference Core v1"));
}

#[test]
fn schema_is_valid_json_describing_a_report() {
    let output = threadstone().arg("schema").assert().success();
    let text = String::from_utf8(output.get_output().stdout.clone()).unwrap();
    let schema: serde_json::Value = serde_json::from_str(&text).expect("schema must be JSON");

    assert!(schema.get("$schema").is_some());
    let properties = schema
        .get("properties")
        .and_then(|p| p.as_object())
        .expect("schema must describe properties");
    for required in ["schema_version", "system", "workloads", "score"] {
        assert!(properties.contains_key(required), "missing {required}");
    }
}

#[test]
fn an_unknown_workload_fails_and_names_the_alternatives() {
    threadstone()
        .args(["run", "--workload", "nonexistent"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("unknown workload"))
        .stderr(predicate::str::contains("dhrystone"));
}

#[test]
fn zero_samples_is_rejected() {
    threadstone()
        .args(["run", "--samples", "0"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("--samples"));
}

#[test]
fn a_run_produces_a_file_that_verifies() {
    let dir = TempDir::new().unwrap();
    let out = dir.path().join("result.json");

    threadstone()
        .args(quick_run("sha256"))
        .args(["--out", out.to_str().unwrap()])
        .assert()
        .success();

    assert!(out.exists(), "run should have written the result file");
    threadstone()
        .args(["verify", out.to_str().unwrap()])
        .assert()
        .success()
        .stdout(predicate::str::contains("OK"))
        .stdout(predicate::str::contains("coherent"));
}

#[test]
fn json_output_is_parseable_and_free_of_progress_noise() {
    // Progress goes to stderr precisely so that this works.
    let output = threadstone()
        .args(quick_run("sha256"))
        .args(["--format", "json"])
        .assert()
        .success();

    let text = String::from_utf8(output.get_output().stdout.clone()).unwrap();
    let report: serde_json::Value = serde_json::from_str(&text).expect("stdout must be pure JSON");

    assert_eq!(report["schema_version"], 2);
    assert_eq!(report["workloads"][0]["id"], "sha256");
    assert!(report["workloads"][0]["single_thread"]["value"]
        .as_f64()
        .is_some_and(|v| v > 0.0));
}

#[test]
fn markdown_output_is_a_table() {
    threadstone()
        .args(quick_run("sha256"))
        .args(["--format", "markdown"])
        .assert()
        .success()
        .stdout(predicate::str::contains("| Workload |"))
        .stdout(predicate::str::contains("SHA-256"));
}

#[test]
fn latency_reports_no_multi_thread_pass() {
    // The workload declares itself single-thread-only; the suite must honour
    // that even when the user asks for more threads.
    let output = threadstone()
        .args(quick_run("latency"))
        .args(["--format", "json"])
        .assert()
        .success();

    let text = String::from_utf8(output.get_output().stdout.clone()).unwrap();
    let report: serde_json::Value = serde_json::from_str(&text).unwrap();
    let workload = &report["workloads"][0];

    assert!(workload["single_thread"].is_object());
    assert!(
        workload["multi_thread"].is_null(),
        "must not run multi-thread"
    );
    assert!(
        workload["excluded_from_multi_core"].is_string(),
        "the exclusion must be explained in the document"
    );
}

#[test]
fn keygen_then_sign_then_verify_round_trips() {
    let dir = TempDir::new().unwrap();
    let key = dir.path().join("threadstone.key");
    let out = dir.path().join("signed.json");

    threadstone()
        .args(["keygen", "--dir", dir.path().to_str().unwrap()])
        .assert()
        .success();
    assert!(key.exists());
    assert!(dir.path().join("threadstone.pub").exists());

    threadstone()
        .args(quick_run("sha256"))
        .args(["--out", out.to_str().unwrap()])
        .args(["--sign-key", key.to_str().unwrap()])
        .assert()
        .success();

    threadstone()
        .args(["verify", out.to_str().unwrap(), "--require-signature"])
        .assert()
        .success()
        .stdout(predicate::str::contains("signature   verified"));
}

#[test]
fn an_unsigned_result_can_be_signed_after_the_fact() {
    let dir = TempDir::new().unwrap();
    let key = dir.path().join("threadstone.key");
    let out = dir.path().join("result.json");

    threadstone()
        .args(["keygen", "--dir", dir.path().to_str().unwrap()])
        .assert()
        .success();
    threadstone()
        .args(quick_run("sha256"))
        .args(["--out", out.to_str().unwrap()])
        .assert()
        .success();

    // Unsigned to begin with.
    threadstone()
        .args(["verify", out.to_str().unwrap(), "--require-signature"])
        .assert()
        .failure();

    threadstone()
        .args([
            "sign",
            out.to_str().unwrap(),
            "--key",
            key.to_str().unwrap(),
        ])
        .assert()
        .success();

    threadstone()
        .args(["verify", out.to_str().unwrap(), "--require-signature"])
        .assert()
        .success()
        .stdout(predicate::str::contains("signature   verified"));
}

#[test]
fn signing_can_write_to_a_new_file_and_re_sign_with_another_key() {
    let dir = TempDir::new().unwrap();
    let second = TempDir::new().unwrap();
    let source = dir.path().join("result.json");
    let copy = dir.path().join("signed-copy.json");

    threadstone()
        .args(["keygen", "--dir", dir.path().to_str().unwrap()])
        .assert()
        .success();
    threadstone()
        .args(["keygen", "--dir", second.path().to_str().unwrap()])
        .assert()
        .success();
    threadstone()
        .args(quick_run("sha256"))
        .args(["--out", source.to_str().unwrap()])
        .args([
            "--sign-key",
            dir.path().join("threadstone.key").to_str().unwrap(),
        ])
        .assert()
        .success();

    // Re-signing with a different key must replace the old signature, not
    // leave a stale one that no longer matches.
    threadstone()
        .args([
            "sign",
            source.to_str().unwrap(),
            "--key",
            second.path().join("threadstone.key").to_str().unwrap(),
            "--out",
            copy.to_str().unwrap(),
        ])
        .assert()
        .success();

    threadstone()
        .args(["verify", copy.to_str().unwrap(), "--require-signature"])
        .assert()
        .success();

    let original = std::fs::read_to_string(&source).unwrap();
    let resigned = std::fs::read_to_string(&copy).unwrap();
    assert_ne!(
        original, resigned,
        "a different key must yield a different signature"
    );
}

#[test]
fn signing_with_a_bad_key_fails_clearly() {
    let dir = TempDir::new().unwrap();
    let out = dir.path().join("result.json");
    let junk = dir.path().join("not-a-key");
    std::fs::write(&junk, b"this is not pkcs8").unwrap();

    threadstone()
        .args(quick_run("sha256"))
        .args(["--out", out.to_str().unwrap()])
        .assert()
        .success();
    threadstone()
        .args([
            "sign",
            out.to_str().unwrap(),
            "--key",
            junk.to_str().unwrap(),
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("PKCS#8"));
}

#[test]
fn keygen_refuses_to_overwrite_an_existing_private_key() {
    let dir = TempDir::new().unwrap();
    threadstone()
        .args(["keygen", "--dir", dir.path().to_str().unwrap()])
        .assert()
        .success();
    threadstone()
        .args(["keygen", "--dir", dir.path().to_str().unwrap()])
        .assert()
        .failure()
        .stderr(predicate::str::contains("refusing to overwrite"));
}

#[test]
fn editing_a_signed_result_makes_verification_fail() {
    // The property the previous implementation's stub signature did not have:
    // this must be detected, and must set a non-zero exit code.
    let dir = TempDir::new().unwrap();
    let key = dir.path().join("threadstone.key");
    let out = dir.path().join("signed.json");

    threadstone()
        .args(["keygen", "--dir", dir.path().to_str().unwrap()])
        .assert()
        .success();
    threadstone()
        .args(quick_run("sha256"))
        .args(["--out", out.to_str().unwrap()])
        .args(["--sign-key", key.to_str().unwrap()])
        .assert()
        .success();

    inflate_first_value(&out);

    threadstone()
        .args(["verify", out.to_str().unwrap()])
        .assert()
        .failure()
        .stdout(predicate::str::contains("signature   INVALID"));
}

/// Multiply a report's first recorded value by ten, in place.
fn inflate_first_value(path: &Path) {
    let text = std::fs::read_to_string(path).unwrap();
    let mut report: serde_json::Value = serde_json::from_str(&text).unwrap();
    let value = report["workloads"][0]["single_thread"]["value"]
        .as_f64()
        .unwrap();
    report["workloads"][0]["single_thread"]["value"] = serde_json::json!(value * 10.0);
    std::fs::write(path, serde_json::to_string_pretty(&report).unwrap()).unwrap();
}

#[test]
fn verify_rejects_a_file_that_is_not_a_report() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("junk.json");
    std::fs::write(&path, r#"{"hello": "world"}"#).unwrap();

    threadstone()
        .args(["verify", path.to_str().unwrap()])
        .assert()
        .failure()
        .stdout(predicate::str::contains("not a valid result document"));
}

#[test]
fn verify_reports_a_missing_file_clearly() {
    threadstone()
        .args(["verify", "definitely-not-here.json"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("cannot read"));
}

#[test]
fn comparing_a_report_with_itself_shows_no_change() {
    let dir = TempDir::new().unwrap();
    let out = dir.path().join("a.json");

    threadstone()
        .args(quick_run("sha256"))
        .args(["--out", out.to_str().unwrap()])
        .assert()
        .success();

    threadstone()
        .args(["compare", out.to_str().unwrap(), out.to_str().unwrap()])
        .assert()
        .success()
        .stdout(predicate::str::contains("+0.0%"));
}

#[test]
fn comparing_two_runs_produces_a_delta_for_each_workload() {
    let dir = TempDir::new().unwrap();
    let a = dir.path().join("a.json");
    let b = dir.path().join("b.json");

    for path in [&a, &b] {
        threadstone()
            .args(quick_run("sha256"))
            .args(["--out", path.to_str().unwrap()])
            .assert()
            .success();
    }

    threadstone()
        .args(["compare", a.to_str().unwrap(), b.to_str().unwrap()])
        .assert()
        .success()
        .stdout(predicate::str::contains("sha256"))
        .stdout(predicate::str::contains("single-thread"))
        .stdout(predicate::str::contains("score"));
}

#[test]
fn a_saved_report_can_be_re_rendered() {
    let dir = TempDir::new().unwrap();
    let out = dir.path().join("r.json");

    threadstone()
        .args(quick_run("sha256"))
        .args(["--out", out.to_str().unwrap()])
        .assert()
        .success();

    threadstone()
        .args(["report", out.to_str().unwrap()])
        .assert()
        .success()
        .stdout(predicate::str::contains("ThreadStone Score"));

    threadstone()
        .args(["report", out.to_str().unwrap(), "--format", "markdown"])
        .assert()
        .success()
        .stdout(predicate::str::contains("| Workload |"));
}

#[test]
fn a_run_records_the_machine_it_ran_on() {
    // A result without provenance cannot be checked by anyone, so the fields
    // that make it checkable must always be present.
    let output = threadstone()
        .args(quick_run("sha256"))
        .args(["--format", "json"])
        .assert()
        .success();

    let text = String::from_utf8(output.get_output().stdout.clone()).unwrap();
    let report: serde_json::Value = serde_json::from_str(&text).unwrap();
    let system = &report["system"];

    assert!(system["logical_cores"].as_u64().is_some_and(|n| n >= 1));
    assert!(system["os"].as_str().is_some_and(|s| !s.is_empty()));
    assert!(system["target"]
        .as_str()
        .is_some_and(|s| s != "unknown-target"));
    assert!(system["build_profile"]["opt_level"].as_str().is_some());
    assert!(system["timer"]["resolution_ns"].as_u64().is_some());
    assert!(report["generated_at"]
        .as_str()
        .is_some_and(|s| s.ends_with('Z')));
}

#[test]
fn single_only_and_multi_only_conflict() {
    threadstone()
        .args(["run", "--single-only", "--multi-only"])
        .assert()
        .failure();
}

#[test]
fn sweep_maps_the_cache_hierarchy() {
    let dir = TempDir::new().unwrap();
    let out = dir.path().join("sweep.json");

    threadstone()
        .args(["sweep", "--min-ms", "5", "--out", out.to_str().unwrap()])
        .assert()
        .success();

    let text = std::fs::read_to_string(&out).unwrap();
    let points: Vec<serde_json::Value> = serde_json::from_str(&text).unwrap();
    assert!(points.len() >= 10);

    let first = points.first().unwrap()["latency_ns"].as_f64().unwrap();
    let last = points.last().unwrap()["latency_ns"].as_f64().unwrap();
    assert!(
        last > first * 2.0,
        "a 256 MiB chase ({last:.1}ns) must be far slower than a 4 KiB one ({first:.1}ns)"
    );
}
