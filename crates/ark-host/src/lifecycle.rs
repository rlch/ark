//! Lifecycle hook dispatcher.
//!
//! T-PP-040..T-PP-046 (Tier 4): dispatches the 5 WIT exports
//! (`on-install`, `load`, `update`, `render`, `pipe`) with per-hook
//! failure policies. `load` failure unloads; every other failure logs
//! and keeps the plugin alive. No `deactivate` — sudden-death-safe.
//!
//! Tier 0 scaffold — body empty.
