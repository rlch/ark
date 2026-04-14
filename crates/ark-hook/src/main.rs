//! `ark-hook` binary entry.
//!
//! Skeleton scope (T-046, cavekit-hook-ipc.md R1): parse args, run the
//! pipeline in `ark_hook::run`, exit `0` on success and on any error
//! path. Exit code `2` is reserved for explicit-deny in T-050 and is
//! never produced here.
//!
//! All logging goes to stderr (stdout is reserved for the future
//! `PermissionRequest` allow-payload from T-050).

use std::io;
use std::process::ExitCode;

use clap::Parser;
use tracing::{error, warn};
use tracing_subscriber::{EnvFilter, fmt};

use ark_hook::{Cli, run};

fn main() -> ExitCode {
    init_tracing();

    // clap auto-handles --help / --version with its own non-zero exits;
    // for our argument-validation failures we'd normally let clap print
    // and exit non-zero. The R1 fail-open contract applies to *runtime*
    // errors (stdin, state, pipes), not to misconfigured invocations:
    // if Claude Code can't even spell our flags right, surfacing that
    // loudly is the right call. So the early exit on parse failure is
    // intentional and matches clap defaults.
    let cli = Cli::parse();

    match run(&cli, io::stdin().lock()) {
        Ok(outcome) => ExitCode::from(outcome.exit_code() as u8),
        Err(e) => {
            // Top-level fail-open: log the error chain and exit 0 so we
            // never block claude (kit R3, R1 exit-code clause).
            error!(
                agent = %cli.id,
                event = %cli.event,
                error = %e,
                "ark-hook failed; fail-open with exit 0"
            );
            for cause in e.chain().skip(1) {
                warn!(cause = %cause, "caused by");
            }
            ExitCode::from(0)
        }
    }
}

/// Initialize the tracing subscriber to write to stderr only.
///
/// Stdout is reserved for the future PermissionRequest payload (T-050)
/// — emitting log lines there would corrupt Claude Code's protocol read.
/// Errors here are intentionally swallowed: a tracing init failure must
/// not propagate into a non-zero exit (R3 fail-open).
fn init_tracing() {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    let _ = fmt()
        .with_env_filter(filter)
        .with_writer(io::stderr)
        .try_init();
}
