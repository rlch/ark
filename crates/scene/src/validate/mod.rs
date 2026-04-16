//! Validation passes over the scene AST.
//!
//! Each pass walks `SceneIR` and collects diagnostics without mutating
//! the tree. Passes are independent and can run in any order.
//!
//! - [`scope`]: R2 scope-rule enforcement — rejects misplaced nodes.
//! - [`handles`]: R2 handle validation — `@ident` grammar + flat namespace dedup.
//! - [`pane_views`]: R3 pane arity — each pane holds exactly one view child.

pub mod handles;
pub mod pane_views;
pub mod scope;
