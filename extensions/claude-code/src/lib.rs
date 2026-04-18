//! `ark-ext-claude-code` — Claude Code integration extension for ark.
//!
//! This crate ships a single-crate extension that bridges Anthropic's
//! Claude Code CLI into ark's event bus via a hook subprocess + a
//! transcript tail. Per the 2026-04-18 pivot, this is the v0.1 first-class
//! AI-agent integration (the broader `pi` extension family is deferred to
//! v0.2). See `context/kits/cavekit-claude-code.md`.
//!
//! # Components (populated across Tiers 1..N)
//!
//! * [`ClaudeCodeExtension`] — the `ArkExtension` impl. v0.1 surface
//!   covers scene-compile hook registration, session-start / session-end
//!   lifecycle, a `claude-code` `CommandView`, and a `claude-code-subagent`
//!   stack view. See build-site-claude-code-ext.md for the task graph.
//! * `bin/cc-hook/main.rs` (binary target: `cc-hook`) — a subprocess
//!   invoked by `~/.claude/settings.json` hooks. POSTs a single NDJSON
//!   line per hook invocation to the per-session ark socket at
//!   `$STATE/sessions/<sid>/cc-hook.sock`, then exits. Write-only — no
//!   reverse messages. Per kit R1 + R2.
//!
//! # Non-goal marker — MCP control surface
//!
//! Per kit Non-goals §: do NOT restore the pre-2026-04-18
//! `crates/types/src/permission.rs` (`READ_ONLY_TOOLS`,
//! `PermissionPolicy`, `POLICY_FILE_NAME`) as part of v0.1. That surface
//! belongs to the deferred MCP control-plane work (v0.2-stretch); the
//! original implementation remains in git history for salvage if/when
//! that lands. T-008 (build site) tracks this flag.
//!
//! # Dispatch defaults
//!
//! Every method on [`ark_ext_proto::ArkExtension`] has a default
//! implementation that either returns `method_not_found` or an empty
//! response struct. This crate overrides only the methods it supports —
//! T-020 onwards populates them. Until then, the `impl ArkExtension`
//! block is deliberately empty + all behaviour inherits trait defaults.

#![deny(missing_docs)]

use ark_ext_proto::ArkExtension;
use async_trait::async_trait;

/// The Claude Code extension — an [`ArkExtension`] implementation that
/// registers scene-compile hooks, wires per-session hook sockets, and
/// exposes the `claude-code` + `claude-code-subagent` views.
///
/// v0.1 scaffolding: a zero-sized unit struct. Per-session state
/// (transcript watchers, socket tasks, subagent stack snapshots) lands
/// in later tiers — T-019+ swap this for a struct carrying the live
/// handles, behind the same `impl ArkExtension` surface.
///
/// All trait methods currently inherit their defaults from the upstream
/// trait definition in `ark-ext-proto` — see the module doc for the
/// tier-by-tier population plan.
#[derive(Debug, Default, Clone, Copy)]
pub struct ClaudeCodeExtension;

impl ClaudeCodeExtension {
    /// Construct a new [`ClaudeCodeExtension`]. Zero-cost — the struct
    /// holds no state in v0.1 scaffolding. T-019+ replace this with a
    /// constructor that seeds per-session bookkeeping.
    pub fn new() -> Self {
        Self
    }
}

/// `ArkExtension` implementation for the Claude Code extension.
///
/// T-003 scaffolding: every method stays at its trait default. The
/// upstream trait returns `method_not_found` for the methods that an
/// extension MUST override (e.g. `initialize`) and `Ok(Default)` for
/// the notification-style methods. This crate's overrides land
/// incrementally in T-020..T-045+ per `build-site-claude-code-ext.md`.
#[async_trait]
impl ArkExtension for ClaudeCodeExtension {
    // Intentionally empty — see module doc.
}
