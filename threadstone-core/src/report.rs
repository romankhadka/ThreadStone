//! The result document: what gets written, validated, signed, and compared.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::kernel::{KernelInfo, Scaling, Unit};
use crate::runner::Measurement;
use crate::score::ScoreCard;
use crate::stats::Summary;
use crate::sysinfo::SystemInfo;

/// Schema version of the result document.
///
/// Bumped whenever a change would break a consumer: a removed field, a renamed
/// field, or a changed meaning. Adding an optional field does not bump it.
pub const SCHEMA_VERSION: u32 = 2;

/// A complete benchmark result.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Report {
    /// Version of this document's schema. See [`SCHEMA_VERSION`].
    pub schema_version: u32,
    /// Version of the tool that produced it.
    pub tool_version: String,
    /// UTC completion time, RFC 3339.
    pub generated_at: String,
    /// Total wall-clock duration of the run, in seconds.
    pub duration_secs: f64,
    /// The machine and toolchain this was measured on.
    pub system: SystemInfo,
    /// How the run was configured.
    pub config: RunSettings,
    /// One entry per workload, in execution order.
    pub workloads: Vec<WorkloadReport>,
    /// Composite scores.
    pub score: ScoreCard,
    /// Detached signature over the canonical form of this document.
    ///
    /// Excluded from the bytes it signs; see [`canonical_json`].
    #[serde(skip_serializing_if = "Option::is_none")]
    pub signature: Option<Signature>,
}

/// Parameters the run was executed with.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct RunSettings {
    /// Threads used for the multi-core pass.
    pub threads: usize,
    /// Measured rounds per workload per pass.
    pub samples: u32,
    /// Discarded rounds before measurement.
    pub warmup: u32,
    /// Target measurement window, in milliseconds.
    pub window_ms: u64,
}

/// One workload's results across both passes.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct WorkloadReport {
    /// Stable workload identifier.
    pub id: String,
    /// Display name.
    pub name: String,
    /// What this workload stresses.
    pub summary: String,
    /// Unit of every value in this entry.
    pub unit: Unit,
    /// Reference value used for scoring.
    pub reference: f64,
    /// Single-thread pass.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub single_thread: Option<Pass>,
    /// Multi-thread pass. Absent for single-thread-only workloads.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub multi_thread: Option<Pass>,
    /// Speedup and efficiency, when both passes ran.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scaling: Option<ScalingReport>,
    /// Set when this workload is excluded from the multi-core score, with the
    /// reason. See [`Scaling::SingleThreadOnly`].
    #[serde(skip_serializing_if = "Option::is_none")]
    pub excluded_from_multi_core: Option<String>,
    /// Why this workload produced no result, if it failed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// One measurement pass at a fixed thread count.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Pass {
    /// Threads used.
    pub threads: usize,
    /// Calibrated work units per thread per round.
    pub iters_per_thread: u64,
    /// Headline value: the median of `samples`.
    pub value: f64,
    /// Per-round values, in collection order.
    pub samples: Vec<f64>,
    /// Robust statistics over `samples`.
    pub stats: Summary,
    /// Median round duration, in milliseconds.
    pub window_ms: f64,
    /// Set when rounds were too short for the clock to resolve well.
    ///
    /// `default` is required alongside `skip_serializing_if`: without it the
    /// field is omitted when false and then rejected as missing on read, so
    /// every report the tool wrote would fail to parse.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub window_too_short: bool,
}

impl Pass {
    /// Build a pass from a runner [`Measurement`].
    pub fn from_measurement(m: &Measurement) -> Pass {
        Pass {
            threads: m.threads,
            iters_per_thread: m.iters_per_thread,
            value: m.value(),
            samples: m.samples.clone(),
            stats: m.summary.clone(),
            window_ms: m.window_ms,
            window_too_short: m.window_too_short,
        }
    }
}

/// How well a workload used additional threads.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ScalingReport {
    /// Multi-thread value divided by single-thread value, direction-corrected.
    pub speedup: f64,
    /// `speedup / threads`. 1.0 is perfect linear scaling.
    ///
    /// Values above 1.0 are real and expected on `Partitioned` workloads, where
    /// splitting the working set gives each thread a slice that fits in a
    /// smaller, faster level of cache.
    pub efficiency: f64,
    /// Threads the multi-thread pass used.
    pub threads: usize,
}

impl ScalingReport {
    /// Compute scaling between two passes.
    ///
    /// Returns `None` when the single-thread value is not positive, which would
    /// make the speedup undefined.
    pub fn compute(single: &Pass, multi: &Pass, unit: Unit) -> Option<ScalingReport> {
        if single.value <= 0.0 || !single.value.is_finite() || !multi.value.is_finite() {
            return None;
        }
        let speedup = if unit.higher_is_better() {
            multi.value / single.value
        } else {
            single.value / multi.value
        };
        Some(ScalingReport {
            speedup,
            efficiency: speedup / multi.threads.max(1) as f64,
            threads: multi.threads,
        })
    }
}

/// A detached Ed25519 signature over a report's canonical form.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Signature {
    /// Signature algorithm. Always `"ed25519"` in schema version 2.
    pub algorithm: String,
    /// Standard-base64 public key, so a verifier needs nothing but this file.
    ///
    /// This authenticates *integrity*, not *authority*: anyone can sign with
    /// their own key. It proves a result has not been edited since signing,
    /// and nothing more. Establishing that a given key is trustworthy is out of
    /// scope for the file format.
    pub public_key: String,
    /// Standard-base64 signature bytes.
    pub value: String,
}

impl Report {
    /// Serialise this report to canonical bytes for signing or verification.
    ///
    /// # Why this goes through JSON text rather than straight to a `Value`
    ///
    /// `serde_json`'s float parser is not correctly rounded. It writes the
    /// shortest round-tripping representation, but reading that text back can
    /// land on the adjacent double:
    ///
    /// ```text
    /// 99.0 * 0.8 + 99.5 * 0.2  ->  bits 4058c66666666667
    /// serialised               ->  "99.10000000000001"
    /// parsed back              ->  bits 4058c66666666666   (different!)
    /// re-serialised            ->  "99.1"
    /// ```
    ///
    /// So a signer canonicalising its in-memory struct produces different bytes
    /// from a verifier canonicalising the same document after reading it from
    /// disk, and every signature fails. Statistics like `p05` land on values of
    /// exactly this kind routinely.
    ///
    /// Round-tripping through text first puts both sides on the same footing:
    /// the signer canonicalises the values a reader will actually see. One trip
    /// is enough — parsing the shortest representation of an already-parsed
    /// value is a fixed point.
    pub fn signing_bytes(&self) -> Result<Vec<u8>, serde_json::Error> {
        let text = serde_json::to_string(self)?;
        let mut value: serde_json::Value = serde_json::from_str(&text)?;
        if let Some(map) = value.as_object_mut() {
            map.remove("signature");
        }
        Ok(canonical_json(&value).into_bytes())
    }
}

/// Render a JSON value in canonical form: sorted keys, no insignificant
/// whitespace.
///
/// A signature is over bytes, so the bytes must be reproducible. Signing
/// `serde_json::to_string_pretty` output — as the previous implementation
/// intended to — makes the signature depend on indentation and key order, so a
/// verifier that re-serialises with a different serde version, or with
/// `preserve_order` enabled anywhere in its dependency graph, computes
/// different bytes and rejects a valid file.
///
/// Keys are sorted by Unicode scalar value, matching JCS (RFC 8785) for the
/// subset of JSON this format uses. Numbers are left to `serde_json`, whose
/// float formatting is already shortest-round-trip.
pub fn canonical_json(value: &serde_json::Value) -> String {
    let mut out = String::new();
    write_canonical(value, &mut out);
    out
}

fn write_canonical(value: &serde_json::Value, out: &mut String) {
    use serde_json::Value;
    match value {
        Value::Object(map) => {
            // `serde_json::Map` iterates in sorted order by default, but sorting
            // explicitly keeps this correct even if the `preserve_order`
            // feature is switched on elsewhere in the dependency graph.
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort_unstable();
            out.push('{');
            for (i, key) in keys.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                out.push_str(&serde_json::Value::String((*key).clone()).to_string());
                out.push(':');
                write_canonical(&map[*key], out);
            }
            out.push('}');
        }
        Value::Array(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_canonical(item, out);
            }
            out.push(']');
        }
        // Scalars have exactly one serde_json rendering, so reuse it.
        other => out.push_str(&other.to_string()),
    }
}

/// Assemble a [`WorkloadReport`] from a kernel's metadata and its passes.
pub fn workload_report(
    info: &KernelInfo,
    single: Option<Pass>,
    multi: Option<Pass>,
    error: Option<String>,
) -> WorkloadReport {
    let scaling = match (&single, &multi) {
        (Some(s), Some(m)) => ScalingReport::compute(s, m, info.unit),
        _ => None,
    };
    let excluded = match info.scaling {
        Scaling::SingleThreadOnly => Some(
            "measured single-threaded only: partitioning the working set across \
             threads would shrink each slice into cache and report a flattering \
             number that does not describe the machine"
                .to_string(),
        ),
        Scaling::Scales => None,
    };
    WorkloadReport {
        id: info.id.to_string(),
        name: info.name.to_string(),
        summary: info.summary.to_string(),
        unit: info.unit,
        reference: info.reference,
        single_thread: single,
        multi_thread: multi,
        scaling,
        excluded_from_multi_core: excluded,
        error,
    }
}

/// Current UTC time as an RFC 3339 string, to second precision.
///
/// Implemented directly rather than via a date library: the whole crate has two
/// dependencies and a timestamp is not worth a third. Uses Howard Hinnant's
/// `civil_from_days`, which is exact for the entire proleptic Gregorian
/// calendar. A clock set before 1970 yields the epoch rather than a wrong date.
pub fn now_rfc3339() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    format_rfc3339(secs)
}

fn format_rfc3339(unix_secs: u64) -> String {
    let days = (unix_secs / 86_400) as i64;
    let rem = unix_secs % 86_400;
    let (hour, minute, second) = (rem / 3600, (rem % 3600) / 60, rem % 60);
    let (year, month, day) = civil_from_days(days);
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}Z")
}

/// Convert days since the Unix epoch to a civil (year, month, day).
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    // Shift the epoch to 0000-03-01 so leap days land at the end of the cycle.
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11], March-based
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32; // [1, 12]
    (if m <= 2 { y + 1 } else { y }, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn canonical_json_sorts_keys() {
        let value = json!({ "z": 1, "a": 2, "m": { "y": 3, "b": 4 } });
        assert_eq!(canonical_json(&value), r#"{"a":2,"m":{"b":4,"y":3},"z":1}"#);
    }

    #[test]
    fn canonical_json_preserves_array_order() {
        let value = json!({ "list": [3, 1, 2] });
        assert_eq!(canonical_json(&value), r#"{"list":[3,1,2]}"#);
    }

    #[test]
    fn canonical_json_is_whitespace_free() {
        let value = json!({ "a": [1, { "b": "c d" }] });
        let text = canonical_json(&value);
        assert!(!text.contains(' ') || text.contains("c d"));
        assert!(!text.contains('\n'));
    }

    #[test]
    fn canonical_json_escapes_keys_and_strings() {
        let value = json!({ "a\"b": "line\nbreak" });
        let text = canonical_json(&value);
        assert_eq!(text, r#"{"a\"b":"line\nbreak"}"#);
    }

    #[test]
    fn canonical_json_is_order_independent() {
        // The property that makes signatures verifiable: two documents with the
        // same content in different orders must canonicalise identically.
        let a: serde_json::Value = serde_json::from_str(r#"{"b":1,"a":{"d":2,"c":3}}"#).unwrap();
        let b: serde_json::Value = serde_json::from_str(r#"{"a":{"c":3,"d":2},"b":1}"#).unwrap();
        assert_eq!(canonical_json(&a), canonical_json(&b));
    }

    #[test]
    fn rfc3339_matches_known_instants() {
        assert_eq!(format_rfc3339(0), "1970-01-01T00:00:00Z");
        assert_eq!(format_rfc3339(1_000_000_000), "2001-09-09T01:46:40Z");
        // 2024-02-29: a leap day in a leap century-rule year.
        assert_eq!(format_rfc3339(1_709_164_800), "2024-02-29T00:00:00Z");
        // 2000-03-01, the day after the 400-year leap-year exception.
        assert_eq!(format_rfc3339(951_868_800), "2000-03-01T00:00:00Z");
        assert_eq!(format_rfc3339(1_735_689_599), "2024-12-31T23:59:59Z");
    }

    #[test]
    fn now_is_after_this_code_was_written() {
        let now = now_rfc3339();
        assert!(now.ends_with('Z'), "must be UTC: {now}");
        assert_eq!(now.len(), 20, "unexpected length: {now}");
        assert!(
            now.as_str() > "2025-01-01T00:00:00Z",
            "clock looks wrong: {now}"
        );
    }

    fn pass(value: f64, threads: usize) -> Pass {
        Pass {
            threads,
            iters_per_thread: 100,
            value,
            samples: vec![value],
            stats: Summary::new(&[value]).unwrap(),
            window_ms: 250.0,
            window_too_short: false,
        }
    }

    #[test]
    fn throughput_scaling_uses_the_ratio_directly() {
        let s = ScalingReport::compute(&pass(10.0, 1), &pass(80.0, 8), Unit::Gflops).unwrap();
        assert!((s.speedup - 8.0).abs() < 1e-12);
        assert!((s.efficiency - 1.0).abs() < 1e-12);
    }

    #[test]
    fn latency_scaling_inverts_the_ratio() {
        // Latency getting worse under load is a speedup below 1.
        let s = ScalingReport::compute(&pass(80.0, 1), &pass(160.0, 8), Unit::Nanoseconds).unwrap();
        assert!((s.speedup - 0.5).abs() < 1e-12);
    }

    #[test]
    fn scaling_is_undefined_for_a_zero_baseline() {
        assert!(ScalingReport::compute(&pass(0.0, 1), &pass(8.0, 8), Unit::Gflops).is_none());
    }

    #[test]
    fn canonical_bytes_survive_a_json_round_trip() {
        // The regression this guards is subtle and total: `serde_json` writes
        // 99.10000000000001 and reads it back as the adjacent double, so
        // canonicalising an in-memory struct and canonicalising the same
        // document after a save/load produce different bytes — and every
        // signature ever written fails to verify.
        //
        // 99.0*0.8 + 99.5*0.2 is exactly such a value, and it is what the p05
        // of a five-sample pass works out to.
        let awkward: f64 = 99.0 * 0.8 + 99.5 * 0.2;
        assert_eq!(
            awkward.to_bits(),
            0x4058_c666_6666_6667,
            "test premise: this literal must be the double that does not round-trip"
        );

        #[derive(Serialize, serde::Deserialize)]
        struct Holder {
            p05: f64,
        }

        let original = Holder { p05: awkward };
        let text = serde_json::to_string(&original).unwrap();
        let reloaded: Holder = serde_json::from_str(&text).unwrap();
        assert_ne!(
            reloaded.p05.to_bits(),
            original.p05.to_bits(),
            "premise: serde_json must actually lose this value"
        );

        // Canonicalising via a text round trip must nonetheless agree.
        let canonical = |h: &Holder| {
            let text = serde_json::to_string(h).unwrap();
            let value: serde_json::Value = serde_json::from_str(&text).unwrap();
            canonical_json(&value)
        };
        assert_eq!(canonical(&original), canonical(&reloaded));
    }

    #[test]
    fn signing_bytes_ignore_the_signature_field() {
        let value = json!({
            "schema_version": 2,
            "signature": { "algorithm": "ed25519", "value": "AAAA", "public_key": "BBBB" },
            "tool_version": "2.0.0"
        });
        let mut stripped = value.clone();
        stripped.as_object_mut().unwrap().remove("signature");
        // Any two documents differing only in `signature` must sign the same
        // bytes, or a file could never carry its own signature.
        assert_eq!(
            canonical_json(&stripped),
            r#"{"schema_version":2,"tool_version":"2.0.0"}"#
        );
    }
}
