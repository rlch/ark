//! Filesystem / process watchers that observe cavekit's external surface and
//! translate it into `AgentEvent`s on the shared event bus.
//!
//! Each watcher is exposed as a standalone `pub async fn` with a
//! `cancel: CancellationToken` parameter. Wiring into
//! `CavekitOrchestrator::run` lands in T-083; the watchers here are
//! independently testable.
//!
//! Module layout (one file per kit requirement):
//! - `impl_tracking` (T-077, R4): `context/impl/impl-*.md` markdown table
//!   parser → `TaskDone` + `Progress` events.
//! - `build_site` (T-078, R4): `context/plans/build-site*.md` total-task
//!   extractor consumed by `impl_tracking` as the authoritative
//!   `Progress.total`.
//! - `ralph_loop` (T-079, R5): `.claude/ralph-loop.local.md` key/value
//!   scanner → `Iteration` + `PhaseTransition` events.
//! - `review_tab` (T-080, R6): consumes `PhaseTransition` events from the
//!   bus and spawns/closes a review tab via the multiplexer.
//! - `git_diff` (T-082, R8): `.git/index` watch + 5s poll →
//!   `git diff --numstat HEAD` → `FileEdited` events.
//!
//! See cavekit-orchestrator-cavekit.md R4/R5/R6/R8.

pub mod build_site;
pub mod git_diff;
pub mod impl_tracking;
pub mod ralph_loop;
pub mod review_tab;

pub use build_site::extract_build_site_total;
pub use git_diff::watch_git_diff;
pub use impl_tracking::watch_impl_tracking;
pub use ralph_loop::watch_ralph_loop;
pub use review_tab::{
    default_review_phase_matcher, watch_phase_and_review, watch_phase_and_review_with,
};
