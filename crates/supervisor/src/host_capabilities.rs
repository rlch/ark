//! Host-declared capabilities advertised in `InitializeRequest` (T-029).
//!
//! Per cavekit-soul-phase-2-host-dispatch.md R7: deterministic sorted
//! set of every Phase-2 capability ark supports. The slate is a
//! `&'static [&'static str]` constant — identical across concurrent
//! handshakes, fixed at supervisor startup, and byte-ascending sorted
//! so wire traces are stable across runs.
//!
//! Ark (the host) implements the consumer side of every Phase-2 flag
//! defined by `ark_ext_proto::PHASE_2_CAPABILITY_FLAGS`, so this slate
//! is the complete 8-flag set. The per-extension advertised capability
//! set lives in [`crate::ext_dispatch::ExtensionCapabilities`]; this
//! module carries the host's own self-declaration.
//!
//! ## Wire shape
//!
//! At handshake time (consumer: T-030's extension load sequence) the
//! slate is materialised into `ark_ext_proto::Capabilities` via
//! `Capabilities::from_iter(HOST_PHASE_2_CAPABILITIES.iter().copied())`
//! and rendered to the nested object-of-objects form by
//! `Capabilities::to_wire()` — matching the shape documented on
//! `InitializeRequest::client_capabilities`.
//!
//! ## Consumer
//!
//! T-030 calls [`host_phase_2_capabilities`] (or indexes
//! [`HOST_PHASE_2_CAPABILITIES`] directly) when constructing the
//! `InitializeRequest` payload sent to each extension. The handshake
//! site does not exist yet — this task (T-029) delivers the slate; the
//! load-sequence task (T-030) wires it into the outbound RPC.

/// Sorted list of every Phase-2 capability ark (the host) supports.
///
/// **Byte-ascending** ordering is enforced by
/// [`tests::host_capabilities_sorted`] — do not reorder by hand.
///
/// The slate mirrors `ark_ext_proto::PHASE_2_CAPABILITY_FLAGS` in
/// content (see [`tests::host_capabilities_match_phase_2_slate`] which
/// inlines the expected 8 flags to avoid a cross-crate dep). Ark is
/// the consumer side of every flag, so HOST == proto here; when the
/// two slates diverge in a future phase, this constant stays host-
/// only while the proto constant grows.
pub const HOST_PHASE_2_CAPABILITIES: &[&str] = &[
    "ext.control_verbs.v1",
    "ext.doctor.v1",
    "ext.lifecycle.v1",
    "ext.list_columns.v1",
    "ext.reload_gate.v1",
    "ext.scene_compile_hook.v1",
    "view.pane.v1",
    "view.stack.v1",
];

/// Accessor form of [`HOST_PHASE_2_CAPABILITIES`] for call-sites that
/// prefer a function (e.g. passing through a trait object). The return
/// value is the same `'static` slice.
pub fn host_phase_2_capabilities() -> &'static [&'static str] {
    HOST_PHASE_2_CAPABILITIES
}

#[cfg(test)]
mod tests {
    use super::{HOST_PHASE_2_CAPABILITIES, host_phase_2_capabilities};

    /// The slate's wire ordering MUST be byte-ascending (R7
    /// "deterministic sorted set"). This guards against hand-edits
    /// that reorder by domain/grouping; stable ordering is what makes
    /// wire traces comparable across runs.
    #[test]
    fn host_capabilities_sorted() {
        for pair in HOST_PHASE_2_CAPABILITIES.windows(2) {
            assert!(
                pair[0] < pair[1],
                "HOST_PHASE_2_CAPABILITIES must be byte-ascending; {} >= {}",
                pair[0],
                pair[1]
            );
        }
    }

    /// Ark is the consumer side of every Phase-2 flag, so the host
    /// slate MUST match `ark_ext_proto::PHASE_2_CAPABILITY_FLAGS` set-
    /// wise. We inline the expected slate here (rather than depending
    /// on ark-ext-proto from supervisor) — divergence in either
    /// location will fire this assert on the next `cargo test` run.
    #[test]
    fn host_capabilities_match_phase_2_slate() {
        // Mirrors `ark_ext_proto::PHASE_2_CAPABILITY_FLAGS` verbatim.
        // Keep sorted so the comparison is order-independent via set
        // equality AND list-equality (list form catches accidental
        // duplicates).
        let mut expected: Vec<&'static str> = vec![
            "view.pane.v1",
            "view.stack.v1",
            "ext.lifecycle.v1",
            "ext.scene_compile_hook.v1",
            "ext.control_verbs.v1",
            "ext.doctor.v1",
            "ext.list_columns.v1",
            "ext.reload_gate.v1",
        ];
        expected.sort();
        let ours: Vec<&'static str> = HOST_PHASE_2_CAPABILITIES.to_vec();
        assert_eq!(
            ours, expected,
            "HOST slate diverges from phase-2 capability taxonomy",
        );
    }

    /// Kit R6 / R7 nails the Phase-2 surface at exactly eight flags;
    /// drift in either direction is a spec violation that should trip
    /// CI.
    #[test]
    fn host_capabilities_count_is_eight() {
        assert_eq!(HOST_PHASE_2_CAPABILITIES.len(), 8);
    }

    /// The accessor form returns the same static slice as the const.
    #[test]
    fn host_phase_2_capabilities_fn_matches_const() {
        assert_eq!(host_phase_2_capabilities(), HOST_PHASE_2_CAPABILITIES);
    }

    /// No duplicate flags — a `BTreeSet` collapse MUST preserve every
    /// entry. This catches copy-paste bugs that look fine in a diff
    /// but advertise the same flag twice.
    #[test]
    fn host_capabilities_have_no_duplicates() {
        let unique: std::collections::BTreeSet<&'static str> =
            HOST_PHASE_2_CAPABILITIES.iter().copied().collect();
        assert_eq!(
            unique.len(),
            HOST_PHASE_2_CAPABILITIES.len(),
            "duplicate flag(s) in HOST_PHASE_2_CAPABILITIES",
        );
    }

    /// Every flag follows the `<domain>.<feature>.v<N>` identifier
    /// shape per ark-ext-proto capability taxonomy. Catches typos
    /// like `"view-pane-v1"` that would pass the sort + count gates
    /// but fail capability lookup at runtime.
    #[test]
    fn host_capabilities_use_dotted_v1_form() {
        for flag in HOST_PHASE_2_CAPABILITIES {
            let parts: Vec<&str> = flag.split('.').collect();
            assert_eq!(
                parts.len(),
                3,
                "flag {flag} must have three dotted segments",
            );
            assert!(
                parts[2].starts_with('v'),
                "flag {flag} must end with a vN segment",
            );
        }
    }
}
