//! v0.2 backlog #2 — [`Stack::spawn_pane`] live RPC dispatcher path.
//!
//! The process-global `STACK_DISPATCHER` slot is a
//! [`std::sync::OnceLock`] — registering a dispatcher inside a unit
//! test would leak the registration across every other `#[cfg(test)]`
//! fn in the `ark-view` crate. Lifting the dispatcher test into an
//! integration test (a separate test binary per file, per cargo)
//! isolates the registration so the rest of the unit-test suite keeps
//! running against the fallback (synthetic-handle) path.
//!
//! A SINGLE test lives in this file so the first-writer-wins slot
//! semantics are not racy — parallel integration tests inside one
//! binary would otherwise interleave. Both positive and negative
//! assertions (dispatched-handle wins; idempotent re-registration
//! returns false) run inside the one fn.

use ark_view::{
    HandleId, Pane, PaneAttrs, Stack, StackDispatcher, View, register_stack_dispatcher,
    stack_dispatcher,
};
use std::sync::Mutex;

/// View marker for the test.
struct VX;
impl View for VX {}

/// Recording dispatcher — captures every call's `(stack, view_attrs)`
/// and returns a deterministic handle derived from them so the test can
/// assert the end-to-end round-trip shape.
struct RecordingDispatcher {
    calls: Mutex<Vec<(String, serde_json::Value)>>,
}

impl StackDispatcher for RecordingDispatcher {
    fn spawn_pane(&self, stack: &HandleId, view_attrs: &serde_json::Value) -> Option<HandleId> {
        let mut g = self.calls.lock().unwrap();
        g.push((stack.as_str().to_string(), view_attrs.clone()));
        // Deterministic child handle so the test can assert reach-back.
        Some(HandleId::new(format!(
            "child-of-{}-{}",
            stack.as_str(),
            g.len()
        )))
    }
}

impl RecordingDispatcher {
    fn new() -> &'static RecordingDispatcher {
        // Leak so the dispatcher's mutex stays reachable through a
        // `'static` reference. `register_stack_dispatcher` takes
        // ownership via Box; the leaked reference is for the test to
        // read back the captured calls.
        Box::leak(Box::new(RecordingDispatcher {
            calls: Mutex::new(Vec::new()),
        }))
    }
}

/// Shim that delegates to a leaked static recorder. `Box<Shim>` is what
/// the OnceLock actually owns; `Shim` carries an immutable reference to
/// the leaked recorder so we can inspect call history after the
/// dispatch.
struct Shim(&'static RecordingDispatcher);

impl StackDispatcher for Shim {
    fn spawn_pane(&self, stack: &HandleId, view_attrs: &serde_json::Value) -> Option<HandleId> {
        self.0.spawn_pane(stack, view_attrs)
    }
}

#[test]
fn stack_spawn_pane_dispatches_through_registered_dispatcher() {
    let rec = RecordingDispatcher::new();
    assert!(
        register_stack_dispatcher(Shim(rec)),
        "first dispatcher registration must succeed"
    );
    assert!(
        stack_dispatcher().is_some(),
        "dispatcher must be readable after registration"
    );

    // Dispatch #1 — round-trip a typed attrs payload.
    let s: Stack<VX> = serde_json::from_str("\"my-stack\"").unwrap();
    let attrs = PaneAttrs::from_attrs(&serde_json::json!({
        "id": "child-1",
        "transcript_path": "/tmp/t.jsonl",
    }))
    .unwrap();
    let child: Pane<VX> = s.spawn_pane(attrs);

    // Child handle is the dispatcher's return value, NOT the fallback
    // synthetic shape — proves the live path won.
    assert_eq!(child.handle().as_str(), "child-of-my-stack-1");

    // Dispatcher saw the view_attrs payload in full.
    {
        let calls = rec.calls.lock().unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, "my-stack");
        assert_eq!(
            calls[0].1.get("id").and_then(|v| v.as_str()),
            Some("child-1")
        );
        assert_eq!(
            calls[0].1.get("transcript_path").and_then(|v| v.as_str()),
            Some("/tmp/t.jsonl")
        );
    }

    // Dispatch #2 — default empty PaneAttrs. view_attrs is JSON null
    // but the dispatcher still fires.
    let child2: Pane<VX> = s.spawn_pane(PaneAttrs::default());
    assert_eq!(child2.handle().as_str(), "child-of-my-stack-2");
    {
        let calls = rec.calls.lock().unwrap();
        assert_eq!(calls.len(), 2);
        assert!(calls[1].1.is_null(), "default view_attrs must be null");
    }

    // Re-registering returns false (first-writer-wins).
    struct Noop;
    impl StackDispatcher for Noop {
        fn spawn_pane(&self, _: &HandleId, _: &serde_json::Value) -> Option<HandleId> {
            None
        }
    }
    assert!(
        !register_stack_dispatcher(Noop),
        "second registration must be rejected"
    );
}
