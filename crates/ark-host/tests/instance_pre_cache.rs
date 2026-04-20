//! T-PP-036 (cavekit-plugin-protocol R4): `InstancePreCache` integration test.
//!
//! Builds a minimal WIT component from WAT text, caches its
//! `InstancePre<PluginCtx>` under a `(ContentHash, CapsKey)` pair, and
//! verifies that a second `get_or_compute` call for the same pair
//! returns the cached value WITHOUT invoking the compute closure.

use std::collections::BTreeSet;
use std::sync::atomic::{AtomicUsize, Ordering};

use ark_host::{
    CapsKey, ContentHash, InstancePreCache, LinkerSet, content_hash, engine,
};
use wasmtime::component::Component;

/// Smallest possible valid component: an empty one. We parse the WAT
/// text ourselves via the `wat` crate into binary form so we don't
/// depend on the `wasmtime/wat` feature being enabled.
fn tiny_component() -> Component {
    let binary = wat::parse_str("(component)").expect("parse WAT");
    Component::from_binary(engine(), &binary).expect("tiny component compile")
}

#[test]
fn compute_runs_exactly_once_per_key() {
    let cache = InstancePreCache::new();
    let linker_set = LinkerSet::build(vec![]).expect("LinkerSet::build");
    let linker = linker_set
        .for_caps(&CapsKey::new())
        .expect("empty variant must exist");

    let component = tiny_component();
    let bytes = component.serialize().expect("serialize tiny component");
    let hash: ContentHash = content_hash(&bytes);
    let caps: CapsKey = BTreeSet::new();

    let counter = AtomicUsize::new(0);

    let _a = cache
        .get_or_compute(hash, caps.clone(), || {
            counter.fetch_add(1, Ordering::SeqCst);
            linker.instantiate_pre(&component)
        })
        .expect("get_or_compute 1st call");
    assert_eq!(counter.load(Ordering::SeqCst), 1);

    let _b = cache
        .get_or_compute(hash, caps.clone(), || {
            counter.fetch_add(1, Ordering::SeqCst);
            linker.instantiate_pre(&component)
        })
        .expect("get_or_compute 2nd call");
    assert_eq!(
        counter.load(Ordering::SeqCst),
        1,
        "cache hit must skip the compute closure"
    );

    // Sanity: one entry in the cache.
    assert_eq!(cache.len(), 1);
}

#[test]
fn distinct_caps_keys_get_distinct_cache_entries() {
    let cache = InstancePreCache::new();
    let linker_set = LinkerSet::build(vec![["fs-read".to_string()].into_iter().collect()])
        .expect("LinkerSet::build");

    let component = tiny_component();
    let bytes = component.serialize().expect("serialize");
    let hash: ContentHash = content_hash(&bytes);

    let empty = CapsKey::new();
    let fs_read: CapsKey = ["fs-read".to_string()].into_iter().collect();

    let linker_empty = linker_set.for_caps(&empty).unwrap();
    let linker_fs = linker_set.for_caps(&fs_read).unwrap();

    let counter = AtomicUsize::new(0);
    let _ = cache
        .get_or_compute(hash, empty.clone(), || {
            counter.fetch_add(1, Ordering::SeqCst);
            linker_empty.instantiate_pre(&component)
        })
        .expect("compute empty");
    let _ = cache
        .get_or_compute(hash, fs_read.clone(), || {
            counter.fetch_add(1, Ordering::SeqCst);
            linker_fs.instantiate_pre(&component)
        })
        .expect("compute fs-read");

    assert_eq!(
        counter.load(Ordering::SeqCst),
        2,
        "distinct CapsKeys must trigger distinct computes"
    );
    assert_eq!(cache.len(), 2);
}

#[test]
fn content_hash_is_deterministic() {
    let bytes = b"hello world";
    let a = content_hash(bytes);
    let b = content_hash(bytes);
    assert_eq!(a, b, "content_hash must be deterministic");
    let c = content_hash(b"hello worlD");
    assert_ne!(a, c, "different bytes must produce different hashes");
}
