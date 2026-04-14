//! Zellij multiplexer backend for ark.
//!
//! Implements the `Multiplexer` trait (from `ark-core`) on top of the zellij
//! terminal multiplexer. See `cavekit-mux-zellij.md` for the full spec.
//!
//! Modules:
//! - [`executor`] — `CommandExecutor` abstraction over `tokio::process`.
//! - [`layout_resolver`] — shipped/user KDL layout resolution.
//! - [`mux`] — `ZellijMux` itself (R1–R4, R6).

pub mod executor;
pub mod layout_resolver;
pub mod mux;

pub use executor::{CommandExecutor, CommandOutput, RealExecutor, StubExecutor};
pub use layout_resolver::{LayoutResolveError, LayoutResolver, LayoutSource, SHIPPED_LAYOUTS};
pub use mux::{MIN_ZELLIJ_VERSION, PIPE_TARGET_PICKER, PIPE_TARGET_STATUS, ZellijMux};
