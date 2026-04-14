//! `ark-cli` library crate.
//!
//! Houses helpers reused across the `ark` subcommands. Kept thin so the
//! binary target (`src/main.rs`) stays focused on clap plumbing.
//!
//! See cavekit-cli.md for the CLI surface spec.

pub mod id_resolver;

pub use id_resolver::{ResolveError, list_agent_ids, resolve_agent_id};
