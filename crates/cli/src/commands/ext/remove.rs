//! `ark ext remove` — uninstall an extension.
//!
//! T-12.10 (cavekit-scene R13). Removes
//! `${XDG_DATA_HOME}/ark/extensions/<name>/`.

use clap::Args;

use crate::ctx::Ctx;
use crate::error::CliError;

/// Arguments for `ark ext remove`.
#[derive(Debug, Args)]
pub struct RemoveArgs {
    /// Name of the extension to remove.
    #[arg(required = true, value_name = "NAME")]
    pub name: String,
}

pub fn run(_args: RemoveArgs, _ctx: &Ctx) -> Result<(), CliError> {
    Err(CliError::NotYetWired { subcommand: "ext remove", task: "T-12.10" })
}
