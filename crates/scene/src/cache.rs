//! Scene compile cache keyed by [`SceneId`].
//!
//! Because `SceneId` includes a blake3 content hash, the cache naturally
//! handles content-change detection: same path + different content = different
//! `SceneId` = cache miss. Same path + same content = cache hit, skip re-parse.
//!
//! Today `CachedScene` wraps a parsed [`SceneIR`]; later tiers will add
//! compiled artifacts (validated layout, resolved views, etc.).

use std::collections::HashMap;
use std::path::Path;

use crate::id::SceneId;
use crate::parse::SceneIR;

/// A cached compilation artifact for a single scene file.
///
/// Currently wraps the parsed [`SceneIR`]. Future tiers will extend this
/// with validated layout trees, resolved view bindings, and other compiled
/// outputs that are expensive to recompute on every reload.
#[derive(Debug)]
pub struct CachedScene {
    /// The parsed intermediate representation.
    pub ir: SceneIR,
}

/// Scene compile cache keyed by [`SceneId`].
///
/// A thin `HashMap` wrapper that maps content-addressed scene identities to
/// their cached compilation artifacts. The blake3 hash inside `SceneId`
/// means that any content change to a scene file produces a different key,
/// so stale entries are never served — they simply become unreachable and
/// can be evicted by [`invalidate`](SceneCache::invalidate).
#[derive(Debug, Default)]
pub struct SceneCache {
    inner: HashMap<SceneId, CachedScene>,
}

impl SceneCache {
    /// Create an empty cache.
    pub fn new() -> Self {
        Self::default()
    }

    /// Look up a cached scene by its content-addressed identity.
    pub fn get(&self, id: &SceneId) -> Option<&CachedScene> {
        self.inner.get(id)
    }

    /// Insert a parsed [`SceneIR`] into the cache, keyed by `ir.id`.
    ///
    /// Returns a reference to the newly inserted [`CachedScene`].
    pub fn insert(&mut self, ir: SceneIR) -> &CachedScene {
        let id = ir.id.clone();
        self.inner.insert(id.clone(), CachedScene { ir });
        // SAFETY: we just inserted with this key, so `get` is infallible.
        self.inner.get(&id).expect("just inserted")
    }

    /// Remove the cache entry for `id`. Returns `true` if an entry existed.
    pub fn invalidate(&mut self, id: &SceneId) -> bool {
        self.inner.remove(id).is_some()
    }

    /// Remove ALL cache entries whose path matches `path`, regardless of
    /// content hash. Use on hot-reload: the old hash is unknown but the
    /// file changed, so every generation for that path is stale.
    pub fn invalidate_by_path(&mut self, path: &Path) -> usize {
        let before = self.inner.len();
        self.inner.retain(|id, _| id.path != path);
        before - self.inner.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse::parse_scene;

    fn sample_ir(name: &str, content: &str) -> SceneIR {
        let src = format!(r#"scene "{name}" {{ }}"#);
        parse_scene(&src, content).expect("test scene should parse")
    }

    #[test]
    fn cache_hit_on_same_content() {
        let mut cache = SceneCache::new();
        let ir = sample_ir("a", "a.kdl");
        let id = ir.id.clone();
        cache.insert(ir);

        assert!(cache.get(&id).is_some(), "same SceneId should hit");
    }

    #[test]
    fn cache_miss_on_different_content() {
        let mut cache = SceneCache::new();
        let ir = sample_ir("a", "a.kdl");
        cache.insert(ir);

        // Same path, different content -> different SceneId -> miss.
        let different = SceneId::new("a.kdl", b"different content");
        assert!(
            cache.get(&different).is_none(),
            "different hash should miss"
        );
    }

    #[test]
    fn invalidate_removes_entry() {
        let mut cache = SceneCache::new();
        let ir = sample_ir("a", "a.kdl");
        let id = ir.id.clone();
        cache.insert(ir);

        assert!(cache.invalidate(&id), "invalidate should return true");
        assert!(
            cache.get(&id).is_none(),
            "get after invalidate should be None"
        );
    }
}
