//! Fixture plumbing — claude shim staging + scene-file templates.
//!
//! The claude-code extension spawns the real `claude` CLI from any
//! pane wired to `ClaudeCodeView`. Integration tests swap in a mock
//! (typically `mock-claude-cc` from `ark-test-fixtures-claude-code`)
//! so the extension fires without needing network access or a real
//! Anthropic API key.
//!
//! This module stages the mock as a symlink named `claude` inside a
//! fresh tempdir, and the harness prepends that tempdir to `$PATH`
//! when spawning ark. Any child that execs `claude` then picks up the
//! shim instead of the real binary.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow};
use tempfile::TempDir;

/// Create a tempdir and drop a `claude` symlink in it pointing at
/// `mock_bin`. Returns the tempdir so the caller can compute its path
/// for a `PATH` prepend.
///
/// On unix we use `std::os::unix::fs::symlink`; on other platforms we
/// fall back to a copy (zellij + claude-code-ext are unix-only
/// today, but the copy path keeps `cargo check` happy on Windows
/// build bots).
pub fn stage_claude_shim(mock_bin: &Path) -> Result<TempDir> {
    if !mock_bin.is_file() {
        return Err(anyhow!(
            "mock binary `{}` does not exist — cannot stage claude shim",
            mock_bin.display()
        ));
    }

    let dir = tempfile::Builder::new()
        .prefix("ark-harness-claude-shim-")
        .tempdir()
        .with_context(|| "failed to create tempdir for claude shim")?;

    let dest = dir.path().join("claude");
    link_or_copy(mock_bin, &dest)
        .with_context(|| format!("failed to stage claude shim at {}", dest.display()))?;

    Ok(dir)
}

#[cfg(unix)]
fn link_or_copy(from: &Path, to: &Path) -> std::io::Result<()> {
    std::os::unix::fs::symlink(from, to)
}

#[cfg(not(unix))]
fn link_or_copy(from: &Path, to: &Path) -> std::io::Result<()> {
    std::fs::copy(from, to).map(|_| ())
}

/// Canonical scene KDL body for harness smoke tests. Minimal — one
/// stack with a single "ark" pane. Callers that need more elaborate
/// layouts should pass their own KDL to [`crate::HarnessBuilder::new`].
///
/// This template uses only surface area that has been stable since
/// v0.1: a `layout` root with a single `pane` child. It intentionally
/// avoids the `use "claude-code"` extension hook so the template can
/// be used on hosts without the claude-code extension compiled in.
pub const MINIMAL_SCENE_KDL: &str = r#"
layout {
    pane
}
"#;

/// Convenience: materialize the minimal scene to a file inside
/// `parent` and return the resulting path.
pub fn write_minimal_scene(parent: &Path) -> Result<PathBuf> {
    let path = parent.join("harness-minimal-scene.kdl");
    std::fs::write(&path, MINIMAL_SCENE_KDL.as_bytes())
        .with_context(|| format!("failed to write minimal scene at {}", path.display()))?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stage_claude_shim_rejects_missing_binary() {
        let err = stage_claude_shim(Path::new("/nonexistent/mock-claude-cc")).unwrap_err();
        assert!(
            format!("{err}").contains("does not exist"),
            "unexpected error: {err}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn stage_claude_shim_creates_symlink_when_mock_present() {
        // Use /bin/sh as a stand-in for the mock binary — any
        // existing executable works; we only assert the symlink
        // exists.
        let mock = Path::new("/bin/sh");
        if !mock.is_file() {
            eprintln!("SKIP: /bin/sh missing");
            return;
        }
        let dir = stage_claude_shim(mock).expect("stage shim");
        let shim = dir.path().join("claude");
        let meta = std::fs::symlink_metadata(&shim).expect("symlink metadata");
        assert!(
            meta.file_type().is_symlink(),
            "expected claude shim to be a symlink"
        );
        let target = std::fs::read_link(&shim).expect("readlink");
        assert_eq!(target, mock);
    }

    #[test]
    fn write_minimal_scene_writes_expected_body() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = write_minimal_scene(dir.path()).expect("write scene");
        let body = std::fs::read_to_string(&path).expect("read scene");
        assert!(body.contains("layout"));
        assert!(body.contains("pane"));
    }
}
