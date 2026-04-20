//! T-PP-035 (cavekit-plugin-protocol R1 + R4): cap → WasiCtx mapping.
//!
//! Each covered case builds a `WasiCtx` for a distinct cap set and
//! asserts it succeeds without panic. wasmtime-wasi 43 does not expose
//! public getters for the socket-allow flags or preopen list, so the
//! visible invariant is "factory returns a valid WasiCtx for every
//! granted-cap shape".
//!
//! The factory *does* surface filesystem errors (e.g. `plugin_dir`
//! does not exist) as `wasmtime::Result::Err`; the happy-path tests
//! below use a temp dir created in each test.

use std::collections::BTreeSet;

use ark_host::wasi_ctx_for_caps;

fn key(caps: &[&str]) -> BTreeSet<String> {
    caps.iter().map(|s| s.to_string()).collect()
}

/// Create a fresh temp dir under `std::env::temp_dir()` for the test.
fn tempdir(suffix: &str) -> std::path::PathBuf {
    let base = std::env::temp_dir();
    let pid = std::process::id();
    // `nanos` gives a fresh dir per test invocation when tests share
    // the same pid (cargo test runs them in the same process).
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let path = base.join(format!("ark-host-wasi-ctx-{pid}-{nanos}-{suffix}"));
    std::fs::create_dir_all(&path).expect("mkdir temp test dir");
    path
}

#[test]
fn zero_caps_builds_deny_all_wasi_ctx() {
    let dir = tempdir("zero");
    let _ctx = wasi_ctx_for_caps(&dir, &key(&[])).expect("wasi_ctx_for_caps({})");
    // Surviving this call is the assertion — the builder didn't panic
    // and no preopen was requested, so the dir existing or not is
    // irrelevant.
}

#[test]
fn fs_read_cap_adds_readonly_preopen() {
    let dir = tempdir("fs-read");
    let _ctx = wasi_ctx_for_caps(&dir, &key(&["fs-read"]))
        .expect("wasi_ctx_for_caps({fs-read}) must succeed when plugin_dir exists");
}

#[test]
fn fs_read_plus_fs_write_upgrades_preopen_perms() {
    let dir = tempdir("fs-rw");
    let _ctx = wasi_ctx_for_caps(&dir, &key(&["fs-read", "fs-write"]))
        .expect("wasi_ctx_for_caps({fs-read, fs-write}) must succeed");
    // fs-write alone also should work — the factory OR's the two
    // flags and applies a single preopen.
    let _ctx2 = wasi_ctx_for_caps(&dir, &key(&["fs-write"]))
        .expect("wasi_ctx_for_caps({fs-write}) must succeed");
}

#[test]
fn network_cap_builds_without_preopen() {
    // `network` alone does not trigger a preopen, so the function does
    // not need `plugin_dir` to exist. Use a non-existent path to
    // double-confirm.
    let dir = std::path::PathBuf::from("/definitely-not-a-real-dir-ark-host-test");
    let _ctx = wasi_ctx_for_caps(&dir, &key(&["network"])).expect("wasi_ctx_for_caps({network})");
}

#[test]
fn nonexistent_plugin_dir_with_fs_read_returns_error() {
    // Requesting `fs-read` with a missing plugin_dir must bubble the
    // filesystem error out as `wasmtime::Result::Err`, not panic.
    let dir = std::path::PathBuf::from("/definitely-not-a-real-dir-for-ark-fs-read-test");
    let result = wasi_ctx_for_caps(&dir, &key(&["fs-read"]));
    // `WasiCtx` has no `Debug`, so we can't use `expect_err`; match manually.
    let Err(err) = result else {
        panic!("missing plugin_dir must fail fs-read preopen");
    };
    // The error string contains the path — just assert it's non-empty.
    assert!(!format!("{err:#}").is_empty());
}
