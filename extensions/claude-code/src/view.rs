//! T-029 / T-030 / T-034 (claude-code-ext R5 + R6) — view structs for the
//! Claude Code extension.
//!
//! This module pins the typed view surface the scene author wires up when
//! they write `use "claude-code"` in a scene file:
//!
//! * [`ClaudeCodeView`] — a `CommandView` that backs a `claude` subprocess
//!   pane. Carries optional `model`, `args`, `cwd`, and an optional typed
//!   `Stack<ClaudeCodeSubagent>` handle the extension fans subagent
//!   events into (R7, T-038). Renders the `claude-code` view name
//!   (kebab-cased from the struct identifier by `#[derive(View)]`).
//!
//! * [`ClaudeCodeSubagentView`] — a `CommandView` that backs the stack
//!   children spawned from `subagents`. Carries spawner-set `id` +
//!   `transcript_path`. Alias: `claude-code-subagent` (kebab-cased from
//!   the struct identifier).
//!
//! * [`ClaudeCodeSubagent`] — zero-sized marker used as the generic
//!   parameter for `Stack<ClaudeCodeSubagent>`. The ACTUAL view struct
//!   is [`ClaudeCodeSubagentView`]; the marker exists so the scene
//!   compiler can distinguish a `Stack<ClaudeCodeSubagent>` handle from
//!   any other stack at the Rust type level. T-031 fixtures assert that
//!   only stacks of this marker are accepted by the `subagents` attribute.
//!
//! # `#[derive(View)]` shape
//!
//! `#[derive(View)]` (from `ark-ext-derive`) only accepts `name` +
//! `description` attributes in v0.1 — there is no `kind`, `alias`, or
//! `type` key. The v0.1 derive stamps pane-kind views only; the manifest-
//! level `kind` discriminant (pane vs stack) lives in `extension.kdl`
//! (authored by hand per ark-ext-derive's "derive is a convenience, not a
//! gate" rule). The kebab-cased struct name IS the view alias, so
//! `ClaudeCodeView` → `"claude-code"` and `ClaudeCodeSubagentView` →
//! `"claude-code-subagent"` without additional annotation.
//!
//! `#[derive(CommandView)]` is a body-less marker derive that stamps
//! `impl CommandView for T {}`. The trait itself has no required methods
//! (the affordance methods — `env`, `write_stdin`, `pid` — live on
//! `Pane<V: CommandView>` inherent impls); T-030's "argv / env / cwd"
//! construction surface therefore lives on [`ClaudeCodeView`] as
//! INHERENT methods the spawner calls when launching the subprocess.
//! Widening the `CommandView` trait with required methods would require
//! editing `ark-view` (out of scope for this task per the kit constraint).

use ark_ext_derive::{CommandView, View};
use ark_view::Stack;
use std::collections::BTreeMap;
use std::path::PathBuf;

// ---------------------------------------------------------------------------
// T-035 / T-036 / T-037 subagent state machinery
// ---------------------------------------------------------------------------

/// T-035 subagent status as tracked by [`SubagentState`]. Derived from
/// the Claude Code hook stream: `SubagentStart` → [`Running`], followed
/// by `SubagentStop` which carries a `success` flag — `true` →
/// [`Done`], `false` → [`Failed`]. The three variants are the stable
/// title-fragment strings the kit R6 format string interpolates.
///
/// [`Running`]: SubagentStatus::Running
/// [`Done`]: SubagentStatus::Done
/// [`Failed`]: SubagentStatus::Failed
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SubagentStatus {
    /// Subagent is live — `SubagentStart` fired, no `SubagentStop` yet.
    Running,
    /// Subagent finished successfully.
    Done,
    /// Subagent finished with a failure (any non-success `SubagentStop`
    /// per the Claude Code hook shape).
    Failed,
}

impl SubagentStatus {
    /// Kebab-cased wire form used inside the R6 title format.
    /// Pinned as stable strings so scene-author-visible title text
    /// doesn't drift across refactors.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Done => "done",
            Self::Failed => "failed",
        }
    }
}

impl std::fmt::Display for SubagentStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Per-subagent state cached inside the extension — enough to re-render
/// the R6 pane title on every relevant hook event.
///
/// `agent_type` is captured at `SubagentStart` time and persists through
/// the subagent lifecycle. `status` starts as [`SubagentStatus::Running`]
/// and flips to `Done` / `Failed` at `SubagentStop`. `last_tool` tracks
/// the most recent `PreToolUse` name; `None` until the first tool call
/// fires.
#[derive(Debug, Clone)]
pub struct SubagentState {
    /// Subagent id from `SubagentStart.agent_id` (and `SubagentStop.agent_id`).
    pub id: String,
    /// Subagent type from `SubagentStart.agent_type`.
    pub agent_type: String,
    /// Current status — transitions per hook events. Starts `Running`.
    pub status: SubagentStatus,
    /// Name of the most recent tool invoked by this subagent (from
    /// `PreToolUse.tool_name`). `None` until the first tool call.
    pub last_tool: Option<String>,
}

impl SubagentState {
    /// Fresh state at `SubagentStart` time. `last_tool` starts `None`
    /// because tool calls are reported via subsequent `PreToolUse`
    /// events.
    pub fn new(id: impl Into<String>, agent_type: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            agent_type: agent_type.into(),
            status: SubagentStatus::Running,
            last_tool: None,
        }
    }

    /// Render the zellij `RenamePane` title for this subagent's current
    /// state. Pure function over [`SubagentState`] — no IO. See
    /// [`format_subagent_title`].
    pub fn render_title(&self) -> String {
        format_subagent_title(&self.agent_type, self.status, self.last_tool.as_deref())
    }
}

/// R6 title format: `"{agent_type} · {status} · {last_tool}"`.
///
/// * `last_tool` truncated (char-wise) to 16 chars; `None` renders as
///   the literal `"-"` placeholder so the structure of the title stays
///   predictable for the user.
/// * Full title truncated (char-wise) to 60 chars.
///
/// Truncation appends the single-char ellipsis `"…"` when content was
/// dropped — so truncated-to-16 `last_tool` becomes 17 chars including
/// the ellipsis, and a truncated-to-60 full title likewise lands at 61.
/// The kit pins "truncated to N chars"; we interpret that as an
/// inclusive budget of N chars of content plus the trailing ellipsis
/// when truncation actually occurred (matches common CLI/TUI conventions).
///
/// Char-wise truncation (not byte-wise) is deliberate — the
/// middle-dot separator `·` is multi-byte in UTF-8, as are many agent-
/// type names a user might author.
pub fn format_subagent_title(
    agent_type: &str,
    status: SubagentStatus,
    last_tool: Option<&str>,
) -> String {
    const LAST_TOOL_MAX: usize = 16;
    const TOTAL_MAX: usize = 60;

    let last = last_tool.unwrap_or("-");
    let truncated_last = truncate_chars(last, LAST_TOOL_MAX);
    let raw = format!("{} · {} · {}", agent_type, status, truncated_last);
    truncate_chars(&raw, TOTAL_MAX)
}

/// Truncate `s` to at most `max` CHARS (unicode scalar values), appending
/// a trailing `"…"` when truncation actually occurred. `s.chars().count()
/// <= max` returns `s` unchanged. The returned string owns its bytes.
fn truncate_chars(s: &str, max: usize) -> String {
    let count = s.chars().count();
    if count <= max {
        s.to_string()
    } else {
        let mut out: String = s.chars().take(max).collect();
        out.push('…');
        out
    }
}

// ---------------------------------------------------------------------------
// T-029: ClaudeCodeView
// ---------------------------------------------------------------------------

/// The `claude-code` view (alias derived kebab-case from the struct
/// identifier by `#[derive(View)]`). Renders as a subprocess pane running
/// the `claude` CLI — Anthropic's Claude Code command-line UI.
///
/// Scene authors bind this view to a pane handle:
///
/// ```ignore
/// pane @chat {
///     view "claude-code.claude-code" model="claude-sonnet-4-6" subagents=@subs
/// }
/// stack @subs {
///     view "claude-code.claude-code-subagent"
/// }
/// ```
///
/// The `subagents` attribute (when present) is wired to a
/// `Stack<ClaudeCodeSubagent>` handle — T-038 calls
/// `subagents.spawn_pane(...)` on each `claude-code.subagent.start`
/// event to fan out one stack child per running subagent. When absent
/// (`None`), subagent events still flow to user reactions but no typed
/// fan-out occurs (T-040 covers the untyped path).
///
/// # Environment / cwd rendering
///
/// * argv: `["claude", "--model", <model>?, <args...>]` (T-030).
/// * env: pane env wrapper + `CLAUDE_HOOK_SOCKET=<session-sock-path>`
///   (T-030). The hook-socket path plumbs through from on-session-start
///   (T-011) via a parameter on the builder method — the view struct
///   itself does NOT cache the path so the same view value can be re-
///   instantiated across sessions without state aliasing.
/// * cwd: author-supplied `cwd` Rhai-rendered string (when `Some`) OR
///   the session default (when `None`).
#[derive(Debug, Default, Clone, View, CommandView)]
pub struct ClaudeCodeView {
    /// Optional model passed to `claude --model <model>`. When `None`,
    /// claude's own default model applies (no `--model` flag injected).
    pub model: Option<String>,

    /// Additional command-line args appended after any `--model …`.
    /// Renders verbatim in argv order.
    pub args: Vec<String>,

    /// Optional working directory Rhai-rendered at scene-compile time.
    /// When `None`, the session default cwd is used at launch.
    pub cwd: Option<String>,

    /// Optional typed handle to a stack of [`ClaudeCodeSubagentView`]
    /// children. When `Some`, T-038 fans out a stack child per
    /// `claude-code.subagent.start` event. When `None`, no typed fan-out;
    /// events still flow through the normal extension event path for
    /// Rhai reactions (T-040).
    ///
    /// The handle's V-parameter is [`ClaudeCodeSubagent`] (a zero-sized
    /// marker), NOT [`ClaudeCodeSubagentView`] directly. This mirrors
    /// the pattern `Stack<V: View>` uses elsewhere and keeps the marker
    /// a narrow type-system anchor the scene compiler can assert on.
    pub subagents: Option<Stack<ClaudeCodeSubagent>>,
}

impl ClaudeCodeView {
    /// Build the argv the spawner passes to the `claude` subprocess
    /// (T-030).
    ///
    /// Shape: `["claude", "--model", <model>?, <args...>]`.
    /// * `--model <model>` pair is injected ONLY when `self.model` is
    ///   `Some(_)`.
    /// * `self.args` is appended verbatim; its order is preserved.
    ///
    /// No shell expansion, no env substitution — the caller is expected
    /// to have already Rhai-rendered any dynamic args before constructing
    /// the view.
    pub fn build_argv(&self) -> Vec<String> {
        let mut argv =
            Vec::with_capacity(1 + if self.model.is_some() { 2 } else { 0 } + self.args.len());
        argv.push("claude".to_string());
        if let Some(m) = &self.model {
            argv.push("--model".to_string());
            argv.push(m.clone());
        }
        argv.extend(self.args.iter().cloned());
        argv
    }

    /// Build the env map the spawner hands to the `claude` subprocess
    /// (T-030).
    ///
    /// Layered:
    /// 1. The caller-supplied `pane_env` (per-session / per-scene env
    ///    wrapper from the spawner).
    /// 2. `CLAUDE_HOOK_SOCKET=<cc_hook_socket_path>` — the absolute path
    ///    to the per-session cc-hook unix socket bound in
    ///    `on_session_start` (T-011). The `cc-hook` binary consults this
    ///    env var to find the socket it POSTs NDJSON frames at (per
    ///    cc-hook's settings.json template — `<cmd> --socket
    ///    $CLAUDE_HOOK_SOCKET`). Wiring via env (not via argv) lets
    ///    downstream users rewrite cc-hook's invocation without
    ///    touching every hook entry's command template.
    ///
    /// The `pane_env` layer is merged in FIRST; the `CLAUDE_HOOK_SOCKET`
    /// entry overrides any pane-provided value of the same key. This
    /// ordering is deliberate: the extension authors the hook-socket
    /// contract and must NOT be silently overridden by a scene-author-
    /// supplied env entry.
    pub fn build_env(
        &self,
        pane_env: BTreeMap<String, String>,
        cc_hook_socket_path: &std::path::Path,
    ) -> BTreeMap<String, String> {
        let mut env = pane_env;
        env.insert(
            "CLAUDE_HOOK_SOCKET".to_string(),
            cc_hook_socket_path.to_string_lossy().into_owned(),
        );
        env
    }

    /// Resolve the cwd the spawner uses for the `claude` subprocess
    /// (T-030).
    ///
    /// Returns the author-supplied `self.cwd` (converted to `PathBuf`)
    /// when `Some`, otherwise the caller-supplied session default.
    pub fn resolve_cwd(&self, session_default_cwd: &std::path::Path) -> PathBuf {
        self.cwd
            .as_ref()
            .map(PathBuf::from)
            .unwrap_or_else(|| session_default_cwd.to_path_buf())
    }
}

// ---------------------------------------------------------------------------
// T-029: ClaudeCodeSubagent marker
// ---------------------------------------------------------------------------

/// Zero-sized marker type used as the generic parameter for
/// `Stack<ClaudeCodeSubagent>` on [`ClaudeCodeView::subagents`].
///
/// The actual renderer is [`ClaudeCodeSubagentView`]; this marker exists
/// solely so the scene compiler (via `scene-macros`' KDL-level
/// `validate_scene!` + the runtime `ViewTypeTable`) can distinguish a
/// stack of Claude Code subagents from any other stack at the Rust type
/// level. Handle-kind validation (T-031) asserts the `subagents`
/// attribute is wired to `Stack<ClaudeCodeSubagent>` — nothing else.
///
/// An empty struct (rather than `PhantomData<T>`) is the right shape:
/// `Send + Sync + 'static` fall out automatically for an empty struct,
/// the type carries no runtime footprint, and any derive we might want
/// later (`Debug`, `Clone`, etc.) composes without `where T: …` bounds
/// bleeding in.
#[derive(Debug, Default, Clone, Copy)]
pub struct ClaudeCodeSubagent;

impl ark_view::View for ClaudeCodeSubagent {}

// ---------------------------------------------------------------------------
// T-034: ClaudeCodeSubagentView
// ---------------------------------------------------------------------------

/// The `claude-code-subagent` view (alias derived kebab-case from the
/// struct identifier). Renders as a subprocess pane tailing a single
/// subagent's transcript.
///
/// Both fields are SPAWNER-SET, not author-set. Scene authors do NOT
/// provide these values in KDL — they appear solely via
/// `Stack<ClaudeCodeSubagent>::spawn_pane(ClaudeCodeSubagentAttrs { id,
/// transcript_path })` during T-038's fan-out.
///
/// The view is declared as `CommandView` because its rendering path
/// launches a subprocess (a tail over `transcript_path` in T-036's
/// expanded rendering).
#[derive(Debug, Default, Clone, View, CommandView)]
pub struct ClaudeCodeSubagentView {
    /// Subagent id (from `claude-code.subagent.start`'s
    /// `agent_id`). Spawner-set — NOT author-set.
    pub id: String,

    /// Absolute transcript path (from `claude-code.subagent.start`'s
    /// `agent_transcript_path`). Spawner-set — NOT author-set.
    pub transcript_path: String,
}

impl ClaudeCodeSubagentView {
    /// T-035 (R6 collapsed rendering): build the JSON payload the
    /// extension passes through `pane/emit` to trigger a zellij
    /// `RenamePane` for this subagent's pane.
    ///
    /// Shape:
    ///
    /// ```json
    /// { "kind": "RenamePane", "name": "<title>" }
    /// ```
    ///
    /// `title` is the R6 `"{agent_type} · {status} · {last_tool}"`
    /// format rendered by [`SubagentState::render_title`].
    ///
    /// Pure — no IO, no side effects. Caller owns the state + the
    /// handle the payload is emitted against. `pane/emit` dispatch
    /// itself lives in the host and is exercised through the
    /// `ArkExtension::pane_emit` trait method on the supervisor side
    /// when wiring lands (T-046). v0.1 exposes the payload builder so
    /// downstream wiring has a stable call point.
    pub fn rename_pane_payload(state: &SubagentState) -> serde_json::Value {
        serde_json::json!({
            "kind": "RenamePane",
            "name": state.render_title(),
        })
    }

    /// T-036 (R6 expanded rendering, minimal): pull + format the last
    /// `tail_lines` of the transcript file for display.
    ///
    /// The pure rendering path chosen for v0.1 is a plain-text log
    /// formatter over the transcript's JSONL, bounded by
    /// `tail_lines`. Each line is:
    ///
    /// * `assistant: <text>` for `{"type":"message","role":"assistant",
    ///   "content":[{"type":"text","text":...}]}` records.
    /// * `tool_use: <name>(<input-json>)` for
    ///   `{"type":"tool_use","name":...,"input":...}` records (which
    ///   may appear nested inside an assistant message's `content`).
    /// * `-- <raw-line>` for any JSONL shape the minimal formatter
    ///   doesn't recognise — kept so the user isn't staring at a blank
    ///   pane when Claude introduces new shapes.
    ///
    /// A full ratatui `TextArea`-backed widget is deferred — the kit
    /// R6 wants ratatui-backed rendering, but the host `pane_emit`
    /// surface carries JSON, not a ratatui `Frame`. The minimal text
    /// formatter is the shape the host can stream straight through;
    /// the ratatui hookup is a presentation-layer concern for the
    /// supervisor-side stack renderer (flagged as a Tier-8 / T-048
    /// concern in the ledger).
    ///
    /// `cursor` is taken `&mut` so the caller can persist the byte
    /// offset across polls — `TailCursor` advances atomically on
    /// successful reads.
    ///
    /// Returns the formatted text window: at most `tail_lines` entries
    /// drawn from the most recent transcript lines. On truncation /
    /// rotation `TailCursor` resets to `0` and the whole file is
    /// re-read — this method then keeps the last `tail_lines` of the
    /// rebuilt stream, matching the kit's "survives truncation" claim.
    pub fn render_transcript_tail(
        cursor: &mut crate::transcript::TailCursor,
        tail_lines: usize,
    ) -> std::io::Result<Vec<String>> {
        let lines = cursor.poll_new_lines()?;
        Ok(format_transcript_lines(&lines, tail_lines))
    }
}

/// Format a batch of freshly-polled JSONL transcript lines into the
/// plain-text log shape [`ClaudeCodeSubagentView::render_transcript_tail`]
/// returns, bounded to the last `tail_lines` entries.
///
/// Exposed as a free function so tests can exercise it without touching
/// the filesystem / a real [`TailCursor`][crate::transcript::TailCursor].
pub fn format_transcript_lines(lines: &[String], tail_lines: usize) -> Vec<String> {
    let mut out = Vec::with_capacity(lines.len().min(tail_lines));
    for raw in lines {
        let trimmed = raw.trim_end_matches(['\n', '\r']);
        if trimmed.is_empty() {
            continue;
        }
        let rendered = render_transcript_line(trimmed).unwrap_or_else(|| format!("-- {trimmed}"));
        out.push(rendered);
    }
    if out.len() > tail_lines {
        let drop = out.len() - tail_lines;
        out.drain(0..drop);
    }
    out
}

/// Try to render a single JSONL line in the minimal formatter's known
/// shapes. Returns `None` if the line doesn't parse as JSON or doesn't
/// match any recognised shape — the caller falls back to the raw-line
/// prefix.
fn render_transcript_line(raw: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(raw).ok()?;
    let obj = v.as_object()?;

    // Shape A: assistant message.
    let type_field = obj.get("type").and_then(|t| t.as_str());
    if type_field == Some("message")
        && obj.get("role").and_then(|r| r.as_str()) == Some("assistant")
    {
        // content may be a string OR an array of content blocks.
        if let Some(content) = obj.get("content") {
            if let Some(s) = content.as_str() {
                return Some(format!("assistant: {s}"));
            }
            if let Some(arr) = content.as_array() {
                // Pull first text block; flag tool_use blocks.
                let mut pieces: Vec<String> = Vec::new();
                for block in arr {
                    let Some(bobj) = block.as_object() else {
                        continue;
                    };
                    match bobj.get("type").and_then(|t| t.as_str()) {
                        Some("text") => {
                            if let Some(t) = bobj.get("text").and_then(|t| t.as_str()) {
                                pieces.push(format!("assistant: {t}"));
                            }
                        }
                        Some("tool_use") => {
                            let name = bobj.get("name").and_then(|n| n.as_str()).unwrap_or("?");
                            let input = bobj
                                .get("input")
                                .map(|i| i.to_string())
                                .unwrap_or_else(|| "{}".to_string());
                            pieces.push(format!("tool_use: {name}({input})"));
                        }
                        _ => {}
                    }
                }
                if !pieces.is_empty() {
                    return Some(pieces.join(" | "));
                }
            }
        }
    }

    // Shape B: top-level tool_use record.
    if type_field == Some("tool_use") {
        let name = obj.get("name").and_then(|n| n.as_str()).unwrap_or("?");
        let input = obj
            .get("input")
            .map(|i| i.to_string())
            .unwrap_or_else(|| "{}".to_string());
        return Some(format!("tool_use: {name}({input})"));
    }

    None
}

/// Attrs passed to `Stack<ClaudeCodeSubagent>::spawn_pane` (T-038 will
/// wire this through `spawn_pane` once `PaneAttrs` grows per-view
/// customisation — v0.1 `PaneAttrs` is deliberately empty per
/// `ark-view::typed::PaneAttrs`).
///
/// Captured as a dedicated struct now so T-038 has a stable shape to
/// bind against; additions are MINOR-compatible via `..Default::default()`
/// struct update syntax.
#[derive(Debug, Default, Clone)]
pub struct ClaudeCodeSubagentAttrs {
    /// Subagent id — populates [`ClaudeCodeSubagentView::id`].
    pub id: String,

    /// Absolute transcript path — populates
    /// [`ClaudeCodeSubagentView::transcript_path`].
    pub transcript_path: String,
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use ark_view::{CommandView, View};

    // -- T-029: view trait impls present via derive --------------------------

    // Prove ClaudeCodeView satisfies both View and CommandView at the
    // type level. Done via these _assert_* shims so rustc flags a
    // regression with an unambiguous "required trait not implemented"
    // pointing at the derive.
    #[allow(dead_code)]
    fn assert_view<T: View>() {}
    #[allow(dead_code)]
    fn assert_command_view<T: CommandView>() {}

    #[test]
    fn claude_code_view_is_a_view_and_command_view() {
        assert_view::<ClaudeCodeView>();
        assert_command_view::<ClaudeCodeView>();
    }

    #[test]
    fn claude_code_subagent_view_is_a_view_and_command_view() {
        assert_view::<ClaudeCodeSubagentView>();
        assert_command_view::<ClaudeCodeSubagentView>();
    }

    #[test]
    fn claude_code_subagent_marker_is_a_view() {
        // The zero-sized marker satisfies `View` so it can parameterise
        // `Stack<V: View>`. See T-031 handle-kind validation.
        assert_view::<ClaudeCodeSubagent>();
    }

    // -- T-030: argv + env + cwd construction --------------------------------

    #[test]
    fn build_argv_with_model_and_args() {
        let v = ClaudeCodeView {
            model: Some("claude-sonnet-4-6".to_string()),
            args: vec!["--print".to_string(), "--verbose".to_string()],
            cwd: None,
            subagents: None,
        };
        assert_eq!(
            v.build_argv(),
            vec![
                "claude".to_string(),
                "--model".to_string(),
                "claude-sonnet-4-6".to_string(),
                "--print".to_string(),
                "--verbose".to_string(),
            ]
        );
    }

    #[test]
    fn build_argv_without_model_omits_flag() {
        let v = ClaudeCodeView {
            model: None,
            args: vec!["--headless".to_string()],
            cwd: None,
            subagents: None,
        };
        assert_eq!(
            v.build_argv(),
            vec!["claude".to_string(), "--headless".to_string()]
        );
    }

    #[test]
    fn build_argv_with_no_args_and_no_model() {
        let v = ClaudeCodeView::default();
        assert_eq!(v.build_argv(), vec!["claude".to_string()]);
    }

    #[test]
    fn build_env_injects_claude_hook_socket() {
        let v = ClaudeCodeView::default();
        let pane_env = {
            let mut m = BTreeMap::new();
            m.insert("TERM".to_string(), "xterm-256color".to_string());
            m
        };
        let sock = std::path::Path::new("/state/sessions/abc/cc-hook.sock");
        let env = v.build_env(pane_env, sock);
        assert_eq!(
            env.get("CLAUDE_HOOK_SOCKET").map(|s| s.as_str()),
            Some("/state/sessions/abc/cc-hook.sock")
        );
        // Pane env is preserved — only the hook-socket key is added on
        // top.
        assert_eq!(env.get("TERM").map(|s| s.as_str()), Some("xterm-256color"));
    }

    #[test]
    fn build_env_overrides_user_supplied_claude_hook_socket() {
        // Safety contract: the extension is the sole author of the
        // CLAUDE_HOOK_SOCKET value. A scene-author-supplied value in the
        // pane env wrapper MUST be overridden — otherwise a buggy scene
        // could accidentally shadow the real hook socket and silently
        // lose events.
        let v = ClaudeCodeView::default();
        let pane_env = {
            let mut m = BTreeMap::new();
            m.insert(
                "CLAUDE_HOOK_SOCKET".to_string(),
                "/tmp/evil.sock".to_string(),
            );
            m
        };
        let sock = std::path::Path::new("/state/sessions/abc/cc-hook.sock");
        let env = v.build_env(pane_env, sock);
        assert_eq!(
            env.get("CLAUDE_HOOK_SOCKET").map(|s| s.as_str()),
            Some("/state/sessions/abc/cc-hook.sock")
        );
    }

    #[test]
    fn resolve_cwd_returns_author_supplied_when_some() {
        let v = ClaudeCodeView {
            cwd: Some("/workspace/project".to_string()),
            ..Default::default()
        };
        let default = std::path::Path::new("/home/user");
        assert_eq!(v.resolve_cwd(default), PathBuf::from("/workspace/project"));
    }

    #[test]
    fn resolve_cwd_falls_back_to_session_default_when_none() {
        let v = ClaudeCodeView::default();
        let default = std::path::Path::new("/home/user");
        assert_eq!(v.resolve_cwd(default), PathBuf::from("/home/user"));
    }

    // -- T-034: subagent view + attrs -----------------------------------------

    #[test]
    fn claude_code_subagent_view_carries_spawner_fields() {
        let v = ClaudeCodeSubagentView {
            id: "sub-abc".to_string(),
            transcript_path: "/tmp/claude/projects/x/subagents/sub-abc.jsonl".to_string(),
        };
        assert_eq!(v.id, "sub-abc");
        assert_eq!(
            v.transcript_path,
            "/tmp/claude/projects/x/subagents/sub-abc.jsonl"
        );
    }

    #[test]
    fn claude_code_subagent_attrs_round_trip_into_view() {
        // T-038 will consume these attrs; pin the attrs->view shape
        // now so the later task can't silently rearrange field names.
        let attrs = ClaudeCodeSubagentAttrs {
            id: "sub-xyz".to_string(),
            transcript_path: "/t.jsonl".to_string(),
        };
        let v = ClaudeCodeSubagentView {
            id: attrs.id.clone(),
            transcript_path: attrs.transcript_path.clone(),
        };
        assert_eq!(v.id, "sub-xyz");
        assert_eq!(v.transcript_path, "/t.jsonl");
    }

    // -- T-035: title formatting + RenamePane payload ------------------------

    #[test]
    fn title_format_running_no_tool_renders_dash_placeholder() {
        let title = format_subagent_title("code-writer", SubagentStatus::Running, None);
        assert_eq!(title, "code-writer · running · -");
    }

    #[test]
    fn title_format_done_with_short_tool() {
        let title = format_subagent_title("code-writer", SubagentStatus::Done, Some("Edit"));
        assert_eq!(title, "code-writer · done · Edit");
    }

    #[test]
    fn title_format_failed_variant() {
        let title = format_subagent_title("agent", SubagentStatus::Failed, Some("Bash"));
        assert_eq!(title, "agent · failed · Bash");
    }

    #[test]
    fn title_format_truncates_last_tool_to_16_chars() {
        // 20-char tool name is truncated to 16 + ellipsis.
        let title = format_subagent_title(
            "agent",
            SubagentStatus::Running,
            Some("ExtraLongToolNameHere"),
        );
        assert!(
            title.ends_with("ExtraLongToolNam…"),
            "expected 16-char truncated tool + ellipsis, got {title:?}"
        );
    }

    #[test]
    fn title_format_truncates_total_to_60_chars() {
        let long_type = "a".repeat(80);
        let title = format_subagent_title(&long_type, SubagentStatus::Running, Some("Edit"));
        // Char count is either 60 (no truncation — impossible at 80+
        // chars of type) or 61 (60 kept chars + ellipsis).
        assert_eq!(
            title.chars().count(),
            61,
            "expected 60 chars + ellipsis, got {} chars in {title:?}",
            title.chars().count()
        );
        assert!(title.ends_with('…'));
    }

    #[test]
    fn title_format_handles_multibyte_agent_type_char_wise() {
        // Middle dots + non-ascii in agent_type — char-wise truncation
        // MUST not split mid-codepoint.
        let agent_type = "α".repeat(40); // 40 chars but 80 bytes in UTF-8.
        let title = format_subagent_title(&agent_type, SubagentStatus::Running, Some("Edit"));
        assert!(
            title.chars().count() <= 61,
            "expected char-bounded title; got {} chars",
            title.chars().count()
        );
        // The string MUST be valid UTF-8 (which it is by Rust String
        // construction) — this line is a compile-time assertion that
        // we didn't crash by slicing at a byte boundary.
        let _bytes: &[u8] = title.as_bytes();
    }

    #[test]
    fn subagent_state_render_title_round_trips() {
        let mut s = SubagentState::new("agent-42", "code-writer");
        assert_eq!(s.render_title(), "code-writer · running · -");
        s.last_tool = Some("Edit".to_string());
        assert_eq!(s.render_title(), "code-writer · running · Edit");
        s.status = SubagentStatus::Done;
        assert_eq!(s.render_title(), "code-writer · done · Edit");
    }

    #[test]
    fn rename_pane_payload_shape() {
        let mut s = SubagentState::new("agent-42", "code-writer");
        s.last_tool = Some("Edit".to_string());
        let p = ClaudeCodeSubagentView::rename_pane_payload(&s);
        assert_eq!(p.get("kind").and_then(|v| v.as_str()), Some("RenamePane"));
        assert_eq!(
            p.get("name").and_then(|v| v.as_str()),
            Some("code-writer · running · Edit")
        );
    }

    // -- T-036: transcript tail rendering ------------------------------------

    #[test]
    fn transcript_formatter_assistant_text_block() {
        let raw =
            r#"{"type":"message","role":"assistant","content":[{"type":"text","text":"hello"}]}"#;
        let out = format_transcript_lines(&[raw.to_string()], 200);
        assert_eq!(out, vec!["assistant: hello".to_string()]);
    }

    #[test]
    fn transcript_formatter_tool_use_block_inside_assistant_message() {
        let raw = r#"{"type":"message","role":"assistant","content":[{"type":"tool_use","name":"Edit","input":{"path":"foo"}}]}"#;
        let out = format_transcript_lines(&[raw.to_string()], 200);
        // Preserves the one tool_use piece.
        assert_eq!(out.len(), 1);
        assert!(out[0].starts_with("tool_use: Edit("));
    }

    #[test]
    fn transcript_formatter_top_level_tool_use() {
        let raw = r#"{"type":"tool_use","name":"Bash","input":{"command":"ls"}}"#;
        let out = format_transcript_lines(&[raw.to_string()], 200);
        assert_eq!(out.len(), 1);
        assert!(out[0].starts_with("tool_use: Bash("));
    }

    #[test]
    fn transcript_formatter_unknown_shape_falls_back_to_raw_prefix() {
        let raw = r#"{"type":"weird","foo":"bar"}"#;
        let out = format_transcript_lines(&[raw.to_string()], 200);
        assert_eq!(out.len(), 1);
        assert!(out[0].starts_with("-- "));
    }

    #[test]
    fn transcript_formatter_skips_empty_lines() {
        let out =
            format_transcript_lines(&["".to_string(), "\n".to_string(), "{}".to_string()], 200);
        // Empty / whitespace-only dropped; `{}` is unknown shape so
        // renders as fallback `-- {}`.
        assert_eq!(out, vec!["-- {}".to_string()]);
    }

    #[test]
    fn transcript_formatter_bounds_to_tail_lines_window() {
        let raws: Vec<String> = (0..20)
            .map(|i| {
                format!(
                    r#"{{"type":"message","role":"assistant","content":[{{"type":"text","text":"line {i}"}}]}}"#
                )
            })
            .collect();
        let out = format_transcript_lines(&raws, 5);
        assert_eq!(out.len(), 5);
        // Kept the LAST 5 — not the first 5.
        assert_eq!(out.first().unwrap(), "assistant: line 15");
        assert_eq!(out.last().unwrap(), "assistant: line 19");
    }

    #[test]
    fn render_transcript_tail_integration_with_tail_cursor() {
        // Integration sanity: TailCursor + formatter round-trip through
        // a real file. Drives the public surface
        // `ClaudeCodeSubagentView::render_transcript_tail`.
        use std::io::Write;
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let path = tmp.path().to_path_buf();
        {
            let mut f = std::fs::OpenOptions::new()
                .append(true)
                .open(&path)
                .unwrap();
            writeln!(
                f,
                r#"{{"type":"message","role":"assistant","content":[{{"type":"text","text":"first"}}]}}"#
            )
            .unwrap();
            writeln!(
                f,
                r#"{{"type":"tool_use","name":"Read","input":{{"path":"/x"}}}}"#
            )
            .unwrap();
        }
        let mut cursor = crate::transcript::TailCursor::new(&path);
        let out = ClaudeCodeSubagentView::render_transcript_tail(&mut cursor, 200).unwrap();
        assert_eq!(out.len(), 2);
        assert_eq!(out[0], "assistant: first");
        assert!(out[1].starts_with("tool_use: Read("));
    }
}
