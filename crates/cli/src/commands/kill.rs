//! `ark kill` — scaffold only.
//!
//! Real implementation: T-089 (cavekit-cli R4).

use clap::Args;

use crate::ctx::Ctx;
use crate::error::CliError;

/// Arguments for `ark kill`.
#[derive(Debug, Args)]
#[command(
    about = "Terminate an agent (SIGTERM supervisor; 10s grace)",
    long_about = "Terminate an agent. Default: SIGTERM the supervisor\n\
                  with a 10s grace window for cleanup. Use --force for\n\
                  SIGKILL (orphan cleanup deferred to `ark doctor`).\n\
                  \n\
                  Examples:\n  \
                  ark kill myfeat\n  \
                  ark kill myfeat --force"
)]
pub struct KillArgs {
    /// Agent ID fragment (full / prefix / substring).
    #[arg(value_name = "ID")]
    pub id: String,

    /// SIGKILL immediately (orphan cleanup via `ark doctor`).
    #[arg(long)]
    pub force: bool,

    /// Keep worktree (currently the default in v1; reserved).
    #[arg(long = "keep-worktree")]
    pub keep_worktree: bool,
}

/// Stub handler — replaced by T-089.
pub fn run(_args: KillArgs, _ctx: &Ctx) -> Result<(), CliError> {
    Err(CliError::not_yet_wired("kill", "T-089"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[derive(Debug, Parser)]
    struct Host {
        #[command(flatten)]
        args: KillArgs,
    }

    #[test]
    fn id_is_required() {
        let err = Host::try_parse_from(["kill"]).expect_err("id required");
        assert!(err.to_string().contains("required") || err.to_string().contains("ID"));
    }

    #[test]
    fn id_positional_parses() {
        let h = Host::try_parse_from(["kill", "myfeat"]).expect("parse");
        assert_eq!(h.args.id, "myfeat");
        assert!(!h.args.force);
        assert!(!h.args.keep_worktree);
    }

    #[test]
    fn force_flag_parses() {
        let h = Host::try_parse_from(["kill", "myfeat", "--force"]).expect("parse");
        assert!(h.args.force);
    }

    #[test]
    fn keep_worktree_flag_parses() {
        let h = Host::try_parse_from(["kill", "myfeat", "--keep-worktree"]).expect("parse");
        assert!(h.args.keep_worktree);
    }

    #[test]
    fn run_returns_not_yet_wired() {
        let args = Host::try_parse_from(["kill", "x"]).unwrap().args;
        let err = run(args, &Ctx::default()).expect_err("stub");
        match err {
            CliError::NotYetWired { subcommand, task } => {
                assert_eq!(subcommand, "kill");
                assert_eq!(task, "T-089");
            }
        }
    }
}
