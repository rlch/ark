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
//! - Zellij is invoked as a subprocess — in-session uses `zellij
//!   action new-tab`, new-session uses `zellij --session`.
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
pub fn inside_zellij<F: Fn(&str) -> Option<String>>(getter: F) -> bool {
    matches!(getter("ZELLIJ"), Some(v) if !v.is_empty())
}

/// The two zellij invocation modes R2 calls out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ZellijPlan {
    /// In-session: `zellij action new-tab --name <session>`.
    Attach { session: String },
    /// New detached session: `zellij --session <s> [--layout <p>]`.
    NewSession {
        session: String,
        layout: Option<String>,
    },
}

/// Choose the zellij invocation based on the env snapshot.
pub fn zellij_plan<F: Fn(&str) -> Option<String>>(
    getter: F,
    session: &str,
    layout: Option<&str>,
) -> ZellijPlan {
    if inside_zellij(getter) {
        ZellijPlan::Attach {
            session: session.to_string(),
        }
    } else {
        ZellijPlan::NewSession {
            session: session.to_string(),
            layout: layout.map(ToString::to_string),
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

/// `ark spawn` — T-087.
///
/// Happy path:
/// 1. Resolve orchestrator (read-only detect on cwd).
/// 2. Derive name.
/// 3. Parse env + hooks (pure, no I/O).
/// 4. Preflight zellij on PATH — F-503: run BEFORE any filesystem
///    mutation so preflight failure leaves zero orphan state.
/// 5. Build spec, mint AgentId, write spec.json.
/// 6. (Stubbed) supervisor fork — a future packet lands the real
///    `ark-supervisor` binary and replaces the warning with an
///    actual detached `Command::spawn`.
/// 7. Print `spawned {id} -> Ctrl+o w to switch`.
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
        // `--no-detach` would tail logs — requires a live supervisor.
        eprintln!("warning: --no-detach has no effect without a supervisor (stubbed)");
    }
    Ok(())
}

// -------------------------------------------------------------- tests -------

#[cfg(test)]
mod tests {
    use super::*;
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

    // --- zellij plan branching -----------------------------------------

    #[test]
    fn zellij_plan_inside_session_attaches() {
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
            ZellijPlan::Attach {
                session: "ark-cavekit-auth".into(),
            }
        );
    }

    #[test]
    fn zellij_plan_empty_env_treated_as_not_in_session() {
        let plan = zellij_plan(
            |k| {
                if k == "ZELLIJ" {
                    Some(String::new())
                } else {
                    None
                }
            },
            "ark-cavekit-auth",
            None,
        );
        match plan {
            ZellijPlan::NewSession { session, layout } => {
                assert_eq!(session, "ark-cavekit-auth");
                assert!(layout.is_none());
            }
            other => panic!("expected NewSession, got {other:?}"),
        }
    }

    #[test]
    fn zellij_plan_outside_session_new_session_with_layout() {
        let plan = zellij_plan(|_| None, "ark-cavekit-auth", Some("builder"));
        assert_eq!(
            plan,
            ZellijPlan::NewSession {
                session: "ark-cavekit-auth".into(),
                layout: Some("builder".into()),
            }
        );
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

    // --- require_zellij_on_path error variant -------------------------

    #[test]
    fn run_preflight_fail_leaves_state_untouched() {
        // F-503: when zellij is missing from PATH, run() must bail
        // BEFORE writing spec.json / creating the agent dir. Assert
        // agents_root contents are unchanged across the call.
        use std::sync::Mutex;
        static LOCK: Mutex<()> = Mutex::new(());
        let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());

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
        use std::sync::Mutex;
        static LOCK: Mutex<()> = Mutex::new(());
        let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
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
}
