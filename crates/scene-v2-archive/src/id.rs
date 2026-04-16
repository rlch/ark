//! `SceneId` — canonical identity for a scene file.
//!
//! A `SceneId` pairs a scene's on-disk path with the blake3 hash of its
//! contents. The pair lets three separate subsystems agree on "same
//! scene":
//!
//! * **Hot-reload delta detection (R14):** the file-watcher hashes the
//!   new bytes and compares against the cached `SceneId.content_hash`;
//!   unchanged hash → no-op (skip the parse + merge).
//! * **`ark scene graph` attribution (R11):** reactions / keybinds /
//!   plugins carry a `SceneId` origin so the graph CLI can render
//!   `(from scene.kdl#a1b2c3d4)` next to each entry.
//! * **Compile-cache keying:** `CompiledScene` is memoised under
//!   `SceneId` so re-spawn with an unchanged scene skips the parse
//!   pipeline entirely.
//!
//! `Display` renders as `<path>#<hash-prefix-8>`, matching the form
//! the `ark scene graph` command surfaces to users.

use std::fmt;
use std::io;
use std::path::{Path, PathBuf};

/// Canonical identity for a scene file on disk.
///
/// Equality uses BOTH `path` and `content_hash` — two files with the
/// same content at different paths are distinct scenes (different
/// include contexts). See the module-level docs for the three
/// subsystems that depend on this invariant.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SceneId {
    /// Absolute or canonicalised path of the scene file. Kept as a
    /// `PathBuf` rather than `String` so OS-specific path semantics
    /// (case sensitivity, UNC prefixes) are preserved. Callers that
    /// care about cross-platform equality should canonicalise before
    /// constructing.
    pub path: PathBuf,

    /// blake3 hash of the file's byte contents at the time of
    /// construction. Wrapped in `blake3::Hash` so downstream code
    /// can use the crate's `==` (constant-time) comparison directly.
    pub content_hash: blake3::Hash,
}

impl SceneId {
    /// Construct a `SceneId` for a file on disk. Reads the file fully,
    /// blake3-hashes its bytes, and stores the supplied path verbatim.
    ///
    /// Errors propagate from `std::fs::read` (file missing, permission
    /// denied, etc.). Callers that want canonical paths should
    /// `std::fs::canonicalize` the input before invoking — `for_file`
    /// deliberately does not canonicalise so that tests can operate
    /// against relative-path fixtures.
    pub fn for_file(path: &Path) -> io::Result<Self> {
        let bytes = std::fs::read(path)?;
        Ok(Self::from_bytes(path.to_path_buf(), &bytes))
    }

    /// Construct a `SceneId` from already-loaded bytes. Useful when the
    /// file content is being parsed anyway and the caller doesn't want
    /// a second read.
    pub fn from_bytes(path: PathBuf, bytes: &[u8]) -> Self {
        let content_hash = blake3::hash(bytes);
        Self { path, content_hash }
    }

    /// First 8 hex chars of the content hash — the short-form identity
    /// used by `Display` and `ark scene graph`. Extracted as a method
    /// so tests can assert on it without going through formatting.
    pub fn short_hash(&self) -> String {
        let hex = self.content_hash.to_hex();
        hex.as_str().chars().take(8).collect()
    }
}

impl fmt::Display for SceneId {
    /// Renders as `<path>#<hash-prefix-8>` — matches the form scene
    /// graph output expects. Path rendering uses `Path::display` so
    /// non-UTF-8 paths round-trip losslessly on Unix.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}#{}", self.path.display(), self.short_hash())
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// Known-good fixture: the blake3 hash of the exact byte string
    /// `"hello world"` (the same constant used in the blake3 crate's
    /// own test fixtures — taken from the official blake3 test vectors
    /// via `b3sum`).
    const HELLO_WORLD_BLAKE3_HEX: &str =
        "d74981efa70a0c880b8d8c1985d075dbcbf679b99a5f9914e5aaf96b831a9e24";

    #[test]
    fn hash_matches_known_good_vector() {
        let id = SceneId::from_bytes(PathBuf::from("/tmp/x.kdl"), b"hello world");
        assert_eq!(id.content_hash.to_hex().as_str(), HELLO_WORLD_BLAKE3_HEX);
    }

    #[test]
    fn short_hash_is_eight_chars() {
        let id = SceneId::from_bytes(PathBuf::from("/tmp/x.kdl"), b"hello world");
        let short = id.short_hash();
        assert_eq!(short.len(), 8);
        // Must match the first 8 chars of the full hash.
        assert_eq!(short.as_str(), &HELLO_WORLD_BLAKE3_HEX[..8]);
    }

    #[test]
    fn display_format() {
        let id = SceneId::from_bytes(PathBuf::from("/tmp/x.kdl"), b"hello world");
        let rendered = id.to_string();
        assert_eq!(
            rendered,
            format!("/tmp/x.kdl#{}", &HELLO_WORLD_BLAKE3_HEX[..8])
        );
    }

    #[test]
    fn equality_uses_both_path_and_hash() {
        let a = SceneId::from_bytes(PathBuf::from("/a/scene.kdl"), b"hello world");
        let b = SceneId::from_bytes(PathBuf::from("/b/scene.kdl"), b"hello world");
        let c = SceneId::from_bytes(PathBuf::from("/a/scene.kdl"), b"hello world!");

        // Same content, different paths = distinct SceneIds.
        assert_ne!(a, b);
        // Same path, different content = distinct SceneIds.
        assert_ne!(a, c);
        // Same path + same content = equal.
        assert_eq!(
            a,
            SceneId::from_bytes(PathBuf::from("/a/scene.kdl"), b"hello world")
        );
    }

    #[test]
    fn for_file_reads_and_hashes() {
        // Use the `tempfile` dev-dep already pinned in Cargo.toml.
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("scene.kdl");
        std::fs::write(&path, b"hello world").expect("write fixture");

        let id = SceneId::for_file(&path).expect("for_file reads fixture");
        assert_eq!(id.content_hash.to_hex().as_str(), HELLO_WORLD_BLAKE3_HEX);
        assert_eq!(id.path, path);
    }

    #[test]
    fn for_file_missing_is_io_error() {
        let err = SceneId::for_file(Path::new("/this/path/absolutely/does/not/exist.kdl"))
            .expect_err("missing file must error");
        assert_eq!(err.kind(), io::ErrorKind::NotFound);
    }
}
