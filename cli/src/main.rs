use clap::{Parser, Subcommand, ValueEnum};
use threadstone_core::time::now_nanos;
use workloads::dhrystone::run_dhry;

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

#[derive(Clone, ValueEnum)]
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
    match workload {
        Workload::Dhrystone => {
            println!("Running Dhrystone …");
            let start = now_nanos();
            let score = run_dhry(10_000); // TODO: scale by `samples`
            let end = now_nanos();
            println!(
                "Dhrystone stub score {score}; elapsed {} ns (threads={threads}, samples={samples})",
                end - start
            );
        }
    }
}
