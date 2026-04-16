//! Zellij integration backend for ark.
//!
//! Hosts `ZellijMux` — ark's concrete integration with the zellij terminal
//! multiplexer. See `cavekit-mux-zellij.md` for the full spec. Downstream
//! callers use `ZellijMux` directly, and tests use the [`ZellijMux::for_test`]
//! constructor (gated behind the `test-support` cargo feature) to inject a
//! [`executor::StubExecutor`].
//!
//! Modules:
//! - [`executor`] — `CommandExecutor` abstraction over `tokio::process`.
//! - [`layout_resolver`] — shipped/user KDL layout resolution + listing +
//!   user-layout validation (cavekit-layouts.md R1/R5/R6).
//! - [`layout_template`] — minijinja renderer with bounded variable surface
//!   and post-render KDL syntax validation (R5/R3).
//! - [`layout_writer`] — writes rendered KDL to
//!   `${XDG_RUNTIME_DIR}/ark/layouts/{id}-{tab}.kdl` with strict perms.
//! - [`mux`] — `ZellijMux` itself (R1–R4, R6).

pub mod executor;
pub mod layout_resolver;
pub mod layout_template;
pub mod layout_writer;
pub mod mux;
pub mod pty;

pub use executor::{CommandExecutor, CommandOutput, RealExecutor, StubExecutor};
pub use layout_resolver::{
    LayoutListEntry, LayoutResolveError, LayoutResolver, LayoutSource, LayoutValidation,
    SHIPPED_LAYOUTS, SHIPPED_SCENES, default_layout_for_orchestrator, effective_layout,
};
pub use layout_template::{LayoutTemplateError, LayoutVars, render as render_layout};
pub use layout_writer::{
    LayoutWriteError, cleanup_rendered, rendered_layout_path, rendered_layouts_dir, write_rendered,
};
pub use mux::{MIN_ZELLIJ_VERSION, PIPE_TARGET_PICKER, PIPE_TARGET_STATUS, ZellijMux};
pub use pty::{PtySpawnError, PtyZellijHandle, pty_child_startup_failure, spawn_zellij_with_pty};
