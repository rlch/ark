//! `ark` binary entry.
//!
//! T-084 (cavekit-cli R1): clap-derive parser, `--version`/`--help`,
//! `$NO_COLOR` detection, and subcommand dispatch.
//!
//! T-085 (cavekit-cli R8): all errors are printed to stderr and the
//! process exits with the canonical code from [`ark_cli::CliError::code`].
//!
//! T-093 (cavekit-cli R8): honor `ARK_LOG` / `RUST_LOG` for the tracing
//! subscriber filter, and resolve `ARK_STATE_DIR` / `ARK_CONFIG_DIR` /
//! `ARK_RUNTIME_DIR` into the [`Ctx`] via [`Ctx::from_env`].

use ark_cli::{Cli, Ctx};
use clap::FromArgMatches;
use tracing_subscriber::EnvFilter;

fn main() {
    let ctx = match Ctx::from_env() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("ark: failed to resolve state dirs: {e}");
            std::process::exit(1);
        }
    };

    // Initialise tracing as early as possible so later code can emit
    // events. EnvFilter::new never panics; it just surfaces invalid
    // directives at parse time via the fallback below.
    let filter = EnvFilter::try_new(&ctx.log_level).unwrap_or_else(|_| EnvFilter::new("info"));
    // `.try_init()` because tests may have already set a subscriber;
    // in the binary path this is the first call and always succeeds.
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .try_init();

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
