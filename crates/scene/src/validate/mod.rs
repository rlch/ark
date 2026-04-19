//! Validation passes over the scene AST.
//!
//! Each pass walks `SceneIR` and collects diagnostics without mutating
//! the tree. Passes are independent and can run in any order.
//!
//! - [`scope`]: R2 scope-rule enforcement — rejects misplaced nodes.
//! - [`handles`]: R2 handle validation — `@ident` grammar + flat namespace dedup.
//! - [`pane_views`]: R3 pane arity — each pane holds exactly one view child.
//! - [`op_refs`]: R7 op handle-reference resolution + handle-type rules
//!   (scene-2026-04-18 T-019 adds `spawn_into` / `clear` stack-only
//!   checks via the raw-KDL walker).
//! - [`event_fields`]: R4.2 event-field existence checks per `CoreEvent` variant.
//! - [`view_types`]: scene-2026-04-18 T-018 — cross-checks typed handle
//!   refs (today: `spawn_into @stack { <view> }` inner view) against
//!   the scene-local `ViewTable` per R-8 homogeneous-only.

pub mod event_fields;
pub mod handles;
pub mod op_refs;
pub mod pane_views;
pub mod scope;
pub mod view_types;

// scene-2026-04-18 re-exports so integration tests can write
// `use ark_scene::validate::{validate_scope, validate_handles}` without
// naming the submodule paths directly.
pub use handles::validate_handles;
pub use op_refs::validate_op_refs;
pub use scope::validate_scope;
// scene-2026-04-18 T-020 — view-type validator flagship export. Runs
// after `compile_scene_with_registry` has populated the scene-local
// `ViewTable`; consumed by `ark scene check` + hot-reload gating.
// Deterministic diagnostic ordering is driven by textual source order
// (KDL doc walk).
pub use view_types::validate_view_types;
