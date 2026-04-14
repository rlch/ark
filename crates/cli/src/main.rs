//! `ark` binary entry.
//!
//! Scaffold scope (T-084, cavekit-cli.md R1):
//! - clap-derive parser in the lib crate (see `ark_cli::Cli`)
//! - `--version | -V` and `--help | -h` honored automatically
//! - `$NO_COLOR` detected at startup, fed into subcommand context
//! - subcommand stubs return [`CliError::NotYetWired`], which this
//!   entry turns into a non-zero exit until T-085 installs the real
//!   exit-code contract.

use std::process::ExitCode;

use ark_cli::{Cli, CliError, Ctx};
use clap::FromArgMatches;

fn main() -> ExitCode {
    let ctx = Ctx::from_env();

    // Build the clap command with NO_COLOR awareness, then hand it
    // real argv. `get_matches()` handles --version / --help on its own
    // (exiting 0) before we ever see them.
    let cmd = Cli::command_with_no_color_aware(ctx.no_color);
    let matches = cmd.get_matches();
    let cli = match Cli::from_arg_matches(&matches) {
        Ok(c) => c,
        Err(e) => {
            // Shape errors — let clap do its own pretty-print + exit.
            e.exit();
        }
    };

    match cli.command.run(&ctx) {
        Ok(()) => ExitCode::SUCCESS,
        Err(CliError::NotYetWired { subcommand, task }) => {
            eprintln!("ark: `{subcommand}` is not yet wired (waiting on {task})");
            // Exit 1 for now. T-085 replaces this with the R8 contract.
            ExitCode::from(1)
        }
    }
}
