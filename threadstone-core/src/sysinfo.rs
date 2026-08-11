//! Captures the machine a result was produced on.
//!
//! A benchmark number without provenance cannot be checked by anyone. "2300
//! Dhrystones/sec" is unfalsifiable; "2300 Dhrystones/sec on an Apple M4 Pro,
//! 10 performance cores plus 4 efficiency cores, macOS 15.6, rustc 1.83.0,
//! aarch64-apple-darwin, opt-level 3 with fat LTO" is a claim someone can
//! reproduce or refute.
//!
//! Everything here degrades gracefully. Each field is an `Option`, no probe can
//! panic, and an unrecognised platform simply yields fewer fields rather than
//! an error. The suite must run on a machine this code has never seen.
//!
//! There are no third-party dependencies: macOS is probed through `sysctl`,
//! Linux through `/proc` and `/sys`, and Windows through environment variables
//! the OS itself populates.

use std::process::Command;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Description of the machine and toolchain that produced a result.
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct SystemInfo {
    /// Marketing name of the CPU, e.g. `"Apple M4 Pro"`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cpu_model: Option<String>,
    /// Vendor string where the platform exposes one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cpu_vendor: Option<String>,
    /// Physical cores, excluding SMT siblings.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub physical_cores: Option<usize>,
    /// Logical processors, as the scheduler sees them.
    pub logical_cores: usize,
    /// Performance cores on a heterogeneous CPU.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub performance_cores: Option<usize>,
    /// Efficiency cores on a heterogeneous CPU.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub efficiency_cores: Option<usize>,
    /// L1 data cache per core, in bytes.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub l1d_bytes: Option<u64>,
    /// L2 cache, in bytes. Per-core or per-cluster depending on the design.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub l2_bytes: Option<u64>,
    /// Last-level cache, in bytes.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub l3_bytes: Option<u64>,
    /// Cache line size, in bytes.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_line_bytes: Option<u64>,
    /// Installed physical memory, in bytes.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub memory_bytes: Option<u64>,
    /// Operating system family, e.g. `"macos"`.
    pub os: String,
    /// OS release string where obtainable.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub os_version: Option<String>,
    /// Target triple the binary was compiled for.
    pub target: String,
    /// Compiler version, captured at build time.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rustc_version: Option<String>,
    /// Optimisation settings the binary was built with.
    pub build_profile: BuildProfile,
    /// Behaviour of the timing hardware on this machine.
    pub timer: TimerInfo,
}

/// How the measuring binary itself was compiled.
///
/// Two runs built with different optimisation settings are not comparable, so
/// the settings travel with the result.
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct BuildProfile {
    /// `opt-level` the crate was compiled at.
    pub opt_level: String,
    /// Whether debug assertions were enabled. If true, the numbers are junk.
    pub debug_assertions: bool,
    /// `--target-cpu` if it was set, otherwise the compiler default.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target_cpu: Option<String>,
    /// Target features the compiler was allowed to use, e.g. `"neon"`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target_features: Option<String>,
}

/// Measured characteristics of the clock used for timing.
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct TimerInfo {
    /// Hardware source of the raw cycle counter.
    pub cycle_source: String,
    /// Frequency of that counter, in Hz.
    pub cycle_hz: f64,
    /// Smallest interval the measurement clock can resolve, in nanoseconds.
    pub resolution_ns: u64,
    /// Cost of reading the measurement clock, in nanoseconds.
    pub overhead_ns: f64,
}

impl SystemInfo {
    /// Probe the current machine.
    ///
    /// Never fails and never panics; unavailable fields are left `None`.
    pub fn detect() -> SystemInfo {
        let logical_cores = std::thread::available_parallelism().map_or(1, |n| n.get());

        let mut info = SystemInfo {
            logical_cores,
            os: std::env::consts::OS.to_string(),
            target: build::TARGET.to_string(),
            rustc_version: option_env!("THREADSTONE_RUSTC_VERSION").map(str::to_string),
            build_profile: BuildProfile {
                opt_level: build::OPT_LEVEL.to_string(),
                debug_assertions: cfg!(debug_assertions),
                target_cpu: option_env!("THREADSTONE_TARGET_CPU").map(str::to_string),
                target_features: build::target_features(),
            },
            timer: TimerInfo {
                cycle_source: crate::time::cycle_source().to_string(),
                cycle_hz: crate::time::cycles_per_second(),
                resolution_ns: crate::time::resolution_nanos(),
                overhead_ns: crate::time::call_overhead_nanos(),
            },
            ..SystemInfo::default()
        };

        platform::fill(&mut info);
        info
    }

    /// Cores worth saturating for a throughput run.
    ///
    /// On a heterogeneous CPU this is still every logical core: efficiency
    /// cores do contribute real throughput, and excluding them would
    /// understate the machine.
    pub fn default_threads(&self) -> usize {
        self.logical_cores.max(1)
    }

    /// One-line description for terminal headers.
    pub fn describe(&self) -> String {
        let cpu = self.cpu_model.as_deref().unwrap_or("unknown CPU");
        let topology = match (self.performance_cores, self.efficiency_cores) {
            (Some(p), Some(e)) if e > 0 => format!("{p}P+{e}E"),
            _ => match self.physical_cores {
                Some(p) if p != self.logical_cores => {
                    format!("{p}C/{}T", self.logical_cores)
                }
                _ => format!("{}C", self.logical_cores),
            },
        };
        format!("{cpu} ({topology}) · {} · {}", self.os, self.target)
    }
}

/// Values fixed when this crate was compiled.
mod build {
    /// Target triple, injected by `build.rs`.
    ///
    /// Falls back to a placeholder rather than a guess: there is no way to
    /// reconstruct a triple from `cfg!` alone, and inventing one would put a
    /// false claim into every result file.
    pub const TARGET: &str = match option_env!("THREADSTONE_TARGET") {
        Some(t) => t,
        None => "unknown-target",
    };

    pub const OPT_LEVEL: &str = match option_env!("THREADSTONE_OPT_LEVEL") {
        Some(o) => o,
        None => "unknown",
    };

    /// Architecture features the compiler was permitted to emit.
    ///
    /// Detected with `cfg!` rather than read from the environment, so it stays
    /// correct even when the build script did not run.
    pub fn target_features() -> Option<String> {
        let mut features: Vec<&str> = Vec::new();
        #[cfg(target_arch = "aarch64")]
        {
            if cfg!(target_feature = "neon") {
                features.push("neon");
            }
            if cfg!(target_feature = "fp16") {
                features.push("fp16");
            }
            if cfg!(target_feature = "dotprod") {
                features.push("dotprod");
            }
        }
        #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
        {
            for (enabled, name) in [
                (cfg!(target_feature = "sse2"), "sse2"),
                (cfg!(target_feature = "avx"), "avx"),
                (cfg!(target_feature = "avx2"), "avx2"),
                (cfg!(target_feature = "fma"), "fma"),
                (cfg!(target_feature = "avx512f"), "avx512f"),
            ] {
                if enabled {
                    features.push(name);
                }
            }
        }
        if features.is_empty() {
            None
        } else {
            Some(features.join(","))
        }
    }
}

/// Run a command and return its trimmed stdout, or `None` on any failure.
///
/// Used for platform probes that have no dependency-free API. A missing binary,
/// a non-zero exit, or non-UTF-8 output all yield `None`.
fn capture(program: &str, args: &[&str]) -> Option<String> {
    let out = Command::new(program).args(args).output().ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8(out.stdout).ok()?;
    let trimmed = text.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

#[cfg(target_os = "macos")]
mod platform {
    use super::{capture, SystemInfo};

    /// Read one `sysctl` key as a string.
    fn sysctl(key: &str) -> Option<String> {
        capture("sysctl", &["-n", key])
    }

    /// Read one `sysctl` key as an unsigned integer.
    fn sysctl_u64(key: &str) -> Option<u64> {
        sysctl(key)?.parse().ok()
    }

    pub fn fill(info: &mut SystemInfo) {
        let model = sysctl("machdep.cpu.brand_string");
        // Apple silicon exposes no vendor key; infer it from the SoC name.
        let vendor = sysctl("machdep.cpu.vendor").or_else(|| {
            model
                .as_deref()
                .filter(|m| m.starts_with("Apple"))
                .map(|_| "Apple".to_string())
        });
        info.cpu_model = model;
        info.cpu_vendor = vendor;
        info.physical_cores = sysctl_u64("hw.physicalcpu").map(|v| v as usize);
        info.l1d_bytes = sysctl_u64("hw.l1dcachesize");
        info.l2_bytes = sysctl_u64("hw.l2cachesize");
        info.l3_bytes = sysctl_u64("hw.l3cachesize");
        info.cache_line_bytes = sysctl_u64("hw.cachelinesize");
        info.memory_bytes = sysctl_u64("hw.memsize");
        info.os_version = capture("sw_vers", &["-productVersion"])
            .map(|v| format!("macOS {v}"))
            .or_else(|| sysctl("kern.osrelease"));

        // Apple silicon reports heterogeneous cores as "performance levels",
        // ordered fastest first. Intel Macs have no such keys, so both stay
        // `None` and `describe` falls back to the physical/logical split.
        if let Some(levels) = sysctl_u64("hw.nperflevels") {
            if levels >= 2 {
                info.performance_cores = sysctl_u64("hw.perflevel0.logicalcpu").map(|v| v as usize);
                info.efficiency_cores = sysctl_u64("hw.perflevel1.logicalcpu").map(|v| v as usize);
                // On Apple silicon `hw.l2cachesize` reports only the first
                // performance level; record it explicitly for clarity.
                if let Some(l2) = sysctl_u64("hw.perflevel0.l2cachesize") {
                    info.l2_bytes = Some(l2);
                }
            }
        }
    }
}

#[cfg(target_os = "linux")]
mod platform {
    use super::{capture, SystemInfo};
    use std::collections::BTreeSet;
    use std::fs;

    /// First value for `key` in `/proc/cpuinfo`.
    fn cpuinfo_field(text: &str, key: &str) -> Option<String> {
        text.lines()
            .find_map(|line| line.split_once(':').filter(|(k, _)| k.trim() == key))
            .map(|(_, v)| v.trim().to_string())
            .filter(|v| !v.is_empty())
    }

    /// Parse a cache size written as `"512K"` or `"32M"` into bytes.
    fn parse_size(raw: &str) -> Option<u64> {
        let raw = raw.trim();
        let (digits, scale) = match raw.chars().last()? {
            'K' | 'k' => (&raw[..raw.len() - 1], 1024),
            'M' | 'm' => (&raw[..raw.len() - 1], 1024 * 1024),
            'G' | 'g' => (&raw[..raw.len() - 1], 1024 * 1024 * 1024),
            _ => (raw, 1),
        };
        digits.trim().parse::<u64>().ok().map(|v| v * scale)
    }

    /// Largest cache of `level` and `kind` reported under `/sys`.
    fn sys_cache(level: u32, kind: &str) -> Option<u64> {
        let mut best = None;
        for cpu in 0..256 {
            let base = format!("/sys/devices/system/cpu/cpu{cpu}/cache");
            if !std::path::Path::new(&base).exists() {
                // CPU indices are contiguous from 0; the first gap ends the scan.
                if cpu == 0 {
                    return None;
                }
                break;
            }
            for index in 0..8 {
                let dir = format!("{base}/index{index}");
                let lvl = fs::read_to_string(format!("{dir}/level")).ok();
                let typ = fs::read_to_string(format!("{dir}/type")).ok();
                let size = fs::read_to_string(format!("{dir}/size")).ok();
                let (Some(lvl), Some(typ), Some(size)) = (lvl, typ, size) else {
                    continue;
                };
                if lvl.trim() != level.to_string() {
                    continue;
                }
                let typ = typ.trim();
                if typ != kind && typ != "Unified" {
                    continue;
                }
                if let Some(bytes) = parse_size(&size) {
                    best = Some(best.map_or(bytes, |b: u64| b.max(bytes)));
                }
            }
        }
        best
    }

    pub fn fill(info: &mut SystemInfo) {
        if let Ok(text) = fs::read_to_string("/proc/cpuinfo") {
            info.cpu_model = cpuinfo_field(&text, "model name")
                .or_else(|| cpuinfo_field(&text, "Model"))
                .or_else(|| cpuinfo_field(&text, "Hardware"))
                .or_else(|| cpuinfo_field(&text, "CPU implementer"));
            info.cpu_vendor = cpuinfo_field(&text, "vendor_id");

            // Distinct (physical id, core id) pairs give the true physical core
            // count on SMT systems; arm64 kernels omit these keys entirely.
            let cores: BTreeSet<(String, String)> = text
                .split("\n\n")
                .filter_map(|block| {
                    Some((
                        cpuinfo_field(block, "physical id")?,
                        cpuinfo_field(block, "core id")?,
                    ))
                })
                .collect();
            if !cores.is_empty() {
                info.physical_cores = Some(cores.len());
            }
        }

        info.l1d_bytes = sys_cache(1, "Data");
        info.l2_bytes = sys_cache(2, "Unified");
        info.l3_bytes = sys_cache(3, "Unified");
        info.cache_line_bytes =
            fs::read_to_string("/sys/devices/system/cpu/cpu0/cache/index0/coherency_line_size")
                .ok()
                .and_then(|v| v.trim().parse().ok());

        if let Ok(text) = fs::read_to_string("/proc/meminfo") {
            info.memory_bytes = text
                .lines()
                .find(|l| l.starts_with("MemTotal:"))
                .and_then(|l| l.split_whitespace().nth(1))
                .and_then(|v| v.parse::<u64>().ok())
                .map(|kb| kb * 1024);
        }

        info.os_version = fs::read_to_string("/etc/os-release")
            .ok()
            .and_then(|text| {
                text.lines()
                    .find_map(|l| l.strip_prefix("PRETTY_NAME="))
                    .map(|v| v.trim_matches('"').to_string())
            })
            .or_else(|| capture("uname", &["-r"]));
    }
}

#[cfg(target_os = "windows")]
mod platform {
    use super::SystemInfo;

    pub fn fill(info: &mut SystemInfo) {
        // These are set by the OS for every process, so no API call is needed.
        info.cpu_model = std::env::var("PROCESSOR_IDENTIFIER").ok();
        info.cpu_vendor = std::env::var("PROCESSOR_IDENTIFIER")
            .ok()
            .and_then(|id| id.split_whitespace().last().map(str::to_string));
        info.os_version = std::env::var("OS").ok();
    }
}

#[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
mod platform {
    use super::SystemInfo;

    /// Unknown platform: `logical_cores`, `os`, and `target` are already set
    /// from `std`, and everything else stays `None`.
    pub fn fill(_info: &mut SystemInfo) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detection_never_panics_and_reports_cores() {
        let info = SystemInfo::detect();
        assert!(info.logical_cores >= 1);
        assert!(!info.os.is_empty());
        assert!(!info.target.is_empty());
        assert!(info.default_threads() >= 1);
    }

    #[test]
    fn describe_is_a_single_line() {
        let text = SystemInfo::detect().describe();
        assert!(!text.is_empty());
        assert!(!text.contains('\n'), "describe must be one line: {text}");
    }

    #[test]
    fn detection_is_serialisable_and_round_trips() {
        let info = SystemInfo::detect();
        let json = serde_json::to_string(&info).expect("SystemInfo must serialise");
        let back: SystemInfo = serde_json::from_str(&json).expect("SystemInfo must deserialise");
        assert_eq!(back.logical_cores, info.logical_cores);
        assert_eq!(back.target, info.target);
    }

    #[test]
    fn timer_fields_are_populated() {
        let info = SystemInfo::detect();
        assert!(!info.timer.cycle_source.is_empty());
        assert!(info.timer.cycle_hz > 0.0);
        assert!(info.timer.resolution_ns >= 1);
    }

    #[test]
    fn physical_cores_never_exceed_logical() {
        let info = SystemInfo::detect();
        if let Some(p) = info.physical_cores {
            assert!(
                p <= info.logical_cores,
                "physical {p} > logical {}",
                info.logical_cores
            );
        }
    }

    #[test]
    fn heterogeneous_core_counts_are_consistent() {
        let info = SystemInfo::detect();
        if let (Some(p), Some(e)) = (info.performance_cores, info.efficiency_cores) {
            assert_eq!(
                p + e,
                info.logical_cores,
                "P({p}) + E({e}) should equal logical({})",
                info.logical_cores
            );
        }
    }

    #[test]
    fn capture_returns_none_for_missing_binary() {
        assert!(capture("threadstone-no-such-binary-xyz", &["--version"]).is_none());
    }
}
