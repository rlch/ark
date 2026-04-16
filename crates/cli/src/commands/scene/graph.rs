//! `ark scene graph` — render attribution tree.
//!
//! T-12.5 (cavekit-scene R13). Shows extensions, plugins, reactions,
//! keybinds, intents — each leaf tagged with origin file:line.

use std::path::PathBuf;

use clap::Args;

use crate::ctx::Ctx;
use crate::error::CliError;

/// Arguments for `ark scene graph`.
#[derive(Debug, Args)]
pub struct GraphArgs {
    /// Path to a scene file. Graphs the default scene when omitted.
    #[arg(value_name = "PATH")]
    pub path: Option<PathBuf>,

    /// Output format: `text` (ASCII tree) or `json` (for scripts + future lsp).
    #[arg(long, default_value = "text", value_parser = ["text", "json"])]
    pub format: String,
}

pub fn run(_args: GraphArgs, _ctx: &Ctx) -> Result<(), CliError> {
    Err(CliError::NotYetWired { subcommand: "scene graph", task: "T-12.5" })
}
