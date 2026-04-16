//! Publisher-trust storage + audit log for `ark ext add`.
//!
//! T-13.1, T-13.2 (cavekit-scene R10, v0.5 milestone) shipped
//! publisher trust; T-13.4 (v0.4 declared-caps milestone) layers
//! install-time capability disclosure on top. Trust is an
//! *install-time* check — not a runtime capability gate. The VSCode
//! 1.97 workspace-trust dialog is the direct analog for publisher
//! trust; the Chrome/Firefox extension install prompt ("this
//! extension requests: tabs, storage, network") is the direct analog
//! for capability disclosure.
//!
//! The first time a user installs from a given publisher (github
//! user, url host, local path root), we prompt "Trust this
//! publisher? [y/n]". Yes gets remembered in
//! `${XDG_CONFIG_HOME}/ark/extension-trust.kdl`; subsequent installs
//! from the same publisher skip the prompt. Immediately after, if
//! the extension's declared `capabilities` list is non-empty and
//! contains at least one cap that isn't already trusted for this
//! `<name>@<version>`, we print "Extension … requests capabilities:
//! <csv>" and prompt "Accept these capabilities? [y/n]". Accept
//! persists one `capability "<cap>" extension="<name>@<version>"`
//! node per declared cap.
//!
//! `--accept-all` (T-13.2, extended by T-13.4) is the CI escape
//! hatch — skips both prompts and appends warning lines to
//! `${XDG_DATA_HOME}/ark/extension-audit.log` so an auditor can see
//! every non-interactive install after the fact.
//!
//! # Trust file shape
//!
//! Minimal KDL. Two node types:
//!
//! * `publisher "<key>"` — one per trusted publisher, value is the
//!   publisher's canonical string form.
//! * `capability "<cap>" extension="<name>@<version>"` — one per
//!   trusted capability for a specific `<name>@<version>` pair.
//!
//! ```kdl
//! publisher "github:rlch"
//! publisher "url:example.com"
//! publisher "path:/Users/me/ext-root"
//! capability "exec" extension="picker@0.1.0"
//! capability "pipe" extension="picker@0.1.0"
//! ```
//!
//! Unknown nodes are preserved on re-write (we parse + append rather
//! than reserialize). Rewrite-safety keeps future node types
//! (e.g. per-version cap trust from T-13.5) backward-compatible when
//! older `ark` binaries encounter them.
//!
//! # Audit log shape
//!
//! Line-oriented text — one entry per `--accept-all` install. T-13.2
//! writes `accept-all`; T-13.4 adds a parallel `accept-all-caps`
//! entry when the ext had a non-empty cap list:
//!
//! ```text
//! 2026-04-18T12:34:56Z accept-all publisher=github:rlch source=github:rlch/ark-picker@v0.1
//! 2026-04-18T12:34:56Z accept-all-caps extension=picker@0.1.0 caps=exec,pipe
//! ```
//!
//! Text (not KDL/JSON) because the file is append-only forensics —
//! auditors usually `grep`/`awk` over it, and a rotated log shouldn't
//! need a parser to stay legible.

use std::collections::HashSet;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use super::add::Source;

/// Canonical publisher derived from a [`Source`]. The string form
/// (`github:<user>`, `url:<host>`, `path:<abs>`) is the key written
/// into the trust file.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Publisher {
    /// `github:<user>` — one trust per GitHub account, regardless of
    /// how many of that user's repos get installed.
    GitHub(String),
    /// `url:<host>` — one trust per hostname. `url:https://foo.com/a`
    /// and `url:https://foo.com/b` share a trust entry.
    UrlHost(String),
    /// `path:<canonicalized-or-raw-path>` — local extensions. The
    /// full path is the key so a user's `~/my-ext` doesn't inherit
    /// trust from some shared `/opt/extensions/other` dir.
    LocalPath(String),
}

impl Publisher {
    /// Canonical string form used as the trust-file key.
    pub fn as_key(&self) -> String {
        match self {
            Publisher::GitHub(u) => format!("github:{u}"),
            Publisher::UrlHost(h) => format!("url:{h}"),
            Publisher::LocalPath(p) => format!("path:{p}"),
        }
    }

    /// Human-readable label for the trust prompt.
    pub fn display(&self) -> String {
        self.as_key()
    }
}

/// Derive a [`Publisher`] from a parsed [`Source`].
///
/// Deterministic and total — every [`Source`] maps to exactly one
/// publisher. Callers do not need to handle an error case.
pub fn derive_publisher(source: &Source) -> Publisher {
    match source {
        Source::Github { slug, .. } => {
            // `user/repo` → user. Slug is validated non-empty in
            // add::parse_source so `split_once` always has a head.
            let user = slug.split('/').next().unwrap_or("").to_string();
            Publisher::GitHub(user)
        }
        Source::Url(u) => Publisher::UrlHost(extract_host(u)),
        Source::Path(p) => Publisher::LocalPath(p.display().to_string()),
    }
}

/// Best-effort host extraction from a `http(s)://…` URL without
/// dragging in the `url` crate. Falls back to the full string if the
/// shape is unexpected — callers treat any failure as "no trust
/// entry yet", which is the safe path.
fn extract_host(url: &str) -> String {
    let rest = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))
        .unwrap_or(url);
    let host_and_rest = rest.split('/').next().unwrap_or(rest);
    // Strip optional `user:pass@` + optional `:port`.
    let after_userinfo = host_and_rest
        .rsplit('@')
        .next()
        .unwrap_or(host_and_rest);
    after_userinfo
        .split(':')
        .next()
        .unwrap_or(after_userinfo)
        .to_string()
}

/// Path to the trust file: `${XDG_CONFIG_HOME}/ark/extension-trust.kdl`.
///
/// Mirrors `resolve_xdg_data_home` in `add.rs` — duplicated locally
/// so the trust module stays self-contained and testable without
/// dragging [`ark_types::StateLayout`] into every test that wants to
/// touch trust state.
pub fn trust_file_path() -> Result<PathBuf, String> {
    let base = resolve_xdg_config_home()?;
    Ok(base.join("ark/extension-trust.kdl"))
}

/// Path to the audit log: `${XDG_DATA_HOME}/ark/extension-audit.log`.
pub fn audit_log_path() -> Result<PathBuf, String> {
    let base = resolve_xdg_data_home()?;
    Ok(base.join("ark/extension-audit.log"))
}

fn resolve_xdg_config_home() -> Result<PathBuf, String> {
    if let Some(v) = std::env::var_os("XDG_CONFIG_HOME") {
        let p = PathBuf::from(v);
        if !p.as_os_str().is_empty() {
            return Ok(p);
        }
    }
    let home = std::env::var_os("HOME")
        .ok_or_else(|| "neither XDG_CONFIG_HOME nor HOME is set".to_string())?;
    Ok(PathBuf::from(home).join(".config"))
}

fn resolve_xdg_data_home() -> Result<PathBuf, String> {
    if let Some(v) = std::env::var_os("XDG_DATA_HOME") {
        let p = PathBuf::from(v);
        if !p.as_os_str().is_empty() {
            return Ok(p);
        }
    }
    let home = std::env::var_os("HOME")
        .ok_or_else(|| "neither XDG_DATA_HOME nor HOME is set".to_string())?;
    Ok(PathBuf::from(home).join(".local/share"))
}

/// Read the trust file into a set of canonical publisher keys.
///
/// * File absent → empty set (first-run case).
/// * Parse error → empty set with a `tracing::warn!` (better to
///   re-prompt than to lock the user out of installs behind a
///   corrupt trust file).
///
/// Only `publisher "<key>"` nodes are recognised; everything else is
/// preserved on write but ignored for membership checks.
pub fn load_trust_file() -> HashSet<String> {
    let path = match trust_file_path() {
        Ok(p) => p,
        Err(_) => return HashSet::new(),
    };
    load_trust_file_at(&path)
}

/// Test-friendly variant of [`load_trust_file`] that takes an
/// explicit path, so tests can point at a tempdir without mutating
/// process env.
pub fn load_trust_file_at(path: &Path) -> HashSet<String> {
    let text = match fs::read_to_string(path) {
        Ok(s) => s,
        Err(_) => return HashSet::new(),
    };
    parse_trust_keys(&text)
}

/// Parse every `publisher "<key>"` node from a KDL document, dropping
/// malformed entries silently.
fn parse_trust_keys(text: &str) -> HashSet<String> {
    let doc = match kdl::KdlDocument::parse(text) {
        Ok(d) => d,
        Err(_) => {
            tracing::warn!(
                "extension-trust.kdl parse error — treating as empty trust set"
            );
            return HashSet::new();
        }
    };
    let mut out = HashSet::new();
    for node in doc.nodes() {
        if node.name().to_string() != "publisher" {
            continue;
        }
        if let Some(entry) = node.entries().first() {
            if let Some(s) = entry.value().as_string() {
                out.insert(s.to_string());
            }
        }
    }
    out
}

/// Check whether `publisher` is in the on-disk trust file.
pub fn is_trusted(publisher: &Publisher) -> bool {
    load_trust_file().contains(&publisher.as_key())
}

/// Persist a publisher to the trust file. Creates the file (and its
/// parent directory) on first use; appends a fresh `publisher "<key>"`
/// node if not already present (idempotent).
pub fn save_trust(publisher: &Publisher) -> Result<(), String> {
    let path = trust_file_path()?;
    save_trust_at(&path, publisher)
}

/// Test-friendly variant of [`save_trust`].
pub fn save_trust_at(path: &Path, publisher: &Publisher) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|e| format!("creating {}: {e}", parent.display()))?;
    }
    let existing = fs::read_to_string(path).unwrap_or_default();
    let already = parse_trust_keys(&existing).contains(&publisher.as_key());
    if already {
        return Ok(());
    }

    let mut text = existing;
    if !text.is_empty() && !text.ends_with('\n') {
        text.push('\n');
    }
    text.push_str(&format!("publisher \"{}\"\n", publisher.as_key()));
    fs::write(path, text).map_err(|e| format!("writing {}: {e}", path.display()))
}

/// Append a single audit-log entry. Creates the log file (and its
/// parent directory) on first use. Always succeeds best-effort —
/// callers should log but not fail an install on audit-write errors.
pub fn append_audit(
    publisher: &Publisher,
    source_specifier: &str,
) -> Result<(), String> {
    let path = audit_log_path()?;
    append_audit_at(&path, publisher, source_specifier)
}

/// Test-friendly variant of [`append_audit`].
pub fn append_audit_at(
    path: &Path,
    publisher: &Publisher,
    source_specifier: &str,
) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|e| format!("creating {}: {e}", parent.display()))?;
    }
    let line = format!(
        "{} accept-all publisher={} source={}\n",
        chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        publisher.as_key(),
        source_specifier,
    );
    let mut f = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|e| format!("opening {}: {e}", path.display()))?;
    f.write_all(line.as_bytes())
        .map_err(|e| format!("writing {}: {e}", path.display()))
}

/// Prompt the user on stdin: `Trust this publisher? [y/n]`. Returns
/// true on a `y`/`yes` (case-insensitive), false on anything else
/// including EOF.
///
/// Lives behind a trait-free free function because `std::io::stdin`
/// is easy to stub in tests via [`prompt_from`] when callers need
/// to drive a fake reader.
pub fn prompt_trust(publisher: &Publisher) -> bool {
    let stdin = std::io::stdin();
    let mut lock = stdin.lock();
    prompt_from(publisher, &mut lock, &mut std::io::stdout())
}

/// Core prompt loop, parameterised over reader + writer for tests.
pub fn prompt_from<R: std::io::BufRead, W: std::io::Write>(
    publisher: &Publisher,
    reader: &mut R,
    writer: &mut W,
) -> bool {
    let _ = writeln!(
        writer,
        "ark: install source publisher is `{}`",
        publisher.display()
    );
    let _ = write!(writer, "Trust this publisher? [y/n]: ");
    let _ = writer.flush();
    let mut line = String::new();
    if reader.read_line(&mut line).is_err() {
        return false;
    }
    matches!(line.trim().to_ascii_lowercase().as_str(), "y" | "yes")
}

// ---------------------------------------------------------------------
// T-13.4: install-time capability disclosure.
//
// The publisher-trust path above decides "do we trust the source at
// all?". The capability path below decides "does the operator accept
// the specific permissions this ext's manifest declares?" — a
// Chrome-install-prompt analog (see the module doc comment). Trust is
// recorded per `<name>@<version>` so T-13.5 can re-prompt on a
// version bump that *adds* a cap while leaving previously-accepted
// caps alone.
// ---------------------------------------------------------------------

/// Build the canonical per-version extension key used to scope
/// capability trust. Format: `<name>@<version>`.
///
/// The concatenation matches the on-disk KDL property value so
/// callers can look up trust with the same key string they persist.
pub fn ext_version_key(name: &str, version: &str) -> String {
    format!("{name}@{version}")
}

/// Load the set of `(extension-key, capability)` pairs recorded in the
/// trust file. Missing file / parse errors degrade to the empty set,
/// mirroring [`load_trust_file`].
pub fn load_trusted_caps() -> std::collections::HashSet<(String, String)> {
    let path = match trust_file_path() {
        Ok(p) => p,
        Err(_) => return std::collections::HashSet::new(),
    };
    load_trusted_caps_at(&path)
}

/// Test-friendly variant of [`load_trusted_caps`].
pub fn load_trusted_caps_at(
    path: &Path,
) -> std::collections::HashSet<(String, String)> {
    let text = match fs::read_to_string(path) {
        Ok(s) => s,
        Err(_) => return std::collections::HashSet::new(),
    };
    parse_trusted_caps(&text)
}

/// Parse every `capability "<cap>" extension="<name>@<version>"` node
/// from a KDL trust document.
///
/// Malformed nodes (missing extension property, non-string capability
/// argument) are silently dropped, matching [`parse_trust_keys`]'s
/// defensive behaviour.
fn parse_trusted_caps(text: &str) -> std::collections::HashSet<(String, String)> {
    let doc = match kdl::KdlDocument::parse(text) {
        Ok(d) => d,
        Err(_) => {
            tracing::warn!(
                "extension-trust.kdl parse error — treating capability \
                 trust as empty"
            );
            return std::collections::HashSet::new();
        }
    };
    let mut out = std::collections::HashSet::new();
    for node in doc.nodes() {
        if node.name().to_string() != "capability" {
            continue;
        }
        // Argument: the capability name (`"exec"`).
        let Some(arg) = node.entries().iter().find(|e| e.name().is_none()) else {
            continue;
        };
        let Some(cap) = arg.value().as_string() else {
            continue;
        };
        // Property: `extension="<name>@<version>"`.
        let ext_key = node
            .entries()
            .iter()
            .find(|e| e.name().map(|n| n.to_string()) == Some("extension".into()))
            .and_then(|e| e.value().as_string().map(|s| s.to_string()));
        let Some(ext_key) = ext_key else {
            continue;
        };
        out.insert((ext_key, cap.to_string()));
    }
    out
}

/// Check whether a specific capability is already trusted for a
/// specific `<name>@<version>`.
pub fn is_cap_trusted(ext_key: &str, capability: &str) -> bool {
    load_trusted_caps().contains(&(ext_key.to_string(), capability.to_string()))
}

/// Persist every capability in `caps` as trusted for `ext_key`
/// (produced by [`ext_version_key`]).
///
/// Already-trusted entries are skipped so the file stays duplicate-
/// free. Appending preserves unrelated nodes (publisher entries,
/// future node types) verbatim — identical semantics to
/// [`save_trust_at`].
pub fn save_caps(ext_key: &str, caps: &[&str]) -> Result<(), String> {
    let path = trust_file_path()?;
    save_caps_at(&path, ext_key, caps)
}

/// Test-friendly variant of [`save_caps`].
pub fn save_caps_at(
    path: &Path,
    ext_key: &str,
    caps: &[&str],
) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|e| format!("creating {}: {e}", parent.display()))?;
    }
    let existing = fs::read_to_string(path).unwrap_or_default();
    let already = parse_trusted_caps(&existing);

    let mut text = existing;
    let mut appended = 0usize;
    for cap in caps {
        let key_pair = (ext_key.to_string(), (*cap).to_string());
        if already.contains(&key_pair) {
            continue;
        }
        if !text.is_empty() && !text.ends_with('\n') {
            text.push('\n');
        }
        text.push_str(&format!(
            "capability \"{cap}\" extension=\"{ext_key}\"\n"
        ));
        appended += 1;
    }
    if appended == 0 {
        return Ok(());
    }
    fs::write(path, text).map_err(|e| format!("writing {}: {e}", path.display()))
}

/// Append one audit-log line documenting an `--accept-all` capability
/// bypass. Paired with [`append_audit`] (publisher bypass); the
/// T-13.4 CI path emits both so the forensic trail covers both trust
/// dimensions.
pub fn append_caps_audit(ext_key: &str, caps: &[&str]) -> Result<(), String> {
    let path = audit_log_path()?;
    append_caps_audit_at(&path, ext_key, caps)
}

/// Test-friendly variant of [`append_caps_audit`].
pub fn append_caps_audit_at(
    path: &Path,
    ext_key: &str,
    caps: &[&str],
) -> Result<(), String> {
    if caps.is_empty() {
        return Ok(());
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|e| format!("creating {}: {e}", parent.display()))?;
    }
    let line = format!(
        "{} accept-all-caps extension={} caps={}\n",
        chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        ext_key,
        caps.join(","),
    );
    let mut f = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|e| format!("opening {}: {e}", path.display()))?;
    f.write_all(line.as_bytes())
        .map_err(|e| format!("writing {}: {e}", path.display()))
}

/// Prompt the user on stdin: `Accept these capabilities? [y/n]`.
///
/// Parallels [`prompt_trust`]. Prints the ext identifier and the
/// requested-caps CSV so the operator sees exactly what they're
/// accepting; returns true on `y`/`yes`, false on anything else
/// (including EOF). Used by `ark ext add` after the publisher trust
/// step has cleared.
pub fn prompt_caps(ext_key: &str, caps: &[&str]) -> bool {
    let stdin = std::io::stdin();
    let mut lock = stdin.lock();
    prompt_caps_from(ext_key, caps, &mut lock, &mut std::io::stdout())
}

/// Core capability-prompt loop, parameterised over reader + writer
/// for test injection.
pub fn prompt_caps_from<R: std::io::BufRead, W: std::io::Write>(
    ext_key: &str,
    caps: &[&str],
    reader: &mut R,
    writer: &mut W,
) -> bool {
    let _ = writeln!(
        writer,
        "ark: extension `{ext_key}` requests capabilities: {}",
        caps.join(", ")
    );
    let _ = write!(writer, "Accept these capabilities? [y/n]: ");
    let _ = writer.flush();
    let mut line = String::new();
    if reader.read_line(&mut line).is_err() {
        return false;
    }
    matches!(line.trim().to_ascii_lowercase().as_str(), "y" | "yes")
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn sample_github() -> Source {
        Source::Github {
            slug: "rlch/ark-picker".into(),
            git_ref: Some("v0.1.0".into()),
        }
    }

    fn sample_url() -> Source {
        Source::Url("https://example.com/pkg/foo.tar.gz".into())
    }

    fn sample_path() -> Source {
        Source::Path(PathBuf::from("/Users/me/my-ext"))
    }

    #[test]
    fn derive_publisher_github_uses_user_only() {
        let p = derive_publisher(&sample_github());
        assert_eq!(p, Publisher::GitHub("rlch".into()));
        assert_eq!(p.as_key(), "github:rlch");
    }

    #[test]
    fn derive_publisher_url_keeps_host_only() {
        let p = derive_publisher(&sample_url());
        assert_eq!(p, Publisher::UrlHost("example.com".into()));
        assert_eq!(p.as_key(), "url:example.com");
    }

    #[test]
    fn derive_publisher_path_is_full_path() {
        let p = derive_publisher(&sample_path());
        assert_eq!(p.as_key(), "path:/Users/me/my-ext");
    }

    #[test]
    fn extract_host_handles_port_and_userinfo() {
        assert_eq!(extract_host("https://u:p@example.com:8443/x"), "example.com");
        assert_eq!(extract_host("http://example.com"), "example.com");
        assert_eq!(extract_host("https://example.com/foo/bar"), "example.com");
    }

    #[test]
    fn parse_trust_keys_reads_publisher_nodes() {
        let text = r#"
publisher "github:rlch"
publisher "url:example.com"
other-node "ignored"
"#;
        let got = parse_trust_keys(text);
        assert!(got.contains("github:rlch"));
        assert!(got.contains("url:example.com"));
        assert!(!got.contains("ignored"));
    }

    #[test]
    fn parse_trust_keys_tolerates_malformed_kdl() {
        // Garbage input → empty set, no panic.
        let got = parse_trust_keys("publisher \"unclosed");
        assert!(got.is_empty());
    }

    #[test]
    fn save_trust_at_creates_file_and_is_idempotent() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("ark/extension-trust.kdl");

        let p = Publisher::GitHub("rlch".into());
        save_trust_at(&path, &p).unwrap();
        save_trust_at(&path, &p).unwrap(); // idempotent

        let text = fs::read_to_string(&path).unwrap();
        let matches = text.matches("github:rlch").count();
        assert_eq!(matches, 1, "publisher should be written exactly once");
    }

    #[test]
    fn save_trust_at_preserves_existing_nodes() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("extension-trust.kdl");
        fs::write(&path, "publisher \"github:existing\"\n").unwrap();

        let p = Publisher::UrlHost("new.example.com".into());
        save_trust_at(&path, &p).unwrap();

        let text = fs::read_to_string(&path).unwrap();
        assert!(text.contains("github:existing"));
        assert!(text.contains("url:new.example.com"));
    }

    #[test]
    fn load_trust_file_at_returns_empty_for_missing() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("nope.kdl");
        assert!(load_trust_file_at(&path).is_empty());
    }

    #[test]
    fn load_trust_file_at_round_trips_saved_entries() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("trust.kdl");
        let a = Publisher::GitHub("a".into());
        let b = Publisher::UrlHost("b.com".into());
        save_trust_at(&path, &a).unwrap();
        save_trust_at(&path, &b).unwrap();
        let set = load_trust_file_at(&path);
        assert!(set.contains("github:a"));
        assert!(set.contains("url:b.com"));
    }

    #[test]
    fn append_audit_at_creates_and_appends() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("ark/extension-audit.log");
        let p = Publisher::GitHub("rlch".into());
        append_audit_at(&path, &p, "github:rlch/ark-picker@v0.1").unwrap();
        append_audit_at(&path, &p, "github:rlch/ark-other").unwrap();
        let text = fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 2);
        for line in &lines {
            assert!(line.contains("accept-all"));
            assert!(line.contains("publisher=github:rlch"));
        }
        assert!(lines[0].contains("source=github:rlch/ark-picker@v0.1"));
        assert!(lines[1].contains("source=github:rlch/ark-other"));
    }

    #[test]
    fn prompt_from_yes_accepts() {
        let p = Publisher::GitHub("rlch".into());
        let mut input = std::io::Cursor::new(b"y\n");
        let mut out = Vec::new();
        assert!(prompt_from(&p, &mut input, &mut out));
    }

    #[test]
    fn prompt_from_case_insensitive_yes() {
        let p = Publisher::GitHub("rlch".into());
        let mut input = std::io::Cursor::new(b"YES\n");
        let mut out = Vec::new();
        assert!(prompt_from(&p, &mut input, &mut out));
    }

    #[test]
    fn prompt_from_no_rejects() {
        let p = Publisher::GitHub("rlch".into());
        let mut input = std::io::Cursor::new(b"n\n");
        let mut out = Vec::new();
        assert!(!prompt_from(&p, &mut input, &mut out));
    }

    #[test]
    fn prompt_from_eof_rejects() {
        let p = Publisher::GitHub("rlch".into());
        let mut input = std::io::Cursor::new(b"");
        let mut out = Vec::new();
        assert!(!prompt_from(&p, &mut input, &mut out));
    }

    #[test]
    fn prompt_from_writes_publisher_label() {
        let p = Publisher::UrlHost("example.com".into());
        let mut input = std::io::Cursor::new(b"y\n");
        let mut out = Vec::new();
        let _ = prompt_from(&p, &mut input, &mut out);
        let text = String::from_utf8(out).unwrap();
        assert!(text.contains("url:example.com"));
        assert!(text.contains("Trust this publisher"));
    }

    // -----------------------------------------------------------------
    // T-13.4: capability-trust surface
    // -----------------------------------------------------------------

    #[test]
    fn ext_version_key_joins_name_and_version() {
        assert_eq!(ext_version_key("picker", "0.1.0"), "picker@0.1.0");
    }

    #[test]
    fn parse_trusted_caps_reads_capability_nodes() {
        let text = r#"
publisher "github:rlch"
capability "exec" extension="picker@0.1.0"
capability "pipe" extension="picker@0.1.0"
capability "network" extension="other@0.2.0"
unrelated "ignored"
"#;
        let got = parse_trusted_caps(text);
        assert!(got.contains(&("picker@0.1.0".into(), "exec".into())));
        assert!(got.contains(&("picker@0.1.0".into(), "pipe".into())));
        assert!(got.contains(&("other@0.2.0".into(), "network".into())));
        assert_eq!(got.len(), 3);
    }

    #[test]
    fn parse_trusted_caps_tolerates_malformed_kdl() {
        let got = parse_trusted_caps("capability \"unclosed");
        assert!(got.is_empty());
    }

    #[test]
    fn parse_trusted_caps_skips_nodes_missing_extension_property() {
        // A bare `capability "exec"` with no extension= property is
        // ambiguous — skip it rather than implicitly binding to some
        // default extension key.
        let text = r#"
capability "exec"
capability "pipe" extension="ok@0.1"
"#;
        let got = parse_trusted_caps(text);
        assert_eq!(got.len(), 1);
        assert!(got.contains(&("ok@0.1".into(), "pipe".into())));
    }

    #[test]
    fn save_caps_at_creates_and_is_idempotent() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("ark/extension-trust.kdl");

        save_caps_at(&path, "picker@0.1.0", &["exec", "pipe"]).unwrap();
        save_caps_at(&path, "picker@0.1.0", &["exec", "pipe"]).unwrap();

        let text = fs::read_to_string(&path).unwrap();
        let exec_matches = text.matches("\"exec\"").count();
        let pipe_matches = text.matches("\"pipe\"").count();
        assert_eq!(exec_matches, 1, "exec should be written once: {text}");
        assert_eq!(pipe_matches, 1, "pipe should be written once: {text}");
    }

    #[test]
    fn save_caps_at_preserves_publisher_entries() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("extension-trust.kdl");
        fs::write(&path, "publisher \"github:rlch\"\n").unwrap();

        save_caps_at(&path, "picker@0.1.0", &["exec"]).unwrap();

        let text = fs::read_to_string(&path).unwrap();
        assert!(text.contains("publisher \"github:rlch\""));
        assert!(text.contains("capability \"exec\" extension=\"picker@0.1.0\""));
    }

    #[test]
    fn save_caps_at_appends_new_caps_to_existing_ext() {
        // Re-running install after a version bump that adds one cap
        // should extend the trust file with only the new cap, not
        // duplicate the prior entries. (T-13.5 preconditions.)
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("trust.kdl");

        save_caps_at(&path, "picker@0.1.0", &["exec"]).unwrap();
        save_caps_at(&path, "picker@0.1.0", &["exec", "pipe"]).unwrap();

        let got = load_trusted_caps_at(&path);
        assert!(got.contains(&("picker@0.1.0".into(), "exec".into())));
        assert!(got.contains(&("picker@0.1.0".into(), "pipe".into())));
        assert_eq!(got.len(), 2);
    }

    #[test]
    fn save_caps_at_handles_empty_cap_list() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("trust.kdl");
        save_caps_at(&path, "picker@0.1.0", &[]).unwrap();
        // No entries means nothing to persist — file may or may not
        // exist, but no caps should be recorded.
        assert!(load_trusted_caps_at(&path).is_empty());
    }

    #[test]
    fn load_trusted_caps_at_empty_for_missing_file() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("nope.kdl");
        assert!(load_trusted_caps_at(&path).is_empty());
    }

    #[test]
    fn append_caps_audit_at_writes_accept_all_caps_line() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("ark/extension-audit.log");
        append_caps_audit_at(&path, "picker@0.1.0", &["exec", "pipe"]).unwrap();
        let text = fs::read_to_string(&path).unwrap();
        assert!(text.contains("accept-all-caps"));
        assert!(text.contains("extension=picker@0.1.0"));
        assert!(text.contains("caps=exec,pipe"));
    }

    #[test]
    fn append_caps_audit_at_no_op_for_empty_caps() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("audit.log");
        append_caps_audit_at(&path, "x@1", &[]).unwrap();
        // Empty cap list means no event worth recording — no file.
        assert!(!path.exists());
    }

    #[test]
    fn prompt_caps_from_yes_accepts() {
        let mut input = std::io::Cursor::new(b"y\n");
        let mut out = Vec::new();
        assert!(prompt_caps_from(
            "picker@0.1.0",
            &["exec", "pipe"],
            &mut input,
            &mut out,
        ));
        let text = String::from_utf8(out).unwrap();
        assert!(text.contains("picker@0.1.0"));
        assert!(text.contains("requests capabilities: exec, pipe"));
    }

    #[test]
    fn prompt_caps_from_no_rejects() {
        let mut input = std::io::Cursor::new(b"n\n");
        let mut out = Vec::new();
        assert!(!prompt_caps_from("x@1", &["exec"], &mut input, &mut out));
    }

    #[test]
    fn prompt_caps_from_eof_rejects() {
        let mut input = std::io::Cursor::new(b"");
        let mut out = Vec::new();
        assert!(!prompt_caps_from("x@1", &["exec"], &mut input, &mut out));
    }
}
