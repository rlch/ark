//! Validation passes over the scene AST.
//!
//! Each pass walks `SceneIR` and collects diagnostics without mutating
//! the tree. Passes are independent and can run in any order.
//!
//! - [`scope`]: R2 scope-rule enforcement — rejects misplaced nodes.
//! - [`handles`]: R2 handle validation — `@ident` grammar + flat namespace dedup.
//! - [`pane_views`]: R3 pane arity — each pane holds exactly one view child.
//! - [`op_refs`]: R7 op handle-reference resolution + handle-type rules.
//! - [`event_fields`]: R4.2 event-field existence checks per `CoreEvent` variant.

pub mod event_fields;
pub mod handles;
pub mod op_refs;
pub mod pane_views;
pub mod scope;
