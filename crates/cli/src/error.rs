//! CLI error type used by subcommand stubs and future handlers.
//!
//! T-085 (cavekit-cli R8) adds [`CliError::code`] which maps every
//! variant to its canonical [`crate::ExitCode`].
//!
//! # Variant → ExitCode mapping
//!
//! | Variant         | ExitCode        | Numeric |
//! |-----------------|-----------------|---------|
//! | `NotYetWired`   | `NotYetWired`   | 7       |
//! | `NotFound`      | `NotFound`      | 3       |
//! | `Ambiguous`     | `NotFound`      | 3       |
//! | `PreflightFail` | `PreflightFail` | 2       |
//! | `OrphanOrDead`  | `OrphanOrDead`  | 4       |
//! | `ConfigError`   | `ConfigError`   | 5       |
//! | `Generic`       | `GenericError`  | 1       |
//! | `Internal`      | `Internal`      | 99      |
//!
//! `Ambiguous` folds into `NotFound` because `exit.rs` does not
//! allocate a distinct code for ambiguous-ID failures — both surface
//! as "the ID you gave me isn't a unique hit".

use thiserror::Error;

use crate::ExitCode;

/// Top-level CLI error.
///
/// Every variant maps to exactly one [`ExitCode`]; see [`CliError::code`].
#[derive(Debug, Error)]
pub enum CliError {
    /// A subcommand handler has not been wired yet. The argument is the
    /// task ID (e.g. `"T-087"`) that will replace the stub.
    #[error("subcommand `{subcommand}` is not yet wired (waiting on {task})")]
    NotYetWired {
        /// Which subcommand the user invoked.
        subcommand: &'static str,
        /// Task ID responsible for implementing this subcommand.
        task: &'static str,
    },
    /// Agent / resource id not found (exit 3).
    #[error("not found: {what}")]
    NotFound {
        /// What was being looked up (e.g. `"agent \"abc\""`).
        what: String,
    },
    /// Ambiguous ID fragment — multiple matches (exit 3).
    #[error("ambiguous {what}: {} candidates", candidates.len())]
    Ambiguous {
        /// What was being looked up.
        what: String,
        /// All matching candidates.
        candidates: Vec<String>,
    },
    /// Preflight / dependency check failed (exit 2).
    #[error("preflight failed: {reason}")]
    PreflightFail {
        /// Which check failed and why.
        reason: String,
    },
    /// Orphan or already-dead agent (exit 4).
    #[error("orphan or dead: {reason}")]
    OrphanOrDead {
        /// Why the agent is orphaned / dead.
        reason: String,
    },
    /// Config parse / validation error (exit 5).
    #[error("config error: {reason}")]
    ConfigError {
        /// What is wrong with the config.
        reason: String,
    },
    /// Unclassified runtime error (exit 1).
    #[error("{reason}")]
    Generic {
        /// Human-readable description.
        reason: String,
    },
    /// Internal / unexpected error (exit 99).
    #[error("internal error: {reason}")]
    Internal {
        /// Human-readable description.
        reason: String,
    },
}

impl CliError {
    /// Constructor helper used by every stub.
    pub const fn not_yet_wired(subcommand: &'static str, task: &'static str) -> Self {
        Self::NotYetWired { subcommand, task }
    }

    /// Map this error to its canonical exit code (cavekit-cli R8).
    ///
    /// Exhaustive by design — no wildcard arm — so adding a new
    /// `CliError` variant forces the compiler to prompt for an
    /// [`ExitCode`] mapping here.
    pub fn code(&self) -> i32 {
        match self {
            Self::NotYetWired { .. } => ExitCode::NotYetWired.code(),
            Self::NotFound { .. } => ExitCode::NotFound.code(),
            Self::Ambiguous { .. } => ExitCode::NotFound.code(),
            Self::PreflightFail { .. } => ExitCode::PreflightFail.code(),
            Self::OrphanOrDead { .. } => ExitCode::OrphanOrDead.code(),
            Self::ConfigError { .. } => ExitCode::ConfigError.code(),
            Self::Generic { .. } => ExitCode::GenericError.code(),
            Self::Internal { .. } => ExitCode::Internal.code(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn not_yet_wired_display() {
        let e = CliError::not_yet_wired("spawn", "T-087");
        assert_eq!(
            format!("{e}"),
            "subcommand `spawn` is not yet wired (waiting on T-087)"
        );
    }

    #[test]
    fn not_yet_wired_carries_task_id() {
        assert!(matches!(
            CliError::not_yet_wired("list", "T-088"),
            CliError::NotYetWired {
                subcommand: "list",
                task: "T-088",
            }
        ));
    }

    #[test]
    fn not_yet_wired_exit_code_is_seven() {
        let e = CliError::not_yet_wired("spawn", "T-087");
        assert_eq!(e.code(), 7);
    }

    #[test]
    fn not_yet_wired_maps_to_exit_code_enum() {
        // Guards against drift between `CliError::code()` and the
        // canonical `ExitCode::NotYetWired` constant (cavekit-cli R8).
        let e = CliError::not_yet_wired("list", "T-088");
        assert_eq!(e.code(), ExitCode::NotYetWired as i32);
    }

    #[test]
    fn not_found_maps_to_exit_3() {
        let e = CliError::NotFound {
            what: "agent \"abc\"".into(),
        };
        assert_eq!(e.code(), ExitCode::NotFound as i32);
        assert_eq!(e.code(), 3);
    }

    #[test]
    fn ambiguous_maps_to_exit_3() {
        let e = CliError::Ambiguous {
            what: "agent".into(),
            candidates: vec!["a".into(), "b".into()],
        };
        assert_eq!(e.code(), ExitCode::NotFound as i32);
        assert_eq!(e.code(), 3);
    }

    #[test]
    fn preflight_fail_maps_to_exit_2() {
        let e = CliError::PreflightFail {
            reason: "tmux not found".into(),
        };
        assert_eq!(e.code(), ExitCode::PreflightFail as i32);
        assert_eq!(e.code(), 2);
    }

    #[test]
    fn orphan_or_dead_maps_to_exit_4() {
        let e = CliError::OrphanOrDead {
            reason: "pid gone".into(),
        };
        assert_eq!(e.code(), ExitCode::OrphanOrDead as i32);
        assert_eq!(e.code(), 4);
    }

    #[test]
    fn config_error_maps_to_exit_5() {
        let e = CliError::ConfigError {
            reason: "parse failure".into(),
        };
        assert_eq!(e.code(), ExitCode::ConfigError as i32);
        assert_eq!(e.code(), 5);
    }

    #[test]
    fn generic_maps_to_exit_1() {
        let e = CliError::Generic {
            reason: "boom".into(),
        };
        assert_eq!(e.code(), ExitCode::GenericError as i32);
        assert_eq!(e.code(), 1);
    }

    #[test]
    fn internal_maps_to_exit_99() {
        let e = CliError::Internal {
            reason: "unreachable hit".into(),
        };
        assert_eq!(e.code(), ExitCode::Internal as i32);
        assert_eq!(e.code(), 99);
    }

    #[test]
    fn ambiguous_display_includes_candidate_count() {
        let e = CliError::Ambiguous {
            what: "id".into(),
            candidates: vec!["one".into(), "two".into(), "three".into()],
        };
        assert_eq!(format!("{e}"), "ambiguous id: 3 candidates");
    }
}
