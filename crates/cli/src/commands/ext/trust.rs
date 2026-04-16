//! Publisher-trust storage + audit log for `ark ext add`.
//!
//! T-13.1, T-13.2 (cavekit-scene R10, v0.5 milestone). Trust is an
//! *install-time* check — not a runtime capability gate. The VSCode
//! 1.97 workspace-trust dialog is the direct analog: the first time
//! a user installs from a given publisher (github user, url host,
//! local path root), we prompt "Trust this publisher? [y/n]". Yes
//! gets remembered in `${XDG_CONFIG_HOME}/ark/extension-trust.kdl`;
//! subsequent installs from the same publisher skip the prompt.
//!
//! `--accept-all` (T-13.2) is the CI escape hatch — skips the prompt
//! and appends a warning line to
//! `${XDG_DATA_HOME}/ark/extension-audit.log` so an auditor can see
//! every non-interactive install after the fact.
//!
//! # Trust file shape
//!
//! Minimal KDL, one `publisher` node per trusted publisher, value is
//! the publisher's canonical string form:
//!
//! ```kdl
//! publisher "github:rlch"
//! publisher "url:example.com"
//! publisher "path:/Users/me/ext-root"
//! ```
//!
//! Unknown nodes are preserved on re-write (we parse + append rather
//! than reserialize). Rewrite-safety keeps future node types
//! (e.g. per-version cap trust from T-13.5) backward-compatible when
//! older `ark` binaries encounter them.
//!
//! # Audit log shape
//!
//! Line-oriented text — one entry per `--accept-all` install:
//!
//! ```text
//! 2026-04-18T12:34:56Z accept-all publisher=github:rlch source=github:rlch/ark-picker@v0.1
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
}
