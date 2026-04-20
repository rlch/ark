//! T-PP-026 (cavekit-plugin-protocol R1): `Engine` singleton invariant.
//!
//! `ark_host::engine()` must return the same `Engine` on every call, and
//! the underlying construction closure must run exactly once for the
//! lifetime of the process — even under thread contention.
//!
//! This is a crate-level integration test (binary target) rather than a
//! unit test inside `src/` so it exercises the public `ark_host::engine`
//! surface the way downstream crates will.

use std::sync::Barrier;
use std::thread;

use ark_host::engine;
use ark_host::engine::engine_init_count;

#[test]
fn engine_returns_same_pointer_across_calls() {
    // Calling `engine()` 10 times must all return the same `&'static Engine`.
    let first = engine();
    for _ in 0..10 {
        let same = engine();
        assert!(
            std::ptr::eq(first, same),
            "engine() returned a different Engine pointer — singleton violated"
        );
    }
    // The construction closure ran exactly once regardless of the number
    // of `engine()` calls.
    assert_eq!(
        engine_init_count(),
        1,
        "Engine construction closure must run exactly once"
    );
}

#[test]
fn engine_singleton_under_thread_contention() {
    // Touch the engine from the main thread first only if no other test
    // has; we want the counter to reflect a single construction total.
    // Because cargo runs integration tests in the same process, the
    // counter is already at 1 from the previous test — assert that and
    // verify concurrent callers don't bump it.
    let baseline = engine_init_count();
    // baseline is 0 if this test happens to run first, or 1 if the
    // previous test ran first — either way the invariant is "no more
    // constructions past this point".

    let barrier = std::sync::Arc::new(Barrier::new(4));
    let handles: Vec<_> = (0..4)
        .map(|_| {
            let barrier = barrier.clone();
            thread::spawn(move || {
                // All four threads hit `engine()` as close to
                // simultaneously as std::sync::Barrier allows.
                barrier.wait();
                let _ = engine();
            })
        })
        .collect();

    for h in handles {
        h.join().expect("worker thread panicked");
    }

    let after = engine_init_count();
    assert!(
        after <= baseline.max(1),
        "engine() must be constructed at most once; baseline={baseline} after={after}"
    );
    assert_eq!(
        after, 1,
        "engine_init_count must end at 1 after any sequence of engine() calls"
    );
}
