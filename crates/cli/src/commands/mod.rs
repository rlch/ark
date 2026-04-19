//! Top-level subcommand modules.
//!
//! Each module defines a `*Args` struct (via clap-derive) plus a `run`
//! stub. Wiring tasks:
//!
//! - T-088 → [`list`]
//! - T-089 → [`kill`]
//! - T-091 → [`doctor`]
//! - T-090 → [`config`]
//! - T-092 → [`pane`]
//!
//! T-115 removed the `spawn` subcommand — bare `ark` now handles
//! session launch via [`launch`]. Shared zellij helpers live in
//! [`session`].
//!
//! See cavekit-cli.md R2-R7 for the per-subcommand flag specs.

pub mod bus;
pub mod config;
pub mod doctor;
pub mod ext;
pub mod kill;
pub mod launch;
pub mod list;
pub mod pane;
pub mod scene;
pub mod session;

use clap::Subcommand;

use crate::ctx::Ctx;
use crate::error::CliError;

/// The user-facing top-level subcommands.
///
/// T-115 removed `Spawn` — bare `ark` handles session launch.
#[derive(Debug, Subcommand)]
pub enum Commands {
    List(list::ListArgs),
    Kill(kill::KillArgs),
    Doctor(doctor::DoctorArgs),
    Config(config::ConfigArgs),
    Pane(pane::PaneArgs),
    /// Inspect, list, and show info for ark extensions.
    Ext(ext::ExtArgs),
    /// Manage and inspect scene files.
    Scene(scene::SceneArgs),
    /// ark-bus bridge verbs (hidden command-pane dispatch).
    ///
    /// The zellij-side `ark-bus` wasm plugin cannot open unix sockets
    /// directly (wasi sandbox), so it spawns a hidden command pane
    /// running `ark bus intent` / `ark bus emit` which bridges to the
    /// supervisor control socket.
    Bus(bus::BusArgs),
}

impl Commands {
    /// Dispatch to the matching subcommand's `run` stub.
    pub fn run(self, ctx: &Ctx) -> Result<(), CliError> {
        match self {
            Commands::List(args) => list::run(args, ctx),
            Commands::Kill(args) => kill::run(args, ctx),
            Commands::Doctor(args) => doctor::run(args, ctx),
            Commands::Config(args) => config::run(args, ctx),
            Commands::Pane(args) => pane::run(args, ctx),
            Commands::Ext(args) => ext::run(args, ctx),
            Commands::Scene(args) => scene::run(args, ctx),
            Commands::Bus(args) => bus::run(args, ctx),
        }
    }
}
