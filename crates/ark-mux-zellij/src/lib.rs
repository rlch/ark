//! Zellij multiplexer backend for ark.
//!
//! This crate implements the `Multiplexer` trait (from `ark-core`) on top of
//! the zellij terminal multiplexer. See `cavekit-mux-zellij.md` for the full
//! specification.
//!
//! Current progress: leaf helpers only. `ZellijMux` itself is a placeholder
//! until T-025/26/27/28 land.

pub mod executor;
pub mod layout_resolver;

pub use executor::{CommandExecutor, CommandOutput, RealExecutor, StubExecutor};
pub use layout_resolver::{LayoutResolveError, LayoutResolver, LayoutSource, SHIPPED_LAYOUTS};

/// Zellij-backed multiplexer. Will implement `ark_core::Multiplexer` once
/// T-025/26/27/28 wire the executor + layout resolver into the trait impl.
pub struct ZellijMux {
    _placeholder: (),
}
