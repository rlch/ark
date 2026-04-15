//! `ark ext info` — T-10.9 stub.
//!
//! Full implementation lands in T-10.9 (same tier, next commit). This
//! module is pinned here so the `ark ext` subcommand tree compiles in
//! the T-10.8 commit without gaps.

use clap::Args;

use crate::ctx::Ctx;
use crate::error::CliError;

/// Arguments for `ark ext info` — stub.
#[derive(Debug, Args)]
#[command(about = "Show full metadata for an installed extension (T-10.9)")]
pub struct InfoArgs {
    /// Extension name.
    pub name: String,
}

/// Stub run handler — returns `NotYetWired` until T-10.9.
pub fn run(_args: InfoArgs, _ctx: &Ctx) -> Result<(), CliError> {
    Err(CliError::not_yet_wired("ext info", "T-10.9"))
}
