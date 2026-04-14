//! Placeholder `Config` type.
//!
//! Real schema lands in T-018 (figment-loaded TOML). This stand-in lets the
//! `World` capability bag (cavekit-architecture.md R3) carry a stable
//! `Arc<Config>` slot today; T-018 swaps in real fields without breaking the
//! call sites because the type is `#[non_exhaustive]`.

/// Placeholder configuration.
///
/// Marked `#[non_exhaustive]` so adding fields in T-018 is not a breaking
/// change for downstream construction patterns.
#[derive(Clone, Debug, Default)]
#[non_exhaustive]
pub struct Config {
    // intentionally empty — fields land in T-018
}

impl Config {
    /// Construct an empty placeholder. T-018 replaces this with `from_path`
    /// / figment-driven loading.
    pub fn placeholder() -> Self {
        Self::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn placeholder_is_constructible() {
        let _c = Config::placeholder();
    }

    #[test]
    fn config_is_clone_and_default() {
        let c = Config::default();
        let _c2 = c.clone();
    }
}
