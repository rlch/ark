//! `ark scene reload` — hot-reload via supervisor control socket.
//!
//! T-12.7 (cavekit-scene R13, R14). Sends `ReloadScene` message to
//! supervisor via control socket (cavekit-hook-ipc R1); handler invokes
//! T-11.1. Reuses existing IPC path — no new socket architecture.

use clap::Args;

use crate::ctx::Ctx;
use crate::error::CliError;

/// Arguments for `ark scene reload`.
#[derive(Debug, Args)]
pub struct ReloadArgs {
    /// Agent session to reload. Reloads the default session when omitted.
    #[arg(long)]
    pub session: Option<String>,
}

pub fn run(_args: ReloadArgs, _ctx: &Ctx) -> Result<(), CliError> {
    Err(CliError::NotYetWired { subcommand: "scene reload", task: "T-12.7" })
}
