//! ThreadStone — a transparent CPU benchmark suite.
//!
//! See `threadstone --help`, or the methodology notes in `docs/METHODOLOGY.md`.

#![warn(missing_docs)]

mod compare;
mod observer;
mod render;
mod signing;
mod verify;

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use clap::{Parser, Subcommand, ValueEnum};

use threadstone_core::report::Report;
use threadstone_core::{suite, SuiteConfig};

/// Boxed error, so every failure path can use `?` without a dependency.
type Failure = Box<dyn std::error::Error>;

/// Version reported by `--version` and stamped into results.
const VERSION: &str = env!("CARGO_PKG_VERSION");

/// A transparent, reproducible CPU benchmark suite.
#[derive(Parser)]
#[command(name = "threadstone", version = VERSION, about, long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Run the benchmark suite.
    Run(RunArgs),

    /// Describe the available workloads and what each one measures.
    List,

    /// Check a result file's structure and signature.
    Verify {
        /// Result file to check.
        file: PathBuf,
        /// Fail if the file carries no signature at all.
        #[arg(long)]
        require_signature: bool,
    },

    /// Compare two result files and report significant changes.
    Compare {
        /// Baseline result file.
        baseline: PathBuf,
        /// Candidate result file.
        candidate: PathBuf,
    },

    /// Re-render a saved result file.
    Report {
        /// Result file to render.
        file: PathBuf,
        /// Output format.
        #[arg(long, value_enum, default_value_t = Format::Table)]
        format: Format,
    },

    /// Measure latency across working-set sizes to map the cache hierarchy.
    Sweep {
        /// Minimum measurement time per size, in milliseconds.
        #[arg(long, default_value_t = 120)]
        min_ms: u64,
        /// Write results as JSON to this path instead of a table.
        #[arg(short, long)]
        out: Option<PathBuf>,
    },

    /// Print the JSON Schema for result files.
    Schema {
        /// Write to this path instead of stdout.
        #[arg(short, long)]
        out: Option<PathBuf>,
    },

    /// Generate an Ed25519 key pair for signing results.
    Keygen {
        /// Directory to write `threadstone.key` and `threadstone.pub` into.
        #[arg(short, long, default_value = ".")]
        dir: PathBuf,
    },
}

#[derive(clap::Args)]
struct RunArgs {
    /// Workload to run; repeat for several. Defaults to all of them.
    #[arg(short, long, value_name = "ID")]
    workload: Vec<String>,

    /// Threads for the multi-core pass. 0 uses every logical core.
    #[arg(short, long, default_value_t = 0)]
    threads: usize,

    /// Measured rounds per pass.
    #[arg(short, long, default_value_t = threadstone_core::runner::defaults::SAMPLES)]
    samples: u32,

    /// Discarded rounds before measuring.
    #[arg(long, default_value_t = threadstone_core::runner::defaults::WARMUP)]
    warmup: u32,

    /// Target duration of each measurement round, in milliseconds.
    #[arg(long, default_value_t = 250)]
    window_ms: u64,

    /// Run only the single-thread pass.
    #[arg(long, conflicts_with = "multi_only")]
    single_only: bool,

    /// Run only the multi-thread pass.
    #[arg(long, conflicts_with = "single_only")]
    multi_only: bool,

    /// Write the result document to this path.
    #[arg(short, long)]
    out: Option<PathBuf>,

    /// Format for stdout.
    #[arg(long, value_enum, default_value_t = Format::Table)]
    format: Format,

    /// Sign the result with this PKCS#8 Ed25519 private key.
    #[arg(long, value_name = "PATH")]
    sign_key: Option<PathBuf>,

    /// Suppress progress output.
    #[arg(short, long)]
    quiet: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum Format {
    /// Aligned terminal table.
    Table,
    /// The full result document as JSON.
    Json,
    /// Markdown table, for pasting into issues and READMEs.
    Markdown,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match dispatch(cli.command) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("threadstone: {error}");
            ExitCode::FAILURE
        }
    }
}

fn dispatch(command: Command) -> Result<(), Failure> {
    match command {
        Command::Run(args) => run(args),
        Command::List => list(),
        Command::Verify {
            file,
            require_signature,
        } => verify_file(&file, require_signature),
        Command::Compare {
            baseline,
            candidate,
        } => compare_files(&baseline, &candidate),
        Command::Report { file, format } => report_file(&file, format),
        Command::Sweep { min_ms, out } => sweep(min_ms, out.as_deref()),
        Command::Schema { out } => schema(out.as_deref()),
        Command::Keygen { dir } => keygen(&dir),
    }
}

fn run(args: RunArgs) -> Result<(), Failure> {
    let kernels = select_workloads(&args.workload)?;

    let cfg = SuiteConfig {
        threads: args.threads,
        samples: args.samples,
        warmup: args.warmup,
        window: Duration::from_millis(args.window_ms),
        single_thread: !args.multi_only,
        multi_thread: !args.single_only,
    };
    if cfg.samples == 0 {
        return Err("--samples must be at least 1".into());
    }
    if args.window_ms == 0 {
        return Err("--window-ms must be at least 1".into());
    }

    // Progress goes to stderr so that `--format json > file` stays clean.
    let quiet = args.quiet || args.format == Format::Json;
    let progress = observer::Progress::new(quiet);
    let mut report = suite::run(&kernels, cfg, VERSION, &progress);
    progress.finish();

    if let Some(key_path) = &args.sign_key {
        sign_report(&mut report, key_path)?;
    }

    if let Some(path) = &args.out {
        let json = serde_json::to_string_pretty(&report)?;
        write_file(path, json.as_bytes())?;
        eprintln!("wrote {}", path.display());
    }

    print!("{}", format_report(&report, args.format)?);
    Ok(())
}

/// Resolve requested workload ids, or return all of them.
fn select_workloads(
    requested: &[String],
) -> Result<Vec<Box<dyn threadstone_core::Kernel>>, Failure> {
    if requested.is_empty() {
        return Ok(threadstone_workloads::all());
    }
    let mut kernels = Vec::with_capacity(requested.len());
    for id in requested {
        let kernel = threadstone_workloads::by_id(id).ok_or_else(|| {
            format!(
                "unknown workload '{id}'; available: {}",
                threadstone_workloads::ids().join(", ")
            )
        })?;
        kernels.push(kernel);
    }
    Ok(kernels)
}

fn sign_report(report: &mut Report, key_path: &Path) -> Result<(), Failure> {
    let pkcs8 = std::fs::read(key_path)
        .map_err(|e| format!("cannot read signing key {}: {e}", key_path.display()))?;
    let message = report.signing_bytes()?;
    report.signature = Some(signing::sign(&message, &pkcs8)?);
    Ok(())
}

fn format_report(report: &Report, format: Format) -> Result<String, Failure> {
    Ok(match format {
        Format::Table => render::table(report, render::Color::detect()),
        Format::Json => format!("{}\n", serde_json::to_string_pretty(report)?),
        Format::Markdown => render::markdown(report),
    })
}

fn list() -> Result<(), Failure> {
    println!("ThreadStone {VERSION} workloads\n");
    for kernel in threadstone_workloads::all() {
        let info = kernel.info();
        println!("  {:<10} {}", info.id, info.name);
        println!("             {}", info.summary);
        println!(
            "             unit {} · reference {} · {} · {}",
            info.unit.label(),
            render::si(info.reference),
            match info.footprint {
                threadstone_core::Footprint::PerThread => "working set per thread",
                threadstone_core::Footprint::Partitioned => "working set split across threads",
            },
            match info.scaling {
                threadstone_core::Scaling::Scales => "scales to all cores",
                threadstone_core::Scaling::SingleThreadOnly =>
                    "single-thread only (excluded from the multi-core score)",
            },
        );
        println!();
    }
    println!(
        "Reference values define {}; a machine matching it scores 1000.",
        threadstone_core::score::REFERENCE_NAME
    );
    Ok(())
}

fn verify_file(path: &Path, require_signature: bool) -> Result<(), Failure> {
    let text = read_file(path)?;
    let outcome = verify::check(&text, require_signature);
    print!("{}", verify::render(&outcome, path));
    if outcome.is_ok() {
        Ok(())
    } else {
        Err(format!("{} failed verification", path.display()).into())
    }
}

fn compare_files(baseline: &Path, candidate: &Path) -> Result<(), Failure> {
    let a: Report = serde_json::from_str(&read_file(baseline)?)
        .map_err(|e| format!("{}: {e}", baseline.display()))?;
    let b: Report = serde_json::from_str(&read_file(candidate)?)
        .map_err(|e| format!("{}: {e}", candidate.display()))?;
    let comparison = compare::compare(&a, &b);
    print!(
        "{}",
        compare::render(
            &comparison,
            &baseline.display().to_string(),
            &candidate.display().to_string()
        )
    );
    Ok(())
}

fn report_file(path: &Path, format: Format) -> Result<(), Failure> {
    let report: Report =
        serde_json::from_str(&read_file(path)?).map_err(|e| format!("{}: {e}", path.display()))?;
    print!("{}", format_report(&report, format)?);
    Ok(())
}

fn sweep(min_ms: u64, out: Option<&Path>) -> Result<(), Failure> {
    use threadstone_workloads::latency;

    let sizes = latency::default_sweep_sizes();
    eprintln!(
        "Measuring pointer-chase latency across {} working-set sizes…",
        sizes.len()
    );

    let points = latency::sweep(&sizes, min_ms);

    if let Some(path) = out {
        let json: Vec<serde_json::Value> = points
            .iter()
            .map(|p| serde_json::json!({ "bytes": p.bytes, "latency_ns": p.latency_ns }))
            .collect();
        write_file(path, serde_json::to_string_pretty(&json)?.as_bytes())?;
        eprintln!("wrote {}", path.display());
        return Ok(());
    }

    println!("{:>12}  {:>10}", "working set", "latency");
    for point in &points {
        println!(
            "{:>12}  {:>7.1} ns",
            human_bytes(point.bytes),
            point.latency_ns
        );
    }
    println!("\nPlateaus mark cache levels; each step up is a level boundary.");
    Ok(())
}

fn human_bytes(bytes: usize) -> String {
    const UNITS: [&str; 4] = ["B", "KiB", "MiB", "GiB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    format!("{value:.0} {}", UNITS[unit])
}

fn schema(out: Option<&Path>) -> Result<(), Failure> {
    let schema = schemars::schema_for!(Report);
    let json = serde_json::to_string_pretty(&schema)?;
    match out {
        Some(path) => {
            write_file(path, json.as_bytes())?;
            eprintln!("wrote {}", path.display());
        }
        None => println!("{json}"),
    }
    Ok(())
}

fn keygen(dir: &Path) -> Result<(), Failure> {
    let key = signing::generate()?;
    let private = dir.join("threadstone.key");
    let public = dir.join("threadstone.pub");

    if private.exists() {
        return Err(format!(
            "{} already exists; refusing to overwrite a private key",
            private.display()
        )
        .into());
    }

    write_file(&private, &key.pkcs8)?;
    restrict_permissions(&private)?;
    write_file(&public, &key.public)?;

    println!("private key  {}", private.display());
    println!("public key   {}", public.display());
    println!("\nKeep the private key out of version control. Sign a run with:");
    println!("  threadstone run --sign-key {}", private.display());
    Ok(())
}

/// Restrict a private key to owner-only access where the platform supports it.
#[cfg(unix)]
fn restrict_permissions(path: &Path) -> Result<(), Failure> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    Ok(())
}

#[cfg(not(unix))]
fn restrict_permissions(_path: &Path) -> Result<(), Failure> {
    // Windows ACL defaults already limit a user-profile file to its owner.
    Ok(())
}

fn read_file(path: &Path) -> Result<String, Failure> {
    std::fs::read_to_string(path).map_err(|e| format!("cannot read {}: {e}", path.display()).into())
}

fn write_file(path: &Path, bytes: &[u8]) -> Result<(), Failure> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("cannot create {}: {e}", parent.display()))?;
        }
    }
    let mut file =
        std::fs::File::create(path).map_err(|e| format!("cannot write {}: {e}", path.display()))?;
    file.write_all(bytes)
        .map_err(|e| format!("cannot write {}: {e}", path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn cli_definition_is_valid() {
        Cli::command().debug_assert();
    }

    #[test]
    fn workload_selection_defaults_to_everything() {
        let all = select_workloads(&[]).unwrap();
        assert_eq!(all.len(), threadstone_workloads::all().len());
    }

    #[test]
    fn workload_selection_honours_explicit_ids() {
        let picked = select_workloads(&["sgemm".into(), "stream".into()]).unwrap();
        let ids: Vec<&str> = picked.iter().map(|k| k.info().id).collect();
        assert_eq!(ids, vec!["sgemm", "stream"]);
    }

    #[test]
    fn an_unknown_workload_lists_the_valid_ones() {
        // `Box<dyn Kernel>` is not `Debug`, so unwrap the error by hand.
        let err = match select_workloads(&["nope".into()]) {
            Ok(_) => panic!("an unknown workload must not resolve"),
            Err(e) => e.to_string(),
        };
        assert!(err.contains("unknown workload 'nope'"));
        assert!(
            err.contains("dhrystone"),
            "error should list options: {err}"
        );
    }

    #[test]
    fn human_bytes_uses_binary_units() {
        assert_eq!(human_bytes(512), "512 B");
        assert_eq!(human_bytes(4096), "4 KiB");
        assert_eq!(human_bytes(1 << 20), "1 MiB");
        assert_eq!(human_bytes(256 << 20), "256 MiB");
    }
}
