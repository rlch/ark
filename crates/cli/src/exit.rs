//! Exit-code constants for the `ark` CLI.
//!
//! T-085, cavekit-cli R8: clear exit semantics.
//!
//! # Table
//!
//! | Code | Constant         | Meaning                                   |
//! |------|------------------|-------------------------------------------|
//! | 0    | `Success`        | Normal completion                         |
//! | 1    | `GenericError`   | Unclassified runtime error                |
//! | 2    | `PreflightFail`  | Preflight / dependency missing            |
//! | 3    | `NotFound`       | Agent / resource ID not found             |
//! | 4    | `OrphanOrDead`   | Orphan or already-dead agent              |
//! | 5    | `ConfigError`    | Config parse or validation error          |
//! | 7    | `NotYetWired`    | Subcommand stub not yet implemented       |
//! | 99   | `Internal`       | Internal / unexpected error               |

/// Exit codes emitted by the `ark` binary.
///
/// Defined as `#[repr(i32)]` so that each variant is directly usable with
/// [`std::process::exit`] via the [`ExitCode::code`] method.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(i32)]
pub enum ExitCode {
    /// Normal completion (0).
    Success = 0,
    /// Unclassified runtime error (1).
    GenericError = 1,
    /// Preflight check or dependency missing (2).
    PreflightFail = 2,
    /// Agent / resource ID not found (3).
    NotFound = 3,
    /// Orphan or already-dead agent (4).
    OrphanOrDead = 4,
    /// Config parse or validation error (5).
    ConfigError = 5,
    /// Subcommand stub not yet wired (7).
    NotYetWired = 7,
    /// Internal / unexpected error (99).
    Internal = 99,
}

impl ExitCode {
    /// Return the numeric exit code.
    #[inline]
    pub fn code(self) -> i32 {
        self as i32
    }
}

#[cfg(test)]
mod tests {
    use super::ExitCode;

    #[test]
    fn success_is_zero() {
        assert_eq!(ExitCode::Success.code(), 0);
    }

    #[test]
    fn generic_error_is_one() {
        assert_eq!(ExitCode::GenericError.code(), 1);
    }

    #[test]
    fn preflight_fail_is_two() {
        assert_eq!(ExitCode::PreflightFail.code(), 2);
    }

    #[test]
    fn not_found_is_three() {
        assert_eq!(ExitCode::NotFound.code(), 3);
    }

    #[test]
    fn orphan_or_dead_is_four() {
        assert_eq!(ExitCode::OrphanOrDead.code(), 4);
    }

    #[test]
    fn config_error_is_five() {
        assert_eq!(ExitCode::ConfigError.code(), 5);
    }

    #[test]
    fn not_yet_wired_is_seven() {
        assert_eq!(ExitCode::NotYetWired.code(), 7);
    }

    #[test]
    fn internal_is_ninety_nine() {
        assert_eq!(ExitCode::Internal.code(), 99);
    }

    #[test]
    fn codes_are_distinct() {
        let mut codes = vec![
            ExitCode::Success.code(),
            ExitCode::GenericError.code(),
            ExitCode::PreflightFail.code(),
            ExitCode::NotFound.code(),
            ExitCode::OrphanOrDead.code(),
            ExitCode::ConfigError.code(),
            ExitCode::NotYetWired.code(),
            ExitCode::Internal.code(),
        ];
        let expected_len = codes.len();
        codes.sort();
        codes.dedup();
        assert_eq!(codes.len(), expected_len, "exit codes must be unique");
    }
}
