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
}
