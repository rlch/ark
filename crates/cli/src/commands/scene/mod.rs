//! `ark scene` — scene management subcommands.
//!
//! T-12.1 (cavekit-scene R13). Houses the scene lifecycle commands:
//!
//! * [`check`]       — validate scene KDL files
//! * [`fmt`]         — auto-format scene KDL files
//! * [`schema_dump`] — emit scene-grammar schema from facet SHAPE
//! * [`dry_run`]     — simulate event dispatch without side-effects
//! * [`graph`]       — render attribution tree (extensions, plugins, reactions, keybinds)
//! * [`explain`]     — trace resolution of a specific ref (intent, keybind, plugin, reaction, ext)
//! * [`reload`]      — hot-reload scene via supervisor control socket

use clap::{Args, Subcommand};

use crate::ctx::Ctx;
use crate::error::CliError;

pub mod check;
pub mod dry_run;
pub mod explain;
pub mod fmt;
pub mod graph;
pub mod reload;
pub mod schema_dump;

/// Top-level `ark scene` flags + subcommand dispatch.
#[derive(Debug, Args)]
#[command(
    about = "Manage and inspect scene files",
    long_about = "Manage, validate, and inspect scene files.\n\
                  \n\
                  Examples:\n  \
                  ark scene check\n  \
                  ark scene fmt --check\n  \
                  ark scene dry-run --event 'Started'\n  \
                  ark scene graph\n  \
                  ark scene explain intent:ark.core.close_tab\n  \
                  ark scene schema-dump\n  \
                  ark scene reload"
)]
pub struct SceneArgs {
    /// Which `scene` subcommand to run.
    #[command(subcommand)]
    pub command: SceneCommand,
}

/// Subcommands of `ark scene`.
#[derive(Debug, Subcommand)]
pub enum SceneCommand {
    /// Validate scene KDL files (parse + resolve + CEL-compile + template-check).
    Check(check::CheckArgs),
    /// Canonical-format scene files (idempotent).
    Fmt(fmt::FmtArgs),
    /// Emit scene-grammar schema from facet SHAPE reflection.
    SchemaDump(schema_dump::SchemaDumpArgs),
    /// Simulate one event fire against the current scene; print matching ops.
    DryRun(dry_run::DryRunArgs),
    /// Render attribution tree of extensions, plugins, reactions, keybinds, intents.
    Graph(graph::GraphArgs),
    /// Trace resolution of a ref (intent, keybind, plugin, reaction, ext).
    Explain(explain::ExplainArgs),
    /// Hot-reload the active scene via supervisor control socket.
    Reload(reload::ReloadArgs),
}

/// Dispatch a `scene` subcommand through its handler module.
pub fn run(args: SceneArgs, ctx: &Ctx) -> Result<(), CliError> {
    match args.command {
        SceneCommand::Check(a) => check::run(a, ctx),
        SceneCommand::Fmt(a) => fmt::run(a, ctx),
        SceneCommand::SchemaDump(a) => schema_dump::run(a, ctx),
        SceneCommand::DryRun(a) => dry_run::run(a, ctx),
        SceneCommand::Graph(a) => graph::run(a, ctx),
        SceneCommand::Explain(a) => explain::run(a, ctx),
        SceneCommand::Reload(a) => reload::run(a, ctx),
    }
}
