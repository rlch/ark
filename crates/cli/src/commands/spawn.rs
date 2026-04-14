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
//!   per-agent session via `setsid zellij -s <name> --layout <path>`
//!   (R2: 1:1 agent↔session). The inside-vs-outside-zellij
//!   distinction collapses at spawn time (F-516 / F-517).
//! - `setsid` detaches zellij from the caller's controlling TTY so
//!   `spawn()` with null stdio works cleanly; zellij forks its own
//!   daemon and the user can attach later with `zellij attach`.
//! - Zellij invocation is factored through `build_zellij_command`
//!   so tests can introspect argv without actually spawning a
//!   process (F-511).
//! - All parsing / detection helpers are pure functions so the
//!   tests don't touch the filesystem unless they want to.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

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
    #[arg(last = true, value_name = "CMD")]
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
/// Always emits `setsid zellij -s <session> [--layout <path>]`.
/// `setsid` detaches zellij from the caller's controlling TTY so
/// `Command::spawn()` with null stdio completes cleanly — zellij
/// itself forks a detached daemon; `setsid`'s only job is to hand
/// it a fresh session id so it doesn't exit when the parent's TTY
/// goes away. This is the same invocation used by
/// `crates/mux/zellij/src/mux.rs` for outside-zellij first-spawn
/// (F-517).
pub fn build_zellij_command(plan: &ZellijSpawn) -> Command {
    let mut c = Command::new("setsid");
    c.arg("zellij").arg("-s").arg(&plan.session);
    if let Some(p) = &plan.layout {
        c.arg("--layout").arg(p);
    }
    c
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

    let spec = build_spec(
        orchestrator,
        args.engine.as_str(),
        &name,
        args.cwd.clone(),
        args.cmd.clone(),
        env,
        args.layout.clone(),
        hooks,
    );

    let spec_path = write_spec_json(&ctx.state_dir, &spec)?;
    tracing::debug!(path = %spec_path.display(), "wrote spec.json");

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
    let session = unique_session_name(&spec.id);
    let plan = zellij_plan(|k| std::env::var(k).ok(), &session, args.layout.as_deref());
    let mut zcmd = build_zellij_command(&plan);
    zcmd.stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    let mut child = zcmd.spawn().map_err(|e| CliError::Internal {
        reason: format!("launch zellij: {e}"),
    })?;

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
        let _ = std::fs::remove_dir_all(spec.id.state_dir(&ctx.state_dir));
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
    if args.no_detach {
        // `--no-detach` would tail logs — real log-tail is still
        // deferred until the supervisor exists.
        eprintln!("note: --no-detach log-tail deferred until supervisor lands");
    }
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
    fn build_zellij_command_setsid_with_layout() {
        // F-516/F-517: always `setsid zellij -s <name> --layout <path>`,
        // regardless of whether we were launched from inside zellij.
        let cmd = build_zellij_command(&ZellijSpawn {
            session: "ark-cavekit-auth".into(),
            layout: Some("/tmp/builder.kdl".into()),
        });
        assert_eq!(
            argv_of(&cmd),
            vec![
                "setsid",
                "zellij",
                "-s",
                "ark-cavekit-auth",
                "--layout",
                "/tmp/builder.kdl",
            ]
        );
    }

    #[test]
    fn build_zellij_command_setsid_without_layout_omits_layout_arg() {
        let cmd = build_zellij_command(&ZellijSpawn {
            session: "ark-cavekit-auth".into(),
            layout: None,
        });
        assert_eq!(
            argv_of(&cmd),
            vec!["setsid", "zellij", "-s", "ark-cavekit-auth"]
        );
    }

    #[test]
    fn build_zellij_command_inside_zellij_env_still_emits_setsid() {
        // Guard against regression: even when the env getter reports
        // $ZELLIJ is set, the produced command must be the setsid
        // session-creator, NOT `zellij action new-tab`.
        let plan = zellij_plan(
            |k| (k == "ZELLIJ").then(|| "1".to_string()),
            "ark-cavekit-auth",
            Some("/tmp/b.kdl".into()),
        );
        let cmd = build_zellij_command(&plan);
        let argv = argv_of(&cmd);
        assert_eq!(argv[0], "setsid");
        assert_eq!(argv[1], "zellij");
        assert!(!argv.iter().any(|a| a == "new-tab"));
        assert!(!argv.iter().any(|a| a == "attach"));
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

    #[test]
    fn zellij_startup_failure_success_when_still_alive() {
        // A child that sleeps longer than the grace window is
        // considered alive — the helper must return None without
        // killing the process. We reap it afterwards so the test
        // doesn't leak a zombie.
        let mut child = Command::new("/bin/sh")
            .arg("-c")
            .arg("sleep 2")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("spawn sleep");
        let got = zellij_startup_failure(&mut child);
        let _ = child.kill();
        let _ = child.wait();
        assert!(got.is_none(), "still-alive child must be treated as ok");
    }
}
