//! ClaudeCodeEngine — adapter for Anthropic Claude Code CLI.
//!
//! This crate is the engine layer for Claude Code: hook injection,
//! transcript tailing, permission enforcement, and stall/done detection.
//!
//! Currently implemented: [`settings`] — `.claude/settings.local.json`
//! injection (cavekit-engine-claude-code R1).
//!
//! Future modules (separate tasks): `transcript`, `permission`, `stall`,
//! `handle`, `preflight`.

pub mod settings;

pub use settings::{
    InjectAction, InjectError, InjectReport, RestoreError, inject_hooks, restore_settings,
};
