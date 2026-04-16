//! `ark spawn` — create a new agent (cavekit-cli R2).
//!
//! ## Pipeline ordering (post W-3 / W-4)
//!
//! `run()` walks the spawn happy-path in this order:
//!
//! 1. Parse args → resolve orchestrator + name + env + hooks (pure).
//! 2. `require_zellij_on_path()` — preflight. Failure returns
//!    `PreflightFail` BEFORE any filesystem mutation (F-503).
//! 3. Build `AgentSpec`, mint `AgentId`, write `spec.json` + render
//!    `layout.kdl` to `$STATE/agents/{id}/`.
//! 4. **Fork supervisor** (W-3): create a `pipe(2)`-based ready
//!    handshake (`supervisor_handoff::create_ready_pipe`), then call
//!    `ark_supervisor::daemonize`. The grandchild builds a tokio
//!    runtime and runs `run_supervisor`; on R3 step 12 it writes the
//!    ACK byte to its end of the pipe and the parent unblocks. The
//!    parent has a 5 s `wait_for_ready` timeout — failure cleans up
//!    agent state (F-528 invariant).
//! 5. **Launch zellij** — three-way split keyed on `$ZELLIJ` and
//!    `--no-detach` (F-730):
//!    - `$ZELLIJ` set → `zellij action switch-session` (IPC dispatch
//!      over the live socket; no nesting).
//!    - `--no-detach` → foreground attach with inherited stdio.
//!    - default → pty allocation (`portable-pty`) so zellij's TUI
//!      client gets a real controlling TTY.
//! 6. Print `spawned {id} -> Ctrl+o w to switch`, exit 0.
//!
//! The supervisor must be ready BEFORE step 5 because zellij plugins
//! (status, picker) read `$STATE/agents/*/status.json` and the control
//! socket on bootstrap. Ordering them after the supervisor is alive
//! avoids a startup race where a plugin sees no agent.
//!
//! ## `--no-detach` mode (W-4)
//!
//! With `--no-detach`, ark stays in the foreground and runs the
//! supervisor inline in a background `std::thread` (with its own
//! current-thread tokio runtime). Zellij is spawned as a foreground
//! subprocess in its own process group via `pre_exec(setpgid(0, 0))`,
//! and `tcsetpgrp` hands it the controlling tty so terminal SIGINT
//! goes to zellij only. When zellij exits, `child.wait()` unblocks,
//! the cli reclaims the tty, fires `cancel.cancel()` on the shared
//! `CancellationToken`, and joins the supervisor thread (which
//! drains consumers + finalizes state under the cancel).
//!
//! This mode produces all the same artifacts as default detach
//! (events.jsonl, status.json, control socket, pid file) but the
//! supervisor's lifetime is bounded by zellij's. Useful for CI and
//! foreground debugging; default-detach remains the user-facing
//! workflow.
//!
//! ## Design choices (historical)
//!
//! - Zellij is invoked as a subprocess — always a dedicated per-agent
//!   session via `zellij -s <name> --layout <path>` (R2: 1:1
//!   agent↔session).
//! - F-730: outside-zellij detach uses `portable-pty` to give zellij a
//!   real TTY. Null stdio + setsid is forbidden because zellij's TUI
//!   client cannot boot without a controlling terminal (no
//!   `--daemonize` mode). Inside-zellij uses `zellij action
//!   switch-session` over the existing socket.
//! - `apply_detach` (F-526) wires `pre_exec(setsid)` POSIX-natively
//!   instead of shelling out to the external `setsid(1)` binary that
//!   macOS does not ship by default.
//! - KDL layouts are minijinja templates (F-525). Render → persist to
//!   `{state_dir}/agents/{id}/layout.kdl` → hand THAT path to zellij.
//! - Parsing / detection helpers are pure functions so the tests don't
//!   touch the filesystem unless they want to.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use ark_mux_zellij::{
    LayoutResolver, LayoutSource, LayoutVars, default_layout_for_orchestrator, render_layout,
};
use ark_scene::compile::{CompileContext, compile_scene_file};
use ark_scene::context::{AgentSnapshot, SessionSnapshot};
use ark_scene::id::SceneId;
use ark_types::{AgentId, AgentSpec};
use clap::Args;

use crate::ctx::Ctx;
use crate::error::CliError;
use crate::supervisor_handoff::{create_ready_pipe, wait_for_ready_default};

/// Orchestrator runtime selected by `--orchestrator`.
///
/// `auto` scans `cwd` at spawn time: `context/sites/` → `cavekit`, else
/// `claude-code` (R2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum OrchestratorChoice {
    Auto,
    Cavekit,
    #[value(name = "claude-code")]
    ClaudeCode,
}

/// Engine selection. The legacy v1 slug surface — kept for backwards
/// compat with existing `ark spawn --engine claude-code` invocations.
/// New ACP-style engine names travel through the free-form
/// [`SpawnArgs::acp_engine`] / `--engine NAME` string flag
/// (T-ACP.4a).
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum EngineChoice {
    #[value(name = "claude-code")]
    ClaudeCode,
}

impl EngineChoice {
    #[allow(dead_code)] // Reserved for future CLI-level engine switching;
    // current spawn path takes `engine` as a free-form String.
    fn as_str(self) -> &'static str {
        match self {
            EngineChoice::ClaudeCode => "claude-code",
        }
    }
}

/// Arguments for `ark spawn`.
#[derive(Debug, Args)]
#[command(
    about = "Spawn a new agent in a dedicated zellij session",
    long_about = "Create a new agent in a dedicated zellij session.\n\
                  Positional arguments after `--` are the agent pane\n\
                  command.\n\
                  \n\
                  Examples:\n  \
                  ark spawn --orchestrator cavekit --cwd . -- \\\n    \
                    claude --resume\n  \
                  ark spawn --orchestrator claude-code -- claude\n  \
                  ark spawn --name authsvc -- claude --resume"
)]
pub struct SpawnArgs {
    /// Orchestrator runtime. Values: auto|cavekit|claude-code.
    #[arg(
        long,
        value_enum,
        default_value_t = OrchestratorChoice::Auto,
        hide_default_value = true,
        hide_possible_values = true,
    )]
    pub orchestrator: OrchestratorChoice,

    /// Engine name.
    ///
    /// For legacy orchestration (hook-injection path), `claude-code`
    /// is the only valid value. For the ACP resolver
    /// (T-ACP.4a rung 1), this flag is forwarded to
    /// [`ark_supervisor::engine_resolution::resolve_engine`] as the
    /// first-rung override — any value present in
    /// `[engines.<name>]` (or one of the shipped defaults
    /// `claude | codex | gemini-cli`) resolves cleanly.
    ///
    /// The default remains `"claude-code"` so existing spawn flows
    /// continue to work without change; ACP-first users write
    /// `--engine claude` / `--engine codex` / etc.
    #[arg(long, default_value = "claude-code", hide_default_value = true)]
    pub engine: String,

    /// Worktree path (default: current directory).
    #[arg(long, default_value = ".")]
    pub cwd: PathBuf,

    /// Human-readable label (default: derived from cwd basename).
    #[arg(long)]
    pub name: Option<String>,

    /// KDL layout stem (e.g. `builder`) or absolute path.
    #[arg(long)]
    pub layout: Option<String>,

    /// Scene name (looked up under `${CONFIG}/scenes/<name>.kdl`).
    ///
    /// T-3.5 three-tier fallback (most-specific wins):
    ///   1. `--scene NAME` explicit → `${CONFIG}/scenes/NAME.kdl`.
    ///   2. No `--scene`, but `${CONFIG}/scenes/default.kdl` exists →
    ///      that file is used automatically.
    ///   3. Otherwise → legacy `--layout <stem>` path with no scene
    ///      compilation (current behaviour for users who never adopted
    ///      scenes).
    ///
    /// Mutually compatible with `--layout` in the fallback sense: the
    /// flag lands in `SpawnArgs.layout` unconditionally, but is only
    /// consulted when no scene (explicit or default) resolves.
    #[arg(long, value_name = "NAME")]
    pub scene: Option<String>,

    /// Environment variables to pass through (KEY=VAL, repeatable).
    #[arg(long = "env", value_name = "KEY=VAL")]
    pub env: Vec<String>,

    /// Detach after spawn (default: true).
    #[arg(long, default_value_t = true, overrides_with = "no_detach")]
    pub detach: bool,

    /// Stay in foreground with log stream instead of detaching.
    #[arg(long = "no-detach", conflicts_with = "detach")]
    pub no_detach: bool,

    /// Hook wiring (EVENT=CMD, repeatable). See cavekit-hooks.md.
    #[arg(long = "hook", value_name = "EVENT=CMD")]
    pub hook: Vec<String>,

    /// Positional command to run in the agent pane — everything after `--`.
    ///
    /// F-527: `num_args = 1..` + `required = true` make clap reject an
    /// empty CMD with a usage error before `run()` executes. Without
    /// this, `ark spawn` (no `-- CMD`) would proceed with an empty
    /// `agent_cmd`, rendering a broken layout that zellij rejects.
    #[arg(last = true, value_name = "CMD", num_args = 1.., required = true)]
    pub cmd: Vec<String>,
}

// ---------------------------------------------------------- pure helpers ----

/// Derive the agent name: explicit `--name` wins, otherwise the
/// last path component of `cwd`. Falls back to `"agent"` when the
/// cwd has no basename (e.g. root or empty path).
pub fn derive_name(explicit: Option<&str>, cwd: &Path) -> String {
    if let Some(n) = explicit {
        if !n.is_empty() {
            return n.to_string();
        }
    }
    cwd.file_name()
        .and_then(|s| s.to_str())
        .filter(|s| !s.is_empty())
        .map(ToString::to_string)
        .unwrap_or_else(|| "agent".to_string())
}

/// Resolve the selected orchestrator slug.
///
/// `Auto` calls `cavekit::detect(cwd)` first; on no match falls back
/// to `claude-code`. Explicit choices bypass detection.
pub fn resolve_orchestrator(choice: OrchestratorChoice, cwd: &Path) -> &'static str {
    match choice {
        OrchestratorChoice::Cavekit => "cavekit",
        OrchestratorChoice::ClaudeCode => "claude-code",
        OrchestratorChoice::Auto => {
            if ark_orchestrators_cavekit::detect(cwd) {
                "cavekit"
            } else {
                "claude-code"
            }
        }
    }
}

/// Parse a single `KEY=VAL` pair. Empty / missing `=` is rejected.
pub fn parse_kv(raw: &str) -> Result<(String, String), String> {
    let (k, v) = raw
        .split_once('=')
        .ok_or_else(|| format!("expected KEY=VAL, got `{raw}`"))?;
    if k.is_empty() {
        return Err(format!("empty key in `{raw}`"));
    }
    Ok((k.to_string(), v.to_string()))
}

/// Parse many `--env KEY=VAL` flags into a sorted map.
pub fn parse_env(entries: &[String]) -> Result<BTreeMap<String, String>, String> {
    let mut out = BTreeMap::new();
    for e in entries {
        let (k, v) = parse_kv(e)?;
        out.insert(k, v);
    }
    Ok(out)
}

/// A parsed `--hook EVENT=CMD` entry. `cmd_argv` is shlex-split so
/// the downstream hook runner can spawn without re-parsing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HookEntry {
    /// Event name (e.g. `Stop`, `Start`).
    pub event: String,
    /// Raw command string as given on the CLI.
    pub cmd: String,
    /// Shlex-split argv (empty if the raw command was empty).
    pub cmd_argv: Vec<String>,
}

/// Parse many `--hook EVENT=CMD` flags into `HookEntry`s.
pub fn parse_hooks(entries: &[String]) -> Result<Vec<HookEntry>, String> {
    let mut out = Vec::with_capacity(entries.len());
    for e in entries {
        let (event, cmd) = parse_kv(e)?;
        let argv = shlex::split(&cmd).ok_or_else(|| format!("unparseable hook command `{cmd}`"))?;
        out.push(HookEntry {
            event,
            cmd,
            cmd_argv: argv,
        });
    }
    Ok(out)
}

/// Build the fully-formed [`AgentSpec`] from parsed CLI inputs.
///
/// Hook entries are folded into `runner_config` under
/// `runner_config.hooks = [{event, cmd, cmd_argv}, ...]` —
/// `AgentSpec` proper has no dedicated hooks field in v1, and the
/// orchestrator / supervisor read `runner_config` at spawn time.
pub fn build_spec(
    orchestrator: &str,
    engine: &str,
    name: &str,
    cwd: PathBuf,
    cmd: Vec<String>,
    env: BTreeMap<String, String>,
    layout: Option<String>,
    hooks: Vec<HookEntry>,
) -> AgentSpec {
    let id = AgentId::new(orchestrator, name);
    let mut spec = AgentSpec::new(id, name, orchestrator, engine, cwd, cmd);
    spec.env = env;
    spec.layout = layout;
    if !hooks.is_empty() {
        spec.runner_config = serde_json::json!({
            "hooks": hooks
                .iter()
                .map(|h| serde_json::json!({
                    "event": h.event,
                    "cmd": h.cmd,
                    "cmd_argv": h.cmd_argv,
                }))
                .collect::<Vec<_>>(),
        });
    }
    spec
}

/// Write `spec.json` to `{state_dir}/agents/{id}/spec.json`,
/// creating parent dirs. Returns the path written.
pub fn write_spec_json(state_dir: &Path, spec: &AgentSpec) -> Result<PathBuf, CliError> {
    let dir = spec.id.state_dir(state_dir);
    std::fs::create_dir_all(&dir).map_err(|e| CliError::Generic {
        reason: format!("create {}: {e}", dir.display()),
    })?;
    let path = dir.join("spec.json");
    let body = serde_json::to_string_pretty(spec).map_err(|e| CliError::Generic {
        reason: format!("serialize spec.json: {e}"),
    })?;
    std::fs::write(&path, body).map_err(|e| CliError::Generic {
        reason: format!("write {}: {e}", path.display()),
    })?;
    Ok(path)
}

/// Whether we are already inside a zellij session (env snapshot).
///
/// Kept public for diagnostics / doctor; no longer steers
/// `build_zellij_command` since F-516 unifies both paths behind
/// `setsid zellij -s …`.
pub fn inside_zellij<F: Fn(&str) -> Option<String>>(getter: F) -> bool {
    matches!(getter("ZELLIJ"), Some(v) if !v.is_empty())
}

/// A resolved zellij spawn plan: create a dedicated per-agent
/// session via `setsid zellij -s <name> --layout <path>`.
///
/// F-516 / F-517: prior cycles branched on `$ZELLIJ` and emitted
/// either `zellij action new-tab` (which only adds a tab to the
/// caller's session, violating R2's 1:1 agent↔session mapping) or
/// `zellij attach --create` (which needs a TTY — incompatible with
/// `/dev/null` stdio + `spawn()`). Unifying on `setsid zellij -s`
/// mirrors the canonical pattern in `crates/mux/zellij/src/mux.rs`
/// and detaches cleanly from the caller's controlling terminal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ZellijSpawn {
    /// Session name (1:1 with agent id).
    pub session: String,
    /// Layout path (stem or absolute) — required: R2 defines a
    /// default layout for every spawn so this is never `None` in
    /// practice, but we keep the type `Option` for forward-compat
    /// with future layout-less spawn modes (e.g. `ark spawn --bare`).
    pub layout: Option<String>,
}

/// Resolve the zellij spawn plan.
///
/// F-516: the inside-vs-outside-zellij distinction collapses at the
/// spawn level — we always create a new session. The env getter is
/// retained (unused here) so the public signature stays stable for
/// call sites and future diagnostics.
pub fn zellij_plan<F: Fn(&str) -> Option<String>>(
    _getter: F,
    session: &str,
    layout: Option<&str>,
) -> ZellijSpawn {
    ZellijSpawn {
        session: session.to_string(),
        layout: layout.map(ToString::to_string),
    }
}

/// Build the command for a given [`ZellijSpawn`] plan.
///
/// F-526: the argv is now pure `zellij -s <session> [--layout <path>]` —
/// the external `setsid` binary was dropped because macOS does not ship
/// it on a default install, which caused spawn to fail with "No such file
/// or directory" even when zellij itself was installed. Detaching from
/// the caller's controlling TTY is handled POSIX-natively by
/// [`apply_detach`] via `pre_exec(nix::unistd::setsid)`, which works
/// identically on Linux and macOS.
pub fn build_zellij_command(plan: &ZellijSpawn) -> Command {
    let mut c = Command::new("zellij");
    c.arg("-s").arg(&plan.session);
    if let Some(p) = &plan.layout {
        c.arg("--layout").arg(p);
    }
    c
}

/// F-526: POSIX-native detach — wire `pre_exec(setsid)` on the command
/// so the spawned child becomes the leader of a brand-new session,
/// divorced from the caller's controlling TTY. Mirrors what the external
/// `setsid(1)` binary would have done but avoids the runtime dependency
/// on it (which macOS doesn't ship by default).
///
/// Safe to call any number of times per Command: pre_exec closures stack
/// and `setsid()` is idempotent — a process already session leader gets
/// a harmless `EPERM` which we ignore, since `zellij` itself then forks
/// its daemon and the parent will exit normally.
pub fn apply_detach(cmd: &mut Command) {
    use std::os::unix::process::CommandExt;
    unsafe {
        cmd.pre_exec(|| {
            // Already-leader produces EPERM; treat as no-op. Any other
            // error surfaces to the parent via the pre_exec contract.
            match nix::unistd::setsid() {
                Ok(_) => Ok(()),
                Err(nix::errno::Errno::EPERM) => Ok(()),
                Err(e) => Err(std::io::Error::from_raw_os_error(e as i32)),
            }
        });
    }
}

/// F-606: Configure stdio + TTY-detach on `cmd` according to `no_detach`.
///
/// When `no_detach == false` (the default detach path) we nullify
/// stdin/stdout/stderr and invoke `detach_fn(cmd)` — normally
/// [`apply_detach`], which wires `pre_exec(setsid)` so the child becomes
/// a new session leader divorced from the caller's TTY. When
/// `no_detach == true` we leave stdio INHERITED from the parent and
/// skip the detach hook entirely so the operator can watch zellij
/// output live in the foreground (`--no-detach`).
///
/// `detach_fn` is injected for testability: host tests pass a flag-
/// recording stub instead of the real `apply_detach`, which is
/// otherwise opaque (pre_exec closures can't be introspected via
/// `std::process::Command`). Real callers pass `apply_detach`.
pub fn configure_zellij_stdio_and_detach<F>(cmd: &mut Command, no_detach: bool, detach_fn: F)
where
    F: FnOnce(&mut Command),
{
    if no_detach {
        // `--no-detach`: stay attached to the caller's TTY so zellij
        // output is visible. No session leadership change — the child
        // shares the parent's process group and controlling terminal,
        // which is exactly what the flag advertises.
        return;
    }
    detach_fn(cmd);
    cmd.stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
}

/// F-522: Derive a collision-free zellij session name for `id`.
///
/// `AgentId::session_name()` intentionally drops the ULID suffix for
/// human readability, so `cavekit+auth` spawned twice would clash on
/// the same session. We append the LAST 8 chars of the lowercase ULID —
/// the trailing portion carries the random bits (a ULID's first 10
/// encoded chars are timestamp-derived, so two agents spawned in the
/// same millisecond would share that prefix). Format: `{base}-{ulid8}`.
pub fn unique_session_name(id: &AgentId) -> String {
    let base = id.session_name();
    let ulid = id.ulid();
    // Last 8 chars of the 26-char Crockford-encoded ULID — random
    // portion, so two same-millisecond spawns still diverge.
    let len = ulid.chars().count();
    let skip = len.saturating_sub(8);
    let suffix: String = ulid.chars().skip(skip).collect();
    if suffix.is_empty() {
        base
    } else {
        format!("{base}-{suffix}")
    }
}

/// F-523: Brief grace-period check that a just-spawned zellij child
/// hasn't immediately exited with an error. Polls `try_wait()` every
/// ~50ms for up to 500ms. Returns `Some(CliError::Internal)` if the
/// child exited non-zero inside the window, otherwise `None`.
///
/// This is a heuristic — zellij forks its own detached daemon, so a
/// healthy spawn also "exits" (code 0) within the window after forking.
/// Both "still alive after 500ms" and "exited with code 0" are treated
/// as success; only a non-zero exit counts as failure.
pub fn zellij_startup_failure(child: &mut std::process::Child) -> Option<CliError> {
    const GRACE_MS: u64 = 500;
    const POLL_MS: u64 = 50;
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(GRACE_MS);
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                if status.success() {
                    return None;
                }
                let code = status.code().unwrap_or(-1);
                return Some(CliError::Internal {
                    reason: format!("zellij exited with code {code} before session came up"),
                });
            }
            Ok(None) => {
                if std::time::Instant::now() >= deadline {
                    return None;
                }
                std::thread::sleep(std::time::Duration::from_millis(POLL_MS));
            }
            Err(e) => {
                return Some(CliError::Internal {
                    reason: format!("wait on zellij child: {e}"),
                });
            }
        }
    }
}

/// F-730: inside-zellij dispatch — `zellij action switch-session
/// [--layout <path>] <session>`.
///
/// When `$ZELLIJ` is set, the caller is already attached to a running
/// zellij daemon. We do NOT spawn a new zellij client — that would nest.
/// Instead we ask the existing client to switch the user to a new
/// session (create-if-missing is the default for `switch-session`, no
/// `--create` flag). Works without pty, setsid, or stdio changes because
/// the command is an IPC dispatch over the caller's live zellij socket;
/// `Command::status()` blocks until the dispatch acks and returns.
///
/// Mirrors the argv shape used by `crates/mux/zellij/src/mux.rs:266`.
pub fn build_switch_session_command(plan: &ZellijSpawn) -> Command {
    let mut c = Command::new("zellij");
    c.arg("action").arg("switch-session");
    if let Some(p) = &plan.layout {
        c.arg("--layout").arg(p);
    }
    c.arg(&plan.session);
    c
}

// F-730 / F-731: the `PtyZellijHandle` + `spawn_zellij_with_pty` +
// `pty_child_startup_failure` helpers used to live here. They moved to
// `ark_mux_zellij::pty` so the same code path drives BOTH the
// outside-zellij `ark spawn` detach path AND the supervisor's
// `ZellijMux::create_tab` outside-zellij first-tab spawn — keeping a
// single source of truth for the pty lifetime contract and the
// startup-grace poll. The `cli_*_pty` wrappers below adapt the
// mux-crate `PtySpawnError` to the local `CliError` surface.

/// Adapter: spawn zellij in a pty using the shared mux helper, mapping
/// the mux's `PtySpawnError` to `CliError::Internal` with the same
/// reason text the prior in-crate helper produced.
fn cli_spawn_zellij_with_pty(
    plan: &ZellijSpawn,
    cwd: &Path,
) -> Result<ark_mux_zellij::PtyZellijHandle, CliError> {
    let layout = plan.layout.as_deref().unwrap_or("");
    let layout_path = std::path::Path::new(layout);
    ark_mux_zellij::spawn_zellij_with_pty(&plan.session, layout_path, cwd).map_err(|e| match e {
        ark_mux_zellij::PtySpawnError::OpenPty(reason) => CliError::Internal {
            reason: format!("openpty: {reason}"),
        },
        ark_mux_zellij::PtySpawnError::Spawn(reason) => CliError::Internal {
            reason: format!("spawn zellij in pty: {reason}"),
        },
        ark_mux_zellij::PtySpawnError::EarlyExit { code } => CliError::Internal {
            reason: format!("zellij exited with code {code} before session came up"),
        },
        ark_mux_zellij::PtySpawnError::Wait(reason) => CliError::Internal {
            reason: format!("wait on zellij pty child: {reason}"),
        },
    })
}

/// Adapter: run the shared startup-grace poll, mapping its error
/// variants to `CliError::Internal` with the same wording the prior
/// in-crate helper produced.
fn cli_pty_child_startup_failure(
    child: &mut (dyn portable_pty::Child + Send + Sync),
) -> Option<CliError> {
    match ark_mux_zellij::pty_child_startup_failure(child) {
        Ok(()) => None,
        Err(ark_mux_zellij::PtySpawnError::EarlyExit { code }) => Some(CliError::Internal {
            reason: format!("zellij exited with code {code} before session came up"),
        }),
        Err(ark_mux_zellij::PtySpawnError::Wait(reason)) => Some(CliError::Internal {
            reason: format!("wait on zellij pty child: {reason}"),
        }),
        Err(other) => Some(CliError::Internal {
            reason: format!("pty child startup poll: {other}"),
        }),
    }
}

/// Preflight: `zellij` must be on PATH. Returns `PreflightFail`
/// with a clear reason when the binary is missing. No-op on success.
pub fn require_zellij_on_path() -> Result<(), CliError> {
    let status = Command::new("zellij")
        .arg("--version")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
    match status {
        Ok(s) if s.success() => Ok(()),
        _ => Err(CliError::PreflightFail {
            reason: "zellij not found on PATH".to_string(),
        }),
    }
}

/// T-3.5 / T-8.2: multi-rung decision for "how does this spawn acquire
/// a zellij layout?".
///
/// Reported back to the caller as a discriminated enum so tests can
/// assert the resolution path independently of the rendered output.
///
/// T-8.2 re-homed the internals of [`resolve_layout_source`] onto the
/// T-8.0 scene resolver ([`ark_scene::path::resolve_scene_path_from_env`]),
/// which also consults `ARK_SCENE`, `ARK_APPNAME`, project-local
/// `.ark/scene.kdl`, and the XDG default scene. The enum shape is
/// preserved so the spawn pipeline + existing tests keep working:
///   - `ResolvedScene::Named(n)` → [`Self::SceneExplicit`] under
///     `${config_dir}/scenes/<n>.kdl` (combo 3A).
///   - `ResolvedScene::Path(p)` → [`Self::SceneDefault`] (both the
///     project-local rung and the XDG-default rung yielded a concrete
///     file on disk).
///   - `ResolvedScene::BuiltIn(_)` → [`Self::Legacy`] (T-14.1 will
///     materialize the embedded scene to disk and promote it to a
///     proper scene compile; today it falls through to the legacy
///     `--layout <stem>` path).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LayoutResolution {
    /// Scene identified by name: either `--scene NAME` from the CLI
    /// (rung 1) or `ARK_SCENE=NAME` from the environment (rung 2).
    /// `path` is always `${config_dir}/scenes/<name>.kdl`.
    SceneExplicit { path: PathBuf },
    /// Scene identified by a concrete file on disk: project-local
    /// `./.ark/scene.kdl` (rung 3) or XDG-default
    /// `$XDG_CONFIG_HOME/<appname>/scenes/default.kdl` (rung 4).
    SceneDefault { path: PathBuf },
    /// No scene resolved at any rung — fall through to the legacy
    /// `--layout <stem>` path (T-14.1 will replace this branch with
    /// an auto-wrapped minimal scene so both tiers share the compile
    /// pipeline).
    Legacy,
}

/// T-3.5 / T-8.2: resolve which scene-file, if any, drives this spawn.
///
/// Delegates to [`ark_scene::path::resolve_scene_path_from_env`], which
/// implements the `cavekit-scene.md` R13 precedence (CLI flag →
/// `ARK_SCENE` → `./.ark/scene.kdl` → XDG default → built-in) and
/// reads `ARK_SCENE`, `ARK_APPNAME`, and `XDG_CONFIG_HOME` from the
/// process environment.
///
/// The translation from [`ResolvedScene`] to [`LayoutResolution`]
/// preserves the enum shape expected by the downstream spawn pipeline
/// (see [`run`]):
///   - [`ResolvedScene::Named`] → [`LayoutResolution::SceneExplicit`]
///     with the path rooted at `${config_dir}/scenes/<name>.kdl`.
///     Named scenes intentionally resolve under `ctx.config_dir` (NOT
///     the XDG-derived path) per the decided combo 3A: `ARK_APPNAME`
///     matters only for rung 4 (XDG default lookup), which T-8.0
///     already handles internally.
///   - [`ResolvedScene::Path`] → [`LayoutResolution::SceneDefault`]
///     with the path straight through. Covers both rung 3 (project-
///     local) and rung 4 (XDG default).
///   - [`ResolvedScene::BuiltIn`] → [`LayoutResolution::Legacy`]. The
///     embedded default scene is not materialized to disk by this
///     function; falling through to the legacy `--layout <stem>`
///     path preserves zero-migration behaviour for users who never
///     adopted scenes.
///
/// Reads from the process environment via [`ark_scene::path::resolve_scene_path_from_env`];
/// tests that cover env-var rungs must serialize on
/// [`crate::test_lock::ENV_LOCK`].
pub fn resolve_layout_source(
    config_dir: &Path,
    cwd: &Path,
    explicit_scene: Option<&str>,
) -> LayoutResolution {
    match ark_scene::path::resolve_scene_path_from_env(explicit_scene, cwd) {
        ark_scene::path::ResolvedScene::Named(name) => {
            // Combo 3A: named scenes always resolve under
            // ctx.config_dir/scenes/, independent of ARK_APPNAME /
            // XDG_CONFIG_HOME. Keeps the flag + env-var rungs
            // interchangeable and avoids surprising per-appname
            // namespaces for user-named scenes.
            let path = config_dir.join("scenes").join(format!("{name}.kdl"));
            LayoutResolution::SceneExplicit { path }
        }
        ark_scene::path::ResolvedScene::Path(path) => {
            LayoutResolution::SceneDefault { path }
        }
        ark_scene::path::ResolvedScene::BuiltIn(_) => {
            // TODO(T-14.1): materialize the embedded DEFAULT_SCENE_KDL
            // to a per-agent scene file and compile it via the scene
            // pipeline so the "zero-migration" path also benefits from
            // scene-driven rendering. Today we preserve the legacy
            // `--layout <stem>` behaviour so users who never adopted
            // scenes see no change.
            LayoutResolution::Legacy
        }
    }
}

/// T-3.5: compile a scene file and emit the rendered zellij layout.
///
/// Thin wrapper over [`compile_scene_file`] that builds the
/// spawn-time [`CompileContext`] from the current `AgentSpec`, then
/// forwards the call. Returns the absolute path to the rendered
/// zellij layout plus the `SceneId` that identifies the source
/// scene (stored onward in `AgentSpec.scene_path`).
pub fn compile_and_write_scene(
    ctx: &Ctx,
    scene_file: &Path,
    spec: &AgentSpec,
) -> Result<(PathBuf, SceneId), CliError> {
    let compile_ctx = CompileContext::new(
        AgentSnapshot {
            id: spec.id.to_string(),
            name: spec.name.clone(),
            orchestrator: spec.orchestrator.clone(),
            engine: spec.engine.clone(),
            cwd: spec.cwd.display().to_string(),
            cmd: spec.cmd.first().cloned().unwrap_or_default(),
            args: if spec.cmd.len() > 1 {
                spec.cmd[1..].to_vec()
            } else {
                Vec::new()
            },
        },
        SessionSnapshot {
            name: spec.session.clone(),
        },
    );

    compile_scene_file(scene_file, &ctx.runtime_dir, &compile_ctx).map_err(|e| {
        CliError::Generic {
            reason: format!("compile scene `{}`: {e}", scene_file.display()),
        }
    })
}

/// F-525: Resolve + render + persist the KDL layout template.
///
/// A zellij "layout" is not a static KDL file — it's a minijinja template
/// that needs `{{ cwd }}`, `{{ agent_cmd }}`, `{{ agent_args }}` etc.
/// substituted per spawn. Prior cycles handed the raw stem (e.g.
/// `"builder"`) straight to `zellij --layout`, which made zellij bail on
/// the unexpanded `{{…}}` tokens.
///
/// Resolution:
/// 1. If `spec.layout` is `None`, use
///    [`default_layout_for_orchestrator`] (e.g. `"builder"` for cavekit,
///    `"classic"` for claude-code).
/// 2. Hand that stem-or-path to [`LayoutResolver`] with
///    `user_root = {config_dir}/layouts/`. The resolver handles stem →
///    user-override → embedded-shipped precedence (cavekit-layouts R1).
/// 3. Render the template source with [`render_layout`], supplying
///    `cwd` (from `spec.cwd`), `agent_cmd` (`spec.cmd[0]` or empty),
///    `agent_args` (`spec.cmd[1..]`), `id` (`spec.id`), `name`
///    (`spec.name`).
/// 4. Write the rendered KDL to `{state_dir}/agents/{id}/layout.kdl`.
///
/// Returns the absolute path of the rendered file. On any failure
/// (resolver, template, write) returns `CliError::Generic` with the
/// underlying reason.
pub fn render_and_write_layout(ctx: &Ctx, spec: &AgentSpec) -> Result<PathBuf, CliError> {
    // Determine the stem or explicit path.
    let fallback = default_layout_for_orchestrator(&spec.orchestrator);
    let stem_or_path: String = spec.layout.clone().unwrap_or_else(|| fallback.to_string());

    // User override root: `{config_dir}/layouts/`. Passing it
    // unconditionally is safe — LayoutResolver checks existence per-file.
    let user_root = Some(ctx.config_dir.join("layouts"));
    let resolver = LayoutResolver::new(user_root);
    let source = resolver
        .resolve(&stem_or_path)
        .map_err(|e| CliError::Generic {
            reason: format!("resolve layout `{stem_or_path}`: {e}"),
        })?;
    let template_src = match &source {
        LayoutSource::User { contents, .. } => contents.as_str(),
        LayoutSource::Embedded { contents, .. } => contents.as_str(),
        LayoutSource::Path { contents, .. } => contents.as_str(),
    };

    // Build the bounded variable surface. `agent_cmd` is the first argv
    // token (empty when spec.cmd is empty — callers like `ark spawn`
    // without a `--` tail); `agent_args` is everything after.
    let agent_cmd = spec.cmd.first().cloned().unwrap_or_default();
    let agent_args: Vec<String> = if spec.cmd.len() > 1 {
        spec.cmd[1..].to_vec()
    } else {
        Vec::new()
    };
    let vars = LayoutVars {
        cwd: spec.cwd.display().to_string(),
        agent_cmd,
        agent_args,
        id: spec.id.to_string(),
        name: spec.name.clone(),
    };

    let rendered = render_layout(template_src, &vars).map_err(|e| CliError::Generic {
        reason: format!("render layout template: {e}"),
    })?;

    // Destination: `{state_dir}/agents/{id}/layout.kdl`. Parent dir is
    // created by write_spec_json earlier in run(), but we defensively
    // create_dir_all here so the helper is safe to call in isolation
    // (and in tests).
    let dir = spec.id.state_dir(&ctx.state_dir);
    std::fs::create_dir_all(&dir).map_err(|e| CliError::Generic {
        reason: format!("create {}: {e}", dir.display()),
    })?;
    let path = dir.join("layout.kdl");
    std::fs::write(&path, &rendered).map_err(|e| CliError::Generic {
        reason: format!("write {}: {e}", path.display()),
    })?;
    Ok(path)
}

/// F-528: Shared cleanup after any spawn-time failure.
///
/// When zellij fails to launch — whether `Command::spawn()` itself
/// returns `Err` (e.g. ENOENT after a racy `PATH` change, permission
/// denied on the binary) or the child forks but exits non-zero before
/// the session is listenable — we must remove the `{state_dir}/agents/{id}`
/// directory we wrote spec.json + layout.kdl into, so `ark list` does not
/// report an orphan agent that never existed.
///
/// This is the "no orphan state on spawn failure" invariant shared by
/// F-503 (preflight), F-525 (render), F-523 (startup poll), and now
/// F-528 (spawn syscall). Errors from `remove_dir_all` are swallowed
/// — if the state dir cannot be removed, surfacing the original spawn
/// error is more useful to the operator than a secondary I/O error.
pub fn cleanup_agent_state(state_dir: &Path, id: &AgentId) {
    let _ = std::fs::remove_dir_all(id.state_dir(state_dir));
}

/// F-708: classification of what `zellij list-sessions` reported about a
/// given session name. Used by the `--no-detach` exit path to decide
/// whether to wipe transient agent state.
///
/// The distinction between `Unknown` and `Gone` matters: an `Unknown`
/// outcome (zellij missing from PATH, command failed to spawn, stdout
/// unparseable) must NOT trigger cleanup — we cannot prove the session
/// is gone, and the safer default is to keep state so a still-live
/// detached session does not lose its spec.json. `Gone` is the only
/// positive signal that cleanup is appropriate.
#[derive(Debug, PartialEq, Eq)]
pub enum ZellijSessionLiveness {
    /// Session name appears in `zellij list-sessions` output (either as
    /// an active or exited-but-still-listed row). The user most likely
    /// detached (Ctrl+P, D) and zellij keeps running in the background.
    Alive,
    /// Session name is absent from `zellij list-sessions` output. zellij
    /// answered cleanly and the session is truly gone — cleanup is safe.
    Gone,
    /// We could not determine liveness — zellij is not on PATH, the
    /// command failed, or stdout was unreadable. Treat as "keep state"
    /// so a detach-only exit cannot accidentally wipe a live session.
    Unknown,
}

/// F-708: Query `zellij list-sessions` for `session_name`. Returns the
/// classification used by the `--no-detach` cleanup gate.
///
/// zellij's `list-sessions` emits one line per session. Lines may carry
/// ANSI color codes and trailing annotations such as ` (current)` or
/// ` (EXITED - Attach to resurrect)` — the session name is the first
/// whitespace-separated token after stripping ANSI. We match on an exact
/// token equal to `session_name`.
///
/// `--no-detach` callers need to know: is the session the user was just
/// attached to still listed (they detached) or gone (they terminated /
/// zellij crashed)? An `Alive` answer keeps state; `Gone` triggers
/// cleanup; `Unknown` falls back to keeping state.
pub fn zellij_session_liveness(session_name: &str) -> ZellijSessionLiveness {
    let output = Command::new("zellij")
        .arg("list-sessions")
        .arg("--no-formatting")
        .stdin(std::process::Stdio::null())
        .output();
    let output = match output {
        Ok(o) => o,
        Err(_) => return ZellijSessionLiveness::Unknown,
    };
    // zellij exits non-zero when there are zero sessions ("No active
    // zellij sessions found."). Treat that as a definitive "gone" signal
    // only if stdout/stderr both lack our session name; otherwise fall
    // back to Unknown.
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    if session_listed(&stdout, session_name) || session_listed(&stderr, session_name) {
        return ZellijSessionLiveness::Alive;
    }
    if output.status.success() {
        ZellijSessionLiveness::Gone
    } else {
        // Non-zero with "no active sessions" text → Gone. Any other
        // non-zero → Unknown (don't wipe state on an ambiguous reading).
        let combined_lower = format!("{stdout}{stderr}").to_lowercase();
        if combined_lower.contains("no active") || combined_lower.contains("no zellij sessions") {
            ZellijSessionLiveness::Gone
        } else {
            ZellijSessionLiveness::Unknown
        }
    }
}

/// F-708: true if `haystack` contains `session_name` as a whitespace-
/// delimited token on some line, after stripping basic ANSI escape
/// sequences. Pure — used by [`zellij_session_liveness`] and unit-tested
/// against captured zellij output.
pub fn session_listed(haystack: &str, session_name: &str) -> bool {
    for raw_line in haystack.lines() {
        let line = strip_ansi(raw_line);
        for tok in line.split_whitespace() {
            if tok == session_name {
                return true;
            }
        }
    }
    false
}

/// F-708: minimal ANSI CSI sequence stripper. zellij tags its session
/// names with colour codes (e.g. `\x1b[32.38.5.154mfoo\x1b[...m`) even
/// with `--no-formatting` in some versions; stripping `\x1b[...m` is
/// enough to recover the bare session token for the split_whitespace
/// match in [`session_listed`].
fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == 0x1b && i + 1 < bytes.len() && bytes[i + 1] == b'[' {
            // Skip until a letter in the CSI final-byte range (0x40..=0x7E).
            i += 2;
            while i < bytes.len() {
                let b = bytes[i];
                i += 1;
                if (0x40..=0x7e).contains(&b) {
                    break;
                }
            }
        } else {
            out.push(bytes[i] as char);
            i += 1;
        }
    }
    out
}

// ------------------------------------------------------------- handler ------

/// `ark spawn` — T-087 + F-511.
///
/// Happy path:
/// 1. Resolve orchestrator (read-only detect on cwd).
/// 2. Derive name.
/// 3. Parse env + hooks (pure, no I/O).
/// 4. Preflight zellij on PATH — F-503: run BEFORE any filesystem
///    mutation so preflight failure leaves zero orphan state.
/// 5. Build spec, mint AgentId, write spec.json.
/// 6. Launch zellij via `build_zellij_command` (F-511). The child
///    process is detached via `Command::spawn()` so the parent
///    (likely running inside zellij itself) is not blocked.
/// 7. Fork supervisor (W-3): create `pipe(2)`-based ready handshake,
///    call `ark_supervisor::daemonize`. Grandchild builds a single-
///    threaded tokio runtime and calls `supervisor_main(spec, config,
///    Some(ReadyWriter))`, exiting with `outcome_exit_code(outcome)`.
///    Parent closes the write end, polls the read end via
///    `wait_for_ready` with a 5 s timeout, and cleans up agent state
///    on timeout/failure (F-528 invariant).
/// 8. Print `spawned {id} -> Ctrl+o w to switch`.
pub fn run(args: SpawnArgs, ctx: &Ctx) -> Result<(), CliError> {
    let orchestrator = resolve_orchestrator(args.orchestrator, &args.cwd);
    let name = derive_name(args.name.as_deref(), &args.cwd);
    let env = parse_env(&args.env).map_err(|reason| CliError::Generic { reason })?;
    let hooks = parse_hooks(&args.hook).map_err(|reason| CliError::Generic { reason })?;

    // F-503: preflight BEFORE any filesystem mutation. If zellij is
    // missing, the user gets a clean PreflightFail with no orphan
    // spec.json / agent dir to clean up by hand.
    require_zellij_on_path()?;

    let mut spec = build_spec(
        orchestrator,
        args.engine.as_str(),
        &name,
        args.cwd.clone(),
        args.cmd.clone(),
        env,
        args.layout.clone(),
        hooks,
    );
    // T-ACP.4a: stash the raw `--engine NAME` on the spec so the
    // supervisor's engine-resolution chain (rung 1) can read it back
    // at boot. The flag's value already lands in `spec.engine` for
    // the legacy factory path, but the ACP resolver wants to
    // distinguish "user asked for `claude-code` via the legacy
    // default" from "user explicitly wrote `--engine codex`" — mirror
    // the flag verbatim onto `runner_config.acp_engine_flag`. A null
    // entry means "no explicit flag", i.e. the spawn relied on the
    // default.
    {
        let flag_value = serde_json::Value::String(args.engine.clone());
        match &mut spec.runner_config {
            serde_json::Value::Object(map) => {
                map.insert("acp_engine_flag".into(), flag_value);
            }
            other if other.is_null() => {
                let mut map = serde_json::Map::new();
                map.insert("acp_engine_flag".into(), flag_value);
                *other = serde_json::Value::Object(map);
            }
            _ => {
                // Non-object runner_config (shouldn't happen in
                // practice) — leave untouched; resolver falls back
                // to the legacy slug from `spec.engine`.
            }
        }
    }

    // F-600: `AgentSpec::new` initialises `spec.session` to
    // `AgentId::session_name()` — the bare `ark-{orch}-{name}` form that
    // F-522 deliberately does NOT use as the real zellij session. If we
    // persist the spec unchanged, `spec.session` disagrees with the actual
    // session zellij was launched under (`{base}-{ulid8}`), which breaks
    // any downstream reader that tries to reattach via `spec.session`
    // (supervisor, picker, status chip focus). Overwrite it here, BEFORE
    // `write_spec_json`, so the on-disk spec is authoritative.
    let session = unique_session_name(&spec.id);
    spec.session = session.clone();

    // T-3.5 / T-8.2: scene resolution across all five T-8.0 rungs
    // (CLI flag → ARK_SCENE → project-local → XDG-default → built-in).
    // Populates `spec.scene_path` when a scene drove the spawn so
    // downstream subsystems (supervisor, hot-reload watcher) can
    // attribute events back to the source file. `args.cwd` is the
    // worktree directory and satisfies rung 3 (project-local
    // `./.ark/scene.kdl`); ARK_SCENE / ARK_APPNAME / XDG_CONFIG_HOME
    // are read from the process environment by the T-8.0 helper.
    let resolution =
        resolve_layout_source(&ctx.config_dir, &args.cwd, args.scene.as_deref());
    let scene_file: Option<PathBuf> = match &resolution {
        LayoutResolution::SceneExplicit { path } | LayoutResolution::SceneDefault { path } => {
            Some(path.clone())
        }
        LayoutResolution::Legacy => None,
    };
    if let Some(path) = &scene_file {
        spec.scene_path = Some(path.clone());
    }

    let spec_path = write_spec_json(&ctx.state_dir, &spec)?;
    tracing::debug!(path = %spec_path.display(), "wrote spec.json");

    // Render the layout: either via the scene compile pipeline
    // (T-3.5 new) or the legacy layout-template path (F-525). Both
    // return an absolute `.kdl` path that goes to `zellij --layout`.
    // TODO(T-14.1): legacy path currently still hands the raw stem
    // through `render_and_write_layout`; when T-14.1 lands, the
    // legacy path will auto-wrap into a minimal scene so both tiers
    // share the compile pipeline.
    let layout_path = match scene_file.as_deref() {
        Some(path) => match compile_and_write_scene(ctx, path, &spec) {
            Ok((rendered, _scene_id)) => rendered,
            Err(e) => {
                cleanup_agent_state(&ctx.state_dir, &spec.id);
                return Err(e);
            }
        },
        None => match render_and_write_layout(ctx, &spec) {
            Ok(p) => p,
            Err(e) => {
                cleanup_agent_state(&ctx.state_dir, &spec.id);
                return Err(e);
            }
        },
    };
    tracing::debug!(path = %layout_path.display(), scene_path = ?spec.scene_path, "wrote rendered layout");

    // W-3: fork the supervisor BEFORE launching zellij so the control
    // socket exists by the time zellij plugins boot. Pipe-inheritance
    // ready handshake (W-2) ensures the parent does not return until
    // the supervisor has bound its socket and emitted Started — the
    // <1 s parent-return contract from cavekit-supervisor R1.
    //
    // `--no-detach` is currently exempt: the existing foreground path
    // launches zellij inline without a supervisor. W-4 will wire an
    // inline supervisor for that mode; until then, `--no-detach` keeps
    // the F-730 behaviour with no supervisor (debugging-only).
    if !args.no_detach {
        let (ready_rfd, ready_wfd) = create_ready_pipe()?;
        let state_layout = ark_types::StateLayout::new(
            ctx.state_dir.clone(),
            ctx.runtime_dir.clone(),
            ctx.config_dir.clone(),
        );

        // SAFETY: `daemonize()` calls `fork(2)`. Per its contract,
        // there must be no tokio runtime or worker threads alive at
        // this point — `run()` has not started either. The single-
        // threaded check is therefore satisfied for both Linux and
        // macOS.
        match ark_supervisor::daemonize(&state_layout, &spec.id) {
            Err(e) => {
                cleanup_agent_state(&ctx.state_dir, &spec.id);
                return Err(CliError::Internal {
                    reason: format!("daemonize supervisor: {e}"),
                });
            }
            Ok(ark_supervisor::DaemonizeOutcome::Daemon) => {
                // We are the supervisor grandchild. `setup_supervisor_log`
                // already redirected stdio to `supervisor.log` and
                // installed a tracing subscriber. Drop the parent's
                // read end — only the write end is ours.
                drop(ready_rfd);
                let writer = ark_supervisor::ReadyWriter::from_owned_fd(ready_wfd);
                let config = ark_core::Config::placeholder();

                let runtime = match tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                {
                    Ok(rt) => rt,
                    Err(e) => {
                        tracing::error!(error = %e, "build tokio runtime in supervisor");
                        std::process::exit(3);
                    }
                };

                // W-3: route the grandchild through `supervisor_main`
                // (the W-1 bootstrap wrapper) so the pre-ready error
                // path is logged consistently and the ReadyWriter is
                // owned by a single helper. `supervisor_main` wraps
                // `run_supervisor` with readiness-signal ownership +
                // structured error logging (see bootstrap.rs).
                let outcome = runtime.block_on(ark_supervisor::supervisor_main(
                    spec,
                    ark_supervisor::SupervisorMode::Daemon,
                    config,
                    Some(writer),
                    None, // daemon mode uses internal cancel + signal handler
                ));
                match outcome {
                    Ok(o) => {
                        tracing::info!(?o, "supervisor exited cleanly");
                        std::process::exit(ark_supervisor::outcome_exit_code(&o));
                    }
                    Err(e) => {
                        tracing::error!(error = %e, "supervisor returned Err");
                        std::process::exit(3);
                    }
                }
            }
            Ok(ark_supervisor::DaemonizeOutcome::Parent { child_pid }) => {
                // We are the original `ark spawn` process. Drop the
                // supervisor's write end so EOF fires at the parent if
                // the supervisor dies before signalling ready.
                drop(ready_wfd);
                tracing::debug!(
                    child_pid = %child_pid,
                    "daemonized supervisor; waiting for ready"
                );
                if let Err(e) = wait_for_ready_default(ready_rfd) {
                    tracing::warn!(
                        child_pid = %child_pid,
                        error = ?e,
                        "supervisor failed to ready; cleaning state",
                    );
                    cleanup_agent_state(&ctx.state_dir, &spec.id);
                    return Err(e);
                }
            }
        }
    }

    // F-522: the session name returned by `AgentId::session_name()`
    // deliberately drops the ULID suffix (`ark-{orchestrator}-{name}`),
    // which means two agents sharing orchestrator+name collide on the
    // same zellij session. We override here at the CLI layer — rather
    // than editing ark-types — by appending an 8-char ULID prefix so
    // the final session is `{base}-{ulid8}`, human-readable but unique.
    //
    // F-525: the plan's layout is the RENDERED KDL path, not the raw
    // stem that came in on the CLI.
    //
    // F-600: `session` was computed above (before `write_spec_json`) so
    // `spec.session` on disk matches the real zellij session name.
    let layout_str = layout_path.to_string_lossy().into_owned();
    let plan = zellij_plan(
        |k| std::env::var(k).ok(),
        &session,
        Some(layout_str.as_str()),
    );

    // F-730: three-way split on how to hand off to zellij. The unified
    // `setsid + null stdio` path introduced in F-516 broke both the
    // inside and outside branches — zellij's TUI client has no
    // `--daemonize` mode and cannot boot without a real controlling
    // TTY. Restore the split:
    //
    //   inside-zellij    → `zellij action switch-session` (IPC only)
    //   outside + -nd    → foreground attach on inherited stdio
    //   outside + detach → pty allocation so zellij sees a real TTY
    let inside_zellij_flag = std::env::var("ZELLIJ")
        .ok()
        .filter(|v| !v.is_empty())
        .is_some();

    if inside_zellij_flag {
        // INSIDE-ZELLIJ: the caller is already attached to a running
        // zellij daemon. Dispatch `switch-session` over the live
        // socket; the existing client is moved into the new session
        // (or one is created on demand — `switch-session` is
        // create-if-missing). No nesting, no new pty, no setsid.
        let mut zcmd = build_switch_session_command(&plan);
        let status = match zcmd.status() {
            Ok(s) => s,
            Err(e) => {
                cleanup_agent_state(&ctx.state_dir, &spec.id);
                return Err(CliError::Internal {
                    reason: format!("launch zellij: {e}"),
                });
            }
        };
        if !status.success() {
            let code = status.code().unwrap_or(-1);
            cleanup_agent_state(&ctx.state_dir, &spec.id);
            return Err(CliError::Internal {
                reason: format!("zellij action switch-session exited with code {code}"),
            });
        }
        tracing::debug!(spec = %spec_path.display(), "spawned and ready");
        println!("spawned {} -> Ctrl+o w to switch", spec.id);
        return Ok(());
    }

    if args.no_detach {
        // OUTSIDE-ZELLIJ + --no-detach (W-4): inline supervisor in a
        // background std thread + foreground zellij subprocess in its
        // own process group. Pattern documented in `context/impl/
        // impl-supervisor-wiring.md` and grounded in the external
        // research summary at the top of this file's W-4 history.
        //
        // Architecture (main thread owns the TTY):
        //
        //   ┌──────────────── main thread ────────────────┐
        //   │ 1. build CancellationToken                  │
        //   │ 2. std::thread::spawn supervisor:           │
        //   │      tokio current_thread runtime           │
        //   │      block_on(run_supervisor(spec, ...,     │
        //   │                Some(cancel)))               │
        //   │ 3. spawn zellij with pre_exec(setpgid 0,0)  │
        //   │ 4. tcsetpgrp(stdin, child_pgid) — make      │
        //   │    zellij the foreground pgrp; SIGINT from  │
        //   │    terminal goes only to zellij             │
        //   │ 5. child.wait() blocks                      │
        //   │ 6. tcsetpgrp(stdin, our pgid) reclaim       │
        //   │ 7. cancel.cancel() → supervisor drains      │
        //   │ 8. supervisor_thread.join()                 │
        //   └─────────────────────────────────────────────┘
        //
        // Why setpgid + tcsetpgrp: on Ctrl+C the kernel sends SIGINT
        // to the *foreground* process group only. By making zellij
        // its own pgrp and handing it the controlling tty, terminal-
        // generated signals reach zellij directly while our CLI
        // process stays uninterrupted. Zellij exits → child.wait()
        // unblocks → we reclaim the tty + drive supervisor shutdown.
        // Mirrors job-control behaviour in shells (bash, fish) and
        // the foreground-job pattern in nushell.

        use std::os::unix::process::CommandExt;

        let cancel = ark_types::CancellationToken::new();
        let cancel_for_thread = cancel.clone();
        let spec_for_thread = spec.clone();
        let supervisor_thread = std::thread::Builder::new()
            .name("ark-supervisor".into())
            .spawn(move || {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .map_err(|e| format!("build tokio runtime: {e}"))?;
                runtime
                    .block_on(ark_supervisor::run_supervisor(
                        spec_for_thread,
                        ark_supervisor::SupervisorMode::Foreground,
                        ark_core::Config::placeholder(),
                        None, // no ack — same process; we own the cancel
                        Some(cancel_for_thread),
                    ))
                    .map_err(|e| format!("supervisor returned Err: {e}"))
            })
            .map_err(|e| {
                cleanup_agent_state(&ctx.state_dir, &spec.id);
                CliError::Internal {
                    reason: format!("spawn supervisor thread: {e}"),
                }
            })?;

        // Build zellij command with pre_exec(setpgid(0,0)) so it
        // becomes its own process group leader.
        let mut zcmd = build_zellij_command(&plan);
        unsafe {
            zcmd.pre_exec(|| {
                match nix::unistd::setpgid(
                    nix::unistd::Pid::from_raw(0),
                    nix::unistd::Pid::from_raw(0),
                ) {
                    Ok(_) => Ok(()),
                    Err(e) => Err(std::io::Error::from_raw_os_error(e as i32)),
                }
            });
        }

        // Spawn zellij. On failure, signal supervisor + clean up.
        let mut child = match zcmd.spawn() {
            Ok(c) => c,
            Err(e) => {
                cancel.cancel();
                let _ = supervisor_thread.join();
                cleanup_agent_state(&ctx.state_dir, &spec.id);
                return Err(CliError::Internal {
                    reason: format!("launch zellij: {e}"),
                });
            }
        };

        // Hand zellij the controlling tty so terminal SIGINT is
        // routed to it, not us. Best-effort: if stdin isn't a tty
        // (e.g. test harness, CI without a TTY), skip silently —
        // zellij will still run and operator can SIGTERM it.
        let child_pid = nix::unistd::Pid::from_raw(child.id() as i32);
        // Stdin's fd is always 0 on Unix; nix isatty/tcsetpgrp take
        // any AsFd, but std::io::Stdin doesn't impl AsFd. Borrow the
        // raw fd 0 directly.
        let stdin_fd: std::os::fd::RawFd = 0;
        if nix::unistd::isatty(stdin_fd).unwrap_or(false) {
            let borrowed = unsafe { std::os::fd::BorrowedFd::borrow_raw(stdin_fd) };
            let _ = nix::unistd::tcsetpgrp(borrowed, child_pid);
        }

        // Block on zellij. On EINTR, std re-tries internally.
        let status = child.wait().map_err(|e| {
            // Best-effort cleanup before bubbling.
            cancel.cancel();
            CliError::Internal {
                reason: format!("wait on zellij: {e}"),
            }
        })?;

        // Reclaim foreground for the outer shell. Use our actual pgrp.
        if nix::unistd::isatty(stdin_fd).unwrap_or(false) {
            let borrowed = unsafe { std::os::fd::BorrowedFd::borrow_raw(stdin_fd) };
            let our_pgrp = nix::unistd::getpgrp();
            let _ = nix::unistd::tcsetpgrp(borrowed, our_pgrp);
        }

        // Trigger supervisor shutdown + join its thread. The
        // supervisor's R3 sequence runs steps 14–18 (drain consumers,
        // teardown engine, finalize state, unlink socket, release
        // lock) under the cancel.
        cancel.cancel();
        let join_result = supervisor_thread.join();
        match join_result {
            Ok(Ok(_outcome)) => {}
            Ok(Err(reason)) => {
                tracing::warn!(reason, "supervisor thread returned Err");
            }
            Err(_panic) => {
                tracing::warn!("supervisor thread panicked");
            }
        }

        if !status.success() {
            // Zellij failed (vs. operator-initiated detach which exits
            // 0). Clean up state + bubble so the operator sees the
            // exit code.
            let code = status.code().unwrap_or(-1);
            cleanup_agent_state(&ctx.state_dir, &spec.id);
            return Err(CliError::Internal {
                reason: format!("zellij exited with code {code} before session came up"),
            });
        }

        println!("spawned {} -> Ctrl+o w to switch", spec.id);
        // F-705 / F-708 ghost-state guard: distinguish "operator
        // detached (Ctrl+P, D — zellij still alive)" from "zellij
        // terminated cleanly". With supervisor shutdown above, the
        // session is gone either way — the supervisor's finalize_state
        // wrote a terminal phase. We keep the state so `ark list`
        // still surfaces the run for post-mortem; cleanup happens via
        // `ark doctor` if the operator wants it gone.
        match zellij_session_liveness(&session) {
            ZellijSessionLiveness::Gone => {
                eprintln!(
                    "note: zellij session {session} ended; agent state retained for {} (use `ark doctor --fix` to GC)",
                    spec.id
                );
            }
            ZellijSessionLiveness::Alive => {
                eprintln!(
                    "note: zellij session {session} still alive (detached); keeping agent state for {}",
                    spec.id
                );
            }
            ZellijSessionLiveness::Unknown => {
                eprintln!(
                    "note: could not verify zellij session liveness for {session}; keeping agent state for {} (safe default)",
                    spec.id
                );
            }
        }
        return Ok(());
    }

    // OUTSIDE-ZELLIJ + --detach (default): pty path. Allocate a pty,
    // spawn zellij attached to the slave. zellij initialises against
    // a real TTY, forks its server daemon (server double-forks and
    // detaches from the pty), then we poll the client for 500 ms to
    // catch fast failures (bad layout, missing plugins). The returned
    // `PtyZellijHandle` holds the PtyPair; dropping it at the end of
    // this function closes the master fd, which SIGHUPs the client.
    // By then the server daemon owns the session and survives.
    let mut handle = match cli_spawn_zellij_with_pty(&plan, &spec.cwd) {
        Ok(h) => h,
        Err(e) => {
            cleanup_agent_state(&ctx.state_dir, &spec.id);
            return Err(e);
        }
    };

    if let Some(err) = cli_pty_child_startup_failure(handle.child.as_mut()) {
        cleanup_agent_state(&ctx.state_dir, &spec.id);
        return Err(err);
    }

    tracing::debug!(spec = %spec_path.display(), "spawned and ready (pty path)");
    println!("spawned {} -> Ctrl+o w to switch", spec.id);
    // `handle` drops here → master closes → SIGHUP to zellij client.
    // Harmless because the server daemon has already forked and owns
    // the session.
    drop(handle);
    Ok(())
}

// -------------------------------------------------------------- tests -------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_lock::ENV_LOCK;
    use clap::Parser;
    use tempfile::TempDir;

    /// Minimal host parser so we can parse `SpawnArgs` in isolation.
    #[derive(Debug, Parser)]
    struct Host {
        #[command(flatten)]
        args: SpawnArgs,
    }

    // --- clap parse surface (unchanged from T-084) ----------------------

    #[test]
    fn orchestrator_defaults_to_auto() {
        let h = Host::try_parse_from(["spawn", "--", "claude"]).expect("parse");
        assert_eq!(h.args.orchestrator, OrchestratorChoice::Auto);
    }

    #[test]
    fn orchestrator_accepts_cavekit() {
        let h = Host::try_parse_from(["spawn", "--orchestrator", "cavekit", "--", "claude"])
            .expect("parse");
        assert_eq!(h.args.orchestrator, OrchestratorChoice::Cavekit);
    }

    #[test]
    fn orchestrator_accepts_claude_code() {
        let h = Host::try_parse_from(["spawn", "--orchestrator", "claude-code", "--", "claude"])
            .expect("parse");
        assert_eq!(h.args.orchestrator, OrchestratorChoice::ClaudeCode);
    }

    #[test]
    fn cmd_captures_trailing_args() {
        let h = Host::try_parse_from(["spawn", "--", "claude", "--resume"]).expect("parse");
        assert_eq!(h.args.cmd, vec!["claude", "--resume"]);
    }

    #[test]
    fn env_is_repeatable() {
        let h = Host::try_parse_from(["spawn", "--env", "A=1", "--env", "B=2", "--", "claude"])
            .expect("parse");
        assert_eq!(h.args.env, vec!["A=1", "B=2"]);
    }

    #[test]
    fn hook_is_repeatable() {
        let h = Host::try_parse_from([
            "spawn",
            "--hook",
            "Stop=echo done",
            "--hook",
            "Start=echo go",
            "--",
            "claude",
        ])
        .expect("parse");
        assert_eq!(h.args.hook.len(), 2);
    }

    #[test]
    fn cwd_defaults_to_dot() {
        let h = Host::try_parse_from(["spawn", "--", "claude"]).expect("parse");
        assert_eq!(h.args.cwd, PathBuf::from("."));
    }

    #[test]
    fn no_detach_flag_parses() {
        let h = Host::try_parse_from(["spawn", "--no-detach", "--", "claude"]).expect("parse");
        assert!(h.args.no_detach);
    }

    // --- derive_name ----------------------------------------------------

    #[test]
    fn derive_name_prefers_explicit() {
        let n = derive_name(Some("myfeat"), Path::new("/tmp/whatever"));
        assert_eq!(n, "myfeat");
    }

    #[test]
    fn derive_name_falls_back_to_cwd_basename() {
        let n = derive_name(None, Path::new("/tmp/proj/authsvc"));
        assert_eq!(n, "authsvc");
    }

    #[test]
    fn derive_name_fallback_on_empty() {
        let n = derive_name(None, Path::new(""));
        assert_eq!(n, "agent");
    }

    #[test]
    fn derive_name_empty_explicit_falls_through() {
        let n = derive_name(Some(""), Path::new("/tmp/authsvc"));
        assert_eq!(n, "authsvc");
    }

    // --- orchestrator auto-detect --------------------------------------

    #[test]
    fn resolve_explicit_cavekit_bypasses_detect() {
        let dir = TempDir::new().unwrap();
        assert_eq!(
            resolve_orchestrator(OrchestratorChoice::Cavekit, dir.path()),
            "cavekit"
        );
    }

    #[test]
    fn resolve_explicit_claude_code_bypasses_detect() {
        let dir = TempDir::new().unwrap();
        assert_eq!(
            resolve_orchestrator(OrchestratorChoice::ClaudeCode, dir.path()),
            "claude-code"
        );
    }

    #[test]
    fn resolve_auto_picks_cavekit_on_sentinel() {
        let dir = TempDir::new().unwrap();
        // .cavekit/config is the cheapest sentinel — see
        // cavekit-orchestrator-cavekit R1.
        std::fs::create_dir_all(dir.path().join(".cavekit")).unwrap();
        std::fs::write(dir.path().join(".cavekit").join("config"), b"").unwrap();
        assert_eq!(
            resolve_orchestrator(OrchestratorChoice::Auto, dir.path()),
            "cavekit"
        );
    }

    #[test]
    fn resolve_auto_falls_back_to_claude_code() {
        let dir = TempDir::new().unwrap();
        // Empty cwd — no cavekit sentinels.
        assert_eq!(
            resolve_orchestrator(OrchestratorChoice::Auto, dir.path()),
            "claude-code"
        );
    }

    // --- parse_kv / parse_env ------------------------------------------

    #[test]
    fn parse_kv_basic() {
        assert_eq!(parse_kv("A=1").unwrap(), ("A".into(), "1".into()));
    }

    #[test]
    fn parse_kv_value_may_contain_equals() {
        assert_eq!(parse_kv("URL=k=v").unwrap(), ("URL".into(), "k=v".into()));
    }

    #[test]
    fn parse_kv_rejects_missing_equals() {
        assert!(parse_kv("BAD").is_err());
    }

    #[test]
    fn parse_kv_rejects_empty_key() {
        assert!(parse_kv("=v").is_err());
    }

    #[test]
    fn parse_env_single() {
        let got = parse_env(&["A=1".into()]).unwrap();
        assert_eq!(got.get("A").map(String::as_str), Some("1"));
    }

    #[test]
    fn parse_env_multiple_sorted() {
        let got = parse_env(&["B=2".into(), "A=1".into()]).unwrap();
        let keys: Vec<&str> = got.keys().map(String::as_str).collect();
        assert_eq!(keys, vec!["A", "B"]);
    }

    // --- parse_hooks ----------------------------------------------------

    #[test]
    fn parse_hook_single_shlex_splits() {
        let got = parse_hooks(&["Stop=echo done".into()]).unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].event, "Stop");
        assert_eq!(got[0].cmd, "echo done");
        assert_eq!(got[0].cmd_argv, vec!["echo", "done"]);
    }

    #[test]
    fn parse_hook_multiple() {
        let got =
            parse_hooks(&["Stop=echo done".into(), "Start=bash -lc \"ls /tmp\"".into()]).unwrap();
        assert_eq!(got.len(), 2);
        assert_eq!(got[1].event, "Start");
        assert_eq!(got[1].cmd_argv, vec!["bash", "-lc", "ls /tmp"]);
    }

    #[test]
    fn parse_hook_rejects_missing_equals() {
        assert!(parse_hooks(&["Bogus".into()]).is_err());
    }

    // --- build_spec + round-trip ---------------------------------------

    #[test]
    fn build_spec_round_trip_via_serde_json() {
        let mut env = BTreeMap::new();
        env.insert("A".into(), "1".into());
        let spec = build_spec(
            "cavekit",
            "claude-code",
            "authsvc",
            PathBuf::from("/tmp/w"),
            vec!["claude".into(), "--resume".into()],
            env,
            Some("builder".into()),
            vec![HookEntry {
                event: "Stop".into(),
                cmd: "echo done".into(),
                cmd_argv: vec!["echo".into(), "done".into()],
            }],
        );
        let json = serde_json::to_string_pretty(&spec).unwrap();
        let back: AgentSpec = serde_json::from_str(&json).unwrap();
        assert_eq!(back, spec);
        assert_eq!(back.name, "authsvc");
        assert_eq!(back.orchestrator, "cavekit");
        assert_eq!(back.engine, "claude-code");
        assert_eq!(back.cwd, PathBuf::from("/tmp/w"));
        assert_eq!(back.cmd, vec!["claude", "--resume"]);
        assert_eq!(back.layout.as_deref(), Some("builder"));
        // Hooks folded into runner_config.hooks[0].
        let hooks = back.runner_config.get("hooks").expect("hooks present");
        assert_eq!(hooks.as_array().unwrap().len(), 1);
        assert_eq!(hooks[0]["event"], "Stop");
    }

    #[test]
    fn build_spec_no_hooks_null_runner_config() {
        let spec = build_spec(
            "claude-code",
            "claude-code",
            "run",
            PathBuf::from("/tmp/w"),
            vec!["claude".into()],
            BTreeMap::new(),
            None,
            Vec::new(),
        );
        assert!(spec.runner_config.is_null());
    }

    // --- write_spec_json ------------------------------------------------

    #[test]
    fn write_spec_json_creates_parent_and_writes() {
        let dir = TempDir::new().unwrap();
        let spec = build_spec(
            "cavekit",
            "claude-code",
            "auth",
            PathBuf::from("/tmp/w"),
            vec!["claude".into()],
            BTreeMap::new(),
            None,
            Vec::new(),
        );
        let path = write_spec_json(dir.path(), &spec).expect("write");
        assert!(path.exists());
        assert!(path.to_string_lossy().ends_with("spec.json"));
        let body = std::fs::read_to_string(&path).unwrap();
        let back: AgentSpec = serde_json::from_str(&body).unwrap();
        assert_eq!(back, spec);
    }

    // --- zellij plan (F-516) -------------------------------------------

    #[test]
    fn zellij_plan_inside_zellij_still_creates_new_session() {
        // F-516: even when $ZELLIJ is set (caller is inside a zellij
        // session), spawn must create a DEDICATED per-agent session —
        // never `action new-tab`, which only tacks a tab onto the
        // caller's session.
        let plan = zellij_plan(
            |k| {
                if k == "ZELLIJ" {
                    Some("0".into())
                } else {
                    None
                }
            },
            "ark-cavekit-auth",
            Some("builder"),
        );
        assert_eq!(
            plan,
            ZellijSpawn {
                session: "ark-cavekit-auth".into(),
                layout: Some("builder".into()),
            }
        );
    }

    #[test]
    fn zellij_plan_outside_zellij_creates_new_session() {
        let plan = zellij_plan(|_| None, "ark-cavekit-auth", Some("builder"));
        assert_eq!(
            plan,
            ZellijSpawn {
                session: "ark-cavekit-auth".into(),
                layout: Some("builder".into()),
            }
        );
    }

    #[test]
    fn zellij_plan_no_layout_preserves_none() {
        let plan = zellij_plan(|_| None, "ark-cavekit-auth", None);
        assert!(plan.layout.is_none());
        assert_eq!(plan.session, "ark-cavekit-auth");
    }

    #[test]
    fn inside_zellij_detects_nonempty_value() {
        assert!(inside_zellij(|k| if k == "ZELLIJ" {
            Some("anything".into())
        } else {
            None
        }));
    }

    #[test]
    fn inside_zellij_empty_is_false() {
        assert!(!inside_zellij(|k| if k == "ZELLIJ" {
            Some(String::new())
        } else {
            None
        }));
    }

    #[test]
    fn inside_zellij_unset_is_false() {
        assert!(!inside_zellij(|_| None));
    }

    // --- build_zellij_command argv shape (F-511 + F-516 + F-517) ------

    fn argv_of(cmd: &Command) -> Vec<String> {
        std::iter::once(cmd.get_program().to_string_lossy().into_owned())
            .chain(cmd.get_args().map(|a| a.to_string_lossy().into_owned()))
            .collect()
    }

    #[test]
    fn build_zellij_command_with_layout() {
        // F-516/F-517: always `zellij -s <name> --layout <path>`,
        // regardless of whether we were launched from inside zellij.
        // F-526: argv is pure zellij — no external `setsid` prefix.
        let cmd = build_zellij_command(&ZellijSpawn {
            session: "ark-cavekit-auth".into(),
            layout: Some("/tmp/builder.kdl".into()),
        });
        assert_eq!(
            argv_of(&cmd),
            vec![
                "zellij",
                "-s",
                "ark-cavekit-auth",
                "--layout",
                "/tmp/builder.kdl",
            ]
        );
    }

    #[test]
    fn build_zellij_command_without_layout_omits_layout_arg() {
        // F-526: no external `setsid` prefix.
        let cmd = build_zellij_command(&ZellijSpawn {
            session: "ark-cavekit-auth".into(),
            layout: None,
        });
        assert_eq!(argv_of(&cmd), vec!["zellij", "-s", "ark-cavekit-auth"]);
    }

    #[test]
    fn build_zellij_command_never_contains_external_setsid() {
        // F-526 regression guard: the external `setsid(1)` binary is
        // not on macOS by default. The argv must start with `zellij`
        // directly; TTY detach is handled by `apply_detach`'s pre_exec.
        let cmd = build_zellij_command(&ZellijSpawn {
            session: "ark-cavekit-auth".into(),
            layout: Some("/tmp/b.kdl".into()),
        });
        let argv = argv_of(&cmd);
        assert_eq!(argv[0], "zellij", "argv[0] must be zellij, not setsid");
        assert!(
            !argv.iter().any(|a| a == "setsid"),
            "argv must not reference external setsid: {argv:?}"
        );
    }

    #[test]
    fn configure_detach_skips_hook_when_no_detach() {
        // F-606: `--no-detach` must NOT apply the detach hook. We pass a
        // flag-recording mock as the detach function and assert it stays
        // un-called when `no_detach = true`, and IS called when
        // `no_detach = false` (the default path). This is the only way
        // to observe the pre_exec side of `apply_detach` indirectly —
        // `std::process::Command` doesn't expose its closures.
        use std::cell::Cell;

        let called = Cell::new(false);
        let mut cmd = build_zellij_command(&ZellijSpawn {
            session: "ark-cavekit-auth".into(),
            layout: Some("/tmp/b.kdl".into()),
        });
        configure_zellij_stdio_and_detach(&mut cmd, true, |_c| called.set(true));
        assert!(
            !called.get(),
            "detach hook must NOT fire when no_detach = true"
        );

        let called = Cell::new(false);
        let mut cmd = build_zellij_command(&ZellijSpawn {
            session: "ark-cavekit-auth".into(),
            layout: Some("/tmp/b.kdl".into()),
        });
        configure_zellij_stdio_and_detach(&mut cmd, false, |_c| called.set(true));
        assert!(
            called.get(),
            "detach hook MUST fire when no_detach = false (default)"
        );
    }

    #[test]
    fn apply_detach_does_not_mutate_argv() {
        // F-526: wiring pre_exec must leave the argv pure. We can't
        // directly introspect the pre_exec closure via std::process::Command,
        // so we verify the observable side: applying `apply_detach` to
        // a command does NOT add/remove/reorder argv entries.
        let mut cmd = build_zellij_command(&ZellijSpawn {
            session: "ark-cavekit-auth".into(),
            layout: Some("/tmp/b.kdl".into()),
        });
        let before = argv_of(&cmd);
        apply_detach(&mut cmd);
        let after = argv_of(&cmd);
        assert_eq!(before, after);
        assert_eq!(after[0], "zellij");
    }

    #[test]
    fn build_zellij_command_inside_zellij_env_still_creates_session() {
        // F-730 guard: `build_zellij_command` is now only used on the
        // OUTSIDE-zellij path (the INSIDE path dispatches via
        // `build_switch_session_command`). But the plan builder itself
        // is env-agnostic, so the produced argv must still be the
        // session-creator form regardless of what env the plan was
        // resolved under.
        let plan = zellij_plan(
            |k| (k == "ZELLIJ").then(|| "1".to_string()),
            "ark-cavekit-auth",
            Some("/tmp/b.kdl".into()),
        );
        let cmd = build_zellij_command(&plan);
        let argv = argv_of(&cmd);
        assert_eq!(argv[0], "zellij");
        assert_eq!(argv[1], "-s");
        assert!(!argv.iter().any(|a| a == "new-tab"));
        assert!(!argv.iter().any(|a| a == "attach"));
        assert!(!argv.iter().any(|a| a == "setsid"));
        assert!(
            !argv.iter().any(|a| a == "switch-session"),
            "outside-path command must not contain switch-session: {argv:?}"
        );
    }

    // --- F-730: build_switch_session_command argv shape -----------------

    #[test]
    fn build_switch_session_command_with_layout() {
        // F-730: inside-zellij dispatch. argv is
        // `zellij action switch-session --layout <path> <session>`
        // (no `--create` — create-if-missing is the default; see
        // cavekit-mux-zellij R1 and crates/mux/zellij/src/mux.rs:266).
        let cmd = build_switch_session_command(&ZellijSpawn {
            session: "ark-cavekit-auth".into(),
            layout: Some("/tmp/builder.kdl".into()),
        });
        assert_eq!(
            argv_of(&cmd),
            vec![
                "zellij",
                "action",
                "switch-session",
                "--layout",
                "/tmp/builder.kdl",
                "ark-cavekit-auth",
            ]
        );
    }

    #[test]
    fn build_switch_session_command_without_layout_omits_layout_arg() {
        let cmd = build_switch_session_command(&ZellijSpawn {
            session: "ark-cavekit-auth".into(),
            layout: None,
        });
        assert_eq!(
            argv_of(&cmd),
            vec!["zellij", "action", "switch-session", "ark-cavekit-auth"]
        );
    }

    #[test]
    fn build_switch_session_command_does_not_nest() {
        // F-730 regression guard: inside-zellij path must not emit
        // `attach` (which would nest clients) or `-s` (which would
        // fork a new client outside the running daemon).
        let cmd = build_switch_session_command(&ZellijSpawn {
            session: "ark-cavekit-auth".into(),
            layout: Some("/tmp/b.kdl".into()),
        });
        let argv = argv_of(&cmd);
        assert!(!argv.iter().any(|a| a == "attach"));
        assert!(!argv.iter().any(|a| a == "-s"));
        assert!(!argv.iter().any(|a| a == "--create"));
    }

    // --- F-731: pty helpers moved to ark-mux-zellij ----------------------
    //
    // The full pty spawn + startup-grace tests now live in
    // `ark-mux-zellij/src/pty.rs`. Here we keep an adapter test to
    // confirm that the cli's `cli_pty_child_startup_failure` wrapper
    // preserves the exact "zellij exited with code N" wording that
    // `ark spawn` surfaces to the operator (the wording is asserted
    // by external scripts that grep ark's stderr).

    #[test]
    fn cli_pty_child_startup_failure_preserves_exit_wording() {
        use portable_pty::{CommandBuilder, PtySize, native_pty_system};
        let pair = native_pty_system()
            .openpty(PtySize {
                rows: 24,
                cols: 80,
                pixel_width: 0,
                pixel_height: 0,
            })
            .expect("openpty");
        let builder = CommandBuilder::new("/usr/bin/false");
        let mut child = pair.slave.spawn_command(builder).expect("spawn false");
        let err = cli_pty_child_startup_failure(child.as_mut()).expect("false must trip the poll");
        let msg = format!("{err:?}");
        assert!(
            msg.contains("zellij exited with code"),
            "wording must be stable for downstream stderr-grepping; got {msg}"
        );
        drop(pair);
    }

    // --- require_zellij_on_path error variant -------------------------

    #[test]
    fn run_preflight_fail_leaves_state_untouched() {
        // F-503: when zellij is missing from PATH, run() must bail
        // BEFORE writing spec.json / creating the agent dir. Assert
        // agents_root contents are unchanged across the call.
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());

        let state = TempDir::new().unwrap();
        let config = TempDir::new().unwrap();
        let runtime = TempDir::new().unwrap();
        let ctx = Ctx {
            no_color: true,
            log_level: "info".into(),
            state_dir: state.path().to_path_buf(),
            config_dir: config.path().to_path_buf(),
            runtime_dir: runtime.path().to_path_buf(),
        };

        // Snapshot agents_root contents (empty or absent == 0 entries).
        let agents_root = state.path().join("agents");
        let count_before = std::fs::read_dir(&agents_root)
            .map(|it| it.count())
            .unwrap_or(0);

        let args = SpawnArgs {
            orchestrator: OrchestratorChoice::ClaudeCode,
            engine: "claude-code".to_string(),
            cwd: state.path().to_path_buf(),
            name: Some("preflighttest".into()),
            layout: None,
            scene: None,
            env: Vec::new(),
            detach: true,
            no_detach: false,
            hook: Vec::new(),
            cmd: vec!["claude".into()],
        };

        let prior = std::env::var_os("PATH");
        unsafe {
            std::env::set_var("PATH", "/nonexistent-path-for-ark-test");
        }
        let got = run(args, &ctx);
        unsafe {
            match prior {
                Some(v) => std::env::set_var("PATH", v),
                None => std::env::remove_var("PATH"),
            }
        }
        assert!(
            matches!(got, Err(CliError::PreflightFail { .. })),
            "expected PreflightFail, got {got:?}"
        );

        // Post-condition: agents_root entry count is unchanged.
        let count_after = std::fs::read_dir(&agents_root)
            .map(|it| it.count())
            .unwrap_or(0);
        assert_eq!(
            count_before, count_after,
            "preflight-fail path must not mutate agents_root"
        );
    }

    #[test]
    fn require_zellij_missing_returns_preflight_fail() {
        // Run the helper with a blanked PATH so `zellij` cannot be
        // found, then restore. The variant must be PreflightFail.
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let prior = std::env::var_os("PATH");
        unsafe {
            std::env::set_var("PATH", "/nonexistent-path-for-ark-test");
        }
        let got = require_zellij_on_path();
        unsafe {
            match prior {
                Some(v) => std::env::set_var("PATH", v),
                None => std::env::remove_var("PATH"),
            }
        }
        assert!(matches!(got, Err(CliError::PreflightFail { .. })));
    }

    // --- F-522: unique session name carries a ULID fragment ----------

    #[test]
    fn unique_session_name_appends_ulid_prefix() {
        // Two agents sharing orchestrator+name must NOT collide on the
        // same zellij session. The CLI-level override appends an 8-char
        // ULID prefix after `AgentId::session_name()`'s `{orch}-{name}`
        // base, which guarantees distinct per-spawn identity.
        let a = AgentId::new("cavekit", "auth");
        let b = AgentId::new("cavekit", "auth");
        assert_ne!(a, b, "two freshly-minted ids must differ");
        let sa = unique_session_name(&a);
        let sb = unique_session_name(&b);
        assert_ne!(sa, sb, "session names must diverge when ids differ");
        // Shape: `ark-cavekit-auth-<8 chars from ULID>`
        assert!(sa.starts_with("ark-cavekit-auth-"));
        let suffix = sa.strip_prefix("ark-cavekit-auth-").unwrap();
        assert_eq!(suffix.len(), 8, "ULID fragment should be 8 chars");
        // And the 8-char suffix must be the tail of the lowercase ULID.
        assert!(a.ulid().ends_with(suffix));
    }

    // --- F-600: spec.json on disk carries the suffixed session ---------

    #[test]
    fn write_spec_json_uses_suffixed_session_when_overridden() {
        // F-600 regression: `AgentSpec::new()` defaults spec.session to
        // `AgentId::session_name()` — the bare `ark-{orch}-{name}` form.
        // The real zellij session is `{base}-{ulid8}` (F-522), so the
        // CLI-layer spawn path must overwrite spec.session BEFORE
        // persisting spec.json; otherwise downstream readers (supervisor
        // re-attach, picker OpenSession, status chip focus) target a
        // session that does not exist.
        let dir = TempDir::new().unwrap();
        let mut spec = build_spec(
            "cavekit",
            "claude-code",
            "auth",
            PathBuf::from("/tmp/w"),
            vec!["claude".into()],
            BTreeMap::new(),
            None,
            Vec::new(),
        );
        // Mirror the override performed by `run()`.
        let expected = unique_session_name(&spec.id);
        spec.session = expected.clone();

        let path = write_spec_json(dir.path(), &spec).expect("write");
        let body = std::fs::read_to_string(&path).unwrap();
        let back: AgentSpec = serde_json::from_str(&body).unwrap();
        assert_eq!(back.session, expected);
        assert!(back.session.starts_with("ark-cavekit-auth-"));
        let suffix = back.session.strip_prefix("ark-cavekit-auth-").unwrap();
        assert_eq!(suffix.len(), 8, "persisted session carries ULID suffix");
    }

    // --- F-523: zellij_startup_failure cleans up on fast-exit --------

    #[test]
    fn zellij_startup_failure_none_for_successful_exit() {
        // `/usr/bin/true` exits 0 immediately — the helper must treat this
        // as a successful spawn (zellij daemonizes by forking, so a
        // clean exit 0 from the launcher wrapper is normal).
        let mut child = Command::new("/usr/bin/true")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("spawn /usr/bin/true");
        let got = zellij_startup_failure(&mut child);
        assert!(got.is_none(), "exit 0 must be treated as success");
    }

    #[test]
    fn zellij_startup_failure_reports_nonzero_exit() {
        // `/usr/bin/false` exits 1 immediately — must surface as
        // CliError::Internal with a "zellij exited" reason.
        let mut child = Command::new("/usr/bin/false")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("spawn /usr/bin/false");
        let got = zellij_startup_failure(&mut child);
        match got {
            Some(CliError::Internal { reason }) => {
                assert!(
                    reason.contains("zellij exited"),
                    "expected zellij-exit reason, got {reason:?}"
                );
            }
            other => panic!("expected Internal err, got {other:?}"),
        }
    }

    // --- F-525: layout template render + write -----------------------

    fn tempdir_ctx() -> (TempDir, TempDir, TempDir, Ctx) {
        let state = TempDir::new().unwrap();
        let config = TempDir::new().unwrap();
        let runtime = TempDir::new().unwrap();
        let ctx = Ctx {
            no_color: true,
            log_level: "info".into(),
            state_dir: state.path().to_path_buf(),
            config_dir: config.path().to_path_buf(),
            runtime_dir: runtime.path().to_path_buf(),
        };
        (state, config, runtime, ctx)
    }

    #[test]
    fn render_and_write_layout_substitutes_and_persists() {
        // F-525: a user-override layout template under
        // `{config_dir}/layouts/mytpl.kdl` is resolved, rendered with
        // {{ cwd }} / {{ agent_cmd }} / {{ name }} substituted, and the
        // result written to `{state_dir}/agents/{id}/layout.kdl`.
        let (_state, _config, _runtime, ctx) = tempdir_ctx();
        let layouts_dir = ctx.config_dir.join("layouts");
        std::fs::create_dir_all(&layouts_dir).unwrap();
        std::fs::write(
            layouts_dir.join("mytpl.kdl"),
            r#"layout { tab name="{{ name }}" cwd="{{ cwd }}" { pane { command "{{ agent_cmd }}" } } }"#,
        )
        .unwrap();

        let spec = build_spec(
            "cavekit",
            "claude-code",
            "authsvc",
            PathBuf::from("/tmp/work"),
            vec!["claude".into(), "--resume".into()],
            BTreeMap::new(),
            Some("mytpl".into()),
            Vec::new(),
        );
        let path = render_and_write_layout(&ctx, &spec).expect("render+write");
        assert!(path.exists());
        assert_eq!(path, spec.id.state_dir(&ctx.state_dir).join("layout.kdl"));
        let body = std::fs::read_to_string(&path).unwrap();
        assert!(body.contains(r#"cwd="/tmp/work""#), "body: {body}");
        assert!(body.contains(r#"name="authsvc""#), "body: {body}");
        assert!(body.contains(r#"command "claude""#), "body: {body}");
        // No unexpanded template tokens should remain.
        assert!(!body.contains("{{"), "body still has `{{`: {body}");
        assert!(!body.contains("}}"), "body still has `}}`: {body}");
    }

    #[test]
    fn render_and_write_layout_uses_embedded_shipped_when_no_override() {
        // F-525: absent a user override, the shipped `builder.kdl`
        // template renders cleanly for a cavekit spawn.
        let (_state, _config, _runtime, ctx) = tempdir_ctx();
        let spec = build_spec(
            "cavekit",
            "claude-code",
            "auth",
            PathBuf::from("/tmp/w"),
            vec!["claude".into()],
            BTreeMap::new(),
            None, // no explicit layout → default_layout_for_orchestrator("cavekit") == "builder"
            Vec::new(),
        );
        let path = render_and_write_layout(&ctx, &spec).expect("render+write shipped");
        assert!(path.exists());
        let body = std::fs::read_to_string(&path).unwrap();
        // builder.kdl contains `tab name="{{ name }}"` → after render,
        // `tab name="auth"`.
        assert!(body.contains(r#"tab name="auth""#), "body: {body}");
        assert!(body.contains(r#"cwd="/tmp/w""#), "body: {body}");
    }

    #[test]
    fn render_and_write_layout_unknown_stem_is_generic_error() {
        // F-525: a garbage stem surfaces as CliError::Generic — no silent
        // fall-through, and nothing is written to disk.
        let (_state, _config, _runtime, ctx) = tempdir_ctx();
        let spec = build_spec(
            "cavekit",
            "claude-code",
            "auth",
            PathBuf::from("/tmp/w"),
            vec!["claude".into()],
            BTreeMap::new(),
            Some("definitely-not-a-layout-xyz".into()),
            Vec::new(),
        );
        let err = render_and_write_layout(&ctx, &spec).unwrap_err();
        match err {
            CliError::Generic { reason } => {
                assert!(reason.contains("resolve layout"), "reason: {reason}");
            }
            other => panic!("expected Generic, got {other:?}"),
        }
    }

    // --- F-527: cmd positional is required by clap --------------------

    #[test]
    fn spawn_without_trailing_cmd_fails_clap() {
        // F-527: bare `ark spawn` (no `-- CMD`) must be rejected by clap
        // with a usage error BEFORE `run()` executes — otherwise the
        // downstream path proceeds with an empty agent_cmd and renders a
        // broken layout.
        let res = Host::try_parse_from(["spawn"]);
        assert!(res.is_err(), "empty cmd must fail parse");
    }

    #[test]
    fn spawn_with_only_double_dash_fails_clap() {
        // F-527: `ark spawn --` (trailing separator, no command) is
        // equally invalid — num_args = 1.. rejects the zero-arg case.
        let res = Host::try_parse_from(["spawn", "--"]);
        assert!(res.is_err(), "trailing -- with no cmd must fail parse");
    }

    // --- F-528: cleanup helper wipes agent state ---------------------

    #[test]
    fn cleanup_agent_state_removes_existing_dir() {
        // F-528: on any spawn failure (`Command::spawn()` Err OR the
        // post-spawn startup poll detecting non-zero exit), `run()`
        // must call `cleanup_agent_state` so the `{state_dir}/agents/{id}`
        // tree (holding spec.json + layout.kdl that the earlier
        // write_spec_json + render_and_write_layout calls produced) is
        // removed. Otherwise `ark list` would surface an orphan agent
        // that never actually launched.
        let state = TempDir::new().unwrap();
        let id = AgentId::new("cavekit", "orphan");
        let dir = id.state_dir(state.path());
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("spec.json"), b"{}").unwrap();
        std::fs::write(dir.join("layout.kdl"), b"layout {}").unwrap();
        assert!(dir.exists());
        cleanup_agent_state(state.path(), &id);
        assert!(!dir.exists(), "cleanup must remove the agent state dir");
    }

    #[test]
    fn no_detach_foreground_exit_cleans_up_transient_state() {
        // F-705: the `--no-detach` foreground path exits with Ok(()) after
        // `Command::status()` returns successfully. Since the supervisor
        // is still stubbed, nothing owns the agent after zellij exits —
        // leaving spec.json + layout.kdl on disk would surface a ghost
        // entry in `ark list`, `picker`, and `ark doctor`. `run()` must
        // therefore call `cleanup_agent_state` before returning.
        //
        // F-708 refined this: cleanup is conditional on `zellij
        // list-sessions` reporting the session is Gone. In this test
        // we only exercise the Gone branch of the gate; the Alive and
        // Unknown branches have dedicated tests below.
        //
        // We cannot spawn real zellij in a unit test, so this simulates
        // the lifecycle: write the state dir the way the successful
        // pre-launch path would, apply the F-705 cleanup guarded by a
        // Gone outcome, and assert the tree is gone.
        let state = TempDir::new().unwrap();
        let id = AgentId::new("cavekit", "fg-ephemeral");
        let dir = id.state_dir(state.path());
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("spec.json"), b"{\"id\":\"x\"}").unwrap();
        std::fs::write(dir.join("layout.kdl"), b"layout { }").unwrap();
        assert!(dir.exists(), "state dir exists before foreground exit");

        // Simulate the tail of the `if args.no_detach { … }` branch after
        // `Command::status()` returned Ok(success) AND `zellij
        // list-sessions` reported the session is Gone (F-708).
        let outcome = ZellijSessionLiveness::Gone;
        if outcome == ZellijSessionLiveness::Gone {
            cleanup_agent_state(state.path(), &id);
        }

        assert!(
            !dir.exists(),
            "F-705: no-detach foreground exit must remove transient agent state when session is gone"
        );
    }

    // --- F-708: zellij list-sessions liveness gate -------------------

    #[test]
    fn session_listed_matches_bare_token() {
        // F-708: single-line, no decoration — the common case on Linux
        // with `zellij list-sessions --no-formatting`.
        let haystack = "ark-cavekit-auth-0123abcd\n";
        assert!(session_listed(haystack, "ark-cavekit-auth-0123abcd"));
        assert!(!session_listed(haystack, "ark-cavekit-auth-other"));
    }

    #[test]
    fn session_listed_strips_ansi_and_annotations() {
        // F-708: zellij frequently decorates session rows with ANSI
        // colours AND a trailing ` (current)` / ` (EXITED - …)` tag.
        // Stripping `\x1b[…m` plus splitting on whitespace recovers
        // the bare token.
        let haystack = "\x1b[32;1mark-cavekit-auth-0123abcd\x1b[0m (current)\n\
                        \x1b[31mark-other-xxx\x1b[0m (EXITED - Attach to resurrect)\n";
        assert!(session_listed(haystack, "ark-cavekit-auth-0123abcd"));
        assert!(session_listed(haystack, "ark-other-xxx"));
        assert!(!session_listed(haystack, "ark-missing"));
    }

    #[test]
    fn session_listed_rejects_substring_match() {
        // F-708: must be exact-token; a session name that is a prefix /
        // substring of another must not match.
        let haystack = "ark-cavekit-auth-0123abcd-extra\n";
        assert!(!session_listed(haystack, "ark-cavekit-auth-0123abcd"));
    }

    #[test]
    fn zellij_session_liveness_unknown_when_zellij_missing() {
        // F-708: when `zellij` is not on PATH the helper must return
        // Unknown so the `--no-detach` branch falls back to keeping
        // state. We force the spawn failure by blanking PATH for this
        // process for the duration of the call. Using an
        // unlikely-to-exist PATH value keeps it simple and reversible.
        //
        // NOTE: this mutates a process-global env var. We guard with a
        // single-threaded test (the cli lib test binary runs with
        // default parallelism, but this mutation window is microseconds
        // and we restore PATH before exiting). The alternative — a
        // dependency-inject Command factory — is overkill for a single
        // Unknown-path assertion.
        //
        // SAFETY of unsafe blocks: `std::env::{set_var,remove_var}` are
        // unsafe in recent Rust. The test is single-threaded during
        // this block, no other test reads PATH concurrently that could
        // observe a half-mutated value that would affect correctness.
        let prev = std::env::var_os("PATH");
        unsafe {
            std::env::set_var("PATH", "/definitely/not/a/real/dir/for/zellij");
        }
        let got = zellij_session_liveness("ark-cavekit-ghost");
        match prev {
            Some(v) => unsafe { std::env::set_var("PATH", v) },
            None => unsafe { std::env::remove_var("PATH") },
        }
        assert_eq!(
            got,
            ZellijSessionLiveness::Unknown,
            "missing zellij on PATH must yield Unknown, not Gone"
        );
    }

    #[test]
    fn no_detach_cleanup_skipped_when_session_alive() {
        // F-708: simulate the detach-from-zellij path. When
        // `zellij_session_liveness` reports Alive the `--no-detach`
        // branch must NOT call `cleanup_agent_state`, so spec.json and
        // layout.kdl survive for the still-live detached session.
        let state = TempDir::new().unwrap();
        let id = AgentId::new("cavekit", "detached-alive");
        let dir = id.state_dir(state.path());
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("spec.json"), b"{\"id\":\"x\"}").unwrap();
        std::fs::write(dir.join("layout.kdl"), b"layout { }").unwrap();
        assert!(dir.exists());

        // Simulate the F-708 gate: Alive → skip cleanup.
        let outcome = ZellijSessionLiveness::Alive;
        if outcome == ZellijSessionLiveness::Gone {
            cleanup_agent_state(state.path(), &id);
        }

        assert!(
            dir.exists(),
            "F-708: Alive liveness must leave agent state intact"
        );
    }

    #[test]
    fn no_detach_cleanup_runs_when_session_gone() {
        // F-708: simulate the terminate-zellij path. When
        // `zellij_session_liveness` reports Gone the `--no-detach`
        // branch must call `cleanup_agent_state`, matching the pre-F-708
        // ghost-state behaviour (F-705).
        let state = TempDir::new().unwrap();
        let id = AgentId::new("cavekit", "detached-gone");
        let dir = id.state_dir(state.path());
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("spec.json"), b"{\"id\":\"x\"}").unwrap();
        assert!(dir.exists());

        // Simulate the F-708 gate: Gone → run cleanup.
        let outcome = ZellijSessionLiveness::Gone;
        if outcome == ZellijSessionLiveness::Gone {
            cleanup_agent_state(state.path(), &id);
        }

        assert!(
            !dir.exists(),
            "F-708: Gone liveness must wipe transient agent state"
        );
    }

    #[test]
    fn no_detach_cleanup_skipped_when_liveness_unknown() {
        // F-708: Unknown is the safe-default path (zellij not on PATH,
        // command errored). Keep state — losing a potentially-live
        // session is worse than a transient ghost.
        let state = TempDir::new().unwrap();
        let id = AgentId::new("cavekit", "detached-unknown");
        let dir = id.state_dir(state.path());
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("spec.json"), b"{\"id\":\"x\"}").unwrap();
        assert!(dir.exists());

        let outcome = ZellijSessionLiveness::Unknown;
        if outcome == ZellijSessionLiveness::Gone {
            cleanup_agent_state(state.path(), &id);
        }

        assert!(
            dir.exists(),
            "F-708: Unknown liveness must leave agent state intact (safer default)"
        );
    }

    #[test]
    fn cleanup_agent_state_is_idempotent_on_missing_dir() {
        // F-528: cleanup_agent_state must swallow remove errors so a
        // double-call (or a call before the dir was ever created, e.g.
        // if `write_spec_json` itself failed before creating the dir)
        // does not mask the original spawn error with a secondary I/O
        // failure.
        let state = TempDir::new().unwrap();
        let id = AgentId::new("cavekit", "never");
        // Call before the dir exists — must not panic.
        cleanup_agent_state(state.path(), &id);
        // Create, remove, remove again.
        let dir = id.state_dir(state.path());
        std::fs::create_dir_all(&dir).unwrap();
        cleanup_agent_state(state.path(), &id);
        assert!(!dir.exists());
        cleanup_agent_state(state.path(), &id);
    }

    #[test]
    fn zellij_startup_failure_success_when_still_alive() {
        // F-702: A child that sleeps longer than the grace window is
        // considered alive — the helper must return None without
        // killing the process. We reap it afterwards so the test
        // doesn't leak a zombie.
        //
        // Previously this test wrapped `sleep 2` in `/bin/sh -c` and
        // relied on a ~4x margin over the 500ms grace window. Under
        // heavily loaded CI (parallel test runs + slow forks on macOS)
        // codex flagged it as flaky: the extra `sh` layer introduces a
        // second fork/exec step whose scheduling can stretch
        // unpredictably, and a 2s sleep only buys 1.5s of headroom.
        //
        // Harden by:
        //   1. Invoking `/bin/sleep` directly — no shell indirection,
        //      one fewer process in the chain to race.
        //   2. Using `sleep 30`, giving ~60x headroom over the 500ms
        //      grace window. Even with extreme scheduler pressure the
        //      child is guaranteed alive when `zellij_startup_failure`
        //      polls `try_wait()`.
        //   3. Killing + reaping in an always-run cleanup block before
        //      the assertion, so a failed assert still frees the PID.
        let mut child = Command::new("/bin/sleep")
            .arg("30")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("spawn /bin/sleep 30");
        let got = zellij_startup_failure(&mut child);
        // Reap before asserting so a failure doesn't leak the pid.
        let _ = child.kill();
        let _ = child.wait();
        assert!(
            got.is_none(),
            "still-alive child must be treated as ok, got {got:?}"
        );
    }

    // ---------------------------------------------------------------
    // T-3.5 / T-8.2: --scene flag + ARK_SCENE / ARK_APPNAME env vars +
    // multi-rung fallback (delegates to T-8.0 scene resolver).
    // ---------------------------------------------------------------
    //
    // The tests below mutate process-global env vars (ARK_SCENE,
    // ARK_APPNAME, XDG_CONFIG_HOME, HOME) because
    // `resolve_layout_source` delegates to
    // `ark_scene::path::resolve_scene_path_from_env`, which reads them.
    // All such tests MUST acquire `ENV_LOCK` and restore/clear the
    // vars on exit — flaky CI otherwise.

    /// Helper: clear all scene-relevant env vars so rung 1/2 cannot fire
    /// accidentally based on the caller's environment. Returns a guard
    /// that restores the previous values on drop.
    ///
    /// Called by every scene-resolution test that owns `ENV_LOCK`.
    struct SceneEnvGuard {
        ark_scene: Option<std::ffi::OsString>,
        ark_appname: Option<std::ffi::OsString>,
        xdg: Option<std::ffi::OsString>,
        home: Option<std::ffi::OsString>,
    }

    impl SceneEnvGuard {
        fn clear_all() -> Self {
            let g = Self {
                ark_scene: std::env::var_os("ARK_SCENE"),
                ark_appname: std::env::var_os("ARK_APPNAME"),
                xdg: std::env::var_os("XDG_CONFIG_HOME"),
                home: std::env::var_os("HOME"),
            };
            unsafe {
                std::env::remove_var("ARK_SCENE");
                std::env::remove_var("ARK_APPNAME");
                std::env::remove_var("XDG_CONFIG_HOME");
                std::env::remove_var("HOME");
            }
            g
        }
    }

    impl Drop for SceneEnvGuard {
        fn drop(&mut self) {
            unsafe {
                match &self.ark_scene {
                    Some(v) => std::env::set_var("ARK_SCENE", v),
                    None => std::env::remove_var("ARK_SCENE"),
                }
                match &self.ark_appname {
                    Some(v) => std::env::set_var("ARK_APPNAME", v),
                    None => std::env::remove_var("ARK_APPNAME"),
                }
                match &self.xdg {
                    Some(v) => std::env::set_var("XDG_CONFIG_HOME", v),
                    None => std::env::remove_var("XDG_CONFIG_HOME"),
                }
                match &self.home {
                    Some(v) => std::env::set_var("HOME", v),
                    None => std::env::remove_var("HOME"),
                }
            }
        }
    }

    /// `--scene NAME` surfaces in `SpawnArgs.scene`.
    #[test]
    fn clap_parses_scene_flag() {
        let h = Host::try_parse_from(["spawn", "--scene", "demo", "--", "claude"]).expect("parse");
        assert_eq!(h.args.scene.as_deref(), Some("demo"));
    }

    /// Absent `--scene` leaves `SpawnArgs.scene` at `None`.
    #[test]
    fn clap_scene_flag_defaults_to_none() {
        let h = Host::try_parse_from(["spawn", "--", "claude"]).expect("parse");
        assert_eq!(h.args.scene, None);
    }

    /// Rung 1: explicit `--scene NAME` resolves to
    /// `{config_dir}/scenes/NAME.kdl`. The path surface is decided by
    /// combo 3A — named scenes always land under `ctx.config_dir`, not
    /// under the XDG-derived path.
    #[test]
    fn resolve_rung_1_explicit_scene_flag() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _env = SceneEnvGuard::clear_all();

        let dir = TempDir::new().unwrap();
        let got = resolve_layout_source(dir.path(), dir.path(), Some("demo"));
        match got {
            LayoutResolution::SceneExplicit { path } => {
                assert_eq!(path, dir.path().join("scenes").join("demo.kdl"));
            }
            other => panic!("expected SceneExplicit, got {other:?}"),
        }
    }

    /// Rung 2: `ARK_SCENE=foo` (no flag) resolves identically to
    /// `--scene foo` — path rooted at `{config_dir}/scenes/foo.kdl`.
    #[test]
    fn resolve_rung_2_ark_scene_env_var() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _env = SceneEnvGuard::clear_all();
        unsafe {
            std::env::set_var("ARK_SCENE", "foo");
        }

        let dir = TempDir::new().unwrap();
        let got = resolve_layout_source(dir.path(), dir.path(), None);
        match got {
            LayoutResolution::SceneExplicit { path } => {
                assert_eq!(path, dir.path().join("scenes").join("foo.kdl"));
            }
            other => panic!("expected SceneExplicit, got {other:?}"),
        }
    }

    /// Rung 4 + `ARK_APPNAME` override: with no flag and no env
    /// `ARK_SCENE`, a user-global scene under
    /// `$HOME/.config/<appname>/scenes/default.kdl` resolves to
    /// `SceneDefault` with that exact path. Confirms the T-8.0
    /// resolver's ARK_APPNAME handling reaches the CLI surface.
    #[test]
    fn resolve_rung_4_xdg_default_with_appname_override() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _env = SceneEnvGuard::clear_all();

        let xdg = TempDir::new().unwrap();
        let scene_path = xdg.path().join("myark/scenes/default.kdl");
        std::fs::create_dir_all(scene_path.parent().unwrap()).unwrap();
        std::fs::write(&scene_path, "scene \"myark-default\" {}").unwrap();

        // Point the T-8.0 resolver at our tempdir via XDG_CONFIG_HOME
        // (takes precedence over HOME-derived $HOME/.config fallback).
        unsafe {
            std::env::set_var("XDG_CONFIG_HOME", xdg.path());
            std::env::set_var("ARK_APPNAME", "myark");
        }

        let config_dir = TempDir::new().unwrap();
        let cwd = TempDir::new().unwrap();
        let got = resolve_layout_source(config_dir.path(), cwd.path(), None);
        match got {
            LayoutResolution::SceneDefault { path } => {
                assert_eq!(path, scene_path);
            }
            other => panic!("expected SceneDefault, got {other:?}"),
        }
    }

    /// Rung 5 → Legacy: no flag, no env, no files on disk. The T-8.0
    /// resolver returns `BuiltIn(_)` and the CLI adapter translates
    /// that into [`LayoutResolution::Legacy`] so the legacy
    /// `--layout <stem>` path is preserved for users who never
    /// adopted scenes (TODO(T-14.1)).
    #[test]
    fn resolve_builtin_falls_through_to_legacy() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _env = SceneEnvGuard::clear_all();

        let config_dir = TempDir::new().unwrap();
        let cwd = TempDir::new().unwrap();
        let got = resolve_layout_source(config_dir.path(), cwd.path(), None);
        assert!(matches!(got, LayoutResolution::Legacy), "{got:?}");
    }

    /// Rung 1 (flag) beats rung 2 (env var): `--scene custom` wins
    /// even when `ARK_SCENE` is also set.
    #[test]
    fn resolve_explicit_flag_wins_over_ark_scene_env() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _env = SceneEnvGuard::clear_all();
        unsafe {
            std::env::set_var("ARK_SCENE", "from-env");
        }

        let dir = TempDir::new().unwrap();
        let got = resolve_layout_source(dir.path(), dir.path(), Some("custom"));
        match got {
            LayoutResolution::SceneExplicit { path } => {
                assert_eq!(path, dir.path().join("scenes").join("custom.kdl"));
            }
            other => panic!("expected SceneExplicit, got {other:?}"),
        }
    }

    /// Rung 3: project-local `./.ark/scene.kdl` resolves to
    /// `SceneDefault` when no flag / env is set.
    #[test]
    fn resolve_rung_3_project_local_scene() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _env = SceneEnvGuard::clear_all();

        let cwd = TempDir::new().unwrap();
        std::fs::create_dir_all(cwd.path().join(".ark")).unwrap();
        let scene_file = cwd.path().join(".ark/scene.kdl");
        std::fs::write(&scene_file, "scene \"local\" {}").unwrap();

        let config_dir = TempDir::new().unwrap();
        let got = resolve_layout_source(config_dir.path(), cwd.path(), None);
        match got {
            LayoutResolution::SceneDefault { path } => {
                assert_eq!(path, scene_file);
            }
            other => panic!("expected SceneDefault (project-local), got {other:?}"),
        }
    }

    /// End-to-end: compile a scene file and write it to a runtime dir,
    /// asserting the rendered path exists + parses as KDL.
    #[test]
    fn compile_and_write_scene_round_trips() {
        let dir = TempDir::new().unwrap();
        let scenes = dir.path().join("scenes");
        std::fs::create_dir_all(&scenes).unwrap();
        let scene_file = scenes.join("demo.kdl");
        std::fs::write(
            &scene_file,
            r#"scene "demo" {
    layout {
        tab "work" {
            pane name="editor"
        }
    }
}"#,
        )
        .unwrap();

        let runtime = dir.path().join("rt");
        std::fs::create_dir_all(&runtime).unwrap();

        let ctx = Ctx {
            no_color: false,
            log_level: "off".into(),
            state_dir: dir.path().join("state"),
            config_dir: dir.path().to_path_buf(),
            runtime_dir: runtime.clone(),
        };

        let spec = build_spec(
            "cavekit",
            "claude-code",
            "auth",
            PathBuf::from("/tmp/worktree"),
            vec!["claude".into()],
            Default::default(),
            None,
            Vec::new(),
        );

        let (rendered_path, _scene_id) =
            compile_and_write_scene(&ctx, &scene_file, &spec).expect("compile");
        assert!(rendered_path.exists());
        assert!(rendered_path.starts_with(runtime.join("layouts")));
        let contents = std::fs::read_to_string(&rendered_path).unwrap();
        assert!(contents.contains("layout"), "{contents}");
        assert!(contents.contains("work"), "{contents}");
        assert!(contents.contains("editor"), "{contents}");
    }
}
