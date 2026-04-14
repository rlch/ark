//! Shared ratatui/crossterm chrome for `ark pane` subcommands.
//!
//! Provides a common event loop, terminal lifecycle management, and tracing
//! setup that `ark pane diff/git/log` (T-040/041/042) plug widgets into.
//! See `context/kits/cavekit-pane-commands.md` R4 for requirements.

pub mod app;
pub mod git;
pub mod log;
pub mod tracing_init;

pub use app::{
    PaneEvent, PaneFlow, TerminalGuard, Tui, is_ctrl_c, no_color, no_color_from_env, run_pane,
};
pub use tracing_init::init_tracing_to_stderr;
