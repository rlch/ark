//! Tracing subscriber initialization for pane commands.
//!
//! Pane commands run inside zellij panes; their stderr is captured by
//! zellij (surfaced via layout logs) or by the supervisor. Writing tracing
//! output to stderr keeps stdout clean for ratatui's alternate-screen draws.

/// Initialize a tracing subscriber writing to stderr.
///
/// Respects `RUST_LOG` via `EnvFilter::try_from_default_env()`; falls back
/// to `info` level when unset. Safe to call exactly once per process — a
/// second call returns an error instead of panicking.
pub fn init_tracing_to_stderr() -> anyhow::Result<()> {
    use tracing_subscriber::{EnvFilter, fmt};

    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));

    fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .try_init()
        .map_err(|e| anyhow::anyhow!("tracing init failed: {e}"))?;

    Ok(())
}
