//! `ark scene fmt` — canonical-format scene files.
//!
//! T-12.3 (cavekit-scene R13). Ark-specific node ordering:
//! extends/include/use → layout → plugin → on → keybind. Idempotent.

use std::path::PathBuf;

use clap::Args;

use crate::ctx::Ctx;
use crate::error::CliError;

/// Arguments for `ark scene fmt`.
#[derive(Debug, Args)]
pub struct FmtArgs {
    /// Path to a scene file. Formats the default scene when omitted.
    #[arg(value_name = "PATH")]
    pub path: Option<PathBuf>,

    /// Check formatting without writing. Exit 1 if changes needed.
    #[arg(long)]
    pub check: bool,
}

pub fn run(_args: FmtArgs, _ctx: &Ctx) -> Result<(), CliError> {
    Err(CliError::NotYetWired { subcommand: "scene fmt", task: "T-12.3" })
}
