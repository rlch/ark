//! `ark config` — scaffold only.
//!
//! Real implementation: T-090 (cavekit-cli R6). Nested subcommands
//! `show`, `edit`, `get`, `set`.

use clap::{Args, Subcommand};

use crate::ctx::Ctx;
use crate::error::CliError;

/// Arguments for `ark config`.
#[derive(Debug, Args)]
#[command(
    about = "Show / edit / get / set configuration values",
    long_about = "Inspect or modify the effective ark configuration.\n\
                  Values are written to\n\
                  $XDG_CONFIG_HOME/ark/config.toml.\n\
                  \n\
                  Examples:\n  \
                  ark config show\n  \
                  ark config get orchestrator.cavekit.default_layout\n  \
                  ark config set \\\n    \
                    orchestrator.cavekit.default_layout triple-stack\n  \
                  ark config edit"
)]
pub struct ConfigArgs {
    #[command(subcommand)]
    pub command: ConfigCommand,
}

/// The four config verbs (R6).
#[derive(Debug, Subcommand)]
pub enum ConfigCommand {
    /// Print the effective config (after figment layering) as TOML.
    ///
    /// Example:
    ///   ark config show
    Show,

    /// Open $EDITOR on the user config file; create from template if missing.
    ///
    /// Example:
    ///   ark config edit
    Edit,

    /// Print a single value by dot-path.
    ///
    /// Example:
    ///   ark config get orchestrator.cavekit.default_layout
    Get {
        /// Dot-path key (e.g. `orchestrator.cavekit.default_layout`).
        #[arg(value_name = "KEY")]
        key: String,
    },

    /// Set a single value by dot-path. Validates before writing.
    ///
    /// Example:
    ///   ark config set orchestrator.cavekit.default_layout triple-stack
    Set {
        /// Dot-path key.
        #[arg(value_name = "KEY")]
        key: String,
        /// Value (TOML-compatible literal).
        #[arg(value_name = "VAL")]
        val: String,
    },
}

/// Stub handler — replaced by T-090.
pub fn run(_args: ConfigArgs, _ctx: &Ctx) -> Result<(), CliError> {
    Err(CliError::not_yet_wired("config", "T-090"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[derive(Debug, Parser)]
    struct Host {
        #[command(flatten)]
        args: ConfigArgs,
    }

    #[test]
    fn show_subcommand_parses() {
        let h = Host::try_parse_from(["config", "show"]).expect("parse");
        assert!(matches!(h.args.command, ConfigCommand::Show));
    }

    #[test]
    fn edit_subcommand_parses() {
        let h = Host::try_parse_from(["config", "edit"]).expect("parse");
        assert!(matches!(h.args.command, ConfigCommand::Edit));
    }

    #[test]
    fn get_subcommand_requires_key() {
        let err = Host::try_parse_from(["config", "get"]).expect_err("need key");
        assert!(err.to_string().contains("KEY") || err.to_string().contains("required"));
    }

    #[test]
    fn get_subcommand_parses_key() {
        let h = Host::try_parse_from(["config", "get", "a.b.c"]).expect("parse");
        match h.args.command {
            ConfigCommand::Get { key } => assert_eq!(key, "a.b.c"),
            other => panic!("expected Get, got {other:?}"),
        }
    }

    #[test]
    fn set_subcommand_parses_key_and_val() {
        let h = Host::try_parse_from(["config", "set", "a.b", "42"]).expect("parse");
        match h.args.command {
            ConfigCommand::Set { key, val } => {
                assert_eq!(key, "a.b");
                assert_eq!(val, "42");
            }
            other => panic!("expected Set, got {other:?}"),
        }
    }

    #[test]
    fn set_subcommand_requires_val() {
        let err = Host::try_parse_from(["config", "set", "a.b"]).expect_err("need val");
        assert!(err.to_string().contains("VAL") || err.to_string().contains("required"));
    }

    #[test]
    fn missing_subcommand_errors() {
        let err = Host::try_parse_from(["config"]).expect_err("need subcommand");
        assert!(
            err.to_string().contains("subcommand")
                || err.to_string().contains("show")
                || err.to_string().contains("required")
        );
    }

    #[test]
    fn run_returns_not_yet_wired() {
        let args = Host::try_parse_from(["config", "show"]).unwrap().args;
        let err = run(args, &Ctx::default()).expect_err("stub");
        match err {
            CliError::NotYetWired { subcommand, task } => {
                assert_eq!(subcommand, "config");
                assert_eq!(task, "T-090");
            }
        }
    }
}
