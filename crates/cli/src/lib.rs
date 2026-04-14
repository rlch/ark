//! `ark-cli` library crate.
//!
//! The `ark` binary target in `src/main.rs` stays small — argument
//! parsing, NO_COLOR detection, and dispatch. Everything substantive
//! lives here so it can be unit-tested.
//!
//! # Layout
//!
//! - [`cli`] — top-level [`Cli`] struct, `--version`/`--help` wiring,
//!   80-column help.
//! - [`commands`] — one module per top-level subcommand, each exposing
//!   its clap-derive args struct plus a `run` stub.
//! - [`ctx`] — [`Ctx`] passed to every subcommand handler; carries the
//!   resolved `$NO_COLOR` setting for custom formatters.
//! - [`error`] — [`CliError`] with exit-code mapping (T-085, R8).
//! - [`exit`] — [`ExitCode`] constants (T-085, cavekit-cli R8).
//! - [`id_resolver`] — T-086 territory. ID fragment resolution used by
//!   `list` and `kill`.
//!
//! See `context/kits/cavekit-cli.md` for the CLI surface spec. T-084
//! scaffolds R1 only; the subcommand handlers are wired in T-087–T-093.

pub mod cli;
pub mod commands;
pub mod ctx;
pub mod error;
pub mod exit;
pub mod id_resolver;

pub use cli::Cli;
pub use commands::Commands;
pub use ctx::{Ctx, detect_no_color, no_color_from_env};
pub use error::CliError;
pub use exit::ExitCode;
pub use id_resolver::{ResolveError, list_agent_ids, resolve_agent_id};
