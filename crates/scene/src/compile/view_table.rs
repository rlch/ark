//! Scene-local view table (scene-2026-04-18 T-013..T-016).
//!
//! The `ViewTable` is a per-compiled-scene `BTreeMap<HandleId, ViewDecl>`
//! that maps every pane / stack handle declared in a scene to the
//! resolved [`ark_view::HandleKind`] + [`crate::view::ViewMeta`] pair
//! used at reaction-dispatch time. Tabs don't get an entry — they carry
//! no view.
//!
//! ## Why distinct from [`crate::compile::view_types::ViewTypeTable`]?
//!
//! - [`ViewTypeTable`](crate::compile::view_types::ViewTypeTable) (Phase
//!   2, T-034) is a **manifest-level** cross-extension registry keyed on
//!   the fully-qualified `<ext>.<view>` token. It's shared across every
//!   scene compile in a given manifest-set epoch.
//! - [`ViewTable`] (scene-2026-04-18, this module) is **per-scene**.
//!   Each compiled scene gets its own table built during
//!   [`crate::compile::compile_scene`] by walking the scene AST and
//!   resolving each pane / stack's `view "<alias>"` reference against
//!   the [`crate::view::ViewRegistry`].
//!
//! Keeping the two types distinct is load-bearing — merging them would
//! confuse "what views does my manifest set declare?" (manifest-level)
//! with "what views do the handles in THIS scene resolve to?"
//! (scene-local). Do NOT merge.
//!
//! ## Visibility discipline (R-10)
//!
//! Both [`ViewTable`] and [`ViewDecl`] are `pub(crate)` only. They do
//! NOT appear on [`crate::compile::CompiledScene`]'s public surface; the
//! sole public accessor is [`crate::intent::IntentContext::view_of`],
//! which returns `Option<&ViewDecl>` for runtime handle -> view
//! re-materialisation (see T-015).
//!
//! Internal compile-pipeline lookups that need the table before
//! `IntentContext` is built go through [`CompiledScene::view_table`] —
//! a `pub(crate)` accessor that never leaks beyond the scene crate.

use ark_view::{HandleId, HandleKind};
use std::collections::BTreeMap;

use crate::view::ViewMeta;

/// Per-scene mapping from an `@handle`'s opaque [`HandleId`] to the
/// scene-local [`ViewDecl`] describing its kind + view metadata.
///
/// Tabs do not get entries — they carry no view (R-8). Panes and
/// stacks each map to one entry; homogeneous-only (R-8) means a stack's
/// entry carries a **single** [`ViewMeta`] for the child view type, not
/// a union.
///
/// `BTreeMap` (over `HashMap`) for deterministic iteration order —
/// important for diagnostic stability and test assertions.
pub(crate) type ViewTable = BTreeMap<HandleId, ViewDecl>;

/// One row in the [`ViewTable`]. Pairs a handle's [`HandleKind`] with
/// the resolved [`ViewMeta`] so the reactions dispatcher can route
/// polymorphic ops (`focus`, `close`) AND re-materialise a typed
/// `Pane<V>` / `Stack<V>` from the opaque [`HandleId`] carried on the
/// wire.
///
/// # Homogeneous-only (R-8)
///
/// `view_meta` is a single [`ViewMeta`], not a list — unions are
/// deferred to v0.2. A stack with an empty body still gets a
/// `ViewDecl` entry carrying its declared child `view_meta`; first
/// `spawn_into @stack { <view> }` validates its inner view against
/// this meta.
///
/// # Visibility (R-10)
///
/// The struct itself is `pub` so the sole public accessor,
/// [`crate::intent::IntentContext::view_of`], can return a
/// `Option<&ViewDecl>` across the crate boundary. The constraint is
/// that [`crate::compile::CompiledScene`] MUST NOT expose the
/// `view_table` field publicly — only the `view_of` runtime accessor
/// and the `pub(crate)` [`crate::compile::CompiledScene::view_table`]
/// helper may reach it.
#[derive(Debug, Clone)]
pub struct ViewDecl {
    /// Whether the handle is a pane or a stack. Always one of
    /// [`HandleKind::Pane`] / [`HandleKind::Stack`] — tabs never appear
    /// in the table.
    pub kind: HandleKind,

    /// Resolved view metadata for this handle. For panes, it's the
    /// pane's `view "<alias>"` resolution. For stacks, it's the
    /// declared child view type (which every member must match per
    /// R-8's homogeneous-only rule).
    pub view_meta: ViewMeta,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::view::{RenderMode, ViewSource};

    fn test_meta(name: &str) -> ViewMeta {
        ViewMeta {
            name: name.to_string(),
            source: ViewSource::Primitive,
            render_mode: RenderMode::CommandView,
            config_schema: None,
        }
    }

    #[test]
    fn view_table_stores_and_retrieves() {
        let mut table: ViewTable = BTreeMap::new();
        table.insert(
            HandleId::new("@editor"),
            ViewDecl {
                kind: HandleKind::Pane,
                view_meta: test_meta("command"),
            },
        );
        let entry = table.get(&HandleId::new("@editor")).expect("present");
        assert_eq!(entry.kind, HandleKind::Pane);
        assert_eq!(entry.view_meta.name, "command");
    }

    #[test]
    fn view_table_deterministic_iteration() {
        // BTreeMap guarantees sorted iteration — load-bearing for
        // stable diagnostics.
        let mut table: ViewTable = BTreeMap::new();
        table.insert(
            HandleId::new("@zebra"),
            ViewDecl {
                kind: HandleKind::Pane,
                view_meta: test_meta("shell"),
            },
        );
        table.insert(
            HandleId::new("@alpha"),
            ViewDecl {
                kind: HandleKind::Stack,
                view_meta: test_meta("command"),
            },
        );
        let keys: Vec<_> = table.keys().map(|k| k.as_str().to_string()).collect();
        assert_eq!(keys, vec!["@alpha", "@zebra"]);
    }

    #[test]
    fn view_decl_carries_stack_kind() {
        let d = ViewDecl {
            kind: HandleKind::Stack,
            view_meta: test_meta("command"),
        };
        assert_eq!(d.kind, HandleKind::Stack);
    }
}
