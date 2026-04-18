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
//! * [`hook_event::HookEvent`] + [`hook_payload::HookPayload`] — typed
//!   surface covering Claude Code's 10 hook names (R1) and their
//!   NDJSON payload shape (R2). [`hook_payload::payload_to_ext_event`]
//!   is the R3 translator that produces `claude-code.<kind>` ExtEvents.
//!   Salvaged from the pre-2026-04-18 `crates/hook/` crate (T-004, T-005).
//! * [`transcript`] — R8 transcript fs-watcher + tail cursor primitives.
//!   `notify`-based recursive watch + byte-offset JSONL tail, survives
//!   truncation. Salvaged from the pre-2026-04-18
//!   `crates/orchestrators/claude-code/` crate with all orchestrator-
//!   trait surface stripped (T-007).
//! * `bin/cc-hook/main.rs` (binary target: `cc-hook`) — subprocess
//!   invoked by `~/.claude/settings.json` hooks. POSTs a single NDJSON
//!   line per hook invocation to the per-session ark socket at
//!   `$STATE/sessions/<sid>/cc-hook.sock`, then exits. Write-only — no
//!   reverse messages. Per kit R1 + R2. See T-006 for the salvaged
//!   implementation.
//!
//! # Non-goal marker — MCP control surface (T-008)
//!
//! NOTE: per cavekit-claude-code §Non-goals, permission/policy types
//! (`READ_ONLY_TOOLS` / `PermissionPolicy` / `POLICY_FILE_NAME`) are NOT
//! restored in v0.1. Preserved in git history only. Stretch: MCP server.
//!
//! Previous `crates/types/src/permission.rs` carried a tri-state
//! policy (`Ask` / `AutoApproveRead` / `AutoApproveAll`) plus a
//! `READ_ONLY_TOOLS` list and a `POLICY_FILE_NAME` constant; the old
//! `ark-hook` crate consulted those to decide whether to write Claude
//! Code's allow-payload to stdout. The claude-code extension deliberately
//! delegates that entire surface to Claude's in-TUI permission prompt —
//! users stay in one mental model and ark observes rather than mediates.
//! If/when the v0.2-stretch MCP server lands the surface re-enters via a
//! different mechanism (runtime tool injection rather than stdout
//! hook-payload substitution).
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

pub mod hook_event;
pub mod hook_payload;
pub mod transcript;

pub use hook_event::{HookEvent, UnknownHookEvent};
pub use hook_payload::{EXT_NAME, HookPayload, NdjsonLine, flat_event_name, payload_to_ext_event};
pub use transcript::{TailCursor, TranscriptEvent, TranscriptWatcher, encode_cwd, start_watcher};

/// T-008a stub: embedded `cc-hook` binary bytes for `doctor --fix` to
/// extract to `$XDG_BIN_HOME/cc-hook` at mode `0755`.
///
/// **Current state (PARTIAL):** this is an empty byte-slice placeholder.
/// A real embedding via `crates/cli/build.rs` → `include_bytes!` would
/// require running `cargo build --release --bin cc-hook -p
/// ark-ext-claude-code` from inside `ark-cli`'s build.rs, which risks
/// circular / deadlock contention with the outer `cargo build
/// --workspace` (the same pathology that motivated the
/// wasm-plugin fallback stack — see `crates/cli/build.rs` F-709).
///
/// The downstream tasks that consume this symbol (T-019, T-023,
/// `doctor --fix`) still compile cleanly against `&[]`; they check
/// `.is_empty()` and surface a helpful "cc-hook bytes not embedded;
/// install via `cargo install --bin cc-hook -p ark-ext-claude-code`"
/// message when the real artifact is absent. A future task can replace
/// this stub with a real `include_bytes!` from a build.rs that uses an
/// isolated `CARGO_TARGET_DIR` under `$OUT_DIR` (mirroring the wasm
/// plugin pattern).
///
/// TODO(T-008a-real): wire real embedding via `crates/cli/build.rs`
/// following the wasm plugin pattern. Blocked on confirming the
/// isolated-target-dir approach works for a native binary under cargo's
/// workspace member resolution — the wasm plugins live in a separate
/// target triple (`wasm32-wasip1`), so the build.rs can't conflict with
/// the outer cargo invocation. A native `cc-hook` built from inside the
/// outer `cargo build --workspace` sits in the same target graph and
/// risks the deadlock pathology F-709 documents. Alternative:
/// `cargo install --bin cc-hook` during packaging (cargo-dist),
/// dropping the embedding requirement entirely.
pub const CC_HOOK_BYTES: &[u8] = &[];

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
