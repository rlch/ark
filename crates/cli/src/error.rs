//! CLI error type used by subcommand stubs and future handlers.
//!
//! T-085 (cavekit-cli R8) adds [`CliError::code`] which maps every
//! variant to its canonical [`crate::ExitCode`].

use thiserror::Error;

use crate::ExitCode;

/// Top-level CLI error.
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
}

impl CliError {
    /// Constructor helper used by every stub.
    pub const fn not_yet_wired(subcommand: &'static str, task: &'static str) -> Self {
        Self::NotYetWired { subcommand, task }
    }

    /// Map this error to its canonical exit code (cavekit-cli R8).
    pub fn code(&self) -> i32 {
        match self {
            Self::NotYetWired { .. } => ExitCode::NotYetWired.code(),
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
        match CliError::not_yet_wired("list", "T-088") {
            CliError::NotYetWired { subcommand, task } => {
                assert_eq!(subcommand, "list");
                assert_eq!(task, "T-088");
            }
        }
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
}
