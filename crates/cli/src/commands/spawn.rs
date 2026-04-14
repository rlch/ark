//! `ark spawn` — scaffold only.
//!
//! Real implementation: T-087 (cavekit-cli R2).
//! Every option here is declared to its final shape per R2 so the parse
//! surface is stable; the handler just returns the not-yet-wired error.

use std::path::PathBuf;

use clap::Args;

use crate::ctx::Ctx;
use crate::error::CliError;

/// Orchestrator runtime selected by `--orchestrator`.
///
/// `auto` scans `cwd` at spawn time: `context/sites/` → `cavekit`, else
/// `claude-code` (R2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum OrchestratorChoice {
    Auto,
    Cavekit,
    #[value(name = "claude-code")]
    ClaudeCode,
}

/// Engine selection. Only `claude-code` is valid in v1 — the flag is
/// accepted so end-state scripts stay stable. See cavekit-cli R2.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum EngineChoice {
    #[value(name = "claude-code")]
    ClaudeCode,
}

/// Arguments for `ark spawn`.
#[derive(Debug, Args)]
#[command(
    about = "Spawn a new agent in a dedicated zellij session",
    long_about = "Create a new agent in a dedicated zellij session.\n\
                  Positional arguments after `--` are the agent pane\n\
                  command.\n\
                  \n\
                  Examples:\n  \
                  ark spawn --orchestrator cavekit --cwd . -- \\\n    \
                    claude --resume\n  \
                  ark spawn --orchestrator claude-code -- claude\n  \
                  ark spawn --name authsvc -- claude --resume"
)]
pub struct SpawnArgs {
    /// Orchestrator runtime. Values: auto|cavekit|claude-code.
    #[arg(
        long,
        value_enum,
        default_value_t = OrchestratorChoice::Auto,
        hide_default_value = true,
        hide_possible_values = true,
    )]
    pub orchestrator: OrchestratorChoice,

    /// Engine (v1: only `claude-code`).
    #[arg(
        long,
        value_enum,
        default_value_t = EngineChoice::ClaudeCode,
        hide_default_value = true,
        hide_possible_values = true,
    )]
    pub engine: EngineChoice,

    /// Worktree path (default: current directory).
    #[arg(long, default_value = ".")]
    pub cwd: PathBuf,

    /// Human-readable label (default: derived from cwd basename).
    #[arg(long)]
    pub name: Option<String>,

    /// KDL layout stem (e.g. `builder`) or absolute path.
    #[arg(long)]
    pub layout: Option<String>,

    /// Environment variables to pass through (KEY=VAL, repeatable).
    #[arg(long = "env", value_name = "KEY=VAL")]
    pub env: Vec<String>,

    /// Detach after spawn (default: true).
    #[arg(long, default_value_t = true, overrides_with = "no_detach")]
    pub detach: bool,

    /// Stay in foreground with log stream instead of detaching.
    #[arg(long = "no-detach", conflicts_with = "detach")]
    pub no_detach: bool,

    /// Hook wiring (EVENT=CMD, repeatable). See cavekit-hooks.md.
    #[arg(long = "hook", value_name = "EVENT=CMD")]
    pub hook: Vec<String>,

    /// Positional command to run in the agent pane — everything after `--`.
    #[arg(last = true, value_name = "CMD")]
    pub cmd: Vec<String>,
}

/// Stub handler — replaced by T-087.
pub fn run(_args: SpawnArgs, _ctx: &Ctx) -> Result<(), CliError> {
    Err(CliError::not_yet_wired("spawn", "T-087"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    /// Minimal host parser so we can parse `SpawnArgs` in isolation.
    #[derive(Debug, Parser)]
    struct Host {
        #[command(flatten)]
        args: SpawnArgs,
    }

    #[test]
    fn orchestrator_defaults_to_auto() {
        let h = Host::try_parse_from(["spawn", "--", "claude"]).expect("parse");
        assert_eq!(h.args.orchestrator, OrchestratorChoice::Auto);
    }

    #[test]
    fn orchestrator_accepts_cavekit() {
        let h = Host::try_parse_from(["spawn", "--orchestrator", "cavekit", "--", "claude"])
            .expect("parse");
        assert_eq!(h.args.orchestrator, OrchestratorChoice::Cavekit);
    }

    #[test]
    fn orchestrator_accepts_claude_code() {
        let h = Host::try_parse_from(["spawn", "--orchestrator", "claude-code", "--", "claude"])
            .expect("parse");
        assert_eq!(h.args.orchestrator, OrchestratorChoice::ClaudeCode);
    }

    #[test]
    fn cmd_captures_trailing_args() {
        let h = Host::try_parse_from(["spawn", "--", "claude", "--resume"]).expect("parse");
        assert_eq!(h.args.cmd, vec!["claude", "--resume"]);
    }

    #[test]
    fn env_is_repeatable() {
        let h = Host::try_parse_from(["spawn", "--env", "A=1", "--env", "B=2", "--", "claude"])
            .expect("parse");
        assert_eq!(h.args.env, vec!["A=1", "B=2"]);
    }

    #[test]
    fn hook_is_repeatable() {
        let h = Host::try_parse_from([
            "spawn",
            "--hook",
            "Stop=echo done",
            "--hook",
            "Start=echo go",
            "--",
            "claude",
        ])
        .expect("parse");
        assert_eq!(h.args.hook.len(), 2);
    }

    #[test]
    fn cwd_defaults_to_dot() {
        let h = Host::try_parse_from(["spawn", "--", "claude"]).expect("parse");
        assert_eq!(h.args.cwd, PathBuf::from("."));
    }

    #[test]
    fn no_detach_flag_parses() {
        let h = Host::try_parse_from(["spawn", "--no-detach", "--", "claude"]).expect("parse");
        assert!(h.args.no_detach);
    }

    #[test]
    fn run_returns_not_yet_wired() {
        let args = Host::try_parse_from(["spawn", "--", "claude"])
            .unwrap()
            .args;
        let err = run(args, &Ctx::default()).expect_err("stub");
        assert!(matches!(
            err,
            CliError::NotYetWired {
                subcommand: "spawn",
                task: "T-087",
            }
        ));
    }
}
