//! Extension activation and symbol registry (T-094 / R10).
//!
//! The `use "<ext>"` directive in a scene file activates an extension and
//! registers its declared intents, events, and views under a namespaced
//! symbol table. This module provides the [`ExtensionRegistry`] that
//! tracks activated extensions and supports qualified-name lookups at
//! scene compile time.
//!
//! The registry is intentionally decoupled from filesystem resolution
//! (handled by `ark-ext-metadata::search_path`) and from the compose
//! pipeline (T-095 wires the registry into `compose_scene`). This module
//! is pure data — no I/O, no async.

// T-099: extension-pipe-proto binding (protocol mode + render mode wiring).
pub mod binding;
// T-096: extension config ownership validation.
pub mod config;
// T-100: own-namespace-only emission policy.
pub mod emission;
pub mod registry;
// T-095: transitive `use` resolution with cycle detection and topo-sort.
pub mod resolve;

pub use binding::{ExtensionBinding, ProtocolMode, RenderMode, resolve_binding};
pub use config::validate_config;
pub use emission::validate_emission_namespace;
pub use registry::ExtensionRegistry;
pub use resolve::resolve_uses;
