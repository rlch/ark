//! Top-level subcommand modules.
//!
//! Each module defines a `*Args` struct (via clap-derive) plus a `run`
//! stub that T-084 leaves as "not yet wired". Wiring tasks:
//!
//! - T-087 → [`spawn`]
//! - T-088 → [`list`]
//! - T-089 → [`kill`]
//! - T-091 → [`doctor`]
//! - T-090 → [`config`]
//! - T-092 → [`pane`]
//!
//! See cavekit-cli.md R2-R7 for the per-subcommand flag specs.

pub mod config;
pub mod doctor;
pub mod ext;
pub mod kill;
pub mod list;
pub mod pane;
pub mod scene;
pub mod spawn;

use clap::Subcommand;

use crate::ctx::Ctx;
use crate::error::CliError;

/// The user-facing top-level subcommands.
///
/// Exactly six variants — matches cavekit-cli R1. No `status`, `logs`,
/// `gc`, `plugin install`, or `attach` — all folded elsewhere (see R1).
#[derive(Debug, Subcommand)]
pub enum Commands {
    #[command(alias = "new")]
    Spawn(spawn::SpawnArgs),
    List(list::ListArgs),
    Kill(kill::KillArgs),
    Doctor(doctor::DoctorArgs),
    Config(config::ConfigArgs),
    Pane(pane::PaneArgs),
    /// Inspect, list, and show info for ark extensions.
    Ext(ext::ExtArgs),
    /// Manage and inspect scene files.
    Scene(scene::SceneArgs),
}

impl Commands {
    /// Dispatch to the matching subcommand's `run` stub.
    pub fn run(self, ctx: &Ctx) -> Result<(), CliError> {
        match self {
            Commands::Spawn(args) => spawn::run(args, ctx),
            Commands::List(args) => list::run(args, ctx),
            Commands::Kill(args) => kill::run(args, ctx),
            Commands::Doctor(args) => doctor::run(args, ctx),
            Commands::Config(args) => config::run(args, ctx),
            Commands::Pane(args) => pane::run(args, ctx),
            Commands::Ext(args) => ext::run(args, ctx),
            Commands::Scene(args) => scene::run(args, ctx),
        }
    }
}
