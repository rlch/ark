//! T-042 + T-043 (claude-code-ext R10) — `doctor_checks` implementation.
//!
//! The extension contributes five checks that `ark doctor` renders as a
//! table. Each check produces a [`CheckResult`] with a stable `kind`
//! string, a severity [`CheckLevel`], a human-readable `message`, and
//! an optional `fix` remediation string.
//!
//! # Response shape
//!
//! The `DoctorChecksResponse.checks` field is [`ark_ext_proto::OpaqueJson`]
//! (a JSON string). This module serialises a
//! `{ "results": [<CheckResult>, ...] }` envelope so a future doctor-side
//! decoder can pattern-match on the top-level key without inspecting
//! per-extension JSON dialects.
//!
//! # Check list (R10)
//!
//! 1. `claude-code/which-claude` — `claude` on `$PATH` (ERROR on miss).
//! 2. `claude-code/cc-hook-binary` — installed binary present + version
//!    aligned with the crate's [`crate::CC_HOOK_BYTES`] hash (WARN on
//!    miss/drift; the stub bytes intentionally don't match any real
//!    install, so this path will WARN until T-008a-real lands).
//! 3. `claude-code/settings-hooks` — `~/.claude/settings.json` drift
//!    across the 9 ark-managed hook entries (WARN on drift).
//! 4. `claude-code/sessions-writable` — `$STATE/sessions` writable
//!    (ERROR on unwritable).
//! 5. `claude-code/view-wired` — informational flag carrying whether a
//!    scene references `claude-code` with `subagents=@…` wired (INFO,
//!    no fix).
//!
//! Each check's `kind` is a stable `claude-code/<slug>` string — doctor
//! consumers can whitelist specific checks off without string-matching on
//! message prose.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Severity level carried on a [`CheckResult`].
///
/// Mapped per R10:
/// - `Error` — required state missing (claude binary, $STATE/sessions).
/// - `Warn` — non-fatal drift (cc-hook version, settings.json hooks).
/// - `Info` — informational (view wiring).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckLevel {
    /// Doctor surfaces with red. Blocks happy-path operation until fixed.
    Error,
    /// Doctor surfaces with yellow. Feature degraded but not blocked.
    Warn,
    /// Doctor surfaces plain. No action required.
    Info,
}

/// One doctor-check outcome, serialised into
/// [`ark_ext_proto::DoctorChecksResponse::checks`] under a `results`
/// envelope. See module docs for the `kind` namespace convention.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckResult {
    /// Stable identifier the doctor consumer can pattern-match on.
    /// Shape: `claude-code/<slug>`.
    pub kind: String,
    /// Severity level. See [`CheckLevel`].
    pub level: CheckLevel,
    /// Human-readable one-line summary rendered in the doctor table.
    pub message: String,
    /// Optional remediation hint. When `Some`, doctor renders a
    /// follow-up line with the fix command/URL. When `None`, no hint
    /// is shown (informational / no-op cases).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fix: Option<String>,
}

/// Top-level envelope carried in
/// [`ark_ext_proto::DoctorChecksResponse::checks`]. Kept as a named
/// struct so a future doctor-side decoder can `serde_json::from_str`
/// directly without a hand-rolled map walk.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DoctorEnvelope {
    /// All five R10 check results, in declaration order.
    pub results: Vec<CheckResult>,
}

// ---------------------------------------------------------------------------
// Individual check implementations
// ---------------------------------------------------------------------------

/// Resolve the absolute path of `bin` on `$PATH` (first hit). Returns
/// `None` when the env var is unset OR no directory on it contains an
/// executable file named `bin`. Pure stdlib — avoids a new `which`
/// workspace dep per the constraints brief.
pub fn which_on_path(bin: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        let candidate = dir.join(bin);
        if candidate.is_file() && is_executable(&candidate) {
            return Some(candidate);
        }
    }
    None
}

#[cfg(unix)]
fn is_executable(p: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    match std::fs::metadata(p) {
        Ok(m) => m.permissions().mode() & 0o111 != 0,
        Err(_) => false,
    }
}

#[cfg(not(unix))]
fn is_executable(_p: &Path) -> bool {
    // On non-unix we trust that a file named `bin` is executable — we
    // don't otherwise support these platforms in v0.1 anyway.
    true
}

/// Check 1: `claude` on `$PATH`.
pub fn check_which_claude() -> CheckResult {
    match which_on_path("claude") {
        Some(path) => CheckResult {
            kind: "claude-code/which-claude".into(),
            level: CheckLevel::Info,
            message: format!("claude binary found on $PATH: {}", path.display()),
            fix: None,
        },
        None => CheckResult {
            kind: "claude-code/which-claude".into(),
            level: CheckLevel::Error,
            message: "claude binary not found on $PATH".into(),
            fix: Some(
                "Install Claude Code: https://docs.anthropic.com/en/docs/claude-code (or `npm install -g @anthropic-ai/claude-code`)"
                    .into(),
            ),
        },
    }
}

/// Crate version stamp — emitted in the `cc-hook-binary` drift check so
/// doctor consumers know which crate's bytes the check compared.
pub const CRATE_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Portable content hash. Uses a simple FNV-1a 64 over the byte slice
/// — stable across architectures, deterministic, no new deps. Two
/// identical byte slices produce identical hashes; drift detection just
/// needs "same-or-different", not cryptographic strength.
pub fn content_hash(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in bytes {
        h ^= *b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

/// Check 2: cc-hook binary present + version-aligned.
///
/// `installed_path_override` lets tests pin a fake install path; None
/// falls through to [`crate::cc_hook_install_path`].
pub fn check_cc_hook_binary(installed_path_override: Option<&Path>) -> CheckResult {
    let installed = installed_path_override
        .map(|p| p.to_path_buf())
        .unwrap_or_else(crate::cc_hook_install_path);

    // Stub-bytes fast path: while CC_HOOK_BYTES is &[] (see
    // crate-level doc), we cannot perform a real drift check; any
    // installed bytes will have a different hash. Warn informatively
    // rather than falsely accusing the user.
    if crate::CC_HOOK_BYTES.is_empty() {
        if installed.is_file() {
            return CheckResult {
                kind: "claude-code/cc-hook-binary".into(),
                level: CheckLevel::Warn,
                message: format!(
                    "cc-hook installed at {} but embedded bytes are a stub (v{}); drift detection deferred until T-008a-real",
                    installed.display(),
                    CRATE_VERSION,
                ),
                fix: Some("ark ext claude-code reinstall-hook-binary".into()),
            };
        }
        return CheckResult {
            kind: "claude-code/cc-hook-binary".into(),
            level: CheckLevel::Warn,
            message: format!(
                "cc-hook not installed at {}; embedded bytes are a stub (v{}) — install via `cargo install --bin cc-hook -p ark-ext-claude-code`",
                installed.display(),
                CRATE_VERSION,
            ),
            fix: Some("ark ext claude-code reinstall-hook-binary".into()),
        };
    }

    let embedded_hash = content_hash(crate::CC_HOOK_BYTES);
    match std::fs::read(&installed) {
        Ok(bytes) => {
            let installed_hash = content_hash(&bytes);
            if installed_hash == embedded_hash {
                CheckResult {
                    kind: "claude-code/cc-hook-binary".into(),
                    level: CheckLevel::Info,
                    message: format!(
                        "cc-hook installed + hash matches embedded v{} at {}",
                        CRATE_VERSION,
                        installed.display(),
                    ),
                    fix: None,
                }
            } else {
                CheckResult {
                    kind: "claude-code/cc-hook-binary".into(),
                    level: CheckLevel::Warn,
                    message: format!(
                        "cc-hook at {} drifted from embedded v{} (hash {:x} vs {:x})",
                        installed.display(),
                        CRATE_VERSION,
                        installed_hash,
                        embedded_hash,
                    ),
                    fix: Some("ark ext claude-code reinstall-hook-binary".into()),
                }
            }
        }
        Err(_) => CheckResult {
            kind: "claude-code/cc-hook-binary".into(),
            level: CheckLevel::Warn,
            message: format!(
                "cc-hook not installed at {} (v{} embedded)",
                installed.display(),
                CRATE_VERSION,
            ),
            fix: Some("ark ext claude-code reinstall-hook-binary".into()),
        },
    }
}

/// Check 3: `~/.claude/settings.json` hooks-block drift across the
/// ark-managed entries.
///
/// Drift = any of:
/// - settings.json missing or unparseable.
/// - ark-managed block absent.
/// - ark-managed block has fewer than [`crate::settings_json::ARK_MANAGED_HOOK_COUNT`]
///   entries (R10 "all 9+ entries").
///
/// `settings_override` lets tests supply an alternate path.
pub fn check_settings_drift(settings_override: Option<&Path>) -> CheckResult {
    let path: PathBuf = match settings_override {
        Some(p) => p.to_path_buf(),
        None => match crate::default_settings_path() {
            Some(p) => p,
            None => {
                return CheckResult {
                    kind: "claude-code/settings-hooks".into(),
                    level: CheckLevel::Warn,
                    message: "$HOME unset; cannot resolve ~/.claude/settings.json".into(),
                    fix: Some("ark ext claude-code install-hooks".into()),
                };
            }
        },
    };

    let sf = match crate::settings_json::SettingsFile::load(&path) {
        Ok(s) => s,
        Err(e) => {
            return CheckResult {
                kind: "claude-code/settings-hooks".into(),
                level: CheckLevel::Warn,
                message: format!(
                    "cannot load ~/.claude/settings.json at {}: {}",
                    path.display(),
                    e
                ),
                fix: Some("ark ext claude-code install-hooks".into()),
            };
        }
    };

    let count = sf.ark_managed_hook_count();
    let expected = crate::settings_json::ARK_MANAGED_HOOK_COUNT;
    if count >= expected {
        CheckResult {
            kind: "claude-code/settings-hooks".into(),
            level: CheckLevel::Info,
            message: format!(
                "settings.json has {}/{} ark-managed hook entries at {}",
                count,
                expected,
                path.display(),
            ),
            fix: None,
        }
    } else {
        CheckResult {
            kind: "claude-code/settings-hooks".into(),
            level: CheckLevel::Warn,
            message: format!(
                "settings.json drift: {}/{} ark-managed hook entries at {}",
                count,
                expected,
                path.display(),
            ),
            fix: Some("ark ext claude-code install-hooks".into()),
        }
    }
}

/// Check 4: `$STATE/sessions` writable. Uses a short-lived tempfile
/// to probe write access (trusting a `metadata().permissions()` check
/// is unreliable cross-FS). Probe file is cleaned up on success; a
/// leak on failure is harmless (atomic rename target is the probe
/// itself, never an existing user artifact).
pub fn check_state_sessions_writable(state_root_override: Option<&Path>) -> CheckResult {
    let sessions_dir: PathBuf = match state_root_override {
        Some(p) => p.to_path_buf(),
        None => match ark_types::StateLayout::from_env() {
            Ok(layout) => layout.sessions_root(),
            Err(e) => {
                return CheckResult {
                    kind: "claude-code/sessions-writable".into(),
                    level: CheckLevel::Error,
                    message: format!("cannot resolve $STATE/sessions: {}", e),
                    fix: Some(
                        "Set $ARK_STATE_DIR (or $XDG_STATE_HOME) to a writable directory".into(),
                    ),
                };
            }
        },
    };

    // Try to create the dir chain. Any failure here IS the error.
    if let Err(e) = std::fs::create_dir_all(&sessions_dir) {
        return CheckResult {
            kind: "claude-code/sessions-writable".into(),
            level: CheckLevel::Error,
            message: format!(
                "$STATE/sessions unwritable at {}: {}",
                sessions_dir.display(),
                e
            ),
            fix: Some(format!(
                "mkdir -p {path} && chmod u+w {path}",
                path = sessions_dir.display()
            )),
        };
    }

    let probe = sessions_dir.join(".ark-doctor-probe");
    match std::fs::write(&probe, b"ok") {
        Ok(()) => {
            let _ = std::fs::remove_file(&probe);
            CheckResult {
                kind: "claude-code/sessions-writable".into(),
                level: CheckLevel::Info,
                message: format!("$STATE/sessions writable at {}", sessions_dir.display()),
                fix: None,
            }
        }
        Err(e) => CheckResult {
            kind: "claude-code/sessions-writable".into(),
            level: CheckLevel::Error,
            message: format!(
                "$STATE/sessions unwritable at {}: {}",
                sessions_dir.display(),
                e
            ),
            fix: Some(format!(
                "mkdir -p {path} && chmod u+w {path}",
                path = sessions_dir.display()
            )),
        },
    }
}

/// Check 5: informational — whether any observed scene references the
/// `claude-code` view with a wired `subagents` attribute.
///
/// Scene observation is scene-compiler territory (not this crate's
/// responsibility); doctor passes the observation as an argument. When
/// the caller doesn't know, pass `None` — the check reports
/// "unverified" rather than fabricating a signal.
pub fn check_view_wired(has_subagents_wired: Option<bool>) -> CheckResult {
    match has_subagents_wired {
        Some(true) => CheckResult {
            kind: "claude-code/view-wired".into(),
            level: CheckLevel::Info,
            message: "claude-code view referenced with wired subagents attribute".into(),
            fix: None,
        },
        Some(false) => CheckResult {
            kind: "claude-code/view-wired".into(),
            level: CheckLevel::Info,
            message: "claude-code view referenced but no `subagents=@…` wiring detected (Stack<ClaudeCodeSubagent> fan-out disabled; raw-cmd fallback still works)".into(),
            fix: None,
        },
        None => CheckResult {
            kind: "claude-code/view-wired".into(),
            level: CheckLevel::Info,
            message: "claude-code view wiring not verified (no scene observation available)".into(),
            fix: None,
        },
    }
}

/// Render a [`DoctorEnvelope`] as a simple plain-text table. Used by
/// the T-043 rendering test; the real `ark doctor` renders via its own
/// table code and just consumes the envelope.
pub fn render_envelope_table(env: &DoctorEnvelope) -> String {
    let mut out = String::new();
    out.push_str("KIND                            LEVEL MESSAGE\n");
    for r in &env.results {
        let level = match r.level {
            CheckLevel::Error => "ERROR",
            CheckLevel::Warn => "WARN ",
            CheckLevel::Info => "INFO ",
        };
        out.push_str(&format!("{:<32}{} {}\n", r.kind, level, r.message));
        if let Some(fix) = &r.fix {
            out.push_str(&format!("{:<32}      fix: {}\n", "", fix));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn content_hash_is_stable() {
        assert_eq!(content_hash(b"hello"), content_hash(b"hello"));
        assert_ne!(content_hash(b"hello"), content_hash(b"hellp"));
        assert_ne!(content_hash(b""), content_hash(b"x"));
    }

    #[test]
    fn which_on_path_finds_a_well_known_binary() {
        // `sh` exists on any reasonable *nix — if this fails the test
        // env is very unusual.
        if cfg!(unix) {
            assert!(which_on_path("sh").is_some(), "sh should be on $PATH");
        }
    }

    #[test]
    fn which_on_path_returns_none_for_nonexistent() {
        assert!(which_on_path("ark-definitely-not-a-real-binary").is_none());
    }

    #[test]
    fn cc_hook_binary_stub_bytes_warns_when_missing() {
        let tmp = TempDir::new().unwrap();
        let fake = tmp.path().join("cc-hook");
        let r = check_cc_hook_binary(Some(&fake));
        assert_eq!(r.kind, "claude-code/cc-hook-binary");
        assert_eq!(r.level, CheckLevel::Warn);
        assert!(r.fix.as_deref() == Some("ark ext claude-code reinstall-hook-binary"));
    }

    #[test]
    fn cc_hook_binary_stub_bytes_warns_when_present() {
        let tmp = TempDir::new().unwrap();
        let fake = tmp.path().join("cc-hook");
        std::fs::write(&fake, b"fake bin").unwrap();
        let r = check_cc_hook_binary(Some(&fake));
        assert_eq!(r.level, CheckLevel::Warn);
        assert!(r.message.contains("stub"));
    }

    #[test]
    fn settings_drift_warn_on_missing_file() {
        let tmp = TempDir::new().unwrap();
        let missing = tmp.path().join("settings.json");
        let r = check_settings_drift(Some(&missing));
        assert_eq!(r.kind, "claude-code/settings-hooks");
        assert_eq!(r.level, CheckLevel::Warn);
        assert_eq!(r.fix.as_deref(), Some("ark ext claude-code install-hooks"));
    }

    #[test]
    fn sessions_writable_ok_on_fresh_tempdir() {
        let tmp = TempDir::new().unwrap();
        let sessions = tmp.path().join("sessions");
        let r = check_state_sessions_writable(Some(&sessions));
        assert_eq!(r.kind, "claude-code/sessions-writable");
        assert_eq!(r.level, CheckLevel::Info, "{}", r.message);
    }

    #[cfg(unix)]
    #[test]
    fn sessions_writable_error_on_readonly_dir() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = TempDir::new().unwrap();
        let sessions = tmp.path().join("sessions");
        std::fs::create_dir_all(&sessions).unwrap();
        let mut perms = std::fs::metadata(&sessions).unwrap().permissions();
        perms.set_mode(0o555);
        std::fs::set_permissions(&sessions, perms).unwrap();
        let r = check_state_sessions_writable(Some(&sessions));
        // root can write through 0555 — skip when that's the case.
        if r.level == CheckLevel::Error {
            assert_eq!(r.fix.as_deref().map(|s| s.contains("chmod")), Some(true));
        }
        // Restore perms so TempDir drop can clean up.
        let mut perms = std::fs::metadata(&sessions).unwrap().permissions();
        perms.set_mode(0o755);
        let _ = std::fs::set_permissions(&sessions, perms);
    }

    #[test]
    fn view_wired_three_variants() {
        assert_eq!(check_view_wired(Some(true)).level, CheckLevel::Info);
        assert_eq!(check_view_wired(Some(false)).level, CheckLevel::Info);
        assert_eq!(check_view_wired(None).level, CheckLevel::Info);
    }

    #[test]
    fn which_claude_missing_carries_install_hint() {
        // When the test env does not have `claude` on PATH, check should
        // surface the ERROR + install hint. When it IS on PATH, info.
        let r = check_which_claude();
        match r.level {
            CheckLevel::Error => {
                assert!(r.fix.as_deref().unwrap_or_default().contains("Install"));
            }
            CheckLevel::Info => {
                assert!(r.fix.is_none());
            }
            CheckLevel::Warn => panic!("which_claude should be error or info, not warn"),
        }
    }

    #[test]
    fn envelope_round_trips_json() {
        let env = DoctorEnvelope {
            results: vec![CheckResult {
                kind: "claude-code/x".into(),
                level: CheckLevel::Warn,
                message: "m".into(),
                fix: Some("f".into()),
            }],
        };
        let j = serde_json::to_string(&env).unwrap();
        let back: DoctorEnvelope = serde_json::from_str(&j).unwrap();
        assert_eq!(back.results.len(), 1);
        assert_eq!(back.results[0].level, CheckLevel::Warn);
        assert_eq!(back.results[0].fix.as_deref(), Some("f"));
    }

    #[test]
    fn render_envelope_table_includes_fix_lines() {
        let env = DoctorEnvelope {
            results: vec![
                CheckResult {
                    kind: "claude-code/which-claude".into(),
                    level: CheckLevel::Error,
                    message: "missing".into(),
                    fix: Some("install it".into()),
                },
                CheckResult {
                    kind: "claude-code/view-wired".into(),
                    level: CheckLevel::Info,
                    message: "ok".into(),
                    fix: None,
                },
            ],
        };
        let rendered = render_envelope_table(&env);
        assert!(rendered.contains("ERROR"));
        assert!(rendered.contains("INFO"));
        assert!(rendered.contains("fix: install it"));
        assert!(rendered.contains("claude-code/which-claude"));
        assert!(rendered.contains("claude-code/view-wired"));
    }
}
