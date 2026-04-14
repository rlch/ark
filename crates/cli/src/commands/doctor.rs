//! `ark doctor` — scaffold only.
//!
//! Real implementation: T-091 (cavekit-cli R5). Folds in the old `gc`
//! and `plugin install` subcommands (both gated behind `--fix`).

use clap::Args;

use crate::ctx::Ctx;
use crate::error::CliError;

/// Arguments for `ark doctor`.
#[derive(Debug, Args)]
#[command(
    about = "Diagnose environment; with --fix, prompt to remediate",
    long_about = "Run environment checks (zellij >= 0.44, delta, claude,\n\
                  plugin install, stale locks, orphan state). --fix\n\
                  prompts per item (folds in old `gc` + `plugin\n\
                  install` commands).\n\
                  \n\
                  Examples:\n  \
                  ark doctor\n  \
                  ark doctor --fix\n  \
                  ark doctor --fix --yes"
)]
pub struct DoctorArgs {
    /// Prompt to remediate each fixable finding.
    #[arg(long)]
    pub fix: bool,

    /// Auto-accept all prompts when combined with --fix.
    #[arg(long, requires = "fix")]
    pub yes: bool,
}

/// Stub handler — replaced by T-091.
pub fn run(_args: DoctorArgs, _ctx: &Ctx) -> Result<(), CliError> {
    Err(CliError::not_yet_wired("doctor", "T-091"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[derive(Debug, Parser)]
    struct Host {
        #[command(flatten)]
        args: DoctorArgs,
    }

    #[test]
    fn bare_doctor_parses() {
        let h = Host::try_parse_from(["doctor"]).expect("parse");
        assert!(!h.args.fix);
        assert!(!h.args.yes);
    }

    #[test]
    fn fix_flag_parses() {
        let h = Host::try_parse_from(["doctor", "--fix"]).expect("parse");
        assert!(h.args.fix);
    }

    #[test]
    fn yes_requires_fix() {
        let err = Host::try_parse_from(["doctor", "--yes"]).expect_err("needs --fix");
        assert!(err.to_string().contains("--fix") || err.to_string().contains("required"));
    }

    #[test]
    fn fix_yes_both_parse() {
        let h = Host::try_parse_from(["doctor", "--fix", "--yes"]).expect("parse");
        assert!(h.args.fix);
        assert!(h.args.yes);
    }

    #[test]
    fn run_returns_not_yet_wired() {
        let args = Host::try_parse_from(["doctor"]).unwrap().args;
        let err = run(args, &Ctx::default()).expect_err("stub");
        assert!(matches!(
            err,
            CliError::NotYetWired {
                subcommand: "doctor",
                task: "T-091",
            }
        ));
    }
}
