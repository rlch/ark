//! `ark ext list` — T-10.9 stub.
//!
//! Full implementation lands in T-10.9 (same tier, next commit). This
//! module is pinned here so the `ark ext` subcommand tree compiles in
//! the T-10.8 commit without gaps.

use clap::Args;

use crate::ctx::Ctx;
use crate::error::CliError;

/// Arguments for `ark ext list` — stub.
#[derive(Debug, Args)]
#[command(about = "List installed ark extensions (T-10.9)")]
pub struct ListArgs {}

/// Stub run handler — returns `NotYetWired` until T-10.9.
pub fn run(_args: ListArgs, _ctx: &Ctx) -> Result<(), CliError> {
    Err(CliError::not_yet_wired("ext list", "T-10.9"))
}
