//! `ark doctor` — T-091 (cavekit-cli R5).
//!
//! Environment diagnostics + remediation. Folds the old `gc` and
//! `plugin install` subcommands into `--fix`.
//!
//! Each check produces a [`CheckResult`] with status Ok / Warn /
//! Fail. Default rendering is a compact table with glyphs
//! (✓ ⚠ ✗, plain ASCII when `ctx.no_color`). `--json` emits a
//! machine-readable array. Exit is 0 when every check is Ok or
//! Warn, and [`CliError::PreflightFail`] (exit code 2) when any
//! check fails (F-519).
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

use ark_config::{ConfigLoader, schema::Config};
use ark_types::{SessionId, SessionSpec, StateLayout};
use clap::Args;
use nix::sys::signal::kill as nix_kill;
use nix::unistd::Pid;
use serde::Serialize;

use crate::ctx::Ctx;
use crate::embedded;
use crate::error::CliError;

/// Minimum zellij version required (mirrors mux/zellij R6).
const MIN_ZELLIJ: (u32, u32, u32) = (0, 44, 1);

/// Env var that overrides the user config file location (mirrors
/// `crates/cli/src/commands/config.rs`). F-521 honors this when
/// locating the file to validate.
const ARK_CONFIG_PATH_ENV: &str = "ARK_CONFIG_PATH";

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
    /// Write embedded wasm plugin bytes to a target path (T-098).
    /// Tuple is `(plugin-name, wasm bytes, target path)`.
    WritePluginWasm(&'static str, &'static [u8], PathBuf),
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

/// F-520: `delta` is the preferred renderer for `ark pane diff`.
/// Absence is NOT fatal — diff falls back to plain rendering —
/// so we emit Warn, not Fail, when the binary is missing.
fn check_delta_binary() -> CheckResult {
    match which("delta") {
        None => CheckResult::warn(
            "delta",
            "delta not on PATH — ark pane diff will use fallback rendering \
             (install: brew install git-delta)",
        ),
        Some(p) => {
            let line = run_version(&p).unwrap_or_default();
            match parse_version(&line) {
                None => CheckResult::warn(
                    "delta",
                    format!("found {} but could not parse --version", p.display()),
                ),
                Some(v) => CheckResult::ok("delta", format!("{}.{}.{} on PATH", v.0, v.1, v.2)),
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

/// Resolve the user config file path honoring `ARK_CONFIG_PATH`
/// (mirrors the CLI's `config` subcommand resolution — see F-502).
fn user_config_path(ctx: &Ctx) -> PathBuf {
    if let Some(p) = std::env::var_os(ARK_CONFIG_PATH_ENV)
        && !p.is_empty()
    {
        return PathBuf::from(p);
    }
    ctx.config_dir.join("config.toml")
}

/// F-521: validate the CONTENTS of `config.toml` (not just the
/// directory). An invalid TOML file, or one that parses but fails
/// schema validation, breaks every later subcommand that calls
/// `ConfigLoader::load::<Config>()`. Doctor should surface that
/// early instead of letting the first `ark config show` blow up.
///
/// Contract:
/// - File missing → Ok ("absent — defaults will apply"). The spec
///   (cavekit-cli R1) treats absent user config as a legitimate
///   configuration, not a problem.
/// - File present + invalid TOML syntax → Fail with parse error.
/// - File present + valid TOML but schema-invalid → Fail with
///   the `ConfigLoader` error (mirrors the F-508 pattern used in
///   `config set`).
/// - File present + valid → Ok.
fn check_config_file(ctx: &Ctx) -> CheckResult {
    let path = user_config_path(ctx);
    if !path.exists() {
        return CheckResult::ok(
            "config-file",
            format!("{} absent — defaults apply", path.display()),
        );
    }
    let raw = match fs::read_to_string(&path) {
        Ok(s) => s,
        Err(e) => {
            return CheckResult::fail("config-file", format!("read {}: {e}", path.display()));
        }
    };
    // Step 1: syntactic TOML parse.
    if let Err(e) = raw.parse::<toml::Value>() {
        return CheckResult::fail(
            "config-file",
            format!("invalid TOML in {}: {e}", path.display()),
        );
    }
    // Step 2: schema validation via the same loader used by
    // `config show`/`set`. Project + env layers are intentionally
    // skipped so a bogus env override can't trip the doctor pass.
    let validated = ConfigLoader::new()
        .with_user_path(Some(path.clone()))
        .load::<Config>();
    match validated {
        Ok(_) => CheckResult::ok("config-file", format!("valid {}", path.display())),
        Err(e) => CheckResult::fail(
            "config-file",
            format!("schema error in {}: {e}", path.display()),
        ),
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

/// Read `$STATE/sessions/{id}/pid` if present.
fn read_pid(layout: &StateLayout, id: &SessionId) -> Option<i32> {
    let p = layout.session_pid_path(id);
    let raw = fs::read_to_string(&p).ok()?;
    raw.trim().parse().ok()
}

/// Enumerate agent ids present in `$STATE/sessions/`.
fn state_agent_ids(layout: &StateLayout) -> Vec<SessionId> {
    let mut out = Vec::new();
    let Ok(entries) = fs::read_dir(layout.sessions_root()) else {
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
        if let Ok(id) = SessionId::parse(&name) {
            out.push(id);
        }
    }
    out
}

/// Walk `$RUNTIME/sessions/*.sock` and classify each.
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
        let matched_pid = SessionId::parse(&id_str)
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

/// Walk `$STATE/sessions/*/spec.json` for missing-cwd entries.
/// Emits Warn only; the cwd is user data so we never auto-delete.
fn check_dangling_worktrees(layout: &StateLayout) -> Vec<CheckResult> {
    let mut out = Vec::new();
    for id in state_agent_ids(layout) {
        let spec_path = layout.session_spec_path(&id);
        let Ok(raw) = fs::read_to_string(&spec_path) else {
            continue;
        };
        let Ok(spec) = serde_json::from_str::<SessionSpec>(&raw) else {
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
                r = r.with_fix(FixAction::RemoveAgentDir(layout.session_dir(&id)));
            }
            out.push(r);
        }
    }
    out
}

// --- status plugin (T-098) ----------------------------------------

/// T-098: verify `ark-status.wasm` is installed in the user's ark
/// plugins dir with bytes matching the copy embedded in this binary.
///
/// Contract (cavekit-plugin-status R5 / cavekit-distribution R3):
/// - Build shipped without the embedded plugin → Ok ("plugin not
///   embedded in this build"); skip silently.
/// - Target file missing → Warn with WritePluginWasm fix.
/// - Target file present but content mismatches embedded bytes →
///   Warn with WritePluginWasm fix (stale / overwrite allowed).
/// - Target file present and matches → Ok.
pub(crate) fn check_status_plugin_installed(ctx: &Ctx) -> CheckResult {
    check_status_plugin_installed_with(ctx, embedded::STATUS_WASM, embedded::STATUS_WASM_AVAILABLE)
}

/// Testable variant — accepts the embedded bytes explicitly so
/// tests can simulate both the real and placeholder paths without
/// touching build-time state.
pub(crate) fn check_status_plugin_installed_with(
    ctx: &Ctx,
    embedded_bytes: &'static [u8],
    available: bool,
) -> CheckResult {
    if !available || embedded_bytes.is_empty() {
        return CheckResult::ok(
            "status-plugin",
            "plugin not embedded in this build (run a release build with wasm32-wasip1 \
             installed to embed ark-plugin-status.wasm)",
        );
    }
    let target = ctx.config_dir.join("plugins").join("ark-status.wasm");
    match fs::read(&target) {
        Ok(existing) if existing == embedded_bytes => CheckResult::ok(
            "status-plugin",
            format!("installed and up to date at {}", target.display()),
        ),
        Ok(_) => CheckResult::warn(
            "status-plugin",
            format!(
                "{} differs from embedded plugin ({} bytes) — run with --fix to overwrite",
                target.display(),
                embedded_bytes.len()
            ),
        )
        .with_fix(FixAction::WritePluginWasm(
            "ark-status",
            embedded_bytes,
            target,
        )),
        Err(_) => CheckResult::warn(
            "status-plugin",
            format!(
                "{} missing — run with --fix to install ({} bytes)",
                target.display(),
                embedded_bytes.len()
            ),
        )
        .with_fix(FixAction::WritePluginWasm(
            "ark-status",
            embedded_bytes,
            target,
        )),
    }
}

/// KDL snippet printed after installing a plugin so the user can
/// paste it into their zellij config. Kept as a helper so tests can
/// assert the exact shape. Mirrors the example in
/// cavekit-plugin-status.md §"Example KDL snippet".
fn status_plugin_kdl_snippet(path: &Path) -> String {
    format!(
        "plugins {{\n    ark-status location=\"file:{}\"\n}}\n",
        path.display()
    )
}

// --- picker plugin (T-109) ----------------------------------------

/// T-109: verify `ark-picker.wasm` is installed in the user's ark
/// plugins dir with bytes matching the copy embedded in this binary.
///
/// Contract (cavekit-plugin-picker R1 / cavekit-distribution R3) —
/// same shape as [`check_status_plugin_installed`].
pub(crate) fn check_picker_plugin_installed(ctx: &Ctx) -> CheckResult {
    check_picker_plugin_installed_with(ctx, embedded::PICKER_WASM, embedded::PICKER_WASM_AVAILABLE)
}

/// Testable variant — accepts the embedded bytes explicitly so
/// tests can simulate both the real and placeholder paths without
/// touching build-time state.
pub(crate) fn check_picker_plugin_installed_with(
    ctx: &Ctx,
    embedded_bytes: &'static [u8],
    available: bool,
) -> CheckResult {
    if !available || embedded_bytes.is_empty() {
        return CheckResult::ok(
            "picker-plugin",
            "plugin not embedded in this build (run a release build with wasm32-wasip1 \
             installed to embed ark-plugin-picker.wasm)",
        );
    }
    let target = ctx.config_dir.join("plugins").join("ark-picker.wasm");
    match fs::read(&target) {
        Ok(existing) if existing == embedded_bytes => CheckResult::ok(
            "picker-plugin",
            format!("installed and up to date at {}", target.display()),
        ),
        Ok(_) => CheckResult::warn(
            "picker-plugin",
            format!(
                "{} differs from embedded plugin ({} bytes) — run with --fix to overwrite",
                target.display(),
                embedded_bytes.len()
            ),
        )
        .with_fix(FixAction::WritePluginWasm(
            "ark-picker",
            embedded_bytes,
            target,
        )),
        Err(_) => CheckResult::warn(
            "picker-plugin",
            format!(
                "{} missing — run with --fix to install ({} bytes)",
                target.display(),
                embedded_bytes.len()
            ),
        )
        .with_fix(FixAction::WritePluginWasm(
            "ark-picker",
            embedded_bytes,
            target,
        )),
    }
}

/// KDL keybind snippet printed after installing the picker plugin
/// (cavekit-plugin-picker.md §"Distribution" — `Ctrl+g a` recommended).
/// The path is expanded to match the actual install location.
fn picker_plugin_kdl_snippet(path: &Path) -> String {
    format!(
        "// Add to ~/.config/zellij/config.kdl keybinds section:\n\
         shared_except \"locked\" {{\n    \
             bind \"Ctrl g\" \"a\" {{\n        \
                 LaunchOrFocusPlugin \"file:{}\" {{\n            \
                     floating true\n        \
                 }}\n    \
             }}\n\
         }}\n",
        path.display()
    )
}

// --- T-126: default scene parse + extension resolution ---------------

/// T-126 check 1: verify the built-in default scene parses without errors.
///
/// Uses `ark_scene::default_scene::parse_default_scene()` to exercise
/// the same parse path as `ark launch` / `ark scene check`. A parse failure
/// here means the binary ships a broken default scene — hard fail.
fn check_default_scene() -> CheckResult {
    match ark_scene::default_scene::parse_default_scene() {
        Ok(ir) => CheckResult::ok(
            "default-scene",
            format!("built-in default scene `{}` parses ok", ir.scene.name),
        ),
        Err(e) => CheckResult::fail(
            "default-scene",
            format!("built-in default scene failed to parse: {e}"),
        ),
    }
}

/// T-126 check 2: verify installed extensions resolve via the search path.
///
/// Enumerates every extension visible to `ark ext list` (project, user,
/// system tiers) and verifies each directory carries a parseable
/// `extension.kdl`. Reports the count of resolved extensions. When every
/// installed extension fails to parse, the check degrades to Warn (not
/// Fail) since extensions are optional for basic operation.
fn check_extensions_resolve() -> CheckResult {
    let xdg_data_home = std::env::var_os("XDG_DATA_HOME").map(PathBuf::from);
    let system_dirs: Vec<PathBuf> = vec![PathBuf::from("/usr/share/ark/extensions")];
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));

    let rows = crate::commands::ext::list::enumerate_extensions(
        &cwd,
        xdg_data_home.as_deref(),
        &system_dirs.iter().map(|p| p.as_path()).collect::<Vec<_>>(),
    );

    if rows.is_empty() {
        return CheckResult::ok(
            "extensions",
            "no extensions installed (none required)",
        );
    }

    let total = rows.len();
    let errors: Vec<&str> = rows
        .iter()
        .filter(|r| r.error.is_some())
        .map(|r| r.name.as_str())
        .collect();

    if errors.is_empty() {
        CheckResult::ok(
            "extensions",
            format!("{total} extension(s) resolve and parse ok"),
        )
    } else if errors.len() == total {
        CheckResult::warn(
            "extensions",
            format!(
                "all {total} extension(s) failed to parse: {}",
                errors.join(", ")
            ),
        )
    } else {
        CheckResult::warn(
            "extensions",
            format!(
                "{}/{total} extension(s) failed to parse: {}",
                errors.len(),
                errors.join(", ")
            ),
        )
    }
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
        FixAction::WritePluginWasm(name, bytes, target) => {
            if let Some(parent) = target.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::write(target, bytes)?;
            Ok(format!(
                "wrote {} plugin ({} bytes) to {}",
                name,
                bytes.len(),
                target.display()
            ))
        }
    }
}

/// Apply every `FixAction` attached to a CheckResult. Returns the number
/// of fixes that were successfully applied — the caller uses this to
/// decide whether to re-run the checks (see F-711: a fresh `run_all`
/// after a successful repair ensures the final exit code reflects the
/// POST-fix state, not the pre-fix snapshot).
fn run_fixes(rs: &[CheckResult], auto_yes: bool) -> io::Result<usize> {
    let mut stderr = io::stderr();
    let mut applied: usize = 0;
    for r in rs {
        let Some(fix) = &r.fix else { continue };
        let desc = match fix {
            FixAction::DeleteSocket(p) => format!("delete orphan socket {}", p.display()),
            FixAction::RemoveLock(p) => format!("remove stale lock {}", p.display()),
            FixAction::RemoveAgentDir(p) => format!("remove dead-agent state dir {}", p.display()),
            FixAction::CreateDir(p) => format!("create dir {}", p.display()),
            FixAction::WritePluginWasm(name, bytes, target) => format!(
                "install {} plugin ({} bytes) to {}",
                name,
                bytes.len(),
                target.display()
            ),
        };
        let go = auto_yes || prompt_yes(&desc)?;
        if !go {
            continue;
        }
        match apply_fix(fix) {
            Ok(msg) => {
                writeln!(stderr, "  -> {msg}").ok();
                applied += 1;
                // T-098/T-109: after installing a plugin, print the
                // matching KDL snippet so the user knows how to wire
                // it into their zellij config.
                if let FixAction::WritePluginWasm(name, _bytes, target) = fix {
                    writeln!(
                        stderr,
                        "\n  Add this to your zellij config to enable the plugin:\n"
                    )
                    .ok();
                    let snippet = match *name {
                        "ark-picker" => picker_plugin_kdl_snippet(target),
                        _ => status_plugin_kdl_snippet(target),
                    };
                    for line in snippet.lines() {
                        writeln!(stderr, "    {line}").ok();
                    }
                    writeln!(stderr).ok();
                }
            }
            Err(e) => {
                writeln!(stderr, "  !! {desc}: {e}").ok();
            }
        }
    }
    Ok(applied)
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
    rs.push(check_delta_binary());
    rs.push(check_runtime_dir(ctx));
    rs.push(check_state_dir(ctx));
    rs.push(check_config_dir(ctx));
    rs.push(check_config_file(ctx));
    rs.push(check_editor());
    rs.push(check_status_plugin_installed(ctx));
    rs.push(check_picker_plugin_installed(ctx));
    // T-126: scene + extension health checks.
    rs.push(check_default_scene());
    rs.push(check_extensions_resolve());
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

/// F-519: summarize failed check names into a single reason string
/// e.g. `"3 checks failed: zellij, claude, state-dir"`. Used when
/// mapping aggregate Fail → [`CliError::PreflightFail`].
fn failed_summary(rs: &[CheckResult]) -> String {
    let failed: Vec<&str> = rs
        .iter()
        .filter(|r| r.status == Status::Fail)
        .map(|r| r.name.as_str())
        .collect();
    if failed.is_empty() {
        // Defensive — should never be reached because this helper is
        // only called when aggregate_status == Fail, but keep the
        // fallback so a drift elsewhere doesn't produce a panicky
        // message.
        return "one or more checks failed".to_string();
    }
    format!("{} checks failed: {}", failed.len(), failed.join(", "))
}

/// Dispatch `ark doctor` (T-091).
///
/// - Default: render table, exit 0 on all-Ok-or-Warn, else
///   [`CliError::PreflightFail`] (exit code 2, F-519).
/// - `--json`: emit array, same exit policy.
/// - `--fix`: iterate fixes after the report, with y/N prompts
///   (auto-accepted when `--yes`). Warn-only results do NOT fail
///   the command; only Fail results do.
/// - `--json --fix` (F-513): JSON mode is read-only. Fixes are
///   SKIPPED and a one-line warning is emitted to stderr so the
///   stdout JSON array stays machine-parseable and no state is
///   mutated.
pub fn run(args: DoctorArgs, ctx: &Ctx) -> Result<(), CliError> {
    let mut rs = run_all(ctx);

    if args.json {
        // F-513: JSON mode is read-only. If the caller also passed
        // `--fix`, emit a stderr warning and skip the fix pass so
        // stdout stays pure machine-readable output and disk state
        // is unchanged. F-711: because JSON mode cannot apply fixes,
        // there is nothing to re-check — the pre-fix `rs` is the
        // final snapshot.
        if args.fix {
            eprintln!("warning: --fix ignored in --json mode");
        }
        let stdout = io::stdout();
        let mut h = stdout.lock();
        emit_json(&mut h, &rs)?;
    } else {
        let stdout = io::stdout();
        let mut h = stdout.lock();
        render_table(&mut h, &rs, ctx.no_color).map_err(|e| CliError::Generic {
            reason: format!("write: {e}"),
        })?;

        if args.fix {
            let applied = run_fixes(&rs, args.yes).map_err(|e| CliError::Generic {
                reason: format!("fix: {e}"),
            })?;
            // F-711: recompute check results after a successful repair
            // pass so the final exit code reflects the POST-fix state.
            // Without this, `ark doctor --fix --yes` that creates a
            // missing state_dir still exits 2 (PreflightFail) even
            // though the repair succeeded — automation sees the pre-fix
            // `rs` snapshot and assumes the fix failed.
            if applied > 0 {
                rs = run_all(ctx);
            }
        }
    }

    match aggregate_status(&rs) {
        // F-519: doctor failures are preflight/dependency failures —
        // map to exit code 2, not the generic exit 1. The reason
        // string enumerates which checks failed so CI logs point
        // straight at the missing dependency.
        Status::Fail => Err(CliError::PreflightFail {
            reason: failed_summary(&rs),
        }),
        // Warn-only is still a zero exit — spec: "Warn-only
        // result → Ok (exit 0)".
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_lock::ENV_LOCK;
    use clap::Parser;
    use std::os::unix::fs::PermissionsExt;
    use std::path::PathBuf;
    use tempfile::tempdir;

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

    // F-519: aggregate Fail must map to PreflightFail (exit 2),
    // NOT Generic (exit 1). The failure reason must enumerate the
    // failing check names so CI output points at the missing dep.
    #[test]
    fn run_fail_produces_preflight_fail() {
        use crate::exit::ExitCode;
        let rs = vec![
            CheckResult::fail("zellij", "missing"),
            CheckResult::ok("claude", "1.0"),
            CheckResult::fail("state-dir", "unwritable"),
        ];
        let agg = aggregate_status(&rs);
        assert_eq!(agg, Status::Fail);
        let flow: Result<(), CliError> = match agg {
            Status::Fail => Err(CliError::PreflightFail {
                reason: failed_summary(&rs),
            }),
            _ => Ok(()),
        };
        let err = flow.expect_err("aggregate Fail must produce an error");
        assert!(matches!(err, CliError::PreflightFail { .. }), "{err:?}");
        assert_eq!(err.code(), ExitCode::PreflightFail.code());
        assert_eq!(err.code(), 2);
        // Summary must enumerate the failing checks.
        let msg = err.to_string();
        assert!(msg.contains("2 checks failed"), "{msg}");
        assert!(msg.contains("zellij"), "{msg}");
        assert!(msg.contains("state-dir"), "{msg}");
        assert!(!msg.contains("claude"), "{msg}");
    }

    // F-520: doctor must check for the `delta` binary. Missing
    // delta is a Warn (fallback rendering still works) — NOT a Fail.
    #[test]
    fn delta_missing_on_empty_path_is_warn() {
        let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let empty = tempdir().unwrap();
        let _p = EnvGuard::set("PATH", empty.path().to_str().unwrap());
        let r = check_delta_binary();
        assert_eq!(r.status, Status::Warn, "{r:?}");
        assert!(r.message.contains("delta"), "{r:?}");
        // Absence is fixable by install, not by doctor — no auto-fix.
        assert!(r.fix.is_none(), "{r:?}");
    }

    // F-521: valid + absent + malformed config files.
    #[test]
    fn check_config_file_ok_when_absent() {
        let tmp = tempfile::Builder::new()
            .prefix("arkd-f521abs")
            .tempdir_in("/tmp")
            .unwrap();
        let ctx = test_ctx(tmp.path());
        // No config.toml yet.
        let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        // Ensure no ARK_CONFIG_PATH override leaks in from the env.
        let _guard = EnvGuard::set(ARK_CONFIG_PATH_ENV, "");
        let r = check_config_file(&ctx);
        assert_eq!(r.status, Status::Ok, "{r:?}");
        assert!(r.message.contains("absent"), "{r:?}");
    }

    #[test]
    fn check_config_file_ok_when_valid() {
        let tmp = tempfile::Builder::new()
            .prefix("arkd-f521ok")
            .tempdir_in("/tmp")
            .unwrap();
        let ctx = test_ctx(tmp.path());
        fs::create_dir_all(&ctx.config_dir).unwrap();
        // Minimal but schema-valid TOML — defaults fill the rest.
        fs::write(ctx.config_dir.join("config.toml"), "").unwrap();
        let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _guard = EnvGuard::set(ARK_CONFIG_PATH_ENV, "");
        let r = check_config_file(&ctx);
        assert_eq!(r.status, Status::Ok, "{r:?}");
        assert!(r.message.contains("valid"), "{r:?}");
    }

    #[test]
    fn check_config_file_fails_on_invalid_toml() {
        let tmp = tempfile::Builder::new()
            .prefix("arkd-f521bad")
            .tempdir_in("/tmp")
            .unwrap();
        let ctx = test_ctx(tmp.path());
        fs::create_dir_all(&ctx.config_dir).unwrap();
        // Unbalanced bracket → parser error.
        fs::write(
            ctx.config_dir.join("config.toml"),
            "[unterminated\nkey = \"val\"\n",
        )
        .unwrap();
        let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _guard = EnvGuard::set(ARK_CONFIG_PATH_ENV, "");
        let r = check_config_file(&ctx);
        assert_eq!(r.status, Status::Fail, "{r:?}");
        assert!(r.message.contains("invalid TOML"), "{r:?}");
    }

    #[test]
    fn check_config_file_honors_ark_config_path_override() {
        // Write a broken config at the override path; ctx.config_dir
        // has no config.toml. F-521 must follow the override.
        let tmp = tempfile::Builder::new()
            .prefix("arkd-f521env")
            .tempdir_in("/tmp")
            .unwrap();
        let ctx = test_ctx(tmp.path());
        let alt = tmp.path().join("alt-config.toml");
        fs::write(&alt, "[bad\n").unwrap();
        let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _guard = EnvGuard::set(ARK_CONFIG_PATH_ENV, alt.to_str().unwrap());
        let r = check_config_file(&ctx);
        assert_eq!(r.status, Status::Fail, "{r:?}");
        assert!(r.message.contains(alt.to_str().unwrap()), "{r:?}");
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
        use ark_types::SessionSpec;
        use std::collections::BTreeMap;

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
        let id = SessionId::new("lost");
        fs::create_dir_all(layout.session_dir(&id)).unwrap();
        let spec = SessionSpec {
            id: id.clone(),
            name: id.name.clone(),
            scene_path: None,
            cwd: PathBuf::from("/nonexistent/doctor/probe"),
            env: BTreeMap::new(),
            created_at: chrono::Utc::now(),
            ext_config: BTreeMap::new(),
        };
        let raw = serde_json::to_string(&spec).unwrap();
        fs::write(layout.session_spec_path(&id), raw).unwrap();

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

    // ---- F-513: --json --fix must skip fixes ----

    /// Snapshot every path (relative to `root`) and its type so we can
    /// assert that `--json --fix` left the filesystem untouched.
    fn snapshot_tree(root: &Path) -> Vec<String> {
        fn walk(dir: &Path, out: &mut Vec<String>) {
            if let Ok(rd) = fs::read_dir(dir) {
                for e in rd.flatten() {
                    let p = e.path();
                    let kind = if p.is_dir() { "d" } else { "f" };
                    out.push(format!("{kind}:{}", p.display()));
                    if p.is_dir() {
                        walk(&p, out);
                    }
                }
            }
        }
        let mut v = Vec::new();
        walk(root, &mut v);
        v.sort();
        v
    }

    #[test]
    fn json_fix_combo_skips_fixes_and_leaves_state_untouched() {
        // Seed a runtime dir containing an orphan socket — that's
        // the cheapest fixable Warn. Without F-513, run() would
        // apply DeleteSocket and the file would vanish; with the
        // fix, JSON mode is read-only so the file survives.
        let tmp = tempfile::Builder::new()
            .prefix("arkd-f513")
            .tempdir_in("/tmp")
            .unwrap();
        let ctx = test_ctx(tmp.path());
        fs::create_dir_all(&ctx.state_dir).unwrap();
        fs::create_dir_all(&ctx.config_dir).unwrap();
        fs::create_dir_all(&ctx.runtime_dir).unwrap();
        seed_agents_dir(&ctx.runtime_dir);
        let sock = ctx.runtime_dir.join("agents").join("cavekit-ghost-99.sock");
        fs::File::create(&sock).unwrap();

        let before = snapshot_tree(tmp.path());

        // --json + --fix + --yes: fix pass should be SKIPPED.
        let args = DoctorArgs {
            fix: true,
            yes: true,
            json: true,
        };
        // run() may return Err (Generic) when aggregated status is
        // Fail (e.g. zellij/claude missing on the test host). That's
        // unrelated to the fix-skipping property we're asserting.
        let _ = run(args, &ctx);

        let after = snapshot_tree(tmp.path());
        assert_eq!(
            before, after,
            "--json --fix must not mutate disk state; before={before:?} after={after:?}"
        );
        assert!(
            sock.exists(),
            "orphan socket must still exist after --json --fix"
        );
    }

    // ---- F-711: --fix recomputes status after applying fixes ----

    /// Given a missing runtime-dir (Fail + CreateDir fix), verify that:
    /// 1. the initial `run_all` snapshot contains a Fail,
    /// 2. `run_fixes(&rs, true)` applies one repair,
    /// 3. a FRESH `run_all` sees the newly-created dir as Ok.
    ///
    /// This models the `run()` control flow introduced by F-711: after a
    /// successful repair, the final `aggregate_status` must reflect the
    /// post-fix filesystem, not the pre-fix snapshot. Without the second
    /// `run_all`, automation running `ark doctor --fix --yes` would see
    /// exit 2 even though the repair succeeded.
    #[test]
    fn fix_recheck_sees_repaired_state_as_ok() {
        let tmp = tempfile::Builder::new()
            .prefix("arkd-f711")
            .tempdir_in("/tmp")
            .unwrap();
        let ctx = test_ctx(tmp.path());
        // Intentionally do NOT create ctx.runtime_dir. check_runtime_dir
        // will return Fail + FixAction::CreateDir.
        fs::create_dir_all(&ctx.state_dir).unwrap();
        fs::create_dir_all(&ctx.config_dir).unwrap();

        assert!(
            !ctx.runtime_dir.exists(),
            "precondition: runtime_dir must be missing"
        );

        // Pre-fix: check_runtime_dir sees Fail + CreateDir fix.
        let pre = check_runtime_dir(&ctx);
        assert_eq!(pre.status, Status::Fail, "{pre:?}");
        assert!(matches!(pre.fix, Some(FixAction::CreateDir(_))), "{pre:?}");

        // Apply the fix.
        let applied = run_fixes(&[pre], true).expect("fix");
        assert_eq!(applied, 1, "exactly one repair should have been applied");

        // Post-fix: the dir now exists and the check returns Ok. This is
        // exactly what run() sees after the F-711 re-run_all pass.
        assert!(ctx.runtime_dir.exists(), "runtime_dir should now exist");
        let post = check_runtime_dir(&ctx);
        assert_eq!(post.status, Status::Ok, "{post:?}");
        assert!(post.fix.is_none(), "{post:?}");
    }

    /// Guard the `applied` return contract: when no CheckResult carries
    /// a FixAction, `run_fixes` must return 0. This is the signal `run()`
    /// uses to skip the expensive re-check pass.
    #[test]
    fn run_fixes_returns_zero_when_nothing_fixable() {
        let rs = vec![CheckResult::ok("a", "ok"), CheckResult::warn("b", "warn")];
        let applied = run_fixes(&rs, true).expect("fix");
        assert_eq!(applied, 0);
    }

    // ---- T-098: status plugin distribution ----

    /// Non-empty fake wasm bytes — the test never actually loads this.
    const FAKE_WASM: &[u8] = b"\0asm\x01\x00\x00\x00fake-status-plugin-bytes";

    #[test]
    fn status_plugin_ok_when_installed_and_matches() {
        let tmp = tempfile::Builder::new()
            .prefix("arkd-t098-ok")
            .tempdir_in("/tmp")
            .unwrap();
        let ctx = test_ctx(tmp.path());
        let target = ctx.config_dir.join("plugins").join("ark-status.wasm");
        fs::create_dir_all(target.parent().unwrap()).unwrap();
        fs::write(&target, FAKE_WASM).unwrap();

        let r = check_status_plugin_installed_with(&ctx, FAKE_WASM, true);
        assert_eq!(r.status, Status::Ok, "{r:?}");
        assert!(r.fix.is_none(), "{r:?}");
        assert!(r.message.contains("up to date"), "{r:?}");
    }

    #[test]
    fn status_plugin_warn_when_missing() {
        let tmp = tempfile::Builder::new()
            .prefix("arkd-t098-missing")
            .tempdir_in("/tmp")
            .unwrap();
        let ctx = test_ctx(tmp.path());
        let r = check_status_plugin_installed_with(&ctx, FAKE_WASM, true);
        assert_eq!(r.status, Status::Warn, "{r:?}");
        match &r.fix {
            Some(FixAction::WritePluginWasm(name, bytes, target)) => {
                assert_eq!(*name, "ark-status");
                assert_eq!(*bytes, FAKE_WASM);
                assert_eq!(
                    target,
                    &ctx.config_dir.join("plugins").join("ark-status.wasm")
                );
            }
            other => panic!("expected WritePluginWasm fix, got {other:?}"),
        }
    }

    #[test]
    fn status_plugin_warn_when_stale() {
        let tmp = tempfile::Builder::new()
            .prefix("arkd-t098-stale")
            .tempdir_in("/tmp")
            .unwrap();
        let ctx = test_ctx(tmp.path());
        let target = ctx.config_dir.join("plugins").join("ark-status.wasm");
        fs::create_dir_all(target.parent().unwrap()).unwrap();
        fs::write(&target, b"old-bytes").unwrap();

        let r = check_status_plugin_installed_with(&ctx, FAKE_WASM, true);
        assert_eq!(r.status, Status::Warn, "{r:?}");
        assert!(r.message.contains("differs"), "{r:?}");
        assert!(matches!(r.fix, Some(FixAction::WritePluginWasm(..))));
    }

    #[test]
    fn status_plugin_skips_when_not_embedded() {
        let tmp = tempfile::Builder::new()
            .prefix("arkd-t098-unavail")
            .tempdir_in("/tmp")
            .unwrap();
        let ctx = test_ctx(tmp.path());
        // Empty bytes / not available → Ok with skip message.
        let r = check_status_plugin_installed_with(&ctx, b"", false);
        assert_eq!(r.status, Status::Ok, "{r:?}");
        assert!(r.fix.is_none());
        assert!(r.message.contains("not embedded"), "{r:?}");

        // Defensive: even if available==true but bytes are empty, skip.
        let r2 = check_status_plugin_installed_with(&ctx, b"", true);
        assert_eq!(r2.status, Status::Ok, "{r2:?}");
    }

    #[test]
    fn write_plugin_wasm_fix_creates_file_with_embedded_bytes() {
        let tmp = tempfile::Builder::new()
            .prefix("arkd-t098-fix")
            .tempdir_in("/tmp")
            .unwrap();
        let ctx = test_ctx(tmp.path());
        let target = ctx.config_dir.join("plugins").join("ark-status.wasm");
        assert!(!target.exists());

        let r = check_status_plugin_installed_with(&ctx, FAKE_WASM, true);
        assert_eq!(r.status, Status::Warn);
        run_fixes(&[r], true).expect("fix");

        assert!(target.is_file(), "plugin must be materialized");
        let installed = fs::read(&target).unwrap();
        assert_eq!(installed, FAKE_WASM);
    }

    #[test]
    fn status_plugin_kdl_snippet_shape() {
        let path = PathBuf::from("/home/u/.config/ark/plugins/ark-status.wasm");
        let s = status_plugin_kdl_snippet(&path);
        assert!(s.contains("plugins {"));
        assert!(
            s.contains("ark-status location=\"file:/home/u/.config/ark/plugins/ark-status.wasm\"")
        );
        assert!(s.ends_with("}\n"));
    }

    // ---- T-109: picker plugin distribution ----

    const FAKE_PICKER_WASM: &[u8] = b"\0asm\x01\x00\x00\x00fake-picker-plugin-bytes";

    #[test]
    fn picker_plugin_ok_when_installed_and_matches() {
        let tmp = tempfile::Builder::new()
            .prefix("arkd-t109-ok")
            .tempdir_in("/tmp")
            .unwrap();
        let ctx = test_ctx(tmp.path());
        let target = ctx.config_dir.join("plugins").join("ark-picker.wasm");
        fs::create_dir_all(target.parent().unwrap()).unwrap();
        fs::write(&target, FAKE_PICKER_WASM).unwrap();

        let r = check_picker_plugin_installed_with(&ctx, FAKE_PICKER_WASM, true);
        assert_eq!(r.status, Status::Ok, "{r:?}");
        assert!(r.fix.is_none(), "{r:?}");
        assert!(r.message.contains("up to date"), "{r:?}");
    }

    #[test]
    fn picker_plugin_warn_when_missing() {
        let tmp = tempfile::Builder::new()
            .prefix("arkd-t109-missing")
            .tempdir_in("/tmp")
            .unwrap();
        let ctx = test_ctx(tmp.path());
        let r = check_picker_plugin_installed_with(&ctx, FAKE_PICKER_WASM, true);
        assert_eq!(r.status, Status::Warn, "{r:?}");
        match &r.fix {
            Some(FixAction::WritePluginWasm(name, bytes, target)) => {
                assert_eq!(*name, "ark-picker");
                assert_eq!(*bytes, FAKE_PICKER_WASM);
                assert_eq!(
                    target,
                    &ctx.config_dir.join("plugins").join("ark-picker.wasm")
                );
            }
            other => panic!("expected WritePluginWasm fix, got {other:?}"),
        }
    }

    #[test]
    fn picker_plugin_skips_when_not_embedded() {
        let tmp = tempfile::Builder::new()
            .prefix("arkd-t109-unavail")
            .tempdir_in("/tmp")
            .unwrap();
        let ctx = test_ctx(tmp.path());
        let r = check_picker_plugin_installed_with(&ctx, b"", false);
        assert_eq!(r.status, Status::Ok, "{r:?}");
        assert!(r.fix.is_none());
        assert!(r.message.contains("not embedded"), "{r:?}");

        // Defensive: even if available==true but bytes are empty, skip.
        let r2 = check_picker_plugin_installed_with(&ctx, b"", true);
        assert_eq!(r2.status, Status::Ok, "{r2:?}");
    }

    #[test]
    fn picker_plugin_fix_creates_file_with_embedded_bytes() {
        let tmp = tempfile::Builder::new()
            .prefix("arkd-t109-fix")
            .tempdir_in("/tmp")
            .unwrap();
        let ctx = test_ctx(tmp.path());
        let target = ctx.config_dir.join("plugins").join("ark-picker.wasm");
        assert!(!target.exists());

        let r = check_picker_plugin_installed_with(&ctx, FAKE_PICKER_WASM, true);
        assert_eq!(r.status, Status::Warn);
        run_fixes(&[r], true).expect("fix");

        assert!(target.is_file(), "picker plugin must be materialized");
        let installed = fs::read(&target).unwrap();
        assert_eq!(installed, FAKE_PICKER_WASM);
    }

    #[test]
    fn picker_plugin_kdl_snippet_shape() {
        let path = PathBuf::from("/home/u/.config/ark/plugins/ark-picker.wasm");
        let s = picker_plugin_kdl_snippet(&path);
        // Recommended keybind — Ctrl+g a (cavekit-plugin-picker.md §Distribution).
        assert!(
            s.contains("shared_except \"locked\""),
            "snippet must use shared_except \"locked\": {s}"
        );
        assert!(
            s.contains("bind \"Ctrl g\" \"a\""),
            "snippet must bind Ctrl+g a: {s}"
        );
        assert!(
            s.contains("LaunchOrFocusPlugin \"file:/home/u/.config/ark/plugins/ark-picker.wasm\""),
            "snippet must launch picker at target path: {s}"
        );
        assert!(
            s.contains("floating true"),
            "snippet must set floating: {s}"
        );
        assert!(
            s.contains("Add to ~/.config/zellij/config.kdl"),
            "snippet must include config.kdl hint: {s}"
        );
    }

    // ---- T-126: default scene parse check ----

    #[test]
    fn check_default_scene_succeeds() {
        // The built-in default scene must always parse — this is a
        // compile-time guarantee via `include_str!`.
        let r = check_default_scene();
        assert_eq!(r.status, Status::Ok, "{r:?}");
        assert!(r.message.contains("default"), "{r:?}");
    }

    // ---- T-126: extensions resolve check ----

    #[test]
    fn check_extensions_resolve_empty_is_ok() {
        // When no extensions are installed, the check should still succeed.
        // We can't easily isolate XDG_DATA_HOME here without the env lock,
        // but we can at least verify the function doesn't panic.
        let r = check_extensions_resolve();
        // Either "no extensions installed" (Ok) or some system exts found.
        assert_ne!(r.status, Status::Fail, "{r:?}");
    }

    #[test]
    fn check_extensions_resolve_discovers_installed_extensions() {
        // Seed a project-local extension and verify enumerate_extensions
        // discovers it. The manifest may or may not parse successfully
        // depending on the facet-kdl version's CapabilitySet handling,
        // but the extension must be FOUND in the enumeration.
        let tmp = tempfile::Builder::new()
            .prefix("arkd-t126-ext-ok")
            .tempdir_in("/tmp")
            .unwrap();
        let ext_dir = tmp.path().join(".ark/extensions/demo");
        fs::create_dir_all(&ext_dir).unwrap();
        fs::write(
            ext_dir.join("extension.kdl"),
            "extension {\n    \
                 name \"demo\"\n    \
                 version \"0.1.0\"\n    \
                 ark-range \">=0.1\"\n    \
                 zellij-range \"\"\n    \
                 config { }\n\
             }\n",
        )
        .unwrap();

        let rows = crate::commands::ext::list::enumerate_extensions(
            tmp.path(),
            None,
            &[],
        );
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].name, "demo");
    }

    #[test]
    fn check_extensions_resolve_with_broken_extension() {
        let tmp = tempfile::Builder::new()
            .prefix("arkd-t126-ext-bad")
            .tempdir_in("/tmp")
            .unwrap();
        let ext_dir = tmp.path().join(".ark/extensions/broken");
        fs::create_dir_all(&ext_dir).unwrap();
        fs::write(ext_dir.join("extension.kdl"), "not { valid { kdl {").unwrap();

        let rows = crate::commands::ext::list::enumerate_extensions(
            tmp.path(),
            None,
            &[],
        );
        assert_eq!(rows.len(), 1);
        assert!(rows[0].error.is_some(), "broken ext should surface error");
    }
}
