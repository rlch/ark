//! ClaudeCodeEngine — adapter for Anthropic Claude Code CLI.
//!
//! This crate is the engine layer for Claude Code: hook injection,
//! transcript tailing, permission enforcement, and stall/done detection.
//!
//! Currently implemented:
//! - [`settings`] — `.claude/settings.local.json` injection (R1)
//! - [`transcript`] — JSONL transcript tailer (R2)
//! - [`done`] — Stop / SessionEnd → `Done Success` watcher (R4)
//! - [`preflight`] — environment validation (R7)
//!
//! Future modules (separate tasks): `permission`, `stall`, `handle`.

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
