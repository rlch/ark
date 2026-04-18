//! Name-indexed handle lookup — `SessionHandles` context accessor.
//!
//! Extensions receive a `&SessionHandles` in their session-start hook
//! (wired by the supervisor; see `cavekit-soul-phase-2-host-dispatch.md`
//! R8 load sequence). The accessor lets extensions re-attach to handles
//! by the stable scene-author name across reconciles — needed because
//! a user-close → params-change sequence evicts the suppression and
//! respawns with a NEW opaque [`HandleId`], SAME [`SceneHandleName`].
//!
//! Per cavekit-soul-phase-2-ark-view.md R10. Lookup is a local map
//! read — never an RPC call.

use crate::handle::{HandleId, HandleKind};
use crate::suppression::SceneHandleName;
use crate::typed::{Pane, Stack, TabHandle};
use crate::view::View;
use std::collections::HashMap;

/// Per-handle record the lookup consults. Stores the handle's runtime
/// id and its scene-declared kind so `pane_by_name` / `stack_by_name`
/// / `tab_by_name` can reject kind mismatches.
///
/// The `declared_view_type` is a string since ark-view doesn't know
/// extension view-type identity at this level — the name is the
/// fully-qualified scene view-type token (`"<ext>.<view>"`) as it
/// appears in the manifest / scene AST. A caller asking for
/// `Pane<V>` matches if `V`'s type-id-string equals
/// `declared_view_type` (resolved by the caller, since rust type-id
/// isn't a stable string).
#[derive(Clone, Debug)]
pub struct HandleRecord {
    /// Runtime opaque id — churns across reconciles.
    pub handle: HandleId,
    /// Scene-declared kind — for the kind-mismatch rejection path.
    pub kind: HandleKind,
    /// Scene-declared view-type token (`"<ext>.<view>"` from manifest).
    /// None for `HandleKind::Tab` since tabs are typeless.
    pub declared_view_type: Option<String>,
}

/// Name-indexed handle lookup context. Immutable snapshot of the
/// host's current handle table at hook-call time. Supervisor constructs
/// one per session-start / per-reconcile boundary.
///
/// Per cavekit-soul-phase-2-ark-view.md R10. Operations are pure
/// reads against the inner `HashMap` — no RPC traffic.
#[derive(Clone, Debug, Default)]
pub struct SessionHandles {
    /// Inner table keyed by scene-author name. Flat namespace; the
    /// scene compiler guarantees uniqueness across the whole scene.
    table: HashMap<SceneHandleName, HandleRecord>,
}

impl SessionHandles {
    /// Construct from an iterator of records. Supervisor uses this at
    /// session-start to snapshot the reconciled handle table.
    pub fn from_records<I>(iter: I) -> Self
    where
        I: IntoIterator<Item = (SceneHandleName, HandleRecord)>,
    {
        Self {
            table: iter.into_iter().collect(),
        }
    }

    /// Look up a scene-declared pane by its author name. Returns
    /// `None` when absent (suppressed / removed / never declared) OR
    /// when the declared kind is not [`HandleKind::Pane`].
    ///
    /// ## View-type enforcement (partial — see [`pane_by_name_typed`])
    ///
    /// Kit R10 requires V-mismatch to return `None` + warn log. Rust's
    /// stable-identity-for-`V` mechanism is `#[derive(View)]` (T-025),
    /// which will stamp a `const VIEW_TYPE: &'static str` on every
    /// annotated view type. Until T-025 lands, this untyped path
    /// trusts the caller — a mismatched `V` produces a `Pane<V>`
    /// whose subsequent typed ops will misroute.
    ///
    /// Callers who need enforcement TODAY use [`pane_by_name_typed`]
    /// and pass the scene-declared view-type token explicitly.
    pub fn pane_by_name<V: View>(&self, name: &SceneHandleName) -> Option<Pane<V>> {
        let rec = self.table.get(name)?;
        if rec.kind != HandleKind::Pane {
            // Kind mismatch: caller asked for a pane, scene declared it
            // as something else. None + warn log.
            self.warn_kind_mismatch(name, rec.kind, HandleKind::Pane);
            return None;
        }
        // TODO(T-025): compare `V`'s derive-stamped `VIEW_TYPE` against
        // `rec.declared_view_type`; warn-log + None on mismatch.
        Some(Pane::from_handle(rec.handle.clone()))
    }

    /// Variant of [`pane_by_name`] with explicit view-type enforcement.
    /// Returns `None` + warn-log when the scene-declared view type
    /// for this handle name differs from `declared_view_type`.
    ///
    /// Use this when you know the token statically (e.g. you authored
    /// the scene and know `@handle` is `"<ext>.<view>"`) and want
    /// R10's mismatch rejection today. `#[derive(View)]` in T-025
    /// will fold this into [`pane_by_name`] automatically.
    pub fn pane_by_name_typed<V: View>(
        &self,
        name: &SceneHandleName,
        declared_view_type: &str,
    ) -> Option<Pane<V>> {
        let rec = self.table.get(name)?;
        if rec.kind != HandleKind::Pane {
            self.warn_kind_mismatch(name, rec.kind, HandleKind::Pane);
            return None;
        }
        if rec.declared_view_type.as_deref() != Some(declared_view_type) {
            eprintln!(
                "[ark-view] SessionHandles view-type mismatch on {name:?}: declared {:?}, caller requested {declared_view_type:?}",
                rec.declared_view_type
            );
            return None;
        }
        Some(Pane::from_handle(rec.handle.clone()))
    }

    /// Look up a scene-declared stack by its author name. Returns
    /// `None` when absent or kind-mismatched.
    pub fn stack_by_name<V: View>(&self, name: &SceneHandleName) -> Option<Stack<V>> {
        let rec = self.table.get(name)?;
        if rec.kind != HandleKind::Stack {
            self.warn_kind_mismatch(name, rec.kind, HandleKind::Stack);
            return None;
        }
        Some(Stack::from_handle(rec.handle.clone()))
    }

    /// Look up a scene-declared tab by its author name. Returns
    /// `None` when absent or kind-mismatched.
    pub fn tab_by_name(&self, name: &SceneHandleName) -> Option<TabHandle> {
        let rec = self.table.get(name)?;
        if rec.kind != HandleKind::Tab {
            self.warn_kind_mismatch(name, rec.kind, HandleKind::Tab);
            return None;
        }
        Some(TabHandle::from_handle(rec.handle.clone()))
    }

    /// Number of entries in the snapshot. Informational — callers
    /// should iterate or look up by name rather than treat this as
    /// authoritative.
    pub fn len(&self) -> usize {
        self.table.len()
    }

    /// True when the snapshot has no entries (fresh session, or
    /// scene with zero scene-declared handles).
    pub fn is_empty(&self) -> bool {
        self.table.is_empty()
    }

    fn warn_kind_mismatch(
        &self,
        name: &SceneHandleName,
        declared: HandleKind,
        requested: HandleKind,
    ) {
        // R10: warn-log on mismatch. ark-view has no logger dep; emit via
        // eprintln!. The supervisor wraps SessionHandles with its own
        // logger-aware accessor that routes through tracing; this fallback
        // is harmless in tests and never panics.
        eprintln!(
            "[ark-view] SessionHandles kind mismatch on {name:?}: declared {declared:?}, caller requested {requested:?}"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Test-only view type.
    struct VX;
    impl crate::view::View for VX {}

    fn rec(kind: HandleKind, id: &str, view_type: Option<&str>) -> HandleRecord {
        HandleRecord {
            handle: HandleId::new(id),
            kind,
            declared_view_type: view_type.map(String::from),
        }
    }

    #[test]
    fn session_handles_empty_lookup_returns_none() {
        let sh = SessionHandles::default();
        let name = SceneHandleName::new("missing");
        assert!(sh.pane_by_name::<VX>(&name).is_none());
        assert!(sh.stack_by_name::<VX>(&name).is_none());
        assert!(sh.tab_by_name(&name).is_none());
    }

    #[test]
    fn pane_by_name_returns_none_when_absent() {
        let sh = SessionHandles::from_records([(
            SceneHandleName::new("other"),
            rec(HandleKind::Pane, "p-1", Some("ext.view")),
        )]);
        let name = SceneHandleName::new("not-there");
        assert!(sh.pane_by_name::<VX>(&name).is_none());
    }

    #[test]
    fn pane_by_name_returns_pane_when_kind_matches() {
        let name = SceneHandleName::new("editor");
        let sh = SessionHandles::from_records([(
            name.clone(),
            rec(HandleKind::Pane, "pane-7", Some("ext.view")),
        )]);
        let p: Pane<VX> = sh.pane_by_name(&name).expect("pane should be present");
        assert_eq!(p.handle().as_str(), "pane-7");
    }

    #[test]
    fn stack_by_name_returns_stack_when_kind_matches() {
        let name = SceneHandleName::new("agents");
        let sh = SessionHandles::from_records([(
            name.clone(),
            rec(HandleKind::Stack, "stack-3", Some("ext.view")),
        )]);
        let s: Stack<VX> = sh.stack_by_name(&name).expect("stack should be present");
        assert_eq!(s.handle().as_str(), "stack-3");
    }

    #[test]
    fn tab_by_name_returns_tab_when_kind_matches() {
        let name = SceneHandleName::new("main-tab");
        let sh = SessionHandles::from_records([(
            name.clone(),
            rec(HandleKind::Tab, "tab-0", None),
        )]);
        let t: TabHandle = sh.tab_by_name(&name).expect("tab should be present");
        assert_eq!(t.handle().as_str(), "tab-0");
    }

    #[test]
    fn pane_by_name_returns_none_when_kind_is_stack() {
        let name = SceneHandleName::new("mislabeled");
        let sh = SessionHandles::from_records([(
            name.clone(),
            rec(HandleKind::Stack, "stack-x", Some("ext.view")),
        )]);
        assert!(sh.pane_by_name::<VX>(&name).is_none());
    }

    #[test]
    fn stack_by_name_returns_none_when_kind_is_pane() {
        let name = SceneHandleName::new("mislabeled");
        let sh = SessionHandles::from_records([(
            name.clone(),
            rec(HandleKind::Pane, "pane-x", Some("ext.view")),
        )]);
        assert!(sh.stack_by_name::<VX>(&name).is_none());
    }

    #[test]
    fn tab_by_name_returns_none_when_kind_is_pane() {
        let name = SceneHandleName::new("mislabeled");
        let sh = SessionHandles::from_records([(
            name.clone(),
            rec(HandleKind::Pane, "pane-x", Some("ext.view")),
        )]);
        assert!(sh.tab_by_name(&name).is_none());
    }

    #[test]
    fn lookup_is_pure_read_no_rpc() {
        // Sync fn signature is the guarantee — no async, no Transport
        // state. This test asserts the fns can be called from a pure
        // non-async context and return immediately.
        let sh = SessionHandles::default();
        let name = SceneHandleName::new("x");
        // If these were async they wouldn't compile here without .await.
        let _ = sh.pane_by_name::<VX>(&name);
        let _ = sh.stack_by_name::<VX>(&name);
        let _ = sh.tab_by_name(&name);
    }

    #[test]
    fn session_handles_len_and_is_empty() {
        let empty = SessionHandles::default();
        assert_eq!(empty.len(), 0);
        assert!(empty.is_empty());

        let populated = SessionHandles::from_records([
            (
                SceneHandleName::new("a"),
                rec(HandleKind::Pane, "p-a", Some("ext.view")),
            ),
            (
                SceneHandleName::new("b"),
                rec(HandleKind::Tab, "t-b", None),
            ),
        ]);
        assert_eq!(populated.len(), 2);
        assert!(!populated.is_empty());
    }
}
