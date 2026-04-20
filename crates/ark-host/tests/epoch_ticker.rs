//! T-PP-027 (cavekit-plugin-protocol R1): epoch ticker liveness.
//!
//! After `start_epoch_ticker()`, the background OS thread must be
//! calling `Engine::increment_epoch` at ~50 ms intervals. We can't read
//! wasmtime's internal epoch counter directly, so the ticker also
//! increments a crate-local `TICKS_SEEN` atomic on every tick — this
//! test observes that counter to verify the ticker is alive.
//!
//! Timing budget: at 50 ms per tick, 200 ms of wall-clock should yield
//! ≥3 ticks (4 is the expected count but we allow 3 to absorb
//! scheduling jitter on a loaded CI box).

use std::thread;
use std::time::Duration;

use ark_host::engine::ticks_seen;
use ark_host::start_epoch_ticker;

#[test]
fn ticker_advances_counter_within_200ms() {
    // Start the ticker. Idempotent — if another test already started
    // it, this is a no-op.
    start_epoch_ticker();

    let before = ticks_seen();
    thread::sleep(Duration::from_millis(200));
    let after = ticks_seen();
    let delta = after.saturating_sub(before);

    assert!(
        delta >= 3,
        "epoch ticker must fire at least 3 times in 200ms; observed {delta} \
         (before={before}, after={after})"
    );
}

#[test]
fn ticker_start_is_idempotent() {
    // Calling start_epoch_ticker() multiple times must not spawn extra
    // threads or panic. We can't directly count ticker threads from
    // user code, but we can at least check that repeated calls don't
    // panic and that ticks continue to increase.
    start_epoch_ticker();
    start_epoch_ticker();
    start_epoch_ticker();

    let before = ticks_seen();
    thread::sleep(Duration::from_millis(120));
    let after = ticks_seen();
    assert!(
        after > before,
        "ticker stopped advancing after repeated start_epoch_ticker() calls"
    );
}
