//! Scene: ark's reactive KDL config layer.
//!
//! Preprocessed superset of zellij layout KDL that adds reactions, keybinds,
//! plugin lifecycle, and extension composition via `use`. See
//! `context/kits/cavekit-scene.md` (R1–R17) for the full spec.

pub mod ast;
pub mod cel;
pub mod chord;
pub mod compile;
pub mod cycle;
pub mod context;
pub mod engine;
pub mod error;
pub mod hook_compat;
pub mod id;
pub mod intent;
pub mod ops;
pub mod parse;
pub mod path;
pub mod plugin;
pub mod plugin_reactions;
pub mod reactions;
pub mod selector;
pub mod scope;
pub mod suggest;
pub mod template;
pub mod validate;
