//! ClaudeCodeEngine — adapter for Anthropic Claude Code CLI.
//!
//! This crate is the engine layer for Claude Code: hook injection,
//! transcript tailing, permission enforcement, and stall/done detection.
//!
//! Currently implemented:
//! - [`settings`] — `.claude/settings.local.json` injection (R1)
//! - [`transcript`] — JSONL transcript tailer (R2)
//! - [`permission`] — policy enforcement + always-on event pair (R3)
//! - [`done`] — Stop / SessionEnd → `Done Success` watcher (R4)
//! - [`stall`] — no-activity watcher (R5)
//! - [`handle`] — engine lifecycle token (R6)
//! - [`preflight`] — environment validation (R7)

pub mod done;
pub mod settings;
pub mod transcript;

pub use done::{DoneSignal, done_watcher};
pub use settings::{
    InjectAction, InjectError, InjectReport, RestoreError, inject_hooks, restore_settings,
};
pub use transcript::{
    encode_cwd, parse_line, tail_transcript, tail_transcript_path, transcript_path,
};

pub mod preflight;
pub use preflight::*;

pub mod stall;
pub use stall::stall_watcher;

pub mod handle;
pub use handle::EngineHandle;

pub mod permission;
pub use permission::{
    ParsePermissionPolicyError, PermissionPolicy, READ_ONLY_TOOLS, decide, emit_permission_events,
    read_policy_file, write_policy_file,
};
