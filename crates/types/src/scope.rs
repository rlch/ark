//! v1 scope lock — the mux slugs ark v1 actually ships.
//!
//! `ENGINES_V1` / `ORCHESTRATORS_V1` and their predicates are gone:
//! engine + orchestrator concepts live in extensions, not core. `MUX_V1`
//! stays because the multiplexer is still core-resident for v1.

/// Mux backends shipped in v1.
pub const MUX_V1: &[&str] = &["zellij"];

/// True if the slug names a v1 multiplexer.
pub fn is_v1_mux(slug: &str) -> bool {
    MUX_V1.contains(&slug)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mux_v1_zellij_only() {
        assert!(is_v1_mux("zellij"));
        assert!(!is_v1_mux("tmux"));
    }
}
