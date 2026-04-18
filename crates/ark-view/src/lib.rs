//! `ark-view` — typed opaque handle surface shared by `ark-ext-proto` and
//! `ark-scene`.
//!
//! This crate is intentionally skeleton-only at T-001 (soul phase 2,
//! cavekit R1). Later tiers (T-004..T-017) populate the `Handle` type, its
//! facet-derived reflection surface, and the typed error variants. The
//! crate sits at the bottom of the scene/extension DAG: it must not depend
//! on `ark-ext-proto` or `ark-scene`, since both of those crates depend on
//! it.

pub mod handle;
pub mod invalidation;
pub mod view;
pub use handle::{HandleId, HandleKind};
pub use invalidation::InvalidationCause;
pub use view::{CommandView, View, ZellijView};
