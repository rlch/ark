//! `ark ext update` — re-fetch an extension from its install source.
//!
//! T-12.10 (cavekit-scene R13). Re-fetches from the `.ark-install`
//! source annotation; re-prompts for new caps if version-bumped.

use clap::Args;

use crate::ctx::Ctx;
use crate::error::CliError;

/// Arguments for `ark ext update`.
#[derive(Debug, Args)]
pub struct UpdateArgs {
    /// Extension to update. Updates all when omitted.
    #[arg(value_name = "NAME")]
    pub name: Option<String>,
}

pub fn run(_args: UpdateArgs, _ctx: &Ctx) -> Result<(), CliError> {
    Err(CliError::NotYetWired { subcommand: "ext update", task: "T-12.10" })
}
