use clap::{Parser, Subcommand, ValueEnum};
use num_cpus;
use threadstone_core::time::now_nanos;
use workloads::dhrystone::run_dhry;
use serde::Serialize;
use rayon::prelude::*;

/// Number of Dhrystone iterations per sample; must fit in a u32
const ITERATIONS_PER_SAMPLE: u32 = 1_000_000;

#[derive(Serialize)]
struct BenchmarkResult {
    workload: String,
    threads: usize,
    samples: u32,
    iterations_per_sample: u32,
    values: Vec<f64>,      // dhrystones/sec per sample
    average: f64,
    min: f64,
    max: f64,
}

/// ThreadStone – CPU benchmark suite
#[derive(Parser)]
#[command(author, version, about, long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Run a benchmark workload
    Run {
        /// Which workload to execute
        #[arg(short = 'w', long, value_enum)]
        workload: Workload,

        /// Number of OS threads to use (0 = all logical cores)
        #[arg(short, long, default_value_t = 0)]
        threads: usize,

        /// Number of samples to collect
        #[arg(short, long, default_value_t = 5)]
        samples: u32,
    },

    /// Verify the integrity signature of a result file
    Verify {
        /// Path to result JSON
        file: std::path::PathBuf,
    },

    /// Upload a result file to the ThreadStone server
    Upload {
        /// Path to result JSON
        file: std::path::PathBuf,

        /// Override upload endpoint
        #[arg(short, long)]
        endpoint: Option<String>,
    },
}

#[derive(Debug, Clone, ValueEnum)]
enum Workload {
    Dhrystone,
    // Placeholder for future workloads:
    // Sgemm,
    // Stream,
}

fn main() {
    let cli = Cli::parse();

    match cli.command {
        Commands::Run {
            workload,
            threads,
            samples,
        } => run_workload(workload, threads, samples),

        Commands::Verify { file } => {
            println!("Signature verification not implemented yet: {}", file.display());
        }

        Commands::Upload { file, endpoint } => {
            println!(
                "Upload not implemented yet: {} -> {:?}",
                file.display(),
                endpoint
            );
        }
    }
}

fn run_workload(workload: Workload, threads: usize, samples: u32) {
    // if user passed 0, use all logical cores
    let effective_threads = if threads == 0 {
        num_cpus::get()
    } else {
        threads
    };

    // configure Rayon thread‑pool
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(effective_threads)
        .build()
        .unwrap();

    // run samples in parallel
    let values: Vec<f64> = pool.install(|| {
        (0..samples).into_par_iter().map(|_| {
            let t0 = now_nanos();
            let dps = run_dhry(ITERATIONS_PER_SAMPLE);
            let t1 = now_nanos();
            // you could also weight by elapsed if you want
            dps
        }).collect()
    });

    // summary stats
    let sum: f64 = values.iter().sum();
    let average = sum / (values.len() as f64);
    let min = *values.iter().min_by(|a, b| a.partial_cmp(b).unwrap()).unwrap();
    let max = *values.iter().max_by(|a, b| a.partial_cmp(b).unwrap()).unwrap();

    let result = BenchmarkResult {
        workload: format!("{workload:?}"),
        threads: effective_threads,
        samples,
        iterations_per_sample: ITERATIONS_PER_SAMPLE,
        values,
        average,
        min,
        max,
    };

    // print JSON
    println!("{}", serde_json::to_string_pretty(&result).unwrap());
}