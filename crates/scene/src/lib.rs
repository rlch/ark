//! Scene: ark's reactive KDL config layer.
//!
//! Preprocessed superset of zellij layout KDL that adds reactions, keybinds,
//! plugin lifecycle, and extension composition via `use`. See
//! `context/kits/cavekit-scene.md` (R1–R17) for the full spec.

pub mod ast;
pub mod error;
pub mod id;
pub mod parse;
pub mod scope;
pub mod suggest;
