//! Handle invalidation taxonomy. Every scene-declared handle has
//! exactly one terminal cause; consumers branch on it to decide
//! whether to re-acquire the handle or drop it permanently.
//!
//! Per scene R17 + cavekit-soul-phase-2-ark-view.md R7. The cause
//! rides on the `ark.handle.invalidated { handle, cause }` ExtEvent
//! broadcast on the core event bus AND on the `HandleGone` error
//! variant returned lazily by subsequent ops against a dead handle.

/// Terminal cause for a scene-declared handle. Exactly three causes
/// in v0.1; `#[non_exhaustive]` allows future causes to land without
/// breaking downstream `match` consumers.
///
/// Serialises to stable snake_case strings — these strings are the
/// wire contract consumers pattern-match against.
#[derive(Copy, Clone, Debug, Eq, Hash, PartialEq, serde::Serialize, serde::Deserialize, facet::Facet)]
#[serde(rename_all = "snake_case")]
#[repr(u8)]
#[non_exhaustive]
pub enum InvalidationCause {
    /// User closed the pane via zellij keybind (no `ARK_HANDLE` on the
    /// closed target that matched the handle's current tenant); the
    /// supervisor records suppression so reconcile skips the respawn
    /// until scene params change. See cavekit R8 + host-dispatch R9.
    UserClosed,
    /// Scene reload dropped the handle from the layout — the new scene
    /// no longer declares a node with this handle name.
    SceneReloadDropped,
    /// Session ended; every handle in the session invalidates at once.
    SessionEnded,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn invalidation_cause_snake_case_tag() {
        assert_eq!(
            serde_json::to_value(InvalidationCause::UserClosed).unwrap(),
            serde_json::json!("user_closed")
        );
        assert_eq!(
            serde_json::to_value(InvalidationCause::SceneReloadDropped).unwrap(),
            serde_json::json!("scene_reload_dropped")
        );
        assert_eq!(
            serde_json::to_value(InvalidationCause::SessionEnded).unwrap(),
            serde_json::json!("session_ended")
        );
    }

    #[test]
    fn invalidation_cause_roundtrip_all_variants() {
        for cause in [
            InvalidationCause::UserClosed,
            InvalidationCause::SceneReloadDropped,
            InvalidationCause::SessionEnded,
        ] {
            let json = serde_json::to_string(&cause).unwrap();
            let back: InvalidationCause = serde_json::from_str(&json).unwrap();
            assert_eq!(cause, back);
        }
    }

    #[test]
    fn invalidation_cause_exhaustive_pattern_match() {
        fn describe(c: InvalidationCause) -> &'static str {
            match c {
                InvalidationCause::UserClosed => "user_closed",
                InvalidationCause::SceneReloadDropped => "scene_reload_dropped",
                InvalidationCause::SessionEnded => "session_ended",
                // Wildcard required because of `#[non_exhaustive]` on the enum.
                _ => "unknown",
            }
        }

        let a = describe(InvalidationCause::UserClosed);
        let b = describe(InvalidationCause::SceneReloadDropped);
        let c = describe(InvalidationCause::SessionEnded);
        assert_ne!(a, b);
        assert_ne!(b, c);
        assert_ne!(a, c);
    }

    #[test]
    fn invalidation_cause_is_copy_eq_hash() {
        let cause = InvalidationCause::UserClosed;
        let copy1 = cause;
        let copy2 = cause;
        assert_eq!(copy1, copy2);

        let mut map: HashMap<InvalidationCause, u32> = HashMap::new();
        map.insert(InvalidationCause::UserClosed, 1);
        map.insert(InvalidationCause::SceneReloadDropped, 2);
        map.insert(InvalidationCause::SessionEnded, 3);
        assert_eq!(map.get(&InvalidationCause::UserClosed), Some(&1));
        assert_eq!(map.get(&InvalidationCause::SceneReloadDropped), Some(&2));
        assert_eq!(map.get(&InvalidationCause::SessionEnded), Some(&3));
    }
}
