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
pub use parse::{parse_scene, SceneIR};

pub mod ast;
pub mod cache;
pub mod suggest;
pub mod validate;

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
// T-025: scope builders for spawn / event contexts.
pub mod context;
// T-026 + T-027 + T-028..T-031: view registry + primitives.
pub mod view;
// T-033: typed pane handle wrappers (`CommandPane`, `PluginPane`,
// `TabHandle`) — compile-time inference from `ViewMeta::render_mode`
// lands in T-090's derive macro.
pub mod handle_types;
// T-041..T-044 + T-046: reconciler + mode switching via override-layout
// will land `pub mod reconciler;` here.
