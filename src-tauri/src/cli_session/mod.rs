//! Host-backed CLI agent session discovery.
//!
//! Scans vendor transcript stores on the host home directory and returns
//! metadata only. Resume commands are assembled in the renderer.

pub mod commands;
mod parse;
mod paths;
mod scan;
mod scope;
mod types;
mod walk;

pub use scan::list_cli_sessions;
pub use types::{CliSessionListArgs, CliSessionListResult};
