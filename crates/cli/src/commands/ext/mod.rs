//! `ark ext` — extension inspection + listing subcommands.
//!
//! T-10.8, T-10.9 (cavekit-scene R13). Houses the three user-facing
//! extension commands that don't require a running session:
//!
//! * [`inspect`] — `ark ext inspect <path>` dumps a wasm cartridge's
//!   `ark.metadata` custom section as human-readable KDL. Used to
//!   verify that a cartridge was built correctly before installation.
//! * [`list`] — `ark ext list` walks every installed extension in
//!   `${XDG_DATA_HOME}/ark/extensions/` and prints a tabular summary
//!   (name, version, ark-range, source).
//! * [`info`] — `ark ext info <name>` dumps the full manifest of a
//!   single installed extension plus its `.ark-install` source
//!   annotation.
//!
//! The full `ark ext` tree (T-12.8) adds `install`, `update`, `remove`,
//! `resolve`, and `graph` — those wire in later tiers. The scaffolding
//! here is stable: each new subcommand becomes another module + enum
//! variant.

use clap::{Args, Subcommand};

use crate::ctx::Ctx;
use crate::error::CliError;

pub mod add;
pub mod info;
pub mod inspect;
pub mod list;
pub mod remove;
pub mod trust;
pub mod update;
pub mod verb;

/// Top-level `ark ext` flags + subcommand dispatch.
#[derive(Debug, Args)]
#[command(
    about = "Inspect, list, and invoke control-verbs on ark extensions",
    long_about = "Inspect and manage installed ark extensions.\n\
                  \n\
                  Examples:\n  \
                  ark ext list\n  \
                  ark ext info picker\n  \
                  ark ext inspect ./path/to/plugin.wasm\n  \
                  ark ext invoke claude-code install-hooks"
)]
pub struct ExtArgs {
    /// Which `ext` subcommand to run.
    #[command(subcommand)]
    pub command: ExtCommand,
}

/// Subcommands of `ark ext`.
#[derive(Debug, Subcommand)]
pub enum ExtCommand {
    /// Dump the `ark.metadata` custom section of a wasm cartridge as KDL.
    Inspect(inspect::InspectArgs),
    /// List every installed extension with its version + source.
    List(list::ListArgs),
    /// Show full metadata for a single installed extension.
    Info(info::InfoArgs),
    /// Install an extension from a source.
    Add(add::AddArgs),
    /// Uninstall an extension.
    Remove(remove::RemoveArgs),
    /// Re-fetch an extension from its install source.
    Update(update::UpdateArgs),
    /// Invoke a control-verb on a running extension
    /// (v0.2-backlog #4). `ark ext invoke <name> <verb> [args...]`.
    Invoke(verb::VerbArgs),
}

/// Dispatch an `ext` subcommand through its handler module.
pub fn run(args: ExtArgs, ctx: &Ctx) -> Result<(), CliError> {
    match args.command {
        ExtCommand::Inspect(a) => inspect::run(a, ctx),
        ExtCommand::List(a) => list::run(a, ctx),
        ExtCommand::Info(a) => info::run(a, ctx),
        ExtCommand::Add(a) => add::run(a, ctx),
        ExtCommand::Remove(a) => remove::run(a, ctx),
        ExtCommand::Update(a) => update::run(a, ctx),
        ExtCommand::Invoke(a) => verb::run(a, ctx),
    }
}
