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

/// F-613: per <https://no-color.org> only a NON-EMPTY `NO_COLOR`
/// disables color. The help path previously used
/// `env::var_os("NO_COLOR").is_some()`, which incorrectly stripped
/// color when `NO_COLOR=""` — an inconsistency with the subcommand
/// `Ctx::from_env()` path which uses `detect_no_color()`. This test
/// asserts the help output emits ANSI escapes (or at least does not
/// differ from the unset-env case) when NO_COLOR is empty.
#[cfg(unix)]
#[test]
fn help_with_empty_no_color_does_not_strip_color() {
    // Reference: help output with NO_COLOR unset and color forced on.
    let colored = Command::new(ark_bin())
        .arg("--help")
        .env_clear()
        .env("CLICOLOR_FORCE", "1")
        .env("TERM", "xterm-256color")
        .output()
        .expect("spawn ark --help (reference)");
    assert!(colored.status.success());
    let colored_stdout = String::from_utf8_lossy(&colored.stdout).into_owned();

    // With NO_COLOR set to the empty string, the empty value must NOT
    // disable color, so the help output should match the reference
    // byte-for-byte.
    let empty_no_color = Command::new(ark_bin())
        .arg("--help")
        .env_clear()
        .env("CLICOLOR_FORCE", "1")
        .env("TERM", "xterm-256color")
        .env("NO_COLOR", "")
        .output()
        .expect("spawn ark --help (NO_COLOR empty)");
    assert!(empty_no_color.status.success());
    let empty_stdout = String::from_utf8_lossy(&empty_no_color.stdout).into_owned();

    assert_eq!(
        colored_stdout, empty_stdout,
        "empty NO_COLOR must be treated as unset; help output differed"
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
