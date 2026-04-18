//! T-020 / T-021 / T-024 regression suite — `on_session_start`
//! behaviour around `~/.claude/settings.json` reconciliation.
//!
//! The tests drive the lower-level [`reconcile_settings_for_session`]
//! helper (crate-pub) instead of the full `on_session_start` path — the
//! helper is what the session-start hook delegates to, and driving it
//! directly lets us assert structural outcomes without spinning a
//! tokio runtime + synthesising a SessionSpec JSON blob for every
//! case.
//!
//! - **T-020** (reconcile-on-session-start happy path): `reconcile_settings_for_session`
//!   writes the 10 hook entries.
//! - **T-021** (unwritable settings.json): the helper does NOT panic,
//!   writes the sentinel under `session_dir`, and leaves the session
//!   startable.
//! - **T-024** (scene-without-`use "claude-code"`): `on_session_start`
//!   short-circuits before calling the reconciler AND before binding
//!   the socket.
//!
//! The full end-to-end on_session_start path is also covered by one
//! direct test that constructs a minimal SessionSpec JSON.

use std::fs::{self, Permissions};
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;

use ark_ext_claude_code::{
    ClaudeCodeExtension, HookEvent, InstallOutcome, ReconcileOutcome, SETTINGS_UNWRITABLE_SENTINEL,
    SettingsFile, SettingsUnwritable,
};
use ark_ext_proto::{ArkExtension, OnSessionStartRequest};
use ark_types::SessionId;
use tempfile::TempDir;

fn short_tempdir() -> TempDir {
    // Keep paths under `/tmp` to dodge macOS SUN_LEN limits on the
    // socket the on_session_start path binds.
    tempfile::Builder::new()
        .prefix("arkcc-")
        .tempdir_in("/tmp")
        .expect("tempdir in /tmp")
}

/// T-020: direct driver of `reconcile_settings_for_session` — confirms
/// a fresh settings.json gains the 10 ark-managed entries, and a
/// re-invocation with identical args is a byte-level no-op.
#[test]
fn reconcile_writes_all_ten_kinds_and_is_idempotent() {
    let td = TempDir::new().unwrap();
    let session_dir = td.path().join("sess");
    fs::create_dir_all(&session_dir).unwrap();
    let settings_path = td.path().join("settings.json");
    let cc_hook = PathBuf::from("/usr/local/bin/cc-hook");
    let sock = session_dir.join("cc-hook.sock");
    let sid = SessionId::new("s");

    ark_ext_claude_code::reconcile_settings_for_session(
        &sid,
        &sock,
        &session_dir,
        Some(&settings_path),
        Some(&cc_hook),
    );

    // Settings landed on disk with the expected shape.
    let sf = SettingsFile::load(&settings_path).unwrap();
    let hooks = sf.value().get("hooks").unwrap().as_object().unwrap();
    for kind in HookEvent::ALL {
        let arr = hooks.get(kind.as_str()).unwrap().as_array().unwrap();
        assert_eq!(arr.len(), 1);
        let ark = &arr[0];
        assert_eq!(ark.get("ark_managed").and_then(|v| v.as_bool()), Some(true));
    }
    let bytes_first = fs::read(&settings_path).unwrap();

    // Second call with identical args is a no-op at the byte level.
    ark_ext_claude_code::reconcile_settings_for_session(
        &sid,
        &sock,
        &session_dir,
        Some(&settings_path),
        Some(&cc_hook),
    );
    let bytes_second = fs::read(&settings_path).unwrap();
    assert_eq!(bytes_first, bytes_second);

    // No sentinel written on the happy path.
    assert!(!session_dir.join(SETTINGS_UNWRITABLE_SENTINEL).exists());
}

/// T-021: the settings.json path is unwritable — `on_session_start`
/// must NOT fail, instead dropping a sentinel under the session dir so
/// doctor can surface the warning later.
#[test]
fn unwritable_settings_path_writes_sentinel_and_session_proceeds() {
    let td = TempDir::new().unwrap();
    let session_dir = td.path().join("sess");
    fs::create_dir_all(&session_dir).unwrap();

    // Build a read-only directory to force a write failure.
    let ro = td.path().join("readonly");
    fs::create_dir(&ro).unwrap();
    fs::set_permissions(&ro, Permissions::from_mode(0o555)).unwrap();
    let settings_path = ro.join("settings.json");

    let cc_hook = PathBuf::from("/usr/local/bin/cc-hook");
    let sock = session_dir.join("cc-hook.sock");
    let sid = SessionId::new("s");

    // Helper must not panic.
    ark_ext_claude_code::reconcile_settings_for_session(
        &sid,
        &sock,
        &session_dir,
        Some(&settings_path),
        Some(&cc_hook),
    );

    // Sentinel landed with expected shape.
    let sentinel = session_dir.join(SETTINGS_UNWRITABLE_SENTINEL);
    assert!(sentinel.exists(), "sentinel must exist on unwritable path");
    let parsed: SettingsUnwritable = serde_json::from_slice(&fs::read(&sentinel).unwrap()).unwrap();
    assert_eq!(parsed.path, settings_path);
    assert_eq!(parsed.code, "claude-code/settings-write");
    assert!(!parsed.message.is_empty());
    assert!(!parsed.first_seen_at.is_empty());

    // Restore perms so TempDir can clean up.
    fs::set_permissions(&ro, Permissions::from_mode(0o755)).unwrap();
}

/// T-024: scene does NOT declare `use "claude-code"` → extension
/// leaves settings.json alone AND does not bind the socket.
#[tokio::test]
async fn scene_without_use_claude_code_does_not_touch_settings() {
    let td = short_tempdir();

    // A SessionSpec without "claude-code" in ext_config.
    use std::collections::BTreeMap;
    let spec = ark_types::SessionSpec {
        id: SessionId::new("s"),
        name: "s".into(),
        scene_path: None,
        cwd: td.path().to_path_buf(),
        env: BTreeMap::new(),
        created_at: chrono::Utc::now(),
        ext_config: BTreeMap::new(), // intentionally empty
    };
    let spec_json = serde_json::to_string(&spec).unwrap();

    // Point HOME at our tempdir so a real reconciliation path, if it
    // ran, would hit ~/.claude/settings.json inside the tempdir.
    let prev_home = std::env::var_os("HOME");
    // SAFETY: tests single-threaded per-file default is not guaranteed
    // with integration tests, so we restore on exit.
    unsafe {
        std::env::set_var("HOME", td.path());
    }

    // Clear ARK_STATE_DIR too so StateLayout picks up our HOME
    // sandbox rather than a stale global.
    let prev_state = std::env::var_os("ARK_STATE_DIR");
    unsafe {
        std::env::remove_var("ARK_STATE_DIR");
    }
    let prev_rt = std::env::var_os("ARK_RUNTIME_DIR");
    unsafe {
        std::env::remove_var("ARK_RUNTIME_DIR");
    }

    let ext = ClaudeCodeExtension::new();
    let resp = ext
        .on_session_start(OnSessionStartRequest { spec: spec_json })
        .await;
    assert!(resp.is_ok(), "on_session_start must succeed without use");

    // ~/.claude/settings.json must NOT exist — the extension
    // short-circuited before any reconciliation.
    let settings = td.path().join(".claude/settings.json");
    assert!(
        !settings.exists(),
        "settings.json must not be created for scenes without `use \"claude-code\"`"
    );

    // Restore env.
    unsafe {
        if let Some(v) = prev_home {
            std::env::set_var("HOME", v);
        } else {
            std::env::remove_var("HOME");
        }
        if let Some(v) = prev_state {
            std::env::set_var("ARK_STATE_DIR", v);
        }
        if let Some(v) = prev_rt {
            std::env::set_var("ARK_RUNTIME_DIR", v);
        }
    }
}

/// T-022 handler: the `install-hooks` verb writes the 10 hook entries
/// via explicit overrides (so the test never touches a real $HOME).
#[test]
fn install_hooks_verb_reconciles_to_override_path() {
    let td = TempDir::new().unwrap();
    let settings = td.path().join("settings.json");
    let cc_hook = PathBuf::from("/usr/local/bin/cc-hook");

    let ext = ClaudeCodeExtension::new();
    let outcome = ext
        .run_install_hooks_verb(Some(&settings), Some(&cc_hook))
        .expect("verb ok");
    assert!(matches!(outcome, ReconcileOutcome::Written { .. }));
    assert!(settings.exists());

    let sf = SettingsFile::load(&settings).unwrap();
    let hooks = sf.value().get("hooks").unwrap().as_object().unwrap();
    assert_eq!(hooks.len(), 10);
    for kind in HookEvent::ALL {
        let arr = hooks.get(kind.as_str()).unwrap().as_array().unwrap();
        assert_eq!(arr.len(), 1);
        let cmd = arr[0].get("command").and_then(|v| v.as_str()).unwrap();
        assert!(cmd.contains("__latest__"));
        assert!(cmd.contains("/usr/local/bin/cc-hook"));
    }
}

/// T-023 handler: `reinstall-hook-binary` surfaces StubEmpty while
/// `CC_HOOK_BYTES` remains the T-008a placeholder. When that stub is
/// replaced with real bytes, extending this test with an "installed"
/// branch is a one-line swap.
#[test]
fn reinstall_hook_binary_verb_stub_empty_when_bytes_placeholder() {
    assert!(
        ark_ext_claude_code::CC_HOOK_BYTES.is_empty(),
        "stub invariant for this test"
    );

    let td = TempDir::new().unwrap();
    let target = td.path().join("bin/cc-hook");
    let ext = ClaudeCodeExtension::new();
    let outcome = ext.run_reinstall_hook_binary_verb(Some(&target));
    assert!(matches!(outcome, InstallOutcome::StubEmpty { .. }));
    assert!(!target.exists());
}

/// T-022/T-023 advertisement: the `control_verbs` method surfaces both
/// verbs in its opaque-JSON payload.
#[tokio::test]
async fn control_verbs_advertises_install_and_reinstall() {
    let ext = ClaudeCodeExtension::new();
    let resp = ext
        .control_verbs(Default::default())
        .await
        .expect("control_verbs");
    let parsed: serde_json::Value = serde_json::from_str(&resp.verbs).unwrap();
    let verbs = parsed.get("verbs").unwrap().as_array().unwrap();
    assert_eq!(verbs.len(), 2);
    let names: Vec<&str> = verbs
        .iter()
        .filter_map(|v| v.get("name").and_then(|n| n.as_str()))
        .collect();
    assert!(names.contains(&"install-hooks"));
    assert!(names.contains(&"reinstall-hook-binary"));
}
