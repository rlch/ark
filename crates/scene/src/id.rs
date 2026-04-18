//! `SceneId` — identity key for a parsed scene file.
//!
//! Drives hot-reload delta detection (T-017 compile cache): a reload re-hashes
//! the file and skips work if the new `SceneId` equals the cached one. Also
//! the attribution key surfaced by `ark scene explain` so diagnostics can
//! name both the source path and the exact content revision that produced a
//! rule. blake3 is chosen for speed (sub-microsecond on typical scene files)
//! and collision resistance (256-bit output — cryptographically strong enough
//! that hash equality is safe to treat as content equality).

use std::fmt;
use std::fs;
use std::path::PathBuf;

use blake3;

/// Identity key for a scene file: path plus a blake3 hash of its contents.
///
/// Equality is defined structurally over both fields, so two `SceneId`s
/// compare equal only when they originate from the same path **and** the file
/// contents hash identically. This is the invariant the hot-reload path
/// relies on to decide whether a re-parse is necessary.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct SceneId {
    /// Absolute or user-supplied path to the scene source file. Retained so
    /// `ark scene explain` can attribute rules back to a concrete on-disk
    /// location, and so the `Display` impl can surface the path alongside
    /// the content hash.
    pub path: PathBuf,
    /// blake3 hash of the raw file bytes. Acts as the delta-detection key for
    /// hot-reload: a reload compares the freshly-computed hash against the
    /// cached `SceneId.content_hash` and skips re-parsing when they match.
    /// blake3 is chosen for speed and collision resistance.
    pub content_hash: blake3::Hash,
}

impl SceneId {
    /// Read `path` from disk, hash its contents with blake3, and return the
    /// resulting `SceneId`. I/O errors propagate unchanged.
    pub fn from_file(path: impl Into<PathBuf>) -> std::io::Result<SceneId> {
        let path = path.into();
        let bytes = fs::read(&path)?;
        Ok(SceneId {
            content_hash: blake3::hash(&bytes),
            path,
        })
    }

    /// Build a `SceneId` from an in-memory path + byte slice. Used by tests
    /// and by callers that already hold the scene source (e.g. stdin).
    pub fn new(path: impl Into<PathBuf>, content: &[u8]) -> SceneId {
        SceneId {
            path: path.into(),
            content_hash: blake3::hash(content),
        }
    }
}

impl fmt::Display for SceneId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let hex = self.content_hash.to_hex();
        write!(f, "{}#{}", self.path.display(), &hex.as_str()[..8])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn from_file_roundtrips_via_tempfile() {
        let mut tmp = tempfile::NamedTempFile::new().expect("create tempfile");
        tmp.write_all(b"scene \"hello\" { }")
            .expect("write tempfile");
        let path = tmp.path().to_path_buf();

        let from_disk = SceneId::from_file(&path).expect("from_file ok");
        let from_mem = SceneId::new(&path, b"scene \"hello\" { }");

        assert_eq!(from_disk, from_mem);
        assert_eq!(from_disk.path, path);
        assert_eq!(from_disk.content_hash, from_mem.content_hash);
    }

    #[test]
    fn new_produces_deterministic_blake3_hash() {
        let id = SceneId::new("scene.kdl", b"hello");
        assert_eq!(
            id.content_hash.to_hex().as_str(),
            "ea8f163db38682925e4491c5e58d4bb3506ef8c14eb78a86e908c5624a67200f",
        );
    }

    #[test]
    fn display_renders_path_hash_suffix() {
        let id = SceneId::new("scene.kdl", b"hello");
        let rendered = id.to_string();
        let (prefix, suffix) = rendered.split_once('#').expect("display has # separator");
        assert_eq!(prefix, "scene.kdl");
        assert_eq!(suffix.len(), 8);
        assert!(suffix.chars().all(|c| c.is_ascii_hexdigit()));
        assert_eq!(suffix, "ea8f163d");
    }

    #[test]
    fn differing_content_produces_different_scene_id() {
        let a = SceneId::new("scene.kdl", b"one");
        let b = SceneId::new("scene.kdl", b"two");
        assert_ne!(a, b);
        assert_ne!(a.content_hash, b.content_hash);
    }
}
