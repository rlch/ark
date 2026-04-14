//! CLI error type used by subcommand stubs and future handlers.
//!
//! T-084 only needs a "not yet wired" sentinel so stubs can return a
//! meaningful error that later tasks replace. The exit-code contract
//! from cavekit-cli R8 is T-085 territory, so this module deliberately
//! stays small and does NOT embed code mappings — those will land with
//! T-085 and `CliError` will pick up a `From`/`exit_code()` impl then.

use thiserror::Error;

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
}
