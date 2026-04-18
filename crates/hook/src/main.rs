//! `ark-hook` binary entry.
//!
//! Two distinct flows live behind one binary (`cavekit-hook-ipc.md` R1):
//!
//! 1. **Legacy hook-event** (no subcommand) — Claude Code invokes
//!    `ark-hook --id <AgentId> --event <EVENT_NAME>`, the hook pipeline
//!    in [`ark_hook::run`] runs, exit 0 on success and on any error
//!    path. Exit 2 is reserved for explicit-deny in T-050 and is never
//!    produced here.
//!
//! 2. **Bridge subcommands** — `intent` / `emit` / `permit` invoked by
//!    `ark-bus` (T-6.2 / T-6.3) and the picker (ACP). Connect to the
//!    per-agent control socket, dispatch one command, exit 0 on
//!    `{ok: true}` / 1 otherwise. Stderr carries human-readable error
//!    text so zellij hidden-pane log capture surfaces failures back to
//!    the operator.
//!
//! All logging goes to stderr (stdout is reserved for the hook-event
//! `PermissionRequest` allow-payload).

use std::io;
use std::process::ExitCode;

use clap::Parser;
use tracing::{error, warn};
use tracing_subscriber::{EnvFilter, fmt};

use ark_hook::{Cli, Command, dispatch_emit, dispatch_intent, dispatch_permit, run};

fn main() -> ExitCode {
    init_tracing();

    let cli = Cli::parse();

    match cli.command.clone() {
        // ---- Bridge subcommand path (T-6.2 / T-6.3 / ACP) ----
        Some(Command::Intent(args)) => bridge_exit_code(dispatch_intent(&args)),
        Some(Command::Emit(args)) => bridge_exit_code(dispatch_emit(&args)),
        Some(Command::Permit(args)) => bridge_exit_code(dispatch_permit(&args)),

        // ---- Legacy hook-event path (default — no subcommand) ----
        None => {
            let legacy = match cli.into_legacy() {
                Ok(l) => l,
                Err(msg) => {
                    eprintln!("ark-hook: {msg}");
                    return ExitCode::from(2);
                }
            };
            match run(&legacy, io::stdin().lock(), io::stdout().lock()) {
                Ok(outcome) => ExitCode::from(outcome.exit_code() as u8),
                Err(e) => {
                    // Top-level fail-open: log the error chain and exit 0
                    // so we never block claude (kit R3, R1 exit-code clause).
                    error!(
                        session = %legacy.id.as_str(),
                        event = %legacy.event,
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
    }
}

/// Map a bridge dispatch result to the `ark-hook` exit contract:
/// `0` on `{ok: true}`, `1` on every error path. Stderr carries the
/// rendered error so zellij log capture surfaces it.
fn bridge_exit_code<E: std::fmt::Display>(result: Result<ark_hook::BridgeOutcome, E>) -> ExitCode {
    match result {
        Ok(_) => ExitCode::from(0),
        Err(e) => {
            eprintln!("ark-hook: {e}");
            ExitCode::from(1)
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
