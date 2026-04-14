//! F-512: `--help` and `--version` must not require env resolution.
//!
//! clap handles these flags BEFORE our `Ctx::from_env()` runs, so
//! they must succeed even in a stripped env (no `$HOME`,
//! `$XDG_CONFIG_HOME`, `$ARK_STATE_DIR`, etc.). Prior to F-512 the
//! binary exited 1 with "failed to resolve state dirs" in this
//! environment; the regression is asserted via
//! `std::process::Command::env_clear()` + assertion on exit code
//! and stdout contents.

use std::process::Command;

/// Path to the compiled `ark` binary, injected by cargo.
fn ark_bin() -> &'static str {
    env!("CARGO_BIN_EXE_ark")
}

#[cfg(unix)]
#[test]
fn help_succeeds_with_empty_env() {
    let out = Command::new(ark_bin())
        .arg("--help")
        .env_clear()
        .output()
        .expect("spawn ark --help");
    assert!(
        out.status.success(),
        "--help exit != 0; stderr={:?}",
        String::from_utf8_lossy(&out.stderr),
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    // clap prints a usage line; pick a stable fragment.
    assert!(
        stdout.contains("Usage:") || stdout.contains("USAGE"),
        "expected usage banner, got: {stdout}"
    );
}

#[cfg(unix)]
#[test]
fn version_succeeds_with_empty_env() {
    let out = Command::new(ark_bin())
        .arg("--version")
        .env_clear()
        .output()
        .expect("spawn ark --version");
    assert!(
        out.status.success(),
        "--version exit != 0; stderr={:?}",
        String::from_utf8_lossy(&out.stderr),
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    // clap's derive emits "<name> <version>\n".
    assert!(
        stdout.starts_with("ark "),
        "expected `ark <version>`, got: {stdout}"
    );
}
