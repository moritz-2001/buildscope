//! C++ build capture and evidence-based analysis. Start with `capture::record` or
//! `capture::profile`, or use `launcher::launch` and `launcher::collect` during normal
//! builds. All collectors produce the same versioned model for storage and reports.

pub mod analysis;
pub mod capture;
pub mod cli;
pub mod compiler;
pub mod git;
pub mod launcher;
pub mod model;
pub mod project;
pub mod report;
pub mod store;
pub mod util;

mod decoder;
mod worker_pool;
mod workload;

pub mod compiler_stacks;
pub mod header_history;
pub mod rebuild;
