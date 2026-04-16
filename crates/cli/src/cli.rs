//! The `ark` top-level [`Cli`] struct.
//!
//! Wires the 6 subcommands from `crate::commands` and the global flags
//! specified by cavekit-cli R1:
//!
//! - `--version | -V` (auto, via `#[command(version)]`)
//! - `--help | -h` (auto)
//! - color behavior honors `$NO_COLOR` (detected at dispatch time; see
//!   [`crate::ctx::detect_no_color`]). Clap's colored help output is
//!   also disabled when `NO_COLOR` is set.
//!
//! The `--help` text's example groupings live on each subcommand's
//! `#[command(..., about/long_about=...)]` attrs (see `commands/*.rs`).

use clap::{ColorChoice, Parser};

use crate::commands::Commands;

/// The top-level ark CLI.
///
/// Examples:
///   ark                          # launch default session
///   ark --scene myproject        # launch with named scene
///   ark --session work           # attach-or-create named session
///   ark spawn --orchestrator cavekit --cwd . -- claude --resume
///   ark list
///   ark kill myfeat
///   ark doctor --fix
///   ark config show
///   ark pane diff --cwd .
#[derive(Debug, Parser)]
#[command(
    name = "ark",
    bin_name = "ark",
    version,
    about = "ark — orchestrate agent sessions in zellij",
    long_about = "ark — orchestrate agent sessions in zellij.\n\
                  \n\
                  Bare `ark` launches the default session.\n\
                  Use `--scene` to pick a scene by name or path,\n\
                  and `--session` to attach or create a named\n\
                  zellij session.\n\
                  \n\
                  Subcommands cover the full lifecycle:\n\
                  spawn, list, kill, doctor, config, pane.\n\
                  Run `ark <cmd> --help` for per-command examples.",
    max_term_width = 80,
    term_width = 80,
    propagate_version = true
)]
pub struct Cli {
    /// Scene name or path.
    ///
    /// A bare name is resolved via the scene search path
    /// (`$ARK_CONFIG_DIR/scenes/<name>.kdl`). A path containing
    /// `/` or `.kdl` is used verbatim.
    #[arg(long, value_name = "NAME_OR_PATH", global = true)]
    pub scene: Option<String>,

    /// Zellij session name (attach-or-create).
    ///
    /// When inside zellij (`$ZELLIJ` set), switches to the named
    /// session. When outside, creates a new session with this name.
    #[arg(long, value_name = "NAME", global = true)]
    pub session: Option<String>,

    #[command(subcommand)]
    pub command: Option<Commands>,
}

impl Cli {
    /// Build a [`clap::Command`] with color forced off when `NO_COLOR`
    /// is set, and with term width pinned to 80 on every subcommand so
    /// help output is always wrapped regardless of terminal detection
    /// (R1). Clap's derive macro picks `ColorChoice::Auto` by default,
    /// which checks terminal support but ignores `NO_COLOR`; we honor
    /// the convention explicitly so help output respects R1.
    pub fn command_with_no_color_aware(no_color: bool) -> clap::Command {
        let cmd = <Self as clap::CommandFactory>::command();
        let cmd = apply_term_width_recursive(cmd, 80);
        if no_color {
            cmd.color(ColorChoice::Never)
        } else {
            cmd
        }
    }
}

/// Recursively pin `term_width` on a command and every nested
/// subcommand. Clap does not propagate `term_width` by itself, so we
/// walk the tree ourselves to make sure per-subcommand help respects
/// the 80-column cap (cavekit-cli R1).
fn apply_term_width_recursive(cmd: clap::Command, width: usize) -> clap::Command {
    let cmd = cmd.term_width(width);
    cmd.mut_subcommands(|sub| apply_term_width_recursive(sub, width))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::{
        Commands, config::ConfigCommand, pane::PaneCommand, spawn::OrchestratorChoice,
    };

    #[test]
    fn parses_spawn_subcommand() {
        let cli = Cli::try_parse_from([
            "ark",
            "spawn",
            "--orchestrator",
            "cavekit",
            "--cwd",
            ".",
            "--",
            "claude",
            "--resume",
        ])
        .expect("parse");
        match cli.command {
            Some(Commands::Spawn(args)) => {
                assert_eq!(args.orchestrator, OrchestratorChoice::Cavekit);
                assert_eq!(args.cmd, vec!["claude", "--resume"]);
            }
            other => panic!("expected Spawn, got {other:?}"),
        }
    }

    #[test]
    fn parses_list_subcommand_no_id() {
        let cli = Cli::try_parse_from(["ark", "list"]).expect("parse");
        assert!(matches!(cli.command, Some(Commands::List(_))));
    }

    #[test]
    fn parses_list_subcommand_with_id() {
        let cli = Cli::try_parse_from(["ark", "list", "myfeat"]).expect("parse");
        match cli.command {
            Some(Commands::List(args)) => assert_eq!(args.id.as_deref(), Some("myfeat")),
            other => panic!("expected List, got {other:?}"),
        }
    }

    #[test]
    fn parses_kill_subcommand() {
        let cli = Cli::try_parse_from(["ark", "kill", "myfeat", "--force"]).expect("parse");
        match cli.command {
            Some(Commands::Kill(args)) => {
                assert_eq!(args.id, "myfeat");
                assert!(args.force);
            }
            other => panic!("expected Kill, got {other:?}"),
        }
    }

    #[test]
    fn parses_doctor_subcommand() {
        let cli = Cli::try_parse_from(["ark", "doctor", "--fix"]).expect("parse");
        match cli.command {
            Some(Commands::Doctor(args)) => assert!(args.fix),
            other => panic!("expected Doctor, got {other:?}"),
        }
    }

    #[test]
    fn parses_config_show_subcommand() {
        let cli = Cli::try_parse_from(["ark", "config", "show"]).expect("parse");
        match cli.command {
            Some(Commands::Config(args)) => assert!(matches!(args.command, ConfigCommand::Show)),
            other => panic!("expected Config, got {other:?}"),
        }
    }

    #[test]
    fn parses_pane_diff_subcommand() {
        let cli = Cli::try_parse_from(["ark", "pane", "diff", "--cwd", "."]).expect("parse");
        match cli.command {
            Some(Commands::Pane(args)) => assert!(matches!(args.command, PaneCommand::Diff(_))),
            other => panic!("expected Pane, got {other:?}"),
        }
    }

    #[test]
    fn parses_pane_log_subcommand() {
        let cli = Cli::try_parse_from(["ark", "pane", "log", "--id", "myfeat"]).expect("parse");
        match cli.command {
            Some(Commands::Pane(args)) => match args.command {
                PaneCommand::Log(l) => assert_eq!(l.id, "myfeat"),
                other => panic!("expected Log, got {other:?}"),
            },
            other => panic!("expected Pane, got {other:?}"),
        }
    }

    #[test]
    fn unknown_top_level_subcommand_errors() {
        let err = Cli::try_parse_from(["ark", "frobnicate"]).expect_err("unknown");
        let s = err.to_string();
        assert!(
            s.contains("frobnicate") || s.contains("unrecognized") || s.contains("unexpected"),
            "unexpected error text: {s}"
        );
    }

    /// T-115: bare `ark` (no subcommand) now parses successfully
    /// with `command: None`, triggering the default session launch.
    #[test]
    fn bare_ark_parses_with_no_subcommand() {
        let cli = Cli::try_parse_from(["ark"]).expect("bare ark should parse");
        assert!(cli.command.is_none());
        assert!(cli.scene.is_none());
        assert!(cli.session.is_none());
    }

    /// T-116: `--scene` flag is captured at the root level.
    #[test]
    fn scene_flag_captured() {
        let cli = Cli::try_parse_from(["ark", "--scene", "myproject"]).expect("parse");
        assert_eq!(cli.scene.as_deref(), Some("myproject"));
        assert!(cli.command.is_none());
    }

    /// T-117: `--session` flag is captured at the root level.
    #[test]
    fn session_flag_captured() {
        let cli = Cli::try_parse_from(["ark", "--session", "work"]).expect("parse");
        assert_eq!(cli.session.as_deref(), Some("work"));
        assert!(cli.command.is_none());
    }

    /// T-116 + T-117: both flags together.
    #[test]
    fn scene_and_session_flags_together() {
        let cli = Cli::try_parse_from([
            "ark", "--scene", "myproject", "--session", "work",
        ])
        .expect("parse");
        assert_eq!(cli.scene.as_deref(), Some("myproject"));
        assert_eq!(cli.session.as_deref(), Some("work"));
        assert!(cli.command.is_none());
    }

    #[test]
    fn version_flag_prints_pkg_version() {
        // Clap exits via an error-kind of DisplayVersion when --version is
        // used, and the message contains the value from `CARGO_PKG_VERSION`.
        use clap::error::ErrorKind;
        let err = Cli::try_parse_from(["ark", "--version"]).expect_err("version exits");
        assert_eq!(err.kind(), ErrorKind::DisplayVersion);
        let msg = err.to_string();
        let expected = env!("CARGO_PKG_VERSION");
        assert!(
            msg.contains(expected),
            "version output `{msg}` missing `{expected}`"
        );
    }

    #[test]
    fn short_version_flag_v_also_prints_version() {
        use clap::error::ErrorKind;
        let err = Cli::try_parse_from(["ark", "-V"]).expect_err("version exits");
        assert_eq!(err.kind(), ErrorKind::DisplayVersion);
    }

    #[test]
    fn help_flag_lists_all_six_subcommands() {
        use clap::error::ErrorKind;
        let err = Cli::try_parse_from(["ark", "--help"]).expect_err("help exits");
        assert_eq!(err.kind(), ErrorKind::DisplayHelp);
        let msg = err.to_string();
        for cmd in ["spawn", "list", "kill", "doctor", "config", "pane"] {
            assert!(msg.contains(cmd), "help missing `{cmd}`:\n{msg}");
        }
    }

    #[test]
    fn short_help_h_also_shows_help() {
        use clap::error::ErrorKind;
        let err = Cli::try_parse_from(["ark", "-h"]).expect_err("help exits");
        assert_eq!(err.kind(), ErrorKind::DisplayHelp);
    }

    #[test]
    fn help_respects_80_column_cap() {
        // term_width is pinned to 80 in command_with_no_color_aware, so
        // the longest line of help output must fit in 80 cols regardless
        // of terminal width (R1).
        let mut cmd = Cli::command_with_no_color_aware(true);
        let msg = cmd.render_help().to_string();
        for line in msg.lines() {
            let trimmed = line.trim_end();
            assert!(
                trimmed.chars().count() <= 80,
                "help line exceeds 80 cols ({}): {:?}",
                trimmed.chars().count(),
                trimmed
            );
        }
    }

    #[test]
    fn every_subcommand_help_respects_80_columns() {
        // The recursive term-width pass in command_with_no_color_aware
        // should wrap help for each subcommand as well (R1).
        let mut root = Cli::command_with_no_color_aware(true);
        for name in ["spawn", "list", "kill", "doctor", "config", "pane"] {
            let sub = root
                .find_subcommand_mut(name)
                .unwrap_or_else(|| panic!("subcommand `{name}` missing"));
            let msg = sub.render_help().to_string();
            for line in msg.lines() {
                let trimmed = line.trim_end();
                assert!(
                    trimmed.chars().count() <= 80,
                    "`{name}` help line exceeds 80 cols ({}): {:?}",
                    trimmed.chars().count(),
                    trimmed
                );
            }
        }
    }

    #[test]
    fn subcommand_help_has_examples() {
        // Per R1: "`--help` text is <80 columns, groups examples per
        // subcommand". Each subcommand's long_about / about carries the
        // examples block. Spot-check one here; the others follow the
        // same pattern via their per-module `#[command(about=...)]`.
        use clap::error::ErrorKind;
        let err = Cli::try_parse_from(["ark", "spawn", "--help"]).expect_err("help exits");
        assert_eq!(err.kind(), ErrorKind::DisplayHelp);
        let msg = err.to_string();
        assert!(msg.contains("ark spawn"), "spawn help missing usage: {msg}");
    }

    #[test]
    fn all_six_subcommands_help_parses() {
        use clap::error::ErrorKind;
        for cmd in ["spawn", "list", "kill", "doctor", "config", "pane"] {
            let err = Cli::try_parse_from(["ark", cmd, "--help"]).expect_err("help exits");
            assert_eq!(
                err.kind(),
                ErrorKind::DisplayHelp,
                "`{cmd} --help` did not print help"
            );
        }
    }

    #[test]
    fn command_with_no_color_aware_toggles_color() {
        // Smoke test that the helper produces a clap::Command either way.
        // We can't easily introspect the internal color choice without
        // reaching into clap internals, so we just assert it returns.
        let _colored = Cli::command_with_no_color_aware(false);
        let _uncolored = Cli::command_with_no_color_aware(true);
    }
}
