//! v1 scope lock — the slugs ark v1 actually ships.
//!
//! See cavekit-architecture.md R6.

/// Engines shipped in v1.
pub const ENGINES_V1: &[&str] = &["claude-code"];

/// Orchestrators shipped in v1.
pub const ORCHESTRATORS_V1: &[&str] = &["cavekit", "claude-code"];

/// Multiplexers shipped in v1.
pub const MUX_V1: &[&str] = &["zellij"];

/// True if the slug names a v1 engine.
pub fn is_v1_engine(slug: &str) -> bool {
    ENGINES_V1.contains(&slug)
}

/// True if the slug names a v1 orchestrator.
pub fn is_v1_orchestrator(slug: &str) -> bool {
    ORCHESTRATORS_V1.contains(&slug)
}

/// True if the slug names a v1 multiplexer.
pub fn is_v1_mux(slug: &str) -> bool {
    MUX_V1.contains(&slug)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn engines_v1_contains_claude_code() {
        assert!(is_v1_engine("claude-code"));
        assert!(!is_v1_engine("aider"));
    }

    #[test]
    fn orchestrators_v1_contains_both() {
        assert!(is_v1_orchestrator("cavekit"));
        assert!(is_v1_orchestrator("claude-code"));
        assert!(!is_v1_orchestrator("ralph"));
    }

    #[test]
    fn mux_v1_zellij_only() {
        assert!(is_v1_mux("zellij"));
        assert!(!is_v1_mux("tmux"));
    }
}
