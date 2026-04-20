//! Render-target classification.
//!
//! T-PP-009 (cavekit-plugin-protocol R6): the host has exactly one
//! render target for its process lifetime. Plugins classify each view
//! export by target marker trait (`TerminalView`, reserved `GuiView`
//! post-v1). A plugin loads iff at least one of its views targets the
//! host's active target.
//!
//! # v1 shape
//! Only `Target::Terminal` is present — `Gui` is reserved. Adding the
//! `Gui` variant is a MAJOR ABI bump under cavekit R6 + R14.
//! `#[non_exhaustive]` is load-bearing: downstream materializers must
//! never exhaustive-match `Target`, otherwise the post-v1 `Gui` addition
//! would break every consumer.

/// The render-target a host materializes into.
///
/// v1 = `Terminal` only. `Gui` is reserved (no materializer shipped;
/// addition = MAJOR ABI bump, see `ARK_ABI_VERSION` in `ark-types`).
#[non_exhaustive]
#[derive(Copy, Clone, Eq, PartialEq, Debug, Hash)]
pub enum Target {
    /// Terminal host — materializer lives in `ark-render-terminal`.
    Terminal,
    // NOTE: `Gui` intentionally absent in v1. Added post-v1 as a
    // `#[doc(hidden)]` arm first, then stabilized — bumping
    // `ARK_ABI_VERSION` as the MAJOR break.
}

impl Target {
    /// Stable human/diagnostic name for this target — used in error
    /// messages (e.g. `no-renderable-views`) and logs.
    pub const fn as_str(self) -> &'static str {
        match self {
            Target::Terminal => "terminal",
        }
    }
}

impl std::fmt::Display for Target {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Host-side accessor returning the active render target for this
/// process. v1 is hard-coded to [`Target::Terminal`]; future hosts will
/// resolve this from a build-time feature flag or process-start config.
///
/// Stubbed here as a free function rather than a method on a yet-to-be-
/// defined `Host` struct — the fuller `Host` handle lands in Tier 3
/// (T-PP-025). Callers today can pretend this is `Host::target()`.
pub const fn host_target() -> Target {
    Target::Terminal
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terminal_is_display_terminal() {
        assert_eq!(format!("{}", Target::Terminal), "terminal");
    }

    #[test]
    fn host_target_is_terminal_in_v1() {
        assert_eq!(host_target(), Target::Terminal);
    }

    #[test]
    fn target_is_copy_eq_hash() {
        // These derives are part of R6's "Copy + Eq" acceptance
        // criterion — assert they compile by using them.
        fn assert_copy<T: Copy>() {}
        fn assert_eq<T: Eq>() {}
        fn assert_hash<T: std::hash::Hash>() {}
        assert_copy::<Target>();
        assert_eq::<Target>();
        assert_hash::<Target>();
    }
}
