//! Shared protocol surface between `ark-host` and `ark-plugin-sdk`.
//!
//! See `README.md` + `context/plans/build-site-plugin-protocol.md`.
//! Tier 0 scaffold — WIT contracts + custom-section schemas land in
//! Tier 1 tasks T-PP-012..T-PP-017.

pub mod bus;
pub mod errors;
pub mod target;

pub use bus::{BusError, Intent, IntentTarget, PipeMessage, PipeSource};
pub use errors::PluginLoadError;
pub use target::{Target, host_target};

// Re-export the ABI version constants so plugin authors / host code can
// reach them via a single crate (`ark-plugin-protocol`) without also
// pulling `ark-types` into their dep graph.
pub use ark_types::{ARK_ABI_VERSION, AbiError, SUPPORTED_PLUGIN_ABIS};
