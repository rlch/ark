//! `ark list` — scaffold only.
//!
//! Real implementation: T-088 (cavekit-cli R3). Doubles as single-agent
//! status when `[ID]` is passed (absorbs what used to be `ark status`).

use clap::Args;

use crate::ctx::Ctx;
use crate::error::CliError;

/// Arguments for `ark list`.
#[derive(Debug, Args)]
#[command(
    about = "List agents (or show detail for one when [ID] is given)",
    long_about = "Show active and archived agents. With ID, prints the\n\
                  detail view for that agent (what `ark status` would\n\
                  have shown).\n\
                  \n\
                  Examples:\n  \
                  ark list\n  \
                  ark list --watch\n  \
                  ark list myfeat\n  \
                  ark list myfeat --json"
)]
pub struct ListArgs {
    /// ID fragment (exact/prefix/substring). Shows detail if set.
    #[arg(value_name = "ID")]
    pub id: Option<String>,

    /// Filter by orchestrator.
    #[arg(long)]
    pub orchestrator: Option<String>,

    /// Filter by lifecycle status.
    #[arg(long, value_name = "STATUS")]
    pub status: Option<String>,

    /// Emit a JSON array using the `AgentStatus` schema.
    #[arg(long)]
    pub json: bool,

    /// Re-render every 2s, clearing screen between.
    #[arg(long)]
    pub watch: bool,
}

/// Stub handler — replaced by T-088.
pub fn run(_args: ListArgs, _ctx: &Ctx) -> Result<(), CliError> {
    Err(CliError::not_yet_wired("list", "T-088"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[derive(Debug, Parser)]
    struct Host {
        #[command(flatten)]
        args: ListArgs,
    }

    #[test]
    fn bare_list_has_no_id() {
        let h = Host::try_parse_from(["list"]).expect("parse");
        assert!(h.args.id.is_none());
        assert!(!h.args.json);
        assert!(!h.args.watch);
    }

    #[test]
    fn id_positional_parses() {
        let h = Host::try_parse_from(["list", "myfeat"]).expect("parse");
        assert_eq!(h.args.id.as_deref(), Some("myfeat"));
    }

    #[test]
    fn watch_flag_parses() {
        let h = Host::try_parse_from(["list", "--watch"]).expect("parse");
        assert!(h.args.watch);
    }

    #[test]
    fn json_flag_parses() {
        let h = Host::try_parse_from(["list", "--json"]).expect("parse");
        assert!(h.args.json);
    }

    #[test]
    fn status_filter_parses() {
        let h = Host::try_parse_from(["list", "--status", "running"]).expect("parse");
        assert_eq!(h.args.status.as_deref(), Some("running"));
    }

    #[test]
    fn run_returns_not_yet_wired() {
        let args = Host::try_parse_from(["list"]).unwrap().args;
        let err = run(args, &Ctx::default()).expect_err("stub");
        match err {
            CliError::NotYetWired { subcommand, task } => {
                assert_eq!(subcommand, "list");
                assert_eq!(task, "T-088");
            }
        }
    }
}
