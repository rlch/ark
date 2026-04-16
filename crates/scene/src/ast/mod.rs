//! AST submodule root. T-003 owns this file; today it wires only the
//! T-009 selector grammar so `EventSelector` / `FieldPattern` / `MatchType`
//! are reachable from outside the crate. T-003 will extend this mod.rs
//! with the scene-root and layout AST node types when it lands; the
//! `pub mod selector;` line below stays.

pub mod selector;
