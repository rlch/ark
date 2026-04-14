//! Runtime context passed into every subcommand handler.
//!
//! T-084 scaffolds the CLI shape; real subcommand impls (T-087–T-093)
//! will extend [`Ctx`] with whatever they need (state layout, config,
//! tracing handles). For now it carries just the two pieces that the
//! scaffold is required to wire:
//!
//! - `no_color` — the resolved `$NO_COLOR` setting (cavekit-cli R1).
//! - `stdout` / `stderr` — writers so tests can capture output without
//!   fighting global IO.
//!
//! NO_COLOR precedence for the scaffold:
//!   1. If `NO_COLOR` env var is set to a non-empty string → true.
//!   2. Otherwise → false (default).
//!
//! T-093 will later layer `--color` / config-file settings on top of
//! this — keep the field `pub` so that extension is a no-op.

/// Pure helper: returns `true` when the env getter yields any non-empty
/// value for `NO_COLOR` (per <https://no-color.org>: any set value
/// disables color).
///
/// Mirrors the helper in `ark-pane` so both crates agree on semantics
/// without depending on each other just for this one check.
pub fn no_color_from_env<F>(getter: F) -> bool
where
    F: Fn(&str) -> Option<String>,
{
    matches!(getter("NO_COLOR"), Some(v) if !v.is_empty())
}

/// Convenience: reads the process environment. Equivalent to calling
/// [`no_color_from_env`] with `|k| std::env::var(k).ok()`.
pub fn detect_no_color() -> bool {
    no_color_from_env(|k| std::env::var(k).ok())
}

/// Shared context threaded through subcommand dispatch.
///
/// The scaffold carries only the color flag; later tasks extend this
/// struct with the state layout, config handle, and tracing guard.
#[derive(Debug, Clone, Copy, Default)]
pub struct Ctx {
    /// Whether to suppress ANSI color in any custom output.
    pub no_color: bool,
}

impl Ctx {
    /// Build a [`Ctx`] from the process environment.
    pub fn from_env() -> Self {
        Self {
            no_color: detect_no_color(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_color_env_set_nonempty_is_true() {
        assert!(no_color_from_env(|k| if k == "NO_COLOR" {
            Some("1".to_string())
        } else {
            None
        }));
    }

    #[test]
    fn no_color_env_unset_is_false() {
        assert!(!no_color_from_env(|_| None));
    }

    #[test]
    fn no_color_env_empty_is_false() {
        // Per NO_COLOR spec only non-empty values disable color.
        assert!(!no_color_from_env(|k| if k == "NO_COLOR" {
            Some(String::new())
        } else {
            None
        }));
    }

    #[test]
    fn ctx_default_has_no_color_false() {
        assert!(!Ctx::default().no_color);
    }

    #[test]
    fn ctx_carries_no_color_flag() {
        let ctx = Ctx { no_color: true };
        assert!(ctx.no_color);
    }
}
