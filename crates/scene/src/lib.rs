//! # ark-scene (v3)
//!
//! Fresh v3 scene crate scaffolded by T-001 of `context/plans/build-site-scene.md`.
//!
//! The v2 crate lives at `crates/scene-v2-archive/` (package
//! `ark-scene-v2-archive`) as a frozen reference; this crate reclaims the
//! `ark-scene` name and will be populated by subsequent tasks:
//!
//! - T-003..T-005, T-009: `ast` — facet-derived node types + selector grammar
//! - T-006: `error` — `SceneError` miette diagnostic hierarchy
//! - T-007: `id` — content-hash `SceneId` for hot-reload delta detection
//! - T-011..T-018: `parse` — `parse_scene` + scope / handle validation
//! - T-019..T-025: `rhai`, `interp` — expression-only Rhai engine + `{Rhai}` holes
//!
//! Modules are declared here once they land. Today the crate is intentionally
//! empty — T-001 only establishes the workspace membership and dependency set.

pub mod error;
pub mod id;
pub mod parse;

pub use error::{Result, SceneError};
pub use id::SceneId;
pub use parse::{SceneIR, parse_scene};

// T-041 (soul phase 2 tests R5): re-export the compile-time view-type
// validator so trybuild fixtures carry a short `use ark_scene::validate_scene;`
// line rather than pulling in the `ark-scene-macros` crate directly.
pub use ark_scene_macros::validate_scene;

pub mod ast;
pub mod cache;
pub mod suggest;
pub mod validate;

// ---------------------------------------------------------------------------
// scene-2026-04-18 T-001: re-export typed handle + view surface from
// `ark-view` so downstream crates write `use ark_scene::{Pane, Stack,
// TabHandle, HandleKind, View, CommandView, ZellijView}` without
// reaching into `ark-view` directly. Scene itself also routes internal
// wiring through `ark_view::HandleKind` (narrowed to `{Tab,Pane,Stack}`
// per soul Phase 2 R3/R4) instead of the retired scene-local variants.
// ---------------------------------------------------------------------------
pub use ark_view::{
    CommandView, HandleId, HandleKind, Pane, PaneLike, Stack, TabHandle, View, ZellijView,
};

// T-019 + T-020 + T-021: Rhai expression-only engine wrapper, two-scope
// system, and ark-owned stdlib helpers (`glob`, `matches`, `basename`,
// `dirname`).
pub mod rhai;
// T-022: `{Rhai}` brace-hole interpolation.
pub mod interp;
// T-023 + T-024: `when="<Rhai>"` + full-scene compile pass.
// Layout + mode lowering (T-034..T-040, T-045) will add
// `compile::layout` and `compile::modes` submodules.
pub mod compile;
// T-074: include path resolution + fragment splicing (composition).
pub mod compose;
// T-025: scope builders for spawn / event contexts.
pub mod context;
// T-026 + T-027 + T-028..T-031: view registry + primitives.
pub mod view;
// T-078: namespace enforcement — `<owner>.<name>` for intents/events.
pub mod namespace;
// T-079: load-order enforcement (reaction additive, bind last-wins,
// clear-reactions / clear-bind / disable-extension merge semantics).
pub mod load_order;
// T-041..T-044 + T-046: reconciler + mode switching via override-layout.
pub mod reconciler;
// T-047..T-055: intent registry + op dispatch surface (R7).
pub mod intent;
// T-048..T-051: core op vocabulary implementations.
pub mod ops;
// T-056..T-060: reaction registry + selector matching.
pub mod reactions;
// T-064: chord parsing (R5.2 — `bind` chord grammar).
pub mod chord;
// T-094: extension activation registry — `use "<ext>"` symbol table.
pub mod ext;
// T-110 + T-111: embedded default scene + user override resolution.
pub mod default_scene;
// T-112: file-shape detection — normalizes bare-layout files into
// `scene "default" { … }` before the typed parse pass.
pub mod shape;
// T-113: scene path resolver — pure function for scene file discovery.
pub mod resolve_path;
// T-127 + T-129: scene reload op + re-entry guard.
pub mod reload;
// Compatibility shims: v2 supervisor → v3 scene migration.
pub mod hook_compat;
pub mod plugin_compat;
