//! Compile pipeline — scene AST → emitted artefacts.
//!
//! The compile stage consumes a validated [`crate::ast::SceneNode`] and
//! renders the derivable artefacts a supervisor needs at spawn time.
//! v1 focuses on the layout pipeline (R3 of `cavekit-scene.md`):
//!
//! * [`layout`]  — lower `LayoutNode` → zellij-compatible KDL string via
//!   the `kdl::KdlDocument` builder API, pruning branches whose `when=`
//!   CEL predicate evaluates to false against the static compile-time
//!   context (R3 + R8).
//!
//! Later tiers wire the reaction / plugin / keybind compile steps here
//! once the op registry (T-4.x) and extension merge pass (T-6.x) land.
//! The on-disk writer (`${XDG_RUNTIME_DIR}/ark/layouts/{id}-scene.kdl`)
//! arrives alongside `writer` in T-3.4.

pub mod layout;
pub mod writer;

pub use layout::{CompileContext, compile_layout};
pub use writer::{scene_layout_path, write_scene_layout};
