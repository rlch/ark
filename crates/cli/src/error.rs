//! CLI error type.
//!
//! Each variant maps to exactly one [`ExitCode`] via [`CliError::code`].
//!
//! | Variant         | ExitCode        | Numeric |
//! |-----------------|-----------------|---------|
//! | `NotFound`      | `NotFound`      | 3       |
//! | `Ambiguous`     | `NotFound`      | 3       |
//! | `PreflightFail` | `PreflightFail` | 2       |
//! | `OrphanOrDead`  | `OrphanOrDead`  | 4       |
//! | `ConfigError`   | `ConfigError`   | 5       |
//! | `Generic`       | `GenericError`  | 1       |
//! | `Internal`      | `Internal`      | 99      |
//!
//! `Ambiguous` folds into `NotFound` because both surface as "the ID
//! you gave me isn't a unique hit".

use thiserror::Error;

use crate::ExitCode;

/// Top-level CLI error.
///
/// Every variant maps to exactly one [`ExitCode`]; see [`CliError::code`].
#[derive(Debug, Error)]
pub enum CliError {
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
    /// Map this error to its canonical exit code.
    ///
    /// Exhaustive by design — no wildcard arm — so adding a new
    /// `CliError` variant forces the compiler to prompt for an
    /// [`ExitCode`] mapping here.
    pub fn code(&self) -> i32 {
        match self {
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
