//! `ark scene check` — full parse + resolve + validate + CEL-compile.
//!
//! T-12.2 (cavekit-scene R13). Exit 0 on green; non-zero with
//! diagnostics on any error. Emits every error, not just first.

use std::path::PathBuf;

use clap::Args;

use crate::ctx::Ctx;
use crate::error::CliError;

/// Arguments for `ark scene check`.
#[derive(Debug, Args)]
pub struct CheckArgs {
    /// Path to a scene file. Validates the default scene when omitted.
    #[arg(value_name = "PATH")]
    pub path: Option<PathBuf>,

    /// Enforce v1.0 contract (T-15.3).
    #[arg(long)]
    pub v1_strict: bool,
}

pub fn run(_args: CheckArgs, _ctx: &Ctx) -> Result<(), CliError> {
    Err(CliError::NotYetWired { subcommand: "scene check", task: "T-12.2" })
}
