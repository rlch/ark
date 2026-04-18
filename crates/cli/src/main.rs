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
//!
//! F-512: clap parsing runs BEFORE `Ctx::from_env()` so that
//! `--help` / `--version` succeed even in environments without
//! `$HOME` / `$XDG_CONFIG_HOME` (clap exits 0 before our code
//! runs). `Ctx::from_env()` is only consulted for the NO_COLOR
//! flag on help rendering via a pre-parse; the authoritative ctx
//! used by subcommands is built after parse succeeds.

use ark_cli::{Cli, Ctx, detect_no_color};
use ark_cli::commands::launch;
use clap::FromArgMatches;
use tracing_subscriber::EnvFilter;

fn main() {
    // F-512: parse clap args FIRST. `--help` / `--version` exit 0
    // from inside clap before we ever return from this call, so
    // they never trigger `Ctx::from_env()`. We read `NO_COLOR` from
    // the process env directly here (cheap, no dir resolution) to
    // drive help coloring; full ctx (state/config/runtime dirs) is
    // resolved only AFTER a subcommand has successfully parsed.
    //
    // F-613: use the shared `detect_no_color` helper so the help path
    // honors the NO_COLOR spec (empty string does NOT disable color),
    // matching the `Ctx::from_env()` path used by subcommands.
    let no_color_for_help = detect_no_color();

    // Set ZELLIJ_SOCKET_DIR=/tmp/ark-<uid> when unset so zellij's socket
    // path stays under the 103-byte sun_path cap on darwin (where $TMPDIR
    // is a ~49-byte /var/folders/... path). Done before any thread is
    // spawned. See `ark_mux_zellij::socket_dir` for rationale.
    let _ = ark_mux_zellij::ensure_short_socket_dir();

    let cmd = Cli::command_with_no_color_aware(no_color_for_help);
    let matches = cmd.get_matches();
    let cli = match Cli::from_arg_matches(&matches) {
        Ok(c) => c,
        Err(e) => {
            // Shape errors — let clap do its own pretty-print + exit.
            e.exit();
        }
    };

    // Now that we know a real subcommand was requested, resolve the
    // runtime context (state/config/runtime dirs, log level).
    let ctx = match Ctx::from_env() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("ark: failed to resolve state dirs: {e}");
            std::process::exit(1);
        }
    };

    // Initialise tracing after ctx is built so we honor ARK_LOG.
    // EnvFilter::new never panics; it just surfaces invalid
    // directives at parse time via the fallback below.
    let filter = EnvFilter::try_new(&ctx.log_level).unwrap_or_else(|_| EnvFilter::new("info"));
    // `.try_init()` because tests may have already set a subscriber;
    // in the binary path this is the first call and always succeeds.
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .try_init();

    let result = match cli.command {
        Some(cmd) => cmd.run(&ctx),
        None => launch::run(cli.scene.as_deref(), cli.session.as_deref(), &ctx),
    };
    if let Err(err) = result {
        eprintln!("ark: {err}");
        std::process::exit(err.code());
    }
}
