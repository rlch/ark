//! `ark spawn` — create a new agent (cavekit-cli R2).
//!
//! T-087 wires the full argument surface into a working handler:
//! orchestrator auto-detect, AgentId generation, spec.json write,
//! zellij session branching via subprocess, and a parent-side
//! supervisor launch. The supervisor launch itself is STUBBED —
//! no `ark-supervisor` binary exists yet (tracked under T-062 /
//! T-069). The handler prints a warning and proceeds so the rest
//! of the pipe (spec.json, id echo, zellij dispatch) remains
//! exercisable end-to-end.
//!
//! Design choices:
//! - Supervisor launch uses `std::process::Command` (no fork, no
//!   daemonize crate, no nix).
//! - Zellij is invoked as a subprocess — always a dedicated
//!   per-agent session via `zellij -s <name> --layout <path>`
//!   (R2: 1:1 agent↔session). The inside-vs-outside-zellij
//!   distinction collapses at spawn time (F-516 / F-517).
//! - TTY detach uses a POSIX-native `pre_exec(setsid)` via `nix`
//!   (F-526) rather than shelling out to the external `setsid(1)`
//!   binary, which macOS does not ship by default. Factored into
//!   [`apply_detach`] so call sites stay one line.
//! - Zellij invocation is factored through `build_zellij_command`
//!   so tests can introspect argv without actually spawning a
//!   process (F-511).
//! - KDL layouts are minijinja templates (F-525). The handler
//!   resolves the layout stem to its template (user override in
//!   `{config_dir}/layouts/` or an embedded shipped layout),
//!   renders it with `{cwd, agent_cmd, agent_args, id, name}`,
//!   writes the rendered KDL to `{state_dir}/agents/{id}/layout.kdl`,
//!   and hands THAT path to zellij via `--layout`.
//! - All parsing / detection helpers are pure functions so the
//!   tests don't touch the filesystem unless they want to.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use ark_mux_zellij::{
    LayoutResolver, LayoutSource, LayoutVars, default_layout_for_orchestrator, render_layout,
};
use ark_types::{AgentId, AgentSpec};
use clap::Args;

use crate::ctx::Ctx;
use crate::error::CliError;

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

/// Engine selection. Only `claude-code` is valid in v1 — the flag is
/// accepted so end-state scripts stay stable. See cavekit-cli R2.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum EngineChoice {
    #[value(name = "claude-code")]
    ClaudeCode,
}

impl EngineChoice {
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

    /// Engine (v1: only `claude-code`).
    #[arg(
        long,
        value_enum,
        default_value_t = EngineChoice::ClaudeCode,
        hide_default_value = true,
        hide_possible_values = true,
    )]
    pub engine: EngineChoice,

    /// Worktree path (default: current directory).
    #[arg(long, default_value = ".")]
    pub cwd: PathBuf,

    /// Human-readable label (default: derived from cwd basename).
    #[arg(long)]
    pub name: Option<String>,

    /// KDL layout stem (e.g. `builder`) or absolute path.
    #[arg(long)]
    pub layout: Option<String>,

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
/// 7. Supervisor fork is STILL stubbed — the real `ark-supervisor`
///    binary lands under T-062 / T-069.
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

    let spec_path = write_spec_json(&ctx.state_dir, &spec)?;
    tracing::debug!(path = %spec_path.display(), "wrote spec.json");

    // F-525: render the KDL layout template with per-spawn variable
    // substitution and persist it to `{state}/agents/{id}/layout.kdl`.
    // The rendered path — not the raw stem — is what we pass to
    // `zellij --layout`. If the render fails we clean up the agent dir
    // we just created (matching F-503 / F-523's "no orphan state on
    // spawn failure" invariant).
    let layout_path = match render_and_write_layout(ctx, &spec) {
        Ok(p) => p,
        Err(e) => {
            cleanup_agent_state(&ctx.state_dir, &spec.id);
            return Err(e);
        }
    };
    tracing::debug!(path = %layout_path.display(), "wrote rendered layout");

    // F-511: actually launch zellij. We snapshot the env, pick a
    // plan, and spawn the subprocess via `build_zellij_command`.
    // `Command::spawn()` (not `.status()`) because the parent agent
    // is typically already inside zellij and must not block.
    //
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
    let mut zcmd = build_zellij_command(&plan);
    // F-606: honor `--no-detach`. The detach path (default) nullifies
    // stdio + wires `pre_exec(setsid)` via `apply_detach` so the child
    // becomes a new session leader. The `--no-detach` path skips BOTH
    // — stdio is inherited and no session split happens, so the
    // operator sees zellij output live and we block on the child until
    // zellij exits.
    configure_zellij_stdio_and_detach(&mut zcmd, args.no_detach, apply_detach);

    if args.no_detach {
        // Foreground path: `Command::status()` inherits stdio and blocks
        // on the child. A non-zero exit is a spawn / runtime failure —
        // we still run `cleanup_agent_state` so a foreground launch that
        // fails before the session is listenable doesn't leave an
        // orphan spec.json / layout.kdl behind.
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
                reason: format!("zellij exited with code {code} before session came up"),
            });
        }
        eprintln!(
            "warning: supervisor launch is stubbed in this build; \
             spec.json written at {}",
            spec_path.display()
        );
        println!("spawned {} -> Ctrl+o w to switch", spec.id);
        // F-705: `--no-detach` is inherently ephemeral — zellij ran in
        // the foreground and has now exited, and the supervisor stub
        // means no background process owns the agent. If we leave
        // spec.json / layout.kdl on disk the agent shows up as a ghost
        // entry in `ark list`, the picker, and `ark doctor` even though
        // nothing is running. Clean the agent state dir here (mirrors
        // F-528's cleanup on the failure branch above) and print an
        // informational note so the operator understands why the agent
        // vanished from `ark list` after the foreground exit. When the
        // supervisor (T-062 / T-069) lands, detached spawns will keep
        // their state because the supervisor owns it — this cleanup is
        // specific to the `--no-detach` foreground lifecycle.
        cleanup_agent_state(&ctx.state_dir, &spec.id);
        eprintln!(
            "note: removed transient agent state {} (no-detach mode)",
            spec.id
        );
        return Ok(());
    }

    // F-528: `zcmd.spawn()` itself can fail (ENOENT after racy PATH
    // change, EACCES on a non-executable binary, EAGAIN / ENOMEM under
    // fork pressure). The prior code returned the error directly and
    // left spec.json + layout.kdl on disk, an orphan that `ark list`
    // would then advertise as a live agent. Route the error through
    // the same `cleanup_agent_state` helper the startup-poll branch
    // uses so the "no orphan state on spawn failure" invariant holds
    // for BOTH failure modes.
    let mut child = match zcmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            cleanup_agent_state(&ctx.state_dir, &spec.id);
            return Err(CliError::Internal {
                reason: format!("launch zellij: {e}"),
            });
        }
    };

    // F-523: `Command::spawn()` only confirms the child forked. zellij
    // may still exec-fail, layout-parse-fail, or otherwise die before
    // the session is listenable. Poll briefly with `try_wait()`; if
    // the child has already exited non-zero inside the grace window we
    // treat this as a spawn failure, clean up the agent state dir we
    // just created, and surface an Internal error. A ~500ms grace is
    // enough to catch fast failures (missing layout, bad config) while
    // staying snappy for the success path — zellij is still alive at
    // the end of the window because it forks its own detached daemon.
    if let Some(err) = zellij_startup_failure(&mut child) {
        cleanup_agent_state(&ctx.state_dir, &spec.id);
        return Err(err);
    }

    // Supervisor spawn is STUBBED until the `ark-supervisor` binary
    // lands (see T-062 / T-069). We print a warning so the operator
    // isn't surprised when nothing appears in `ark list`.
    eprintln!(
        "warning: supervisor launch is stubbed in this build; \
         spec.json written at {}",
        spec_path.display()
    );

    println!("spawned {} -> Ctrl+o w to switch", spec.id);
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
        // Guard against regression: even when the env getter reports
        // $ZELLIJ is set, the produced command must be the session-
        // creator, NOT `zellij action new-tab`.
        // F-526: argv is pure zellij — detach is wired separately.
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
            engine: EngineChoice::ClaudeCode,
            cwd: state.path().to_path_buf(),
            name: Some("preflighttest".into()),
            layout: None,
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
        // We cannot spawn real zellij in a unit test, so this simulates
        // the lifecycle: write the state dir the way the successful
        // pre-launch path would, apply the F-705 cleanup, and assert
        // the tree is gone.
        let state = TempDir::new().unwrap();
        let id = AgentId::new("cavekit", "fg-ephemeral");
        let dir = id.state_dir(state.path());
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("spec.json"), b"{\"id\":\"x\"}").unwrap();
        std::fs::write(dir.join("layout.kdl"), b"layout { }").unwrap();
        assert!(dir.exists(), "state dir exists before foreground exit");

        // Simulate the tail of the `if args.no_detach { … }` branch after
        // `Command::status()` returned Ok(success).
        cleanup_agent_state(state.path(), &id);

        assert!(
            !dir.exists(),
            "F-705: no-detach foreground exit must remove transient agent state"
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
}
