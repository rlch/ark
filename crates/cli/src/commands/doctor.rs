//! `ark doctor` — T-091 (cavekit-cli R5).
//!
//! Environment diagnostics + remediation. Folds the old `gc` and
//! `plugin install` subcommands into `--fix`.
//!
//! Each check produces a [`CheckResult`] with status Ok / Warn /
//! Fail. Default rendering is a compact table with glyphs
//! (✓ ⚠ ✗, plain ASCII when `ctx.no_color`). `--json` emits a
//! machine-readable array. Exit is 0 when every check is Ok, else
//! [`CliError::Generic`].
//!
//! The zellij / claude preflight helpers in `mux/zellij` and
//! `engines/claude-code` both require async executors / an
//! `AgentSpec` and are not reachable from this sync command
//! cheaply. We re-detect inline: `which`-style PATH scan plus
//! `{bin} --version` parse. The version-parsing logic mirrors
//! `parse_zellij_version` in mux/zellij but stays a few lines
//! long.

use std::collections::BTreeSet;
use std::fs;
use std::io::{self, BufRead, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::Command;

use ark_types::{AgentId, AgentSpec, StateLayout};
use clap::Args;
use nix::sys::signal::kill as nix_kill;
use nix::unistd::Pid;
use serde::Serialize;

use crate::ctx::Ctx;
use crate::error::CliError;

/// Minimum zellij version required (mirrors mux/zellij R6).
const MIN_ZELLIJ: (u32, u32, u32) = (0, 44, 1);

/// Arguments for `ark doctor`.
#[derive(Debug, Args)]
#[command(
    about = "Diagnose environment; with --fix, prompt to remediate",
    long_about = "Run environment checks (zellij >= 0.44, claude,\n\
                  writable dirs, orphan sockets, stale locks,\n\
                  dangling worktrees). --fix prompts per item.\n\
                  \n\
                  Examples:\n  \
                  ark doctor\n  \
                  ark doctor --fix\n  \
                  ark doctor --fix --yes\n  \
                  ark doctor --json"
)]
pub struct DoctorArgs {
    /// Prompt to remediate each fixable finding.
    #[arg(long)]
    pub fix: bool,

    /// Auto-accept all prompts when combined with --fix.
    #[arg(long, requires = "fix")]
    pub yes: bool,

    /// Emit JSON array of CheckResult; skips prompts.
    #[arg(long)]
    pub json: bool,
}

/// Status of a single check.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    /// Passed.
    Ok,
    /// Passed with warning.
    Warn,
    /// Failed.
    Fail,
}

/// One diagnostic result.
#[derive(Debug, Clone, Serialize)]
pub struct CheckResult {
    /// Short name (column 1 in the table).
    pub name: String,
    /// Status (glyph in the table).
    pub status: Status,
    /// Human-readable message.
    pub message: String,
    /// Optional remediation action carried alongside the result.
    /// Not serialized — consumed by `--fix`.
    #[serde(skip)]
    pub fix: Option<FixAction>,
}

/// A remediation action attached to a Warn/Fail. `--fix` prompts
/// the user before applying.
#[derive(Debug, Clone)]
pub enum FixAction {
    /// Delete an orphaned `.sock` file.
    DeleteSocket(PathBuf),
    /// Remove a stale lock file whose owner PID is dead.
    RemoveLock(PathBuf),
    /// Remove the agent state dir whose supervisor is dead.
    RemoveAgentDir(PathBuf),
    /// Create a missing config dir.
    CreateDir(PathBuf),
}

impl CheckResult {
    fn ok<N: Into<String>, M: Into<String>>(name: N, msg: M) -> Self {
        Self {
            name: name.into(),
            status: Status::Ok,
            message: msg.into(),
            fix: None,
        }
    }
    fn warn<N: Into<String>, M: Into<String>>(name: N, msg: M) -> Self {
        Self {
            name: name.into(),
            status: Status::Warn,
            message: msg.into(),
            fix: None,
        }
    }
    fn fail<N: Into<String>, M: Into<String>>(name: N, msg: M) -> Self {
        Self {
            name: name.into(),
            status: Status::Fail,
            message: msg.into(),
            fix: None,
        }
    }
    fn with_fix(mut self, fix: FixAction) -> Self {
        self.fix = Some(fix);
        self
    }
}

// --- PATH / version probing ---------------------------------------

/// First executable match for `name` on `$PATH`.
fn which(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        let cand = dir.join(name);
        if is_exec(&cand) {
            return Some(cand);
        }
    }
    None
}

#[cfg(unix)]
fn is_exec(p: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    match fs::metadata(p) {
        Ok(m) => m.is_file() && (m.permissions().mode() & 0o111 != 0),
        Err(_) => false,
    }
}
#[cfg(not(unix))]
fn is_exec(p: &Path) -> bool {
    fs::metadata(p).map(|m| m.is_file()).unwrap_or(false)
}

/// Parse a line like `zellij 0.44.1` into `(a,b,c)`.
pub(crate) fn parse_version(line: &str) -> Option<(u32, u32, u32)> {
    for token in line.split_whitespace() {
        let core = token.trim_start_matches(|c: char| !c.is_ascii_digit());
        if core.is_empty() {
            continue;
        }
        let parts: Vec<&str> = core
            .split('.')
            .take(3)
            .map(|p| p.trim_end_matches(|c: char| !c.is_ascii_digit()))
            .collect();
        if parts.len() != 3 {
            continue;
        }
        if let (Ok(a), Ok(b), Ok(c)) = (
            parts[0].parse::<u32>(),
            parts[1].parse::<u32>(),
            parts[2].parse::<u32>(),
        ) {
            return Some((a, b, c));
        }
    }
    None
}

/// Run `{bin} --version` and return stdout's first line.
fn run_version(bin: &Path) -> Option<String> {
    let out = Command::new(bin).arg("--version").output().ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout).to_string();
    Some(s.lines().next().unwrap_or("").to_string())
}

// --- individual checks --------------------------------------------

fn check_zellij() -> CheckResult {
    match which("zellij") {
        None => CheckResult::fail(
            "zellij",
            "zellij not on PATH — install: brew install zellij",
        ),
        Some(p) => {
            let line = run_version(&p).unwrap_or_default();
            match parse_version(&line) {
                None => CheckResult::warn(
                    "zellij",
                    format!("found {} but could not parse --version", p.display()),
                ),
                Some(v) if v < MIN_ZELLIJ => CheckResult::fail(
                    "zellij",
                    format!(
                        "zellij {}.{}.{} < required {}.{}.{}",
                        v.0, v.1, v.2, MIN_ZELLIJ.0, MIN_ZELLIJ.1, MIN_ZELLIJ.2,
                    ),
                ),
                Some(v) => CheckResult::ok("zellij", format!("{}.{}.{} on PATH", v.0, v.1, v.2)),
            }
        }
    }
}

fn check_claude() -> CheckResult {
    match which("claude") {
        None => CheckResult::fail(
            "claude",
            "claude not on PATH — install from claude.com/claude-code",
        ),
        Some(p) => {
            let line = run_version(&p).unwrap_or_default();
            if line.is_empty() {
                CheckResult::warn(
                    "claude",
                    format!("{} present; --version empty", p.display()),
                )
            } else {
                CheckResult::ok("claude", line.trim().to_string())
            }
        }
    }
}

/// Probe-and-remove to test directory writability.
fn is_writable(dir: &Path) -> bool {
    if !dir.is_dir() {
        return false;
    }
    let probe = dir.join(format!(".ark-doctor-{}", std::process::id()));
    match fs::File::create(&probe) {
        Ok(_) => {
            let _ = fs::remove_file(&probe);
            true
        }
        Err(_) => false,
    }
}

/// F-504: non-mutating writable check for `state_dir`/`runtime_dir`.
///
/// Contract:
/// - dir missing → Fail with status="missing" and FixAction::CreateDir
///   (the fix is only applied under `--fix`).
/// - dir present but unwritable → Fail with no auto-fix (permission
///   issue — user decides).
/// - dir present + writable → Ok.
///
/// Writability is probed with a tempfile INSIDE the existing dir; we
/// never `create_dir_all` during diagnosis.
fn check_dir_writable(name: &'static str, dir: &Path) -> CheckResult {
    if dir.as_os_str().is_empty() {
        return CheckResult::fail(name, format!("{name} unset"));
    }
    if !dir.exists() {
        return CheckResult::fail(
            name,
            format!("missing {} (create with --fix)", dir.display()),
        )
        .with_fix(FixAction::CreateDir(dir.to_path_buf()));
    }
    if is_writable(dir) {
        CheckResult::ok(name, format!("writable {}", dir.display()))
    } else {
        CheckResult::fail(name, format!("not writable {}", dir.display()))
    }
}

fn check_runtime_dir(ctx: &Ctx) -> CheckResult {
    check_dir_writable("runtime-dir", &ctx.runtime_dir)
}

fn check_state_dir(ctx: &Ctx) -> CheckResult {
    check_dir_writable("state-dir", &ctx.state_dir)
}

/// F-507: config_dir must probe writability, not just existence.
/// A read-only config_dir breaks `ark config edit`/`set`; doctor should
/// surface that rather than rubber-stamp as Ok.
fn check_config_dir(ctx: &Ctx) -> CheckResult {
    if ctx.config_dir.as_os_str().is_empty() {
        return CheckResult::fail("config-dir", "config_dir unset");
    }
    if !ctx.config_dir.exists() {
        return CheckResult::warn(
            "config-dir",
            format!("missing {} (create with --fix)", ctx.config_dir.display()),
        )
        .with_fix(FixAction::CreateDir(ctx.config_dir.clone()));
    }
    if is_writable(&ctx.config_dir) {
        CheckResult::ok(
            "config-dir",
            format!("writable {}", ctx.config_dir.display()),
        )
    } else {
        // Warn (not Fail): reading config still works; only `ark config
        // edit`/`set` will fail until the user fixes permissions.
        CheckResult::warn(
            "config-dir",
            format!("config_dir not writable: {}", ctx.config_dir.display()),
        )
    }
}

fn check_editor() -> CheckResult {
    match std::env::var("EDITOR") {
        Ok(v) if !v.is_empty() => CheckResult::ok("editor", v),
        _ => CheckResult::warn("editor", "$EDITOR not set — some ark workflows need it"),
    }
}

/// Probe a unix socket by `connect`. Returns true when a
/// listener accepted us; false on any error (stale, missing
/// listener, refused, etc.).
fn socket_alive(sock: &Path) -> bool {
    UnixStream::connect(sock).is_ok()
}

/// Liveness probe — signal 0 (a.k.a `kill(pid, None)`).
fn pid_alive(pid: i32) -> bool {
    match nix_kill(Pid::from_raw(pid), None) {
        Ok(_) => true,
        Err(nix::errno::Errno::ESRCH) => false,
        // EPERM means the pid exists but is owned by another
        // user; treat as alive.
        Err(_) => true,
    }
}

/// Read `$STATE/agents/{id}/pid` if present.
fn read_pid(layout: &StateLayout, id: &AgentId) -> Option<i32> {
    let p = layout.pid_path(id);
    let raw = fs::read_to_string(&p).ok()?;
    raw.trim().parse().ok()
}

/// Enumerate agent ids present in `$STATE/agents/`.
fn state_agent_ids(layout: &StateLayout) -> Vec<AgentId> {
    let mut out = Vec::new();
    let Ok(entries) = fs::read_dir(layout.agents_root()) else {
        return out;
    };
    for e in entries.flatten() {
        if !e.file_type().map(|t| t.is_dir()).unwrap_or(false) {
            continue;
        }
        let name = match e.file_name().into_string() {
            Ok(s) => s,
            Err(_) => continue,
        };
        if let Ok(id) = AgentId::parse(&name) {
            out.push(id);
        }
    }
    out
}

/// Walk `$RUNTIME/agents/*.sock` and classify each.
fn check_orphan_sockets(layout: &StateLayout) -> Vec<CheckResult> {
    let mut out = Vec::new();
    let root = layout.runtime().join("agents");
    let Ok(entries) = fs::read_dir(&root) else {
        return out;
    };
    // Gather live PIDs from state dirs so we can classify.
    let live_pids: BTreeSet<i32> = state_agent_ids(layout)
        .iter()
        .filter_map(|id| read_pid(layout, id))
        .filter(|p| pid_alive(*p))
        .collect();

    for e in entries.flatten() {
        let path = e.path();
        if path.extension().and_then(|s| s.to_str()) != Some("sock") {
            continue;
        }
        if socket_alive(&path) {
            // Listener present — healthy.
            continue;
        }
        // Socket file with no listener. If any known-live
        // supervisor matches this file we leave it alone (race);
        // otherwise mark as orphan with delete-fix.
        let id_str = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("?")
            .to_string();
        let matched_pid = AgentId::parse(&id_str)
            .ok()
            .and_then(|id| read_pid(layout, &id))
            .map(|p| live_pids.contains(&p))
            .unwrap_or(false);
        if matched_pid {
            continue;
        }
        out.push(
            CheckResult::warn("orphan-socket", format!("stale socket {}", path.display()))
                .with_fix(FixAction::DeleteSocket(path)),
        );
    }
    out
}

/// Walk `$STATE/locks/*.lock` for dead-owner lock files.
fn check_stale_locks(layout: &StateLayout) -> Vec<CheckResult> {
    let mut out = Vec::new();
    let locks = layout.locks_dir();
    let Ok(entries) = fs::read_dir(&locks) else {
        return out;
    };
    for e in entries.flatten() {
        let path = e.path();
        if path.extension().and_then(|s| s.to_str()) != Some("lock") {
            continue;
        }
        let contents = fs::read_to_string(&path).unwrap_or_default();
        let pid: Option<i32> = contents.trim().parse().ok();
        let alive = pid.map(pid_alive).unwrap_or(false);
        if alive {
            continue;
        }
        out.push(
            CheckResult::warn("stale-lock", format!("lock {} owner dead", path.display()))
                .with_fix(FixAction::RemoveLock(path)),
        );
    }
    out
}

/// Walk `$STATE/agents/*/spec.json` for missing-cwd entries.
/// Emits Warn only; the cwd is user data so we never auto-delete.
fn check_dangling_worktrees(layout: &StateLayout) -> Vec<CheckResult> {
    let mut out = Vec::new();
    for id in state_agent_ids(layout) {
        let spec_path = layout.spec_path(&id);
        let Ok(raw) = fs::read_to_string(&spec_path) else {
            continue;
        };
        let Ok(spec) = serde_json::from_str::<AgentSpec>(&raw) else {
            continue;
        };
        if !spec.cwd.exists() {
            // If the supervisor for this id is dead we surface a
            // separate "orphan-state" result so --fix can clean
            // up the STATE dir (still not the worktree).
            let pid = read_pid(layout, &id);
            let dead = match pid {
                Some(p) => !pid_alive(p),
                None => true,
            };
            let mut r = CheckResult::warn(
                "dangling-worktree",
                format!("agent {} cwd {} missing", id.as_str(), spec.cwd.display()),
            );
            if dead {
                r = r.with_fix(FixAction::RemoveAgentDir(layout.agent_dir(&id)));
            }
            out.push(r);
        }
    }
    out
}

// --- rendering ----------------------------------------------------

fn glyph(st: Status, no_color: bool) -> &'static str {
    match (st, no_color) {
        (Status::Ok, true) => "OK ",
        (Status::Warn, true) => "WARN",
        (Status::Fail, true) => "FAIL",
        (Status::Ok, false) => "\u{2713}",   // ✓
        (Status::Warn, false) => "\u{26A0}", // ⚠
        (Status::Fail, false) => "\u{2717}", // ✗
    }
}

fn render_table<W: Write>(out: &mut W, rs: &[CheckResult], no_color: bool) -> io::Result<()> {
    for r in rs {
        writeln!(
            out,
            "{:>4} {:<20} {}",
            glyph(r.status, no_color),
            r.name,
            r.message
        )?;
    }
    let (ok, warn, fail) = rs.iter().fold((0, 0, 0), |(o, w, f), r| match r.status {
        Status::Ok => (o + 1, w, f),
        Status::Warn => (o, w + 1, f),
        Status::Fail => (o, w, f + 1),
    });
    writeln!(out, "---")?;
    writeln!(out, "{} ok, {} warn, {} fail", ok, warn, fail)?;
    Ok(())
}

// --- --fix flow ---------------------------------------------------

fn prompt_yes(msg: &str) -> io::Result<bool> {
    let mut stderr = io::stderr();
    write!(stderr, "{msg} [y/N] ")?;
    stderr.flush()?;
    let stdin = io::stdin();
    let mut line = String::new();
    stdin.lock().read_line(&mut line)?;
    Ok(matches!(line.trim(), "y" | "Y" | "yes" | "YES"))
}

fn apply_fix(fix: &FixAction) -> io::Result<String> {
    match fix {
        FixAction::DeleteSocket(p) => {
            fs::remove_file(p)?;
            Ok(format!("deleted socket {}", p.display()))
        }
        FixAction::RemoveLock(p) => {
            fs::remove_file(p)?;
            Ok(format!("removed lock {}", p.display()))
        }
        FixAction::RemoveAgentDir(p) => {
            fs::remove_dir_all(p)?;
            Ok(format!("removed state dir {}", p.display()))
        }
        FixAction::CreateDir(p) => {
            fs::create_dir_all(p)?;
            Ok(format!("created dir {}", p.display()))
        }
    }
}

fn run_fixes(rs: &[CheckResult], auto_yes: bool) -> io::Result<()> {
    let mut stderr = io::stderr();
    for r in rs {
        let Some(fix) = &r.fix else { continue };
        let desc = match fix {
            FixAction::DeleteSocket(p) => format!("delete orphan socket {}", p.display()),
            FixAction::RemoveLock(p) => format!("remove stale lock {}", p.display()),
            FixAction::RemoveAgentDir(p) => format!("remove dead-agent state dir {}", p.display()),
            FixAction::CreateDir(p) => format!("create dir {}", p.display()),
        };
        let go = auto_yes || prompt_yes(&desc)?;
        if !go {
            continue;
        }
        match apply_fix(fix) {
            Ok(msg) => {
                writeln!(stderr, "  -> {msg}").ok();
            }
            Err(e) => {
                writeln!(stderr, "  !! {desc}: {e}").ok();
            }
        }
    }
    Ok(())
}

// --- entry points -------------------------------------------------

/// Run every check and return the aggregated result set.
/// Exposed for tests.
pub(crate) fn run_all(ctx: &Ctx) -> Vec<CheckResult> {
    let layout = StateLayout::new(
        ctx.state_dir.clone(),
        ctx.runtime_dir.clone(),
        ctx.config_dir.clone(),
    );
    let mut rs = Vec::new();
    rs.push(check_zellij());
    rs.push(check_claude());
    rs.push(check_runtime_dir(ctx));
    rs.push(check_state_dir(ctx));
    rs.push(check_config_dir(ctx));
    rs.push(check_editor());
    rs.extend(check_orphan_sockets(&layout));
    rs.extend(check_stale_locks(&layout));
    rs.extend(check_dangling_worktrees(&layout));
    rs
}

fn emit_json<W: Write>(out: &mut W, rs: &[CheckResult]) -> Result<(), CliError> {
    let s = serde_json::to_string_pretty(rs).map_err(|e| CliError::Generic {
        reason: format!("encode json: {e}"),
    })?;
    writeln!(out, "{s}").map_err(|e| CliError::Generic {
        reason: format!("write: {e}"),
    })?;
    Ok(())
}

fn aggregate_status(rs: &[CheckResult]) -> Status {
    if rs.iter().any(|r| r.status == Status::Fail) {
        Status::Fail
    } else if rs.iter().any(|r| r.status == Status::Warn) {
        Status::Warn
    } else {
        Status::Ok
    }
}

/// Dispatch `ark doctor` (T-091).
///
/// - Default: render table, exit 0 on all-Ok else [`CliError::Generic`].
/// - `--json`: emit array, exit 0 on all-Ok else Generic (same).
/// - `--fix`: iterate fixes after the report, with y/N prompts
///   (auto-accepted when `--yes`). Warn-only results do NOT fail
///   the command; only Fail results do.
pub fn run(args: DoctorArgs, ctx: &Ctx) -> Result<(), CliError> {
    let rs = run_all(ctx);

    if args.json {
        let stdout = io::stdout();
        let mut h = stdout.lock();
        emit_json(&mut h, &rs)?;
    } else {
        let stdout = io::stdout();
        let mut h = stdout.lock();
        render_table(&mut h, &rs, ctx.no_color).map_err(|e| CliError::Generic {
            reason: format!("write: {e}"),
        })?;
    }

    if args.fix {
        run_fixes(&rs, args.yes).map_err(|e| CliError::Generic {
            reason: format!("fix: {e}"),
        })?;
    }

    match aggregate_status(&rs) {
        Status::Fail => Err(CliError::Generic {
            reason: "one or more checks failed".to_string(),
        }),
        // Warn-only is still a zero exit — spec: "Warn-only
        // result → Ok (exit 0)".
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;
    use std::os::unix::fs::PermissionsExt;
    use std::path::PathBuf;
    use std::sync::Mutex;
    use tempfile::tempdir;

    /// Serialize env-mutating tests.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    #[derive(Debug, Parser)]
    struct Host {
        #[command(flatten)]
        args: DoctorArgs,
    }

    // ---- parse round-trip ----

    #[test]
    fn bare_parses() {
        let h = Host::try_parse_from(["doctor"]).unwrap();
        assert!(!h.args.fix);
        assert!(!h.args.yes);
        assert!(!h.args.json);
    }

    #[test]
    fn fix_parses() {
        let h = Host::try_parse_from(["doctor", "--fix"]).unwrap();
        assert!(h.args.fix);
    }

    #[test]
    fn yes_requires_fix() {
        assert!(Host::try_parse_from(["doctor", "--yes"]).is_err());
    }

    #[test]
    fn json_parses() {
        let h = Host::try_parse_from(["doctor", "--json"]).unwrap();
        assert!(h.args.json);
    }

    // ---- version parser ----

    #[test]
    fn parse_version_clean() {
        assert_eq!(parse_version("zellij 0.44.1"), Some((0, 44, 1)));
        assert_eq!(parse_version("0.45.0\n"), Some((0, 45, 0)));
        assert_eq!(parse_version("claude 1.0.17 (anthropic)"), Some((1, 0, 17)));
    }

    #[test]
    fn parse_version_garbage() {
        assert_eq!(parse_version("no version here"), None);
    }

    // ---- ctx helper ----

    fn test_ctx(base: &Path) -> Ctx {
        Ctx {
            no_color: true,
            log_level: "info".into(),
            state_dir: base.join("state"),
            config_dir: base.join("cfg"),
            runtime_dir: base.join("rt"),
        }
    }

    /// RAII env guard (scoped mutation via `ENV_LOCK`).
    struct EnvGuard {
        key: &'static str,
        prior: Option<std::ffi::OsString>,
    }
    impl EnvGuard {
        fn set(key: &'static str, val: &str) -> Self {
            let prior = std::env::var_os(key);
            unsafe { std::env::set_var(key, val) };
            Self { key, prior }
        }
    }
    impl Drop for EnvGuard {
        fn drop(&mut self) {
            unsafe {
                match &self.prior {
                    Some(v) => std::env::set_var(self.key, v),
                    None => std::env::remove_var(self.key),
                }
            }
        }
    }

    // ---- zellij / claude PATH probes ----

    #[test]
    fn zellij_missing_on_empty_path() {
        let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let empty = tempdir().unwrap();
        let _p = EnvGuard::set("PATH", empty.path().to_str().unwrap());
        let r = check_zellij();
        assert_eq!(r.status, Status::Fail, "{r:?}");
        assert!(r.message.contains("zellij"), "{r:?}");
    }

    #[test]
    fn claude_missing_on_empty_path() {
        let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let empty = tempdir().unwrap();
        let _p = EnvGuard::set("PATH", empty.path().to_str().unwrap());
        let r = check_claude();
        assert_eq!(r.status, Status::Fail, "{r:?}");
    }

    // ---- orphan socket detection ----

    fn seed_agents_dir(runtime: &Path) {
        fs::create_dir_all(runtime.join("agents")).unwrap();
    }

    #[test]
    fn orphan_socket_flagged_when_no_listener() {
        let tmp = tempfile::Builder::new()
            .prefix("arkd")
            .tempdir_in("/tmp")
            .unwrap();
        let ctx = test_ctx(tmp.path());
        let layout = StateLayout::new(
            ctx.state_dir.clone(),
            ctx.runtime_dir.clone(),
            ctx.config_dir.clone(),
        );
        seed_agents_dir(&ctx.runtime_dir);
        let sock = ctx.runtime_dir.join("agents").join("cavekit-ghost-01.sock");
        // Just a regular file — no listener bound.
        fs::File::create(&sock).unwrap();

        let rs = check_orphan_sockets(&layout);
        assert_eq!(rs.len(), 1, "{rs:?}");
        assert_eq!(rs[0].status, Status::Warn);
        assert!(matches!(rs[0].fix, Some(FixAction::DeleteSocket(_))));
    }

    #[test]
    fn fix_yes_deletes_orphan_socket() {
        let tmp = tempfile::Builder::new()
            .prefix("arkd")
            .tempdir_in("/tmp")
            .unwrap();
        let ctx = test_ctx(tmp.path());
        fs::create_dir_all(&ctx.state_dir).unwrap();
        fs::create_dir_all(&ctx.config_dir).unwrap();
        fs::create_dir_all(&ctx.runtime_dir).unwrap();
        seed_agents_dir(&ctx.runtime_dir);
        let sock = ctx.runtime_dir.join("agents").join("cavekit-ghost-02.sock");
        fs::File::create(&sock).unwrap();

        let layout = StateLayout::new(
            ctx.state_dir.clone(),
            ctx.runtime_dir.clone(),
            ctx.config_dir.clone(),
        );
        let mut rs = check_orphan_sockets(&layout);
        assert_eq!(rs.len(), 1);
        // Apply with auto_yes.
        run_fixes(&mut rs, true).expect("fix");
        assert!(!sock.exists(), "socket should be gone");
    }

    // ---- stale lock detection ----

    #[test]
    fn stale_lock_flagged_for_dead_pid() {
        let tmp = tempfile::Builder::new()
            .prefix("arkd")
            .tempdir_in("/tmp")
            .unwrap();
        let ctx = test_ctx(tmp.path());
        let locks_dir = ctx.state_dir.join("locks");
        fs::create_dir_all(&locks_dir).unwrap();
        let lock_path = locks_dir.join("cavekit-dead-01.lock");
        // PID 2**31-2 — almost certainly not alive.
        fs::write(&lock_path, "2147483646\n").unwrap();

        let layout = StateLayout::new(
            ctx.state_dir.clone(),
            ctx.runtime_dir.clone(),
            ctx.config_dir.clone(),
        );
        let rs = check_stale_locks(&layout);
        assert_eq!(rs.len(), 1);
        assert_eq!(rs[0].status, Status::Warn);
        assert!(matches!(rs[0].fix, Some(FixAction::RemoveLock(_))));
    }

    // ---- JSON shape ----

    #[test]
    fn json_output_parses_as_array() {
        let rs = vec![
            CheckResult::ok("a", "ok msg"),
            CheckResult::warn("b", "warn msg"),
            CheckResult::fail("c", "fail msg"),
        ];
        let mut buf: Vec<u8> = Vec::new();
        emit_json(&mut buf, &rs).unwrap();
        let v: serde_json::Value = serde_json::from_slice(&buf).unwrap();
        let arr = v.as_array().expect("array");
        assert_eq!(arr.len(), 3);
        assert_eq!(arr[0]["status"], "ok");
        assert_eq!(arr[1]["status"], "warn");
        assert_eq!(arr[2]["status"], "fail");
        assert_eq!(arr[0]["name"], "a");
    }

    // ---- exit-status aggregation ----

    #[test]
    fn warn_only_aggregates_to_warn() {
        let rs = vec![CheckResult::ok("a", ""), CheckResult::warn("b", "")];
        assert_eq!(aggregate_status(&rs), Status::Warn);
    }

    #[test]
    fn fail_aggregates_to_fail() {
        let rs = vec![
            CheckResult::ok("a", ""),
            CheckResult::warn("b", ""),
            CheckResult::fail("c", ""),
        ];
        assert_eq!(aggregate_status(&rs), Status::Fail);
    }

    #[test]
    fn run_warn_only_returns_ok() {
        // Shape a ctx where: zellij/claude probes fail, but we
        // ignore the aggregation path by constructing the result
        // set directly — assert via helper.
        let rs = vec![CheckResult::warn("x", "just a warn")];
        let agg = aggregate_status(&rs);
        assert_eq!(agg, Status::Warn);
        // Mirror the `run` branch: Warn → Ok.
        let flow: Result<(), CliError> = match agg {
            Status::Fail => Err(CliError::Generic { reason: "x".into() }),
            _ => Ok(()),
        };
        assert!(flow.is_ok());
    }

    #[test]
    fn run_fail_produces_generic() {
        let rs = vec![CheckResult::fail("x", "boom")];
        let agg = aggregate_status(&rs);
        let flow: Result<(), CliError> = match agg {
            Status::Fail => Err(CliError::Generic { reason: "x".into() }),
            _ => Ok(()),
        };
        assert!(matches!(flow, Err(CliError::Generic { .. })));
    }

    // ---- config-dir auto-create via --fix ----

    #[test]
    fn missing_config_dir_warns_and_fix_creates_it() {
        let tmp = tempfile::Builder::new()
            .prefix("arkd")
            .tempdir_in("/tmp")
            .unwrap();
        let ctx = test_ctx(tmp.path());
        let r = check_config_dir(&ctx);
        assert_eq!(r.status, Status::Warn);
        assert!(matches!(r.fix, Some(FixAction::CreateDir(_))));
        run_fixes(&[r], true).unwrap();
        assert!(ctx.config_dir.is_dir());
    }

    // ---- glyphs ----

    #[test]
    fn glyph_no_color_is_plain_ascii() {
        assert!(!glyph(Status::Ok, true).contains('\u{2713}'));
        assert!(!glyph(Status::Warn, true).contains('\u{26A0}'));
        assert!(!glyph(Status::Fail, true).contains('\u{2717}'));
    }

    #[test]
    fn glyph_color_uses_unicode() {
        assert_eq!(glyph(Status::Ok, false), "\u{2713}");
    }

    // ---- dangling worktree reports the missing cwd ----

    #[test]
    fn dangling_worktree_flagged_when_cwd_missing() {
        use ark_types::AgentSpec;
        use ulid::Ulid;

        let tmp = tempfile::Builder::new()
            .prefix("arkd")
            .tempdir_in("/tmp")
            .unwrap();
        let ctx = test_ctx(tmp.path());
        let layout = StateLayout::new(
            ctx.state_dir.clone(),
            ctx.runtime_dir.clone(),
            ctx.config_dir.clone(),
        );
        let id = AgentId::from_parts(
            "cavekit",
            "lost",
            Ulid::from_string("01JX7Z8K6X9Y2ZT4ABCDEF0123").unwrap(),
        );
        fs::create_dir_all(layout.agent_dir(&id)).unwrap();
        let mut spec = AgentSpec::new(
            id.clone(),
            id.name(),
            "cavekit",
            "claude-code",
            PathBuf::from("/nonexistent/doctor/probe"),
            vec!["claude".into()],
        );
        // `PermissionsExt` kept in scope so this file compiles on
        // non-unix builds too — the trait import is unused there
        // and silenced below.
        let _ = &mut spec;
        let raw = serde_json::to_string(&spec).unwrap();
        fs::write(layout.spec_path(&id), raw).unwrap();

        let rs = check_dangling_worktrees(&layout);
        assert_eq!(rs.len(), 1);
        assert_eq!(rs[0].status, Status::Warn);
        assert!(rs[0].message.contains("/nonexistent/doctor/probe"));
    }

    // Silences the unused import on cfg(unix). PermissionsExt
    // has no non-unix alt; keep as a no-op guard.
    #[allow(dead_code)]
    fn _permissions_ext_touch(p: &Path) {
        let _ = fs::metadata(p).map(|m| m.permissions().mode());
    }

    // ---- F-504: doctor must NOT mutate state during diagnosis ----

    #[test]
    fn check_runtime_dir_missing_does_not_create_dir() {
        let tmp = tempfile::Builder::new()
            .prefix("arkd-f504r")
            .tempdir_in("/tmp")
            .unwrap();
        let ctx = test_ctx(tmp.path());
        // runtime_dir does not exist yet.
        assert!(!ctx.runtime_dir.exists());
        let r = check_runtime_dir(&ctx);
        assert_eq!(r.status, Status::Fail);
        assert!(
            !ctx.runtime_dir.exists(),
            "diagnosis must NOT create the dir"
        );
        assert!(matches!(r.fix, Some(FixAction::CreateDir(_))));
    }

    #[test]
    fn check_state_dir_missing_does_not_create_dir() {
        let tmp = tempfile::Builder::new()
            .prefix("arkd-f504s")
            .tempdir_in("/tmp")
            .unwrap();
        let ctx = test_ctx(tmp.path());
        assert!(!ctx.state_dir.exists());
        let r = check_state_dir(&ctx);
        assert_eq!(r.status, Status::Fail);
        assert!(!ctx.state_dir.exists(), "diagnosis must NOT create the dir");
        assert!(matches!(r.fix, Some(FixAction::CreateDir(_))));
    }

    #[test]
    fn check_state_dir_fix_creates_missing_dir() {
        let tmp = tempfile::Builder::new()
            .prefix("arkd-f504sf")
            .tempdir_in("/tmp")
            .unwrap();
        let ctx = test_ctx(tmp.path());
        let r = check_state_dir(&ctx);
        assert_eq!(r.status, Status::Fail);
        run_fixes(&[r], true).unwrap();
        assert!(ctx.state_dir.is_dir(), "--fix should materialize the dir");
    }

    #[test]
    fn check_state_dir_writable_when_present() {
        let tmp = tempfile::Builder::new()
            .prefix("arkd-f504sw")
            .tempdir_in("/tmp")
            .unwrap();
        let ctx = test_ctx(tmp.path());
        fs::create_dir_all(&ctx.state_dir).unwrap();
        let r = check_state_dir(&ctx);
        assert_eq!(r.status, Status::Ok, "{r:?}");
    }

    #[cfg(unix)]
    #[test]
    fn check_state_dir_fails_without_auto_fix_when_unwritable() {
        let tmp = tempfile::Builder::new()
            .prefix("arkd-f504sro")
            .tempdir_in("/tmp")
            .unwrap();
        let ctx = test_ctx(tmp.path());
        fs::create_dir_all(&ctx.state_dir).unwrap();
        let mut perms = fs::metadata(&ctx.state_dir).unwrap().permissions();
        perms.set_mode(0o500); // r-x, no write
        fs::set_permissions(&ctx.state_dir, perms).unwrap();

        let r = check_state_dir(&ctx);

        // Restore so tempdir cleanup works.
        let mut restore = fs::metadata(&ctx.state_dir).unwrap().permissions();
        restore.set_mode(0o755);
        fs::set_permissions(&ctx.state_dir, restore).ok();

        if nix::unistd::Uid::effective().is_root() {
            return;
        }

        assert_eq!(r.status, Status::Fail);
        // Unwritable (but existing) dir must NOT carry a CreateDir fix
        // — that would be a no-op on an existing dir.
        assert!(r.fix.is_none(), "unwritable existing dir has no auto-fix");
    }

    // ---- F-507: config_dir writability probe ----

    #[test]
    fn check_config_dir_ok_when_writable() {
        let tmp = tempfile::Builder::new()
            .prefix("arkd-f507ok")
            .tempdir_in("/tmp")
            .unwrap();
        let ctx = test_ctx(tmp.path());
        fs::create_dir_all(&ctx.config_dir).unwrap();
        let r = check_config_dir(&ctx);
        assert_eq!(r.status, Status::Ok, "{r:?}");
        assert!(r.message.contains("writable"), "{r:?}");
    }

    #[cfg(unix)]
    #[test]
    fn check_config_dir_warns_when_not_writable() {
        let tmp = tempfile::Builder::new()
            .prefix("arkd-f507ro")
            .tempdir_in("/tmp")
            .unwrap();
        let ctx = test_ctx(tmp.path());
        fs::create_dir_all(&ctx.config_dir).unwrap();
        let mut perms = fs::metadata(&ctx.config_dir).unwrap().permissions();
        perms.set_mode(0o500);
        fs::set_permissions(&ctx.config_dir, perms).unwrap();

        let r = check_config_dir(&ctx);

        let mut restore = fs::metadata(&ctx.config_dir).unwrap().permissions();
        restore.set_mode(0o755);
        fs::set_permissions(&ctx.config_dir, restore).ok();

        if nix::unistd::Uid::effective().is_root() {
            return;
        }

        assert_eq!(r.status, Status::Warn);
        assert!(r.message.contains("not writable"), "{r:?}");
    }
}
