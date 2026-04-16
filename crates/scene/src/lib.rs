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

pub use error::{Result, SceneError};
pub use id::SceneId;

pub mod ast;
