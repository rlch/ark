//! `ark pane` — scaffold only.
//!
//! Real routing: T-092 (cavekit-cli R7). The individual pane widgets
//! (`diff`, `git`, `log`) are already implemented in the `ark-pane`
//! crate (T-040/T-041/T-042); T-092 just wires the CLI subcommands to
//! those widget entry points.
//!
//! For T-084 we only scaffold the nested subcommand surface.

use std::path::PathBuf;

use clap::{Args, Subcommand};

use crate::ctx::Ctx;
use crate::error::CliError;

/// Arguments for `ark pane`.
#[derive(Debug, Args)]
#[command(
    about = "Pane composability primitives (invoked by KDL layouts)",
    long_about = "Pane commands intended for use inside zellij KDL\n\
                  layouts. Each command honors SIGWINCH and exits on\n\
                  q/Esc/Ctrl+C.\n\
                  \n\
                  Examples:\n  \
                  ark pane diff --cwd .\n  \
                  ark pane git  --cwd .\n  \
                  ark pane log  --id myfeat"
)]
pub struct PaneArgs {
    #[command(subcommand)]
    pub command: PaneCommand,
}

/// The three pane commands (R7).
#[derive(Debug, Subcommand)]
pub enum PaneCommand {
    /// Watch-mode git diff (delta + ratatui).
    ///
    /// Example:
    ///   ark pane diff --cwd .
    Diff(DiffArgs),

    /// Compact git status widget (branch, staged, unstaged, last commit).
    ///
    /// Example:
    ///   ark pane git --cwd .
    Git(GitArgs),

    /// Tail `events.jsonl` for an agent, pretty-printed.
    ///
    /// Example:
    ///   ark pane log --id myfeat
    Log(LogArgs),
}

/// Arguments for `ark pane diff`.
#[derive(Debug, Args)]
pub struct DiffArgs {
    /// Worktree to watch.
    #[arg(long, default_value = ".")]
    pub cwd: PathBuf,
}

/// Arguments for `ark pane git`.
#[derive(Debug, Args)]
pub struct GitArgs {
    /// Worktree to inspect.
    #[arg(long, default_value = ".")]
    pub cwd: PathBuf,
}

/// Arguments for `ark pane log`.
#[derive(Debug, Args)]
pub struct LogArgs {
    /// Agent ID fragment whose events.jsonl to tail.
    #[arg(long)]
    pub id: String,
}

/// Stub handler — replaced by T-092.
pub fn run(_args: PaneArgs, _ctx: &Ctx) -> Result<(), CliError> {
    Err(CliError::not_yet_wired("pane", "T-092"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[derive(Debug, Parser)]
    struct Host {
        #[command(flatten)]
        args: PaneArgs,
    }

    #[test]
    fn diff_subcommand_parses_default_cwd() {
        let h = Host::try_parse_from(["pane", "diff"]).expect("parse");
        match h.args.command {
            PaneCommand::Diff(d) => assert_eq!(d.cwd, PathBuf::from(".")),
            other => panic!("expected Diff, got {other:?}"),
        }
    }

    #[test]
    fn diff_subcommand_accepts_explicit_cwd() {
        let h = Host::try_parse_from(["pane", "diff", "--cwd", "/tmp/x"]).expect("parse");
        match h.args.command {
            PaneCommand::Diff(d) => assert_eq!(d.cwd, PathBuf::from("/tmp/x")),
            other => panic!("expected Diff, got {other:?}"),
        }
    }

    #[test]
    fn git_subcommand_parses() {
        let h = Host::try_parse_from(["pane", "git"]).expect("parse");
        assert!(matches!(h.args.command, PaneCommand::Git(_)));
    }

    #[test]
    fn log_subcommand_requires_id() {
        let err = Host::try_parse_from(["pane", "log"]).expect_err("need id");
        assert!(
            err.to_string().contains("--id")
                || err.to_string().contains("id")
                || err.to_string().contains("required")
        );
    }

    #[test]
    fn log_subcommand_parses_id() {
        let h = Host::try_parse_from(["pane", "log", "--id", "myfeat"]).expect("parse");
        match h.args.command {
            PaneCommand::Log(l) => assert_eq!(l.id, "myfeat"),
            other => panic!("expected Log, got {other:?}"),
        }
    }

    #[test]
    fn missing_subcommand_errors() {
        let err = Host::try_parse_from(["pane"]).expect_err("need subcommand");
        assert!(
            err.to_string().contains("subcommand")
                || err.to_string().contains("diff")
                || err.to_string().contains("required")
        );
    }

    #[test]
    fn unknown_pane_subcommand_errors() {
        let err = Host::try_parse_from(["pane", "frobnicate"]).expect_err("unknown");
        assert!(
            err.to_string().contains("frobnicate")
                || err.to_string().contains("unrecognized")
                || err.to_string().contains("unexpected")
        );
    }

    #[test]
    fn run_returns_not_yet_wired() {
        let args = Host::try_parse_from(["pane", "git"]).unwrap().args;
        let err = run(args, &Ctx::default()).expect_err("stub");
        assert!(matches!(
            err,
            CliError::NotYetWired {
                subcommand: "pane",
                task: "T-092",
            }
        ));
    }
}
