//! `InstancePre<PluginCtx>` cache keyed by `(ContentHash, CapsKey)`.
//!
//! T-PP-036 (cavekit-plugin-protocol R4): pre-compiled instances
//! shared across re-instantiations of the same plugin under the same
//! cap profile. The key is `(sha256(wasm_bytes), CapsKey)` so a
//! post-launch grant-set change invalidates cleanly.
//!
//! # Why `Arc<InstancePre<PluginCtx>>` instead of `Mutex<InstancePre>`
//!
//! `wasmtime::component::InstancePre` is already `Clone`
//! (internally refcounted — cheap). We still wrap in `Arc` so the map
//! can hand out clones without cloning the internals; the extra `Arc`
//! layer is O(1).
//!
//! # Concurrency
//!
//! The cache itself is behind a single `Mutex`. Because `compute_fn`
//! runs while the lock is held (to prevent duplicate work), callers
//! should keep the closure cheap: a typical compute_fn is just
//! `linker.instantiate_pre(&component)`, which is a sync but
//! non-trivial wasmtime call. If contention becomes a problem, switch
//! to a `RwLock` + optimistic read path; not needed for v1.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use wasmtime::component::InstancePre;

use crate::PluginCtx;
use crate::linker_set::CapsKey;

/// sha256 digest of the plugin's wasm bytes. Keying on the content
/// hash (not the file path) means two symlinks to the same `.wasm`
/// produce the same cache entry; rebuilds that change the bytes
/// invalidate automatically.
pub type ContentHash = [u8; 32];

/// Process-wide cache of `InstancePre<PluginCtx>` values, keyed by
/// `(ContentHash, CapsKey)`.
pub struct InstancePreCache {
    inner: Mutex<HashMap<(ContentHash, CapsKey), Arc<InstancePre<PluginCtx>>>>,
}

impl InstancePreCache {
    /// Construct an empty cache.
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(HashMap::new()),
        }
    }

    /// Returns the cached `Arc<InstancePre<PluginCtx>>` for
    /// `(hash, caps)`, computing it exactly once via `compute` if
    /// absent.
    ///
    /// `compute` runs with the cache lock held, so it must not itself
    /// call `get_or_compute` (re-entry will deadlock). In the expected
    /// call path `compute` just forwards to
    /// `linker.instantiate_pre(&component)`.
    pub fn get_or_compute<F>(
        &self,
        hash: ContentHash,
        caps: CapsKey,
        compute: F,
    ) -> wasmtime::Result<Arc<InstancePre<PluginCtx>>>
    where
        F: FnOnce() -> wasmtime::Result<InstancePre<PluginCtx>>,
    {
        let mut guard = self.inner.lock().expect("InstancePreCache mutex poisoned");
        if let Some(existing) = guard.get(&(hash, caps.clone())) {
            return Ok(existing.clone());
        }
        let fresh = Arc::new(compute()?);
        guard.insert((hash, caps), fresh.clone());
        Ok(fresh)
    }

    /// Number of entries currently stored. Exposed for tests.
    #[doc(hidden)]
    pub fn len(&self) -> usize {
        self.inner
            .lock()
            .expect("InstancePreCache mutex poisoned")
            .len()
    }

    /// Whether the cache is empty.
    #[doc(hidden)]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl Default for InstancePreCache {
    fn default() -> Self {
        Self::new()
    }
}

/// Compute the `ContentHash` (sha256) of a `.wasm` byte slice.
///
/// Separate from the cache struct so callers can hash lazily (e.g. a
/// mmap'd file) without holding the cache lock.
pub fn content_hash(bytes: &[u8]) -> ContentHash {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hasher.finalize().into()
}
