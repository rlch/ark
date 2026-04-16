//! Validation passes over the scene AST.
//!
//! Each pass walks `SceneIR` and collects diagnostics without mutating
//! the tree. Passes are independent and can run in any order.
//!
//! - [`scope`]: R2 scope-rule enforcement — rejects misplaced nodes.
//! - [`handles`]: R2 handle validation — `@ident` grammar + flat namespace dedup.

pub mod handles;
pub mod scope;
