use clap::{Parser, Subcommand, ValueEnum};
use num_cpus;
use threadstone_core::time::now_nanos;
use workloads::dhrystone::run_dhry;
use serde::Serialize;
use rayon::prelude::*;
use std::{fs, process, path::PathBuf};
use reqwest::blocking::Client;
use reqwest::StatusCode;
use serde_json::Value;
use schemars::{schema_for, JsonSchema};
use jsonschema::JSONSchema;

/// Number of Dhrystone iterations per sample; must fit in a u32
const ITERATIONS_PER_SAMPLE: u32 = 50_000;

#[derive(Serialize, JsonSchema)]
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

        /// Optional output file (JSON)
        #[arg(short = 'o', long)]
        output: Option<PathBuf>,
    },

    /// Verify the integrity signature of a result file
    Verify {
        /// Path to result JSON
        file: PathBuf,
    },

    /// Upload a result file to the ThreadStone server
    Upload {
        /// Path to result JSON
        file: PathBuf,

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
        Commands::Run { workload, threads, samples, output } => {
            let json = run_workload(workload, threads, samples);
            if let Some(path) = output {
                fs::write(path, &json).unwrap_or_else(|e| {
                    eprintln!("Failed to write output file: {}", e);
                    process::exit(1);
                });
            } else {
                println!("{}", json);
            }
        }

        Commands::Verify { file } => {
            // read & parse
            let text = fs::read_to_string(&file)
                .unwrap_or_else(|e| { 
                    eprintln!("Failed to read {}: {}", file.display(), e); 
                    process::exit(1) 
                });
            
            let json: Value = serde_json::from_str(&text)
                .unwrap_or_else(|e| { 
                    eprintln!("Invalid JSON in {}: {}", file.display(), e); 
                    process::exit(1) 
                });

            // compile schema
            let schema = schema_for!(BenchmarkResult);
            let schema_value = serde_json::to_value(&schema).unwrap();
            let compiled = JSONSchema::compile(&schema_value)
                .unwrap_or_else(|e| { 
                    eprintln!("Schema compilation error: {}", e); 
                    process::exit(1) 
                });

            // validate against the schema
            let validation = compiled.validate(&json);

            if let Err(errors) = validation {
                eprintln!("❌ {} failed schema validation:", file.display());
                for err in errors {
                    eprintln!("  - {}", err);
                }
                process::exit(1);
            }

            println!("✅ {} is valid against schema", file.display());
        }

        Commands::Upload { file, endpoint } => {
            // 1) read the file
            let body = fs::read_to_string(&file).unwrap_or_else(|e| {
                eprintln!("❌ failed to read {}: {}", file.display(), e);
                process::exit(1)
            });

            // 2) determine endpoint URL
            //    override with CLI flag, else use a default
            let url = endpoint
                .unwrap_or_else(|| "https://api.threadstone.dev/upload".to_string());

            // 3) POST JSON
            let client = Client::new();
            let resp = client
                .post(&url)
                .header("Content-Type", "application/json")
                .body(body)
                .send()
                    .unwrap_or_else(|e| {
                        eprintln!("❌ request error: {}", e);
                        process::exit(1)
                    });

            // 4) check status
            if resp.status() == StatusCode::OK {
                println!("✅ uploaded successfully to {}", url);
            } else {
                eprintln!(
                    "❌ upload failed: {} - {}",
                    resp.status(),
                    resp.text().unwrap_or_default()
                );
                process::exit(1);
            }
        }
    }
}

/// Runs the given workload and returns the serialized JSON string
fn run_workload(workload: Workload, threads: usize, samples: u32) -> String {
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
        (0..samples)
            .into_par_iter()
            .map(|_| {
                run_dhry(ITERATIONS_PER_SAMPLE)
            })
            .collect()
    });

    // summary stats
    let sum: f64 = values.iter().sum();
    let average = sum / (values.len() as f64);
    let min = *values.iter().min_by(|a, b| a.partial_cmp(b).unwrap()).unwrap();
    let max = *values.iter().max_by(|a, b| a.partial_cmp(b).unwrap()).unwrap();

    let result = BenchmarkResult {
        workload: format!("{:?}", workload),
        threads: effective_threads,
        samples,
        iterations_per_sample: ITERATIONS_PER_SAMPLE,
        values,
        average,
        min,
        max,
    };

    serde_json::to_string_pretty(&result).unwrap()
}
