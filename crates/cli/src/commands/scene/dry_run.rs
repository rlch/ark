//! `ark scene dry-run` — simulate one event fire against the current scene.
//!
//! T-12.4 (cavekit-scene R13). Prints resolved op list per matching
//! reaction without side effects. Uses same reaction registry + CEL eval
//! as runtime.

use clap::Args;

use crate::ctx::Ctx;
use crate::error::CliError;

/// Arguments for `ark scene dry-run`.
#[derive(Debug, Args)]
pub struct DryRunArgs {
    /// Event selector to simulate (e.g. `Started`, `UserEvent:ark.picker.accept`).
    #[arg(long, required = true)]
    pub event: String,

    /// Optional JSON payload for the simulated event.
    #[arg(long)]
    pub payload: Option<String>,
}

pub fn run(_args: DryRunArgs, _ctx: &Ctx) -> Result<(), CliError> {
    Err(CliError::NotYetWired { subcommand: "scene dry-run", task: "T-12.4" })
}
