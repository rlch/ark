//! T-106 — Picker resurrect flow.
//!
//! When the user presses `r` on a crashed agent, the picker:
//!
//! 1. Reads `{state_dir}/agents/{id}/spec.json` to recover the original
//!    spawn parameters (orchestrator, name, cwd, layout, cmd).
//! 2. Archives the old state directory to
//!    `{state_dir}/agents/{id}.archived-{ts_ms}` so a fresh spawn can
//!    re-create `{state_dir}/agents/{id}` without colliding on stale
//!    status.json / events log / layout.kdl.
//! 3. Returns the `ark spawn …` argv the wasm layer then passes to
//!    zellij-tile's `run_command`.
//!
//! The function is a pure pipeline of host-side IO operations: the wasm
//! wiring in `lib.rs` simply drives it from the `PickerAction::Resurrect`
//! branch. Keeping the logic here (and out of `wasm_plugin`) lets the host
//! test suite exercise every error and happy path without spinning up a
//! zellij plugin host.
//!
//! Satisfies `context/kits/cavekit-plugin-picker.md` R7/R8:
//!
//! - R7 keymap binds `r` to resurrect on crashed agents only (enforced by
//!   [`crate::render_list::handle_list_key`] — see T-102).
//! - R8 defines the resurrect semantics: archive old state, re-spawn with
//!   the same parameters. This module owns the archive + argv build.

use std::fs;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::bootstrap::find_string_field;

/// Error shape for [`resurrect`] — the wasm layer maps each variant to a
/// banner in [`crate::state::ErrorState`] per the kit's "error surfaces"
/// bullet in R7.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResurrectError {
    /// `{state_dir}/agents/{id}/spec.json` does not exist. The agent was
    /// probably pruned by `ark prune` between the list render and the
    /// key press — the picker should refresh the cache.
    SpecNotFound,
    /// spec.json exists but we could not parse a required field out of
    /// it. Carries a short explanation of what went wrong so operators
    /// can diagnose supervisors that write malformed specs.
    SpecParseError(String),
    /// The archive rename (`agents/{id}` → `agents/{id}.archived-{ts}`)
    /// failed — typically because the user lacks write permission on
    /// `state_dir` or another process has the directory open.
    ArchiveFailed(String),
}

/// Fields pulled out of `spec.json` that the resurrect argv needs.
///
/// Matches the subset of `ark_types::AgentSpec` the picker cares about:
/// missing optional fields (e.g. `layout`) collapse to `None`; missing
/// required fields default to the empty string and are still surfaced to
/// the caller so the resurrect argv round-trips the on-disk spec as
/// faithfully as possible.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AgentSpecFields {
    /// Orchestrator slug (e.g. `cavekit`, `claude-code`).
    pub orchestrator: String,
    /// Human label — the `--name` argument to `ark spawn`.
    pub name: String,
    /// Worktree path. Serialised as a JSON string by `AgentSpec`
    /// (PathBuf → string via serde default).
    pub cwd: String,
    /// Optional zellij layout KDL stem. `None` means the orchestrator
    /// gets to pick a default at spawn time — omitted from argv.
    pub layout: Option<String>,
    /// Primary agent pane command, e.g. `["claude", "--resume"]`.
    pub cmd: Vec<String>,
}

/// Read and parse `{state_dir}/agents/{agent_id}/spec.json`.
///
/// Uses the same hand-rolled JSON helpers as [`crate::bootstrap`] — we
/// avoid pulling in `serde_json` per R1's "no JSON deserializer in wasm"
/// bullet.
pub fn read_spec(state_dir: &Path, agent_id: &str) -> Result<AgentSpecFields, ResurrectError> {
    let path = state_dir.join("agents").join(agent_id).join("spec.json");
    let contents = match fs::read_to_string(&path) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(ResurrectError::SpecNotFound);
        }
        Err(e) => return Err(ResurrectError::SpecParseError(e.to_string())),
    };
    parse_spec(&contents)
}

/// Parse a `spec.json` body into the subset of fields the resurrect flow
/// needs. Split out so tests can exercise malformed JSON without hitting
/// the filesystem.
fn parse_spec(json: &str) -> Result<AgentSpecFields, ResurrectError> {
    // Basic sanity: must contain at least one `{` — this short-circuits
    // obviously empty / truncated files into SpecParseError instead of
    // silently returning defaults.
    if !json.contains('{') {
        return Err(ResurrectError::SpecParseError(
            "spec.json is not a JSON object".into(),
        ));
    }

    let orchestrator = find_string_field(json, "orchestrator").unwrap_or_default();
    let name = find_string_field(json, "name").unwrap_or_default();
    let cwd = find_string_field(json, "cwd").unwrap_or_default();
    // `layout`: either a JSON string or `null`. Missing → None; null →
    // None. find_string_field already returns None on both.
    let layout = find_string_field(json, "layout").filter(|s| !s.is_empty());
    let cmd = find_string_array_field(json, "cmd").unwrap_or_default();

    Ok(AgentSpecFields {
        orchestrator,
        name,
        cwd,
        layout,
        cmd,
    })
}

/// Locate `"cmd":[ "a", "b", ... ]` and return the strings. Mirrors the
/// other bootstrap helpers — we only need to handle the ASCII-ish tokens
/// that `ark spawn` accepts.
fn find_string_array_field(json: &str, key: &str) -> Option<Vec<String>> {
    let pat = format!("\"{key}\"");
    let rel = json.find(&pat)?;
    let after = &json[rel + pat.len()..];
    let trimmed = after.trim_start();
    let trimmed = trimmed.strip_prefix(':')?.trim_start();
    let rest = trimmed.strip_prefix('[')?;
    // Find matching `]`, respecting strings.
    let bytes = rest.as_bytes();
    let mut end = None;
    let mut in_string = false;
    let mut escaped = false;
    for (i, &b) in bytes.iter().enumerate() {
        if escaped {
            escaped = false;
            continue;
        }
        if in_string {
            match b {
                b'\\' => escaped = true,
                b'"' => in_string = false,
                _ => {}
            }
            continue;
        }
        match b {
            b'"' => in_string = true,
            b']' => {
                end = Some(i);
                break;
            }
            _ => {}
        }
    }
    let end = end?;
    let inner = &rest[..end];
    // Walk through inner, collecting each string literal.
    let mut out = Vec::new();
    let ibytes = inner.as_bytes();
    let mut i = 0;
    while i < ibytes.len() {
        // Skip whitespace/commas.
        while i < ibytes.len() && matches!(ibytes[i], b' ' | b'\n' | b'\r' | b'\t' | b',') {
            i += 1;
        }
        if i >= ibytes.len() {
            break;
        }
        if ibytes[i] != b'"' {
            // Malformed element — bail.
            return None;
        }
        // Read string with escape handling.
        let mut s = String::new();
        i += 1;
        while i < ibytes.len() {
            let b = ibytes[i];
            if b == b'\\' {
                i += 1;
                if i >= ibytes.len() {
                    return None;
                }
                match ibytes[i] {
                    b'"' => s.push('"'),
                    b'\\' => s.push('\\'),
                    b'n' => s.push('\n'),
                    b't' => s.push('\t'),
                    b'r' => s.push('\r'),
                    b'/' => s.push('/'),
                    other => s.push(other as char),
                }
                i += 1;
                continue;
            }
            if b == b'"' {
                i += 1;
                out.push(s);
                break;
            }
            s.push(b as char);
            i += 1;
        }
    }
    Some(out)
}

/// Rename `{state_dir}/agents/{agent_id}` to
/// `{state_dir}/agents/{agent_id}.archived-{ts_ms}`. Missing source is a
/// no-op (returns `Ok`) so callers can always invoke this before spawn.
pub fn archive_old_state_dir(state_dir: &Path, agent_id: &str) -> Result<(), ResurrectError> {
    let src = state_dir.join("agents").join(agent_id);
    if !src.exists() {
        return Ok(());
    }
    let ts_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    let dst = state_dir
        .join("agents")
        .join(format!("{agent_id}.archived-{ts_ms}"));
    fs::rename(&src, &dst)
        .map_err(|e| ResurrectError::ArchiveFailed(format!("{}: {e}", src.display())))
}

/// Build the `ark spawn …` argv that, when executed, re-creates the
/// agent under the same id-space. Matches [`crate::render_new_agent::
/// build_spawn_argv`] for the R6 new-agent form so the semantics are
/// identical — resurrect is a "spawn with remembered params" operation.
pub fn build_respawn_argv(spec: &AgentSpecFields) -> Vec<String> {
    // `ark spawn --orchestrator <o> --cwd <c> --name <n> [--layout <l>] -- <cmd...>`
    let mut argv = vec![
        "ark".to_string(),
        "spawn".to_string(),
        "--orchestrator".to_string(),
        spec.orchestrator.clone(),
        "--cwd".to_string(),
        spec.cwd.clone(),
        "--name".to_string(),
        spec.name.clone(),
    ];
    if let Some(layout) = &spec.layout {
        argv.push("--layout".into());
        argv.push(layout.clone());
    }
    argv.push("--".into());
    argv.extend(spec.cmd.iter().cloned());
    argv
}

/// End-to-end resurrect pipeline: read the spec, archive the stale
/// state dir, and return the argv the caller must exec. Safe to call
/// on agents that no longer have a state dir — the archive step
/// short-circuits (Ok) but the spec read still fails with
/// [`ResurrectError::SpecNotFound`] because spec.json is the required
/// input.
pub fn resurrect(state_dir: &Path, agent_id: &str) -> Result<Vec<String>, ResurrectError> {
    let spec = read_spec(state_dir, agent_id)?;
    archive_old_state_dir(state_dir, agent_id)?;
    Ok(build_respawn_argv(&spec))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    /// Tiny self-contained tempdir — mirrors the helper used in
    /// `bootstrap.rs::tests` so we don't add `tempfile` to the picker's
    /// dependency set (R1 keeps the dep graph narrow).
    struct TempDir(PathBuf);
    impl TempDir {
        fn new(tag: &str) -> Self {
            static COUNTER: AtomicU64 = AtomicU64::new(0);
            let n = COUNTER.fetch_add(1, Ordering::SeqCst);
            let p = std::env::temp_dir().join(format!(
                "ark-picker-resurrect-{}-{}-{}",
                tag,
                std::process::id(),
                n
            ));
            let _ = fs::remove_dir_all(&p);
            fs::create_dir_all(&p).unwrap();
            TempDir(p)
        }
        fn path(&self) -> &Path {
            &self.0
        }
    }
    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn write_spec(dir: &Path, id: &str, body: &str) -> std::path::PathBuf {
        let agent_dir = dir.join("agents").join(id);
        std::fs::create_dir_all(&agent_dir).unwrap();
        let path = agent_dir.join("spec.json");
        std::fs::write(&path, body).unwrap();
        path
    }

    const FULL_SPEC: &str = r#"{
        "id": {"id": "auth-01"},
        "name": "auth",
        "orchestrator": "cavekit",
        "engine": "claude-code",
        "cwd": "/tmp/auth",
        "cmd": ["claude", "--resume"],
        "layout": "builder"
    }"#;

    const NO_LAYOUT_SPEC: &str = r#"{
        "id": {"id": "auth-01"},
        "name": "auth",
        "orchestrator": "cavekit",
        "engine": "claude-code",
        "cwd": "/tmp/auth",
        "cmd": ["claude"],
        "layout": null
    }"#;

    #[test]
    fn read_spec_parses_full_spec() {
        let td = TempDir::new("td");
        write_spec(td.path(), "auth-01", FULL_SPEC);
        let spec = read_spec(td.path(), "auth-01").unwrap();
        assert_eq!(spec.orchestrator, "cavekit");
        assert_eq!(spec.name, "auth");
        assert_eq!(spec.cwd, "/tmp/auth");
        assert_eq!(spec.layout.as_deref(), Some("builder"));
        assert_eq!(spec.cmd, vec!["claude", "--resume"]);
    }

    #[test]
    fn read_spec_missing_file_is_spec_not_found() {
        let td = TempDir::new("td");
        let err = read_spec(td.path(), "ghost").unwrap_err();
        assert_eq!(err, ResurrectError::SpecNotFound);
    }

    #[test]
    fn read_spec_malformed_json_is_parse_error() {
        let td = TempDir::new("td");
        write_spec(td.path(), "bad-01", "not a json object at all");
        match read_spec(td.path(), "bad-01").unwrap_err() {
            ResurrectError::SpecParseError(_) => {}
            other => panic!("expected SpecParseError, got {other:?}"),
        }
    }

    #[test]
    fn read_spec_missing_fields_default_to_empty_or_none() {
        let td = TempDir::new("td");
        // Minimal object — no fields at all beyond id. name/cwd/etc
        // default to empty strings; layout is None; cmd is empty.
        write_spec(td.path(), "sparse-01", r#"{"id":{"id":"sparse-01"}}"#);
        let spec = read_spec(td.path(), "sparse-01").unwrap();
        assert_eq!(spec.orchestrator, "");
        assert_eq!(spec.name, "");
        assert_eq!(spec.cwd, "");
        assert_eq!(spec.layout, None);
        assert!(spec.cmd.is_empty());
    }

    #[test]
    fn read_spec_null_layout_is_none() {
        let td = TempDir::new("td");
        write_spec(td.path(), "auth-01", NO_LAYOUT_SPEC);
        let spec = read_spec(td.path(), "auth-01").unwrap();
        assert_eq!(spec.layout, None);
        assert_eq!(spec.cmd, vec!["claude"]);
    }

    #[test]
    fn archive_old_state_dir_renames_existing() {
        let td = TempDir::new("td");
        let src = td.path().join("agents").join("auth-01");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(src.join("marker"), b"x").unwrap();

        archive_old_state_dir(td.path(), "auth-01").unwrap();

        assert!(!src.exists(), "src should be gone");
        // Scan parent to locate the archived-<ts> entry.
        let parent = td.path().join("agents");
        let mut found = false;
        for entry in std::fs::read_dir(&parent).unwrap() {
            let entry = entry.unwrap();
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with("auth-01.archived-") {
                found = true;
                // Marker file preserved inside the archived dir.
                assert!(entry.path().join("marker").exists());
            }
        }
        assert!(found, "no archived-* dir produced");
    }

    #[test]
    fn archive_old_state_dir_missing_source_is_ok() {
        let td = TempDir::new("td");
        // No agents dir at all — archive should still succeed (no-op).
        archive_old_state_dir(td.path(), "ghost").unwrap();
    }

    #[test]
    fn build_respawn_argv_with_layout() {
        let spec = AgentSpecFields {
            orchestrator: "cavekit".into(),
            name: "auth".into(),
            cwd: "/tmp/auth".into(),
            layout: Some("builder".into()),
            cmd: vec!["claude".into(), "--resume".into()],
        };
        let argv = build_respawn_argv(&spec);
        assert_eq!(
            argv,
            vec![
                "ark",
                "spawn",
                "--orchestrator",
                "cavekit",
                "--cwd",
                "/tmp/auth",
                "--name",
                "auth",
                "--layout",
                "builder",
                "--",
                "claude",
                "--resume",
            ]
        );
    }

    #[test]
    fn build_respawn_argv_without_layout_omits_flag() {
        let spec = AgentSpecFields {
            orchestrator: "claude-code".into(),
            name: "svc".into(),
            cwd: "/tmp/svc".into(),
            layout: None,
            cmd: vec!["claude".into()],
        };
        let argv = build_respawn_argv(&spec);
        // No `--layout` anywhere in argv.
        assert!(!argv.iter().any(|s| s == "--layout"));
        // `--` separator present, cmd after.
        let dash_idx = argv.iter().position(|s| s == "--").unwrap();
        assert_eq!(argv[dash_idx + 1], "claude");
    }

    #[test]
    fn build_respawn_argv_multi_token_cmd() {
        let spec = AgentSpecFields {
            orchestrator: "cavekit".into(),
            name: "n".into(),
            cwd: "/c".into(),
            layout: None,
            cmd: vec!["claude".into(), "--flag".into(), "value".into()],
        };
        let argv = build_respawn_argv(&spec);
        let dash_idx = argv.iter().position(|s| s == "--").unwrap();
        assert_eq!(&argv[dash_idx + 1..], &["claude", "--flag", "value"]);
    }

    #[test]
    fn resurrect_archives_and_returns_argv() {
        let td = TempDir::new("td");
        write_spec(td.path(), "auth-01", FULL_SPEC);
        let argv = resurrect(td.path(), "auth-01").unwrap();
        // Argv content sanity.
        assert_eq!(argv[0], "ark");
        assert_eq!(argv[1], "spawn");
        assert!(argv.iter().any(|s| s == "--layout"));
        // Old dir archived.
        assert!(!td.path().join("agents").join("auth-01").exists());
    }

    #[test]
    fn resurrect_missing_spec_errors_without_archive() {
        let td = TempDir::new("td");
        let err = resurrect(td.path(), "ghost").unwrap_err();
        assert_eq!(err, ResurrectError::SpecNotFound);
        // No archived-* directory should have been created.
        let agents = td.path().join("agents");
        if agents.exists() {
            for entry in std::fs::read_dir(&agents).unwrap() {
                let name = entry.unwrap().file_name().to_string_lossy().into_owned();
                assert!(
                    !name.contains(".archived-"),
                    "unexpected archive on missing-spec path: {name}"
                );
            }
        }
    }
}
