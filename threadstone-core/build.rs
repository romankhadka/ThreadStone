//! Captures build-time facts that the running binary cannot rediscover.
//!
//! A result file must record how the measuring binary was compiled, because two
//! binaries built at different optimisation levels produce numbers that are not
//! comparable. The target triple, opt-level, and compiler version are known
//! only to Cargo at build time, so they are baked in here as `rustc-env`
//! variables and read back through `option_env!`.

use std::process::Command;

fn main() {
    // Cargo always sets these for a build script.
    emit("THREADSTONE_TARGET", std::env::var("TARGET").ok());
    emit("THREADSTONE_OPT_LEVEL", std::env::var("OPT_LEVEL").ok());
    emit("THREADSTONE_RUSTC_VERSION", rustc_version());
    emit("THREADSTONE_TARGET_CPU", target_cpu());

    // Without this, changing RUSTFLAGS would leave a stale `target-cpu` baked
    // into the binary.
    println!("cargo:rerun-if-env-changed=CARGO_ENCODED_RUSTFLAGS");
    println!("cargo:rerun-if-env-changed=RUSTFLAGS");
    println!("cargo:rerun-if-changed=build.rs");
}

fn emit(key: &str, value: Option<String>) {
    if let Some(value) = value {
        // Newlines would corrupt Cargo's line-oriented directive protocol.
        let sanitised = value.replace(['\n', '\r'], " ");
        println!("cargo:rustc-env={key}={sanitised}");
    }
}

fn rustc_version() -> Option<String> {
    let rustc = std::env::var("RUSTC").unwrap_or_else(|_| "rustc".to_string());
    let out = Command::new(rustc).arg("--version").output().ok()?;
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

/// Extract `-C target-cpu=...` from whichever RUSTFLAGS form Cargo provided.
///
/// `CARGO_ENCODED_RUSTFLAGS` is unit-separator delimited and authoritative when
/// present; plain `RUSTFLAGS` is whitespace delimited and used as a fallback.
/// Both spellings of the flag (`-Ctarget-cpu=x` and `-C target-cpu=x`) occur in
/// the wild, so both are handled.
fn target_cpu() -> Option<String> {
    let (raw, separator) = match std::env::var("CARGO_ENCODED_RUSTFLAGS") {
        Ok(v) => (v, '\u{1f}'),
        Err(_) => (std::env::var("RUSTFLAGS").ok()?, ' '),
    };

    let tokens: Vec<&str> = raw.split(separator).filter(|t| !t.is_empty()).collect();
    for (i, token) in tokens.iter().enumerate() {
        if let Some(value) = token.strip_prefix("-Ctarget-cpu=") {
            return Some(value.to_string());
        }
        if *token == "-C" {
            if let Some(value) = tokens
                .get(i + 1)
                .and_then(|t| t.strip_prefix("target-cpu="))
            {
                return Some(value.to_string());
            }
        }
    }
    None
}
