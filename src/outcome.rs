//! What a command concluded, independent of how the process reports it.

use std::process::ExitCode;

/// A command's conclusion. The discriminants are the historical exit codes, so the mapping to
/// `ExitCode` is total and obvious — but call sites read the *name*, not the number.
///
/// `Rejected` is distinct from an `Err`: the command already printed its own diagnostic and
/// wants exit code 2 without duplicating `main`'s `error: {e:#}` envelope, including reports
/// that emit useful partial output after collecting validation errors.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Outcome {
    /// The command completed without errors (a comparison may still report differences).
    Clean = 0,
    /// A failed runtime self-test. Report validation failures use `Rejected` instead.
    Findings = 1,
    /// The command rejected its input and has already said why on stderr.
    Rejected = 2,
}

impl From<Outcome> for ExitCode {
    fn from(outcome: Outcome) -> Self {
        Self::from(outcome as u8)
    }
}

/// `true` → [`Outcome::Findings`], `false` → [`Outcome::Clean`]. The one place the old
/// `i32::from(bool)` idiom is allowed to survive, named.
impl From<bool> for Outcome {
    fn from(has_findings: bool) -> Self {
        if has_findings {
            Self::Findings
        } else {
            Self::Clean
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Outcome;

    #[test]
    fn findings_flag_maps_to_the_two_success_outcomes() {
        assert_eq!(Outcome::from(false), Outcome::Clean);
        assert_eq!(Outcome::from(true), Outcome::Findings);
    }
}
