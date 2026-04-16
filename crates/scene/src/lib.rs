//! Scene: ark's reactive KDL config layer.
//!
//! Preprocessed superset of zellij layout KDL that adds reactions, keybinds,
//! plugin lifecycle, and extension composition via `use`. See
//! `context/kits/cavekit-scene.md` (R1–R17) for the full spec.

pub mod ast;
pub mod cel;
pub mod chord;
pub mod clear;
pub mod compat;
pub mod compile;
pub mod config_schema;
pub mod cycle;
pub mod context;
pub mod engine;
pub mod error;
pub mod extends;
pub mod hook_compat;
pub mod id;
pub mod include;
pub mod intent;
pub mod merge;
pub mod namespace;
pub mod ops;
pub mod parse;
pub mod path;
pub mod plugin;
pub mod plugin_reactions;
pub mod reactions;
pub mod reload;
pub mod selector;
pub mod scope;
pub mod suggest;
pub mod template;
pub mod use_config;
pub mod use_resolution;
pub mod validate;
pub mod wasm_meta;
