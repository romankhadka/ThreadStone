//! Measurement engine for the ThreadStone CPU benchmark suite.
//!
//! This crate knows how to *measure*; it contains no workloads. A workload
//! implements [`Kernel`] and this crate handles everything around it: choosing
//! an iteration count, starting threads in lockstep, rejecting disturbed
//! samples, describing the machine, and scoring the result.
//!
//! # The shape of a run
//!
//! ```no_run
//! use threadstone_core::{suite, SuiteConfig};
//! # use threadstone_core::Kernel;
//! # fn kernels() -> Vec<Box<dyn Kernel>> { Vec::new() }
//!
//! struct Quiet;
//! impl threadstone_core::runner::Observer for Quiet {}
//! impl suite::SuiteObserver for Quiet {}
//!
//! let report = suite::run(&kernels(), SuiteConfig::default(), "2.0.0", &Quiet);
//! println!("{}", serde_json::to_string_pretty(&report).unwrap());
//! ```
//!
//! # Design commitments
//!
//! * **Nothing unmeasured inside a measurement window.** Allocation, page
//!   faulting, and thread spawning all happen before the clock starts.
//! * **Every number carries its uncertainty.** A result says how variable it
//!   was and whether it should be believed — see [`stats::Stability`].
//! * **Every number carries its provenance.** See [`sysinfo::SystemInfo`].
//! * **State what cannot be measured well.** Workloads whose multi-threaded
//!   figure would mislead are marked [`kernel::Scaling::SingleThreadOnly`] and
//!   excluded, rather than reported with a caveat nobody reads.

#![warn(missing_docs)]

pub mod kernel;
pub mod report;
pub mod runner;
pub mod score;
pub mod stats;
pub mod suite;
pub mod sysinfo;
pub mod time;

pub use kernel::{Footprint, Kernel, KernelInfo, KernelState, Scaling, SetupCtx, Unit};
pub use report::{Report, WorkloadReport};
pub use runner::{Measurement, RunConfig, RunError};
pub use score::ScoreCard;
pub use stats::{Stability, Summary};
pub use suite::{SuiteConfig, SuiteObserver};
pub use sysinfo::SystemInfo;

/// Version of this crate, for stamping into result documents.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
