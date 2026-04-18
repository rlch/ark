//! Capability-aware extension RPC dispatcher (T-028).
//!
//! Records per-session extension capabilities at handshake time;
//! gates outbound RPC calls on advertised capabilities; logs a
//! warn-once when an extension advertises a capability but returns
//! `method_not_found` for its gated methods.
//!
//! Per cavekit-soul-phase-2-host-dispatch.md R6.

use std::collections::{HashMap, HashSet};
use std::sync::{Mutex, OnceLock};

/// Capability → gated methods mapping. Static table keyed by flag;
/// each entry lists the RPC method names that require the flag.
///
/// Derived from the phase-2 capability taxonomy + method surface:
/// - view.pane.v1 → pane/emit, pane/replace_view, pane/close
/// - view.stack.v1 → stack/spawn_pane, stack/close_child, stack/clear
/// - ext.lifecycle.v1 → on_session_start, on_session_end
/// - ext.scene_compile_hook.v1 → scene_compile_hook
/// - ext.control_verbs.v1 → control_verbs
/// - ext.doctor.v1 → doctor_checks
/// - ext.list_columns.v1 → list_columns
/// - ext.reload_gate.v1 → (no fixed method — manifest-declared per-gate)
pub fn capability_for_method(method: &str) -> Option<&'static str> {
    match method {
        "pane/emit" | "pane/replace_view" | "pane/close" => Some("view.pane.v1"),
        "stack/spawn_pane" | "stack/close_child" | "stack/clear" => Some("view.stack.v1"),
        "on_session_start" | "on_session_end" => Some("ext.lifecycle.v1"),
        "scene_compile_hook" => Some("ext.scene_compile_hook.v1"),
        "control_verbs" => Some("ext.control_verbs.v1"),
        "doctor_checks" => Some("ext.doctor.v1"),
        "list_columns" => Some("ext.list_columns.v1"),
        _ => None, // Methods not in this table are ungated (e.g. intent_dispatch, ping).
    }
}

/// Per-session extension capability registry. Populated at handshake
/// (T-029 populates; this module consumes).
#[derive(Clone, Debug, Default)]
pub struct ExtensionCapabilities {
    advertised: HashSet<String>,
}

impl ExtensionCapabilities {
    pub fn new<I, S>(flags: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Self {
            advertised: flags.into_iter().map(Into::into).collect(),
        }
    }

    pub fn has(&self, flag: &str) -> bool {
        self.advertised.contains(flag)
    }

    pub fn insert(&mut self, flag: impl Into<String>) {
        self.advertised.insert(flag.into());
    }
}

/// Per-extension-ident capability map. Keyed by extension name (not
/// session id — supervisor may hold one instance per named ext).
type CapMap = HashMap<String, ExtensionCapabilities>;

static CAP_REGISTRY: OnceLock<Mutex<CapMap>> = OnceLock::new();

fn registry() -> &'static Mutex<CapMap> {
    CAP_REGISTRY.get_or_init(|| Mutex::new(CapMap::default()))
}

/// Record an extension's advertised capabilities at handshake time.
/// Replaces any existing entry for the same ext name.
pub fn record_capabilities<I, S>(ext_name: &str, flags: I)
where
    I: IntoIterator<Item = S>,
    S: Into<String>,
{
    let caps = ExtensionCapabilities::new(flags);
    registry()
        .lock()
        .expect("cap registry poisoned")
        .insert(ext_name.to_string(), caps);
}

/// Check whether an RPC call to `ext_name::method` should dispatch.
///
/// Returns:
/// - `true` — method is either ungated (no capability requirement) or
///   the ext advertised the gating capability AND hasn't been marked
///   as an opt-out (via [`warn_advertised_but_unimplemented`]);
/// - `false` — method is gated and either (a) the ext did not
///   advertise the flag, or (b) a prior call observed
///   `method_not_found` and marked the pair as an opt-out.
///   Callers skip the call entirely; no log on the skip path per kit R6.
pub fn should_dispatch(ext_name: &str, method: &str) -> bool {
    let Some(required_cap) = capability_for_method(method) else {
        return true; // Ungated method — always dispatch.
    };
    // Opt-out short-circuit: an ext that returned method_not_found
    // once is treated as opted out for the rest of the session, even
    // though it advertises the capability. Prevents repeated pointless
    // RPC attempts against a partially-implemented ext (F-015).
    if is_opted_out(ext_name, method) {
        return false;
    }
    let reg = registry().lock().expect("cap registry poisoned");
    reg.get(ext_name).is_some_and(|c| c.has(required_cap))
}

/// Session-scoped set of `(ext, method)` pairs the host has marked as
/// opted-out after observing a `method_not_found` response. Entries
/// are additive; there is no un-opt-out path short of supervisor
/// restart (session-scoped semantics).
static OPTED_OUT: OnceLock<Mutex<HashSet<(String, String)>>> = OnceLock::new();

fn opted_out() -> &'static Mutex<HashSet<(String, String)>> {
    OPTED_OUT.get_or_init(|| Mutex::new(HashSet::new()))
}

fn is_opted_out(ext_name: &str, method: &str) -> bool {
    let key = (ext_name.to_string(), method.to_string());
    opted_out()
        .lock()
        .expect("opted_out poisoned")
        .contains(&key)
}

/// Emit a warn-once log when a dispatch proceeded (capability advertised)
/// but the extension returned MethodNotFound. Dedups on (ext, method)
/// across the entire supervisor process lifetime. Also records the
/// pair as opted-out so subsequent [`should_dispatch`] calls return
/// false without round-tripping to the ext (F-015).
pub fn warn_advertised_but_unimplemented(ext_name: &str, method: &str) {
    let key = (ext_name.to_string(), method.to_string());
    // Opt-out insert — first caller wins. Subsequent
    // `should_dispatch(ext, method)` returns false without RPC.
    let inserted = opted_out()
        .lock()
        .expect("opted_out poisoned")
        .insert(key.clone());
    if inserted {
        tracing::warn!(
            target: "ark.supervisor.ext_dispatch",
            ext = %ext_name,
            method = %method,
            "extension advertised capability but returned method_not_found; treating as opt-out for remainder of session"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capability_for_method_covers_phase_2_surface() {
        assert_eq!(capability_for_method("pane/emit"), Some("view.pane.v1"));
        assert_eq!(
            capability_for_method("pane/replace_view"),
            Some("view.pane.v1")
        );
        assert_eq!(capability_for_method("pane/close"), Some("view.pane.v1"));
        assert_eq!(
            capability_for_method("stack/spawn_pane"),
            Some("view.stack.v1")
        );
        assert_eq!(
            capability_for_method("stack/close_child"),
            Some("view.stack.v1")
        );
        assert_eq!(capability_for_method("stack/clear"), Some("view.stack.v1"));
        assert_eq!(
            capability_for_method("on_session_start"),
            Some("ext.lifecycle.v1")
        );
        assert_eq!(
            capability_for_method("on_session_end"),
            Some("ext.lifecycle.v1")
        );
        assert_eq!(
            capability_for_method("scene_compile_hook"),
            Some("ext.scene_compile_hook.v1")
        );
        assert_eq!(
            capability_for_method("control_verbs"),
            Some("ext.control_verbs.v1")
        );
        assert_eq!(
            capability_for_method("doctor_checks"),
            Some("ext.doctor.v1")
        );
        assert_eq!(
            capability_for_method("list_columns"),
            Some("ext.list_columns.v1")
        );
        // Ungated methods return None.
        assert_eq!(capability_for_method("ping"), None);
        assert_eq!(capability_for_method("intent/dispatch"), None);
    }

    #[test]
    fn ungated_methods_always_dispatch() {
        assert!(should_dispatch("nonexistent-ext", "ping"));
        assert!(should_dispatch("nonexistent-ext", "intent/dispatch"));
    }

    #[test]
    fn gated_methods_skip_when_capability_absent() {
        let ext = "test-ext-absent-caps";
        record_capabilities(ext, Vec::<String>::new()); // zero caps
        assert!(!should_dispatch(ext, "pane/emit"));
        assert!(!should_dispatch(ext, "stack/clear"));
    }

    #[test]
    fn gated_methods_dispatch_when_capability_present() {
        let ext = "test-ext-has-view-pane";
        record_capabilities(ext, ["view.pane.v1"]);
        assert!(should_dispatch(ext, "pane/emit"));
        assert!(should_dispatch(ext, "pane/close"));
        assert!(!should_dispatch(ext, "stack/clear")); // different cap
    }

    #[test]
    fn warn_advertised_but_unimplemented_dedups() {
        // Can't easily assert tracing output here, but we can assert
        // the dedup set grows monotonically.
        let before = opted_out().lock().unwrap().len();
        warn_advertised_but_unimplemented("ext-dedup", "pane/emit");
        warn_advertised_but_unimplemented("ext-dedup", "pane/emit");
        warn_advertised_but_unimplemented("ext-dedup", "pane/emit");
        let after = opted_out().lock().unwrap().len();
        assert_eq!(after, before + 1, "subsequent warns should be deduped");
    }

    #[test]
    fn missing_ext_treated_as_no_capabilities() {
        // Ext never recorded → gated methods must be skipped.
        assert!(!should_dispatch("never-registered-ext", "pane/emit"));
        assert!(!should_dispatch("never-registered-ext", "doctor_checks"));
        // But ungated methods still go through.
        assert!(should_dispatch("never-registered-ext", "ping"));
    }
}
