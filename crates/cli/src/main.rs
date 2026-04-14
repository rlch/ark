//! `ark` binary entry.
//!
//! T-084 (cavekit-cli R1): clap-derive parser, `--version`/`--help`,
//! `$NO_COLOR` detection, and subcommand dispatch.
//!
//! T-085 (cavekit-cli R8): all errors are printed to stderr and the
//! process exits with the canonical code from [`ark_cli::CliError::code`].

use ark_cli::{Cli, Ctx};
use clap::FromArgMatches;

fn main() {
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

    if let Err(err) = cli.command.run(&ctx) {
        eprintln!("ark: {err}");
        std::process::exit(err.code());
    }
}
