//! `ark scene explain` — trace resolution of a specific ref.
//!
//! T-12.6 (cavekit-scene R13). Refs: `intent:<name>`,
//! `keybind:<chord>`, `plugin:<name>`, `reaction:<event-selector>`,
//! `ext:<name>`. Prints "defined at <file:line>; overridden by
//! <file:line>; final resolution: <origin>".

use std::path::PathBuf;

use clap::Args;

use crate::ctx::Ctx;
use crate::error::CliError;

/// Arguments for `ark scene explain`.
#[derive(Debug, Args)]
pub struct ExplainArgs {
    /// Ref to explain. Forms: `intent:<name>`, `keybind:<chord>`,
    /// `plugin:<name>`, `reaction:<selector>`, `ext:<name>`.
    #[arg(required = true, value_name = "REF")]
    pub reference: String,

    /// Path to a scene file. Uses the default scene when omitted.
    #[arg(long, value_name = "PATH")]
    pub file: Option<PathBuf>,
}

pub fn run(_args: ExplainArgs, _ctx: &Ctx) -> Result<(), CliError> {
    Err(CliError::NotYetWired { subcommand: "scene explain", task: "T-12.6" })
}
