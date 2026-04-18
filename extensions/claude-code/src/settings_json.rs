//! `~/.claude/settings.json` reader / surgical reconciler + cc-hook
//! binary installer — T-019 (claude-code-ext R1).
//!
//! Claude Code reads hook routing from `~/.claude/settings.json`. This
//! module owns the round-trip on that file:
//!
//! 1. **Load** the user's settings preserving every unknown key +
//!    every hook entry the user wrote by hand. We never rewrite
//!    anything we don't recognise as ark-managed.
//! 2. **Reconcile** just the ark-managed entries under `hooks.<Kind>`
//!    for the 10 Claude Code hook variants (see
//!    [`crate::hook_event::HookEvent::ALL`]). An entry is ark-managed
//!    iff it carries `"ark_managed": true` at the entry level.
//!    Reconciliation is idempotent — running twice produces a
//!    byte-identical file modulo whitespace.
//! 3. **Save** atomically via tmp + rename in the same directory
//!    (cross-device rename fails on some setups, so we stay intra-dir).
//!
//! The settings.json schema Claude Code honours at the time of writing:
//!
//! ```jsonc
//! {
//!   "hooks": {
//!     "SessionStart": [
//!       { "matcher": "*", "command": "/some/other/cmd" },       // user-owned
//!       { "matcher": "*", "command": ".../cc-hook ...",
//!         "ark_managed": true }                                  // ark-owned
//!     ],
//!     "PreToolUse": [ … ],
//!     // … other kinds …
//!   },
//!   "theme": "dark",             // unrelated top-level key, preserved
//!   // … anything else the user had, preserved verbatim.
//! }
//! ```
//!
//! ## cc-hook install path resolution
//!
//! Per decisions-doc R-13, the installed binary lives at
//! `$XDG_BIN_HOME/cc-hook` with fallback to `$HOME/.local/bin/cc-hook`.
//! Hand-rolled (no `xdg` crate in the workspace dep table) — resolution
//! follows the freedesktop spec variable chain:
//!
//! 1. `$XDG_BIN_HOME` if set + non-empty.
//! 2. `$HOME/.local/bin` if `$HOME` is set.
//! 3. Fall through to `./.local/bin/cc-hook` as a last-ditch relative
//!    path so tests in hermetic harnesses (no `$HOME`) still have a
//!    deterministic string to assert against — callers that reach that
//!    branch are expected to override via the explicit path arg.
//!
//! ## Atomic writes
//!
//! [`SettingsFile::save_atomic`] writes to `<target>.tmp` in the SAME
//! directory as the final path, then `fs::rename`s into place. Rename
//! within a directory is atomic on every POSIX filesystem we support.
//! Cross-device rename (`EXDEV`) fails on some tmpfs / overlayfs setups,
//! hence the same-directory rule.

use std::fs::{self, Permissions};
use std::io;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use serde_json::{Value, json};
use tracing::{debug, warn};

use crate::hook_event::HookEvent;

/// Marker key written at the entry level of every ark-managed hook
/// entry. Reconciliation only ever rewrites entries that carry this
/// key with a boolean `true` value; anything else is considered
/// user-authored and preserved verbatim.
pub const ARK_MANAGED_KEY: &str = "ark_managed";

/// Number of ark-managed hook entries a fully-reconciled
/// `~/.claude/settings.json` is expected to carry — one per
/// [`HookEvent::ALL`] kind. T-042 (doctor `settings-hooks` drift check)
/// reads this constant to decide whether the file is drift-free.
pub const ARK_MANAGED_HOOK_COUNT: usize = HookEvent::ALL.len();

/// Default filename for the Claude Code settings file, relative to
/// `$HOME`. Exposed for tests + doctor checks that need to assert the
/// canonical location.
pub const DEFAULT_SETTINGS_REL_PATH: &str = ".claude/settings.json";

/// Default basename of the embedded cc-hook binary once extracted.
pub const CC_HOOK_BIN_NAME: &str = "cc-hook";

/// Outcome of a [`SettingsFile::reconcile_ark_hooks`] call. Surfaced to
/// the caller so they can decide whether to save (only `Written`
/// requires a disk write), and to surface the `Unwritable` path to
/// doctor via the T-020 sentinel without aborting session start.
#[derive(Debug)]
pub enum ReconcileOutcome {
    /// Reconciliation produced a semantically-different tree. Caller
    /// should follow up with [`SettingsFile::save_atomic`]. Counts are
    /// informational — `added` is how many kinds had no ark-managed
    /// entry before, `modified` how many had a drifted entry replaced,
    /// `removed` how many stale ark-managed entries were evicted (e.g.
    /// a prior session pointed at a different socket path).
    Written {
        /// Hook kinds that gained a new ark-managed entry this run.
        added: usize,
        /// Hook kinds whose existing ark-managed entry was replaced
        /// because at least one tracked field (command, matcher)
        /// differed from the desired template.
        modified: usize,
        /// Extra ark-managed entries removed during the pass — e.g. a
        /// hand-edited settings.json that accidentally grew two
        /// ark-managed rows under the same kind.
        removed: usize,
    },
    /// Reconciliation left the tree byte-identical to what we loaded.
    /// No disk write necessary; tests assert this for the idempotent
    /// path.
    NoChange,
    /// Surfaced by [`SettingsFile::load`] when the path exists but
    /// can't be read, or by [`SettingsFile::save_atomic`] when the
    /// parent dir can't be written. Callers (kitchen: session start)
    /// treat this as a doctor warning, NOT a session-fatal error.
    Unwritable(io::Error),
}

impl PartialEq for ReconcileOutcome {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (
                ReconcileOutcome::Written {
                    added: a1,
                    modified: m1,
                    removed: r1,
                },
                ReconcileOutcome::Written {
                    added: a2,
                    modified: m2,
                    removed: r2,
                },
            ) => a1 == a2 && m1 == m2 && r1 == r2,
            (ReconcileOutcome::NoChange, ReconcileOutcome::NoChange) => true,
            // Compare `Unwritable` variants by kind + stringified message —
            // `io::Error` isn't `PartialEq` but tests only need to check
            // the failure class. We deliberately avoid leaking the
            // fmt::Debug impl into the public PartialEq surface.
            (ReconcileOutcome::Unwritable(e1), ReconcileOutcome::Unwritable(e2)) => {
                e1.kind() == e2.kind() && e1.to_string() == e2.to_string()
            }
            _ => false,
        }
    }
}

/// Errors surfaced by [`SettingsFile::load`] for paths we can't even
/// begin to reason about. Writable-but-empty paths flow through the
/// happy path (we synthesise an empty object); `NotFound` is fine.
#[derive(Debug, thiserror::Error)]
pub enum SettingsJsonError {
    /// Underlying IO failure reading the file (permission denied, path
    /// is a directory, etc.).
    #[error("read {path}: {source}")]
    Read {
        /// Path we tried to read.
        path: PathBuf,
        /// Wrapped IO error.
        #[source]
        source: io::Error,
    },
    /// File exists + is readable but content is not valid JSON.
    #[error("parse {path}: {source}")]
    Parse {
        /// Path whose JSON could not be decoded.
        path: PathBuf,
        /// Wrapped serde error.
        #[source]
        source: serde_json::Error,
    },
    /// IO failure during the atomic-write path.
    #[error("write {path}: {source}")]
    Write {
        /// Path we tried to write.
        path: PathBuf,
        /// Wrapped IO error.
        #[source]
        source: io::Error,
    },
}

/// Handle to a parsed settings.json. Constructed via
/// [`SettingsFile::load`]; mutate via [`SettingsFile::reconcile_ark_hooks`];
/// persist via [`SettingsFile::save_atomic`].
#[derive(Debug, Clone)]
pub struct SettingsFile {
    path: PathBuf,
    value: Value,
}

impl SettingsFile {
    /// Load the settings file from `path`. Missing files resolve to an
    /// empty JSON object (caller still has to call `save_atomic` to
    /// materialise). Non-missing IO errors surface as
    /// [`SettingsJsonError::Read`] so the caller can route to doctor.
    pub fn load(path: &Path) -> Result<Self, SettingsJsonError> {
        match fs::read(path) {
            Ok(bytes) if bytes.is_empty() => Ok(Self {
                path: path.to_path_buf(),
                value: Value::Object(Default::default()),
            }),
            Ok(bytes) => {
                let value: Value =
                    serde_json::from_slice(&bytes).map_err(|source| SettingsJsonError::Parse {
                        path: path.to_path_buf(),
                        source,
                    })?;
                Ok(Self {
                    path: path.to_path_buf(),
                    value,
                })
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(Self {
                path: path.to_path_buf(),
                value: Value::Object(Default::default()),
            }),
            Err(e) => Err(SettingsJsonError::Read {
                path: path.to_path_buf(),
                source: e,
            }),
        }
    }

    /// Absolute path this file round-trips through.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Borrow the parsed JSON tree (post-reconciliation if already
    /// reconciled). Exposed for tests that assert structural
    /// preservation of unknown keys.
    pub fn value(&self) -> &Value {
        &self.value
    }

    /// Count of ark-managed entries across every hook kind. Used by
    /// [`crate::doctor::check_settings_drift`] to decide whether the
    /// file is fully reconciled. An entry contributes to the count iff
    /// it carries `ARK_MANAGED_KEY = true` at the entry level.
    pub fn ark_managed_hook_count(&self) -> usize {
        let Some(hooks) = self.value.get("hooks").and_then(Value::as_object) else {
            return 0;
        };
        let mut total = 0usize;
        for kind in HookEvent::ALL {
            let wire = kind.as_str();
            if let Some(arr) = hooks.get(wire).and_then(Value::as_array) {
                for entry in arr {
                    if is_ark_managed(entry) {
                        total += 1;
                        // Kit pins ONE ark entry per kind; count each
                        // kind at most once for this drift check (extra
                        // ark entries are modeled as `removed` by
                        // reconcile, not drift for doctor purposes).
                        break;
                    }
                }
            }
        }
        total
    }

    /// Walk the 10 hook kinds, replacing any `ark_managed: true` entry
    /// under `hooks.<Kind>` with a fresh entry that points at the
    /// desired `<cc_hook_path> --session <sid> --socket <sock_path>
    /// --event <Kind>` template. Preserves every non-ark-managed entry
    /// in place. Preserves every other top-level key untouched.
    pub fn reconcile_ark_hooks(
        &mut self,
        session_id: &str,
        socket_path: &Path,
        cc_hook_path: &Path,
    ) -> ReconcileOutcome {
        // Ensure the top-level value is an object (empty if we
        // synthesised it at load time).
        if !self.value.is_object() {
            warn!(
                path = %self.path.display(),
                "settings.json: top-level is not an object; replacing with empty map"
            );
            self.value = Value::Object(Default::default());
        }

        // Snapshot pre-reconcile tree for NoChange detection. Cheaper
        // than walking every field afterward + more accurate (we catch
        // matching content that happens to serialise differently).
        let before = self.value.clone();

        let mut added = 0usize;
        let mut modified = 0usize;
        let mut removed = 0usize;

        // `hooks` object — materialise if absent.
        let hooks = self
            .value
            .as_object_mut()
            .expect("coerced above")
            .entry("hooks")
            .or_insert_with(|| Value::Object(Default::default()));
        if !hooks.is_object() {
            warn!("settings.json: `hooks` key was not an object; replacing");
            *hooks = Value::Object(Default::default());
        }
        let hooks_map = hooks.as_object_mut().expect("coerced above");

        for kind in HookEvent::ALL {
            let wire_name = kind.as_str();
            let desired = make_ark_entry(kind, session_id, socket_path, cc_hook_path);

            let entry_array = hooks_map
                .entry(wire_name.to_string())
                .or_insert_with(|| Value::Array(Vec::new()));
            if !entry_array.is_array() {
                warn!(
                    kind = wire_name,
                    "settings.json: hooks.<kind> was not an array; replacing"
                );
                *entry_array = Value::Array(Vec::new());
            }
            let arr = entry_array.as_array_mut().expect("coerced above");

            // Separate user-owned + ark-managed entries. Preserve order
            // of user-owned by index; append exactly one ark entry.
            let mut user_owned: Vec<Value> = Vec::with_capacity(arr.len());
            let mut ark_entries_seen: usize = 0;
            let mut ark_entry_matched_existing = false;
            for entry in arr.drain(..) {
                if is_ark_managed(&entry) {
                    ark_entries_seen += 1;
                    if ark_entries_seen == 1 && entry == desired {
                        ark_entry_matched_existing = true;
                    }
                } else {
                    user_owned.push(entry);
                }
            }

            // Accounting. `added` counts kinds that had NO ark entry
            // before; `modified` counts kinds where an ark entry
            // existed but didn't match; `removed` counts stale extras
            // past the first.
            match (ark_entries_seen, ark_entry_matched_existing) {
                (0, _) => added += 1,
                (1, true) => { /* match → no bump */ }
                (1, false) => modified += 1,
                (n, _) => {
                    modified += 1;
                    removed += n - 1;
                }
            }

            // Rebuild the array: user entries first (preserved order),
            // then exactly one ark entry.
            user_owned.push(desired);
            *arr = user_owned;
        }

        if self.value == before {
            ReconcileOutcome::NoChange
        } else {
            ReconcileOutcome::Written {
                added,
                modified,
                removed,
            }
        }
    }

    /// Atomic write: serialise with `to_string_pretty` (deterministic
    /// for a given `serde_json::Value`, so idempotency at the byte
    /// level is stable), write to `<path>.tmp` in the same directory,
    /// then rename into place.
    ///
    /// Returns `Ok(())` on success; all failures surface via
    /// [`SettingsJsonError::Write`] so callers can route to a doctor
    /// warning without aborting.
    pub fn save_atomic(&self) -> Result<(), SettingsJsonError> {
        // Create the parent dir best-effort; if the caller configured a
        // hermetic tempdir with a missing parent we'd rather they see
        // the create failure than the rename one.
        if let Some(parent) = self.path.parent() {
            if let Err(e) = fs::create_dir_all(parent) {
                return Err(SettingsJsonError::Write {
                    path: self.path.clone(),
                    source: e,
                });
            }
        }

        let bytes =
            serde_json::to_vec_pretty(&self.value).map_err(|e| SettingsJsonError::Write {
                path: self.path.clone(),
                source: io::Error::new(io::ErrorKind::InvalidData, e),
            })?;

        let mut tmp = self.path.clone();
        let mut name = tmp
            .file_name()
            .map(|s| s.to_os_string())
            .unwrap_or_else(|| std::ffi::OsString::from("settings.json"));
        name.push(".tmp");
        tmp.set_file_name(name);

        // Remove any stale tmp from a crashed prior writer; ignore
        // NotFound (the common case).
        if let Err(e) = fs::remove_file(&tmp) {
            if e.kind() != io::ErrorKind::NotFound {
                debug!(
                    path = %tmp.display(),
                    error = %e,
                    "settings.json: stale tmp remove failed; continuing"
                );
            }
        }

        fs::write(&tmp, &bytes).map_err(|e| SettingsJsonError::Write {
            path: self.path.clone(),
            source: e,
        })?;
        fs::rename(&tmp, &self.path).map_err(|e| SettingsJsonError::Write {
            path: self.path.clone(),
            source: e,
        })?;
        Ok(())
    }
}

/// Build the single ark-managed settings.json entry for a given hook
/// kind. Shape is pinned here rather than inline so the idempotency
/// guarantee (two runs produce byte-identical JSON) is visible in one
/// place.
fn make_ark_entry(
    kind: &HookEvent,
    session_id: &str,
    socket_path: &Path,
    cc_hook_path: &Path,
) -> Value {
    let command = format!(
        "{bin} --session {sid} --socket {sock} --event {ev}",
        bin = cc_hook_path.display(),
        sid = session_id,
        sock = socket_path.display(),
        ev = kind.as_str(),
    );
    json!({
        "matcher": "*",
        "command": command,
        "ark_managed": true,
    })
}

/// True iff the given JSON entry has `ark_managed: true` at the entry
/// level. User-authored entries that happen to share the marker key
/// with a non-boolean value are NOT treated as ark-managed (kept for
/// forward compat — we never rewrite what we don't recognise).
fn is_ark_managed(entry: &Value) -> bool {
    entry
        .as_object()
        .and_then(|m| m.get(ARK_MANAGED_KEY))
        .and_then(|v| v.as_bool())
        == Some(true)
}

/// Resolve `$XDG_BIN_HOME` with fallback to `$HOME/.local/bin`. Returns
/// the full path to the cc-hook binary (basename appended).
///
/// Precedence (hand-rolled per R-13; no `xdg` crate dependency):
///
/// 1. `$XDG_BIN_HOME` if set + non-empty.
/// 2. `$HOME/.local/bin` if `$HOME` is set.
/// 3. `./.local/bin/cc-hook` — last-ditch relative path for tests in
///    hermetic environments without `$HOME`; callers that hit this
///    branch are expected to override via [`install_cc_hook_at`].
pub fn cc_hook_install_path() -> PathBuf {
    if let Some(xdg_bin) = std::env::var_os("XDG_BIN_HOME") {
        if !xdg_bin.is_empty() {
            return PathBuf::from(xdg_bin).join(CC_HOOK_BIN_NAME);
        }
    }
    if let Some(home) = std::env::var_os("HOME") {
        if !home.is_empty() {
            return PathBuf::from(home)
                .join(".local/bin")
                .join(CC_HOOK_BIN_NAME);
        }
    }
    PathBuf::from(".local/bin").join(CC_HOOK_BIN_NAME)
}

/// Resolve `~/.claude/settings.json` for the current user. Returns
/// `None` if `$HOME` is unset (tests set it explicitly, so this is a
/// defensive fallback).
pub fn default_settings_path() -> Option<PathBuf> {
    std::env::var_os("HOME").map(|h| PathBuf::from(h).join(DEFAULT_SETTINGS_REL_PATH))
}

/// Outcome of [`install_cc_hook_at`] / [`install_cc_hook_default`].
/// Surfaced as-is to doctor + control-verb callers.
#[derive(Debug)]
pub enum InstallOutcome {
    /// Bytes written + chmod 0755 applied.
    Installed {
        /// Path the binary was written to.
        path: PathBuf,
        /// Count of bytes written (matches
        /// [`crate::CC_HOOK_BYTES`]`.len()`).
        bytes: usize,
    },
    /// [`crate::CC_HOOK_BYTES`] is the stub empty slice. Nothing was
    /// written; caller should surface the "install manually via
    /// `cargo build --release --bin cc-hook`" hint.
    StubEmpty {
        /// Path we would have written to.
        path: PathBuf,
    },
    /// IO failure writing the bytes or chmoding.
    Failed {
        /// Path we tried to write.
        path: PathBuf,
        /// Underlying IO error.
        error: io::Error,
    },
}

/// Extract [`crate::CC_HOOK_BYTES`] to `path`, chmod `0755`. Creates
/// the parent directory if missing. Safe to call repeatedly — atomic
/// via tmp-file-rename like [`SettingsFile::save_atomic`].
pub fn install_cc_hook_at(path: &Path) -> InstallOutcome {
    if crate::CC_HOOK_BYTES.is_empty() {
        warn!(
            path = %path.display(),
            "cc-hook embedding is stub (empty bytes). Build via \
             `cargo build --release -p ark-ext-claude-code --bin cc-hook` \
             and install manually, or wire real embedding (see T-008a TODO)."
        );
        return InstallOutcome::StubEmpty {
            path: path.to_path_buf(),
        };
    }

    if let Some(parent) = path.parent() {
        if let Err(e) = fs::create_dir_all(parent) {
            return InstallOutcome::Failed {
                path: path.to_path_buf(),
                error: e,
            };
        }
    }

    let mut tmp = path.to_path_buf();
    let mut name = tmp
        .file_name()
        .map(|s| s.to_os_string())
        .unwrap_or_else(|| std::ffi::OsString::from(CC_HOOK_BIN_NAME));
    name.push(".tmp");
    tmp.set_file_name(name);

    if let Err(e) = fs::write(&tmp, crate::CC_HOOK_BYTES) {
        return InstallOutcome::Failed {
            path: path.to_path_buf(),
            error: e,
        };
    }
    if let Err(e) = fs::set_permissions(&tmp, Permissions::from_mode(0o755)) {
        let _ = fs::remove_file(&tmp);
        return InstallOutcome::Failed {
            path: path.to_path_buf(),
            error: e,
        };
    }
    if let Err(e) = fs::rename(&tmp, path) {
        let _ = fs::remove_file(&tmp);
        return InstallOutcome::Failed {
            path: path.to_path_buf(),
            error: e,
        };
    }
    InstallOutcome::Installed {
        path: path.to_path_buf(),
        bytes: crate::CC_HOOK_BYTES.len(),
    }
}

/// Install `cc-hook` at [`cc_hook_install_path`].
pub fn install_cc_hook_default() -> InstallOutcome {
    install_cc_hook_at(&cc_hook_install_path())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn sock() -> PathBuf {
        PathBuf::from("/tmp/ark/sessions/sess-1/cc-hook.sock")
    }
    fn bin() -> PathBuf {
        PathBuf::from("/home/u/.local/bin/cc-hook")
    }

    #[test]
    fn load_missing_file_yields_empty_object() {
        let td = TempDir::new().unwrap();
        let p = td.path().join("settings.json");
        let sf = SettingsFile::load(&p).expect("load missing");
        assert_eq!(sf.value(), &json!({}));
        assert_eq!(sf.path(), p);
    }

    #[test]
    fn load_empty_file_yields_empty_object() {
        let td = TempDir::new().unwrap();
        let p = td.path().join("settings.json");
        fs::write(&p, b"").unwrap();
        let sf = SettingsFile::load(&p).expect("load empty");
        assert_eq!(sf.value(), &json!({}));
    }

    #[test]
    fn load_malformed_json_errors() {
        let td = TempDir::new().unwrap();
        let p = td.path().join("settings.json");
        fs::write(&p, b"{not json").unwrap();
        let err = SettingsFile::load(&p).expect_err("parse error");
        assert!(matches!(err, SettingsJsonError::Parse { .. }));
    }

    #[test]
    fn reconcile_from_empty_adds_all_10_kinds() {
        let td = TempDir::new().unwrap();
        let p = td.path().join("settings.json");
        let mut sf = SettingsFile::load(&p).unwrap();
        let outcome = sf.reconcile_ark_hooks("sess-1", &sock(), &bin());
        match outcome {
            ReconcileOutcome::Written {
                added,
                modified,
                removed,
            } => {
                assert_eq!(added, 10);
                assert_eq!(modified, 0);
                assert_eq!(removed, 0);
            }
            o => panic!("unexpected: {o:?}"),
        }

        // Every one of the 10 kinds gets exactly one ark entry.
        let hooks = sf.value().get("hooks").unwrap().as_object().unwrap();
        for kind in HookEvent::ALL {
            let arr = hooks.get(kind.as_str()).unwrap().as_array().unwrap();
            assert_eq!(arr.len(), 1, "one entry for {}", kind.as_str());
            assert_eq!(arr[0].get("ark_managed"), Some(&json!(true)));
            let cmd = arr[0].get("command").and_then(|v| v.as_str()).unwrap();
            assert!(cmd.contains("--event"));
            assert!(cmd.contains(kind.as_str()));
            assert!(cmd.contains("/cc-hook"));
            assert!(cmd.contains("sess-1"));
        }
    }

    #[test]
    fn reconcile_is_idempotent() {
        let td = TempDir::new().unwrap();
        let p = td.path().join("settings.json");
        let mut sf = SettingsFile::load(&p).unwrap();

        let first = sf.reconcile_ark_hooks("sess-1", &sock(), &bin());
        assert!(matches!(first, ReconcileOutcome::Written { .. }));
        sf.save_atomic().unwrap();
        let bytes_first = fs::read(&p).unwrap();

        // Second pass on the saved file should be a pure no-op.
        let mut sf2 = SettingsFile::load(&p).unwrap();
        let second = sf2.reconcile_ark_hooks("sess-1", &sock(), &bin());
        assert_eq!(second, ReconcileOutcome::NoChange);

        // Saving the "no-change" tree still yields byte-identical JSON.
        sf2.save_atomic().unwrap();
        let bytes_second = fs::read(&p).unwrap();
        assert_eq!(bytes_first, bytes_second);
    }

    #[test]
    fn reconcile_preserves_unknown_top_level_keys() {
        let td = TempDir::new().unwrap();
        let p = td.path().join("settings.json");
        fs::write(
            &p,
            serde_json::to_vec_pretty(&json!({
                "theme": "dark",
                "custom_commands": [{"name": "foo"}],
                "hooks": {
                    "SessionStart": [
                        {"matcher": "*", "command": "/user/custom/hook"}
                    ]
                }
            }))
            .unwrap(),
        )
        .unwrap();

        let mut sf = SettingsFile::load(&p).unwrap();
        let _ = sf.reconcile_ark_hooks("sess-1", &sock(), &bin());

        let v = sf.value();
        assert_eq!(v.get("theme"), Some(&json!("dark")));
        assert_eq!(v.get("custom_commands"), Some(&json!([{"name": "foo"}])));

        let session_start = v
            .get("hooks")
            .unwrap()
            .get("SessionStart")
            .unwrap()
            .as_array()
            .unwrap();
        assert_eq!(session_start.len(), 2, "user entry + ark entry");
        // User entry preserved verbatim at position 0.
        assert_eq!(
            session_start[0],
            json!({"matcher": "*", "command": "/user/custom/hook"})
        );
        // Ark entry at position 1.
        assert!(is_ark_managed(&session_start[1]));
    }

    #[test]
    fn reconcile_replaces_drifted_ark_entry() {
        let td = TempDir::new().unwrap();
        let p = td.path().join("settings.json");
        // Prior session wrote an entry with a stale socket path.
        fs::write(
            &p,
            serde_json::to_vec_pretty(&json!({
                "hooks": {
                    "Stop": [
                        {"matcher": "*",
                         "command": "/old/cc-hook --session oldsid --socket /stale --event Stop",
                         "ark_managed": true}
                    ]
                }
            }))
            .unwrap(),
        )
        .unwrap();

        let mut sf = SettingsFile::load(&p).unwrap();
        let outcome = sf.reconcile_ark_hooks("sess-1", &sock(), &bin());
        match outcome {
            ReconcileOutcome::Written {
                added,
                modified,
                removed,
            } => {
                // Stop = modified (was present + drifted); other 9 =
                // added. No duplicates → removed=0.
                assert_eq!(modified, 1);
                assert_eq!(added, 9);
                assert_eq!(removed, 0);
            }
            o => panic!("unexpected: {o:?}"),
        }

        let stop = sf
            .value()
            .get("hooks")
            .unwrap()
            .get("Stop")
            .unwrap()
            .as_array()
            .unwrap();
        assert_eq!(stop.len(), 1);
        let cmd = stop[0].get("command").unwrap().as_str().unwrap();
        assert!(cmd.contains("sess-1"));
        assert!(cmd.contains("/home/u/.local/bin/cc-hook"));
        assert!(!cmd.contains("oldsid"));
    }

    #[test]
    fn reconcile_collapses_duplicate_ark_entries() {
        let td = TempDir::new().unwrap();
        let p = td.path().join("settings.json");
        fs::write(
            &p,
            serde_json::to_vec_pretty(&json!({
                "hooks": {
                    "PostToolUse": [
                        {"matcher": "*", "command": "x", "ark_managed": true},
                        {"matcher": "*", "command": "y", "ark_managed": true},
                        {"matcher": "*", "command": "/user-cmd"}
                    ]
                }
            }))
            .unwrap(),
        )
        .unwrap();

        let mut sf = SettingsFile::load(&p).unwrap();
        let outcome = sf.reconcile_ark_hooks("sess-1", &sock(), &bin());
        match outcome {
            ReconcileOutcome::Written {
                added,
                modified,
                removed,
            } => {
                assert_eq!(modified, 1);
                assert_eq!(removed, 1, "second duplicate ark entry dropped");
                assert_eq!(added, 9);
            }
            o => panic!("unexpected: {o:?}"),
        }
        let arr = sf
            .value()
            .get("hooks")
            .unwrap()
            .get("PostToolUse")
            .unwrap()
            .as_array()
            .unwrap();
        assert_eq!(arr.len(), 2, "user entry + 1 ark entry");
        assert_eq!(arr[0].get("command"), Some(&json!("/user-cmd")));
        assert!(is_ark_managed(&arr[1]));
    }

    #[test]
    fn save_atomic_writes_and_tmp_is_removed() {
        let td = TempDir::new().unwrap();
        let p = td.path().join("settings.json");
        let mut sf = SettingsFile::load(&p).unwrap();
        let _ = sf.reconcile_ark_hooks("sess-1", &sock(), &bin());
        sf.save_atomic().expect("atomic write");
        assert!(p.exists());
        // tmp sibling should not linger.
        assert!(!td.path().join("settings.json.tmp").exists());
    }

    #[test]
    fn save_atomic_unwritable_dir_surfaces_error() {
        // A read-only parent directory: mkdir a dir, chmod 0o555.
        let td = TempDir::new().unwrap();
        let ro_dir = td.path().join("readonly");
        fs::create_dir(&ro_dir).unwrap();
        fs::set_permissions(&ro_dir, Permissions::from_mode(0o555)).unwrap();
        let p = ro_dir.join("settings.json");
        let mut sf = SettingsFile::load(&p).unwrap(); // path missing → empty object
        let _ = sf.reconcile_ark_hooks("sess-1", &sock(), &bin());
        let err = sf.save_atomic().expect_err("should fail writing to RO dir");
        assert!(matches!(err, SettingsJsonError::Write { .. }));
        // Restore perms so TempDir drop can clean up.
        fs::set_permissions(&ro_dir, Permissions::from_mode(0o755)).unwrap();
    }

    #[test]
    fn install_cc_hook_path_respects_xdg_bin_home() {
        // Local env override — only for this test. Use a scope-guarded
        // helper to avoid clobbering parallel tests.
        let prev_xdg = std::env::var_os("XDG_BIN_HOME");
        // SAFETY: env mutation in tests is the standard pattern; this
        // test does not run concurrently with itself and the outer
        // suite does not inspect XDG_BIN_HOME.
        unsafe {
            std::env::set_var("XDG_BIN_HOME", "/alt/bin");
        }
        assert_eq!(cc_hook_install_path(), PathBuf::from("/alt/bin/cc-hook"));
        unsafe {
            if let Some(v) = prev_xdg {
                std::env::set_var("XDG_BIN_HOME", v);
            } else {
                std::env::remove_var("XDG_BIN_HOME");
            }
        }
    }

    #[test]
    fn install_cc_hook_stub_empty_surfaces_outcome() {
        // With the T-008a stub in effect, CC_HOOK_BYTES is empty —
        // install_cc_hook_at MUST return StubEmpty + MUST NOT write.
        assert!(crate::CC_HOOK_BYTES.is_empty(), "stub invariant");
        let td = TempDir::new().unwrap();
        let p = td.path().join("sub/cc-hook");
        let outcome = install_cc_hook_at(&p);
        assert!(matches!(outcome, InstallOutcome::StubEmpty { .. }));
        assert!(!p.exists());
    }

    #[test]
    fn is_ark_managed_requires_bool_true() {
        assert!(is_ark_managed(&json!({"ark_managed": true})));
        assert!(!is_ark_managed(&json!({"ark_managed": false})));
        assert!(!is_ark_managed(&json!({"ark_managed": "true"})));
        assert!(!is_ark_managed(&json!({"ark_managed": 1})));
        assert!(!is_ark_managed(&json!({})));
        assert!(!is_ark_managed(&json!(null)));
    }
}
