//! `ark ext add` — install an extension from a source.
//!
//! T-12.9 (cavekit-scene R13). Sources: `path:<dir>` (copy),
//! `url:<https-tarball>` (download + extract), `github:<user>/<repo>[@<ref>]`
//! (shallow clone). Install target: `${XDG_DATA_HOME}/ark/extensions/<name>/`.

use clap::Args;

use crate::ctx::Ctx;
use crate::error::CliError;

/// Arguments for `ark ext add`.
#[derive(Debug, Args)]
pub struct AddArgs {
    /// Source specifier: `path:./local`, `github:user/repo@tag`,
    /// or `url:https://example.com/ext.tar.gz`.
    #[arg(required = true, value_name = "SOURCE")]
    pub source: String,

    /// Skip confirmation prompt (for CI).
    #[arg(long = "accept-all")]
    pub accept_all: bool,
}

pub fn run(_args: AddArgs, _ctx: &Ctx) -> Result<(), CliError> {
    Err(CliError::NotYetWired { subcommand: "ext add", task: "T-12.9" })
}
