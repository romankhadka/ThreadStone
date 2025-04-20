use clap::{Parser, Subcommand, ValueEnum};
use num_cpus;
use workloads::dhrystone::run_dhry;
use serde::{Serialize, Deserialize};
use rayon::prelude::*;
use std::{fs, process, path::PathBuf};
use reqwest::blocking::Client;
use reqwest::StatusCode;
use schemars::{schema_for, JsonSchema};
use jsonschema::JSONSchema;

// Include our signing module
mod signing;

/// Number of Dhrystone iterations per sample; must fit in a u32
const ITERATIONS_PER_SAMPLE: u32 = 50_000;

#[derive(Serialize, Deserialize, JsonSchema)]
struct BenchmarkResult {
    workload: String,
    threads: usize,
    samples: u32,
    iterations_per_sample: u32,
    values: Vec<f64>,      // dhrystones/sec per sample
    average: f64,
    min: f64,
    max: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    sig: Option<String>,
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

fn die(msg: &str) -> ! {
    eprintln!("❌ {msg}");
    std::process::exit(1);
}

fn main() {
    let cli = Cli::parse();

    match cli.command {
        Commands::Run { workload, threads, samples, output } => {
            let mut result = run_workload(workload, threads, samples);

            // Generate JSON without signature first
            let json = serde_json::to_string_pretty(&result).unwrap();

            // Optional signing if environment variable is set
            if let Ok(key_path) = std::env::var("THREADSTONE_PRIVKEY") {
                let raw = match fs::read(&key_path) {
                    Ok(data) => data,
                    Err(e) => {
                        eprintln!("Failed to read private key file: {}", e);
                        process::exit(1);
                    }
                };
                
                // Sign the JSON and update the result
                let sig = signing::sign(json.as_bytes(), &raw);
                result.sig = Some(sig);
                
                // Re-serialize with signature
                let signed_json = serde_json::to_string_pretty(&result).unwrap();
                
                if let Some(path) = output {
                    let path_copy = path.clone(); // Clone to avoid move
                    fs::write(path, &signed_json).unwrap_or_else(|e| {
                        eprintln!("Failed to write output file: {}", e);
                        process::exit(1);
                    });
                    println!("✅ Signed result written to {}", path_copy.display());
                } else {
                    println!("{}", signed_json);
                }
            } else {
                // Use the original unsigned JSON
                if let Some(path) = output {
                    let path_copy = path.clone(); // Clone to avoid move
                    fs::write(path, &json).unwrap_or_else(|e| {
                        eprintln!("Failed to write output file: {}", e);
                        process::exit(1);
                    });
                    println!("✅ Unsigned result written to {}", path_copy.display());
                } else {
                    println!("{}", json);
                }
            }
        }

        Commands::Verify { file } => {
            // Read the file
            let text = fs::read_to_string(&file).unwrap_or_else(|e| {
                eprintln!("Failed to read {}: {}", file.display(), e);
                process::exit(1)
            });
            
            // Parse as BenchmarkResult
            let mut result: BenchmarkResult = serde_json::from_str(&text).unwrap_or_else(|e| {
                eprintln!("Invalid JSON in {}: {}", file.display(), e);
                process::exit(1)
            });

            // Extract signature if present
            let sig = result.sig.take().unwrap_or_default();

            // Generate schema and validate
            let schema = schema_for!(BenchmarkResult);
            let schema_value = serde_json::to_value(&schema).unwrap();
            let compiled = JSONSchema::compile(&schema_value).unwrap_or_else(|e| {
                eprintln!("Schema compilation error: {}", e);
                process::exit(1)
            });

            // Validate against schema
            let json_value = serde_json::to_value(&result).unwrap();
            if let Err(errors) = compiled.validate(&json_value) {
                eprintln!("❌ {} failed schema validation:", file.display());
                for error in errors {
                    eprintln!("  - {}", error);
                }
                process::exit(1);
            }

            // Verify signature if present
            if !sig.is_empty() {
                let data = serde_json::to_string_pretty(&result).unwrap();
                let pubkey = include_bytes!("../keys/threadstone.pub");
                if !signing::verify(data.as_bytes(), &sig, pubkey) {
                    eprintln!("❌ Signature verification failed");
                    process::exit(1);
                }
                println!("✅ {} is valid (signature verified)", file.display());
            } else {
                println!("✅ {} is valid (no signature to verify)", file.display());
            }
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
fn run_workload(workload: Workload, threads: usize, samples: u32) -> BenchmarkResult {
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

    BenchmarkResult {
        workload: format!("{:?}", workload),
        threads: effective_threads,
        samples,
        iterations_per_sample: ITERATIONS_PER_SAMPLE,
        values,
        average,
        min,
        max,
        sig: None,
    }
}
