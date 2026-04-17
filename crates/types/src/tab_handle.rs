//! Minimal `TabHandle` carry-over from the pre-soul `AgentEvent` surface.
//!
//! Under cavekit-soul Phase 1 the tab-scoped events (`TabOpened`,
//! `TabClosed`, etc.) re-home inside the multiplexer extension surface in
//! Phase 2+. The `TabHandle` value itself remains in `ark-types` because
//! the mux abstraction (`ark-mux-zellij`) and the supervisor's tab
//! registry (used by `kill_handler`) both still need a stable handle
//! type for "this tab in this session at this index".
//!
//! The struct is deliberately small: a session name, a numeric index
//! (the multiplexer's own addressing), and a human-friendly name (used
//! for tracing and tests).

use serde::{Deserialize, Serialize};
use std::fmt;

/// Stable handle for a multiplexer tab.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct TabHandle {
    /// Multiplexer session name (e.g. `"ark-cavekit-auth-..."`).
    pub session: String,
    /// 0-based tab index inside the session.
    pub tab_index: u32,
    /// Human-friendly tab label (e.g. `"builder"`).
    pub name: String,
}

impl TabHandle {
    /// Construct a new `TabHandle`.
    pub fn new(session: impl Into<String>, tab_index: u32, name: impl Into<String>) -> Self {
        Self {
            session: session.into(),
            tab_index,
            name: name.into(),
        }
    }
}

impl fmt::Display for TabHandle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}#{}({})", self.session, self.tab_index, self.name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tab_handle_constructs_and_displays() {
        let h = TabHandle::new("ark-foo", 1, "builder");
        assert_eq!(h.session, "ark-foo");
        assert_eq!(h.tab_index, 1);
        assert_eq!(h.name, "builder");
        assert_eq!(h.to_string(), "ark-foo#1(builder)");
    }

    #[test]
    fn tab_handle_serde_roundtrip() {
        let h = TabHandle::new("s", 2, "n");
        let json = serde_json::to_string(&h).unwrap();
        let back: TabHandle = serde_json::from_str(&json).unwrap();
        assert_eq!(back, h);
    }
}
