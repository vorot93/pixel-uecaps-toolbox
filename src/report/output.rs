use crate::outcome::Outcome;
use anyhow::Context;
use std::{io::Write, path::Path};

/// A completed observation, independent of its eventual output destination.
#[derive(Debug)]
pub struct Report {
    pub body: String,
    pub diagnostics: String,
    pub outcome: Outcome,
}

#[derive(Clone, Copy)]
pub enum OutputTarget<'a> {
    Stdout,
    File(&'a Path),
}

/// Publish diagnostics before committing any body output. I/O failure overrides the conclusion.
pub fn publish(report: &Report, target: OutputTarget<'_>) -> anyhow::Result<Outcome> {
    publish_to(
        report,
        target,
        &mut std::io::stdout().lock(),
        &mut std::io::stderr().lock(),
    )
}

fn publish_to(
    report: &Report,
    target: OutputTarget<'_>,
    stdout: &mut impl Write,
    stderr: &mut impl Write,
) -> anyhow::Result<Outcome> {
    stderr.write_all(report.diagnostics.as_bytes())?;
    stderr.flush()?;
    match target {
        OutputTarget::Stdout => {
            stdout.write_all(report.body.as_bytes())?;
            stdout.flush()?;
        }
        OutputTarget::File(path) => crate::atomic::write_bytes_atomic(path, report.body.as_bytes())
            .with_context(|| format!("cannot write {}", path.display()))?,
    }
    Ok(report.outcome)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct FailAfter(usize);
    impl Write for FailAfter {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            if self.0 == 0 {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::BrokenPipe,
                    "closed",
                ));
            }
            let n = self.0.min(bytes.len());
            self.0 -= n;
            Ok(n)
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    struct FlushFailure;
    impl Write for FlushFailure {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Err(std::io::Error::other("flush failed"))
        }
    }

    fn report(outcome: Outcome) -> Report {
        Report {
            body: "complete body".into(),
            diagnostics: "diagnostic".into(),
            outcome,
        }
    }

    #[test]
    fn stdout_write_and_flush_errors_override_every_outcome() {
        for outcome in [Outcome::Clean, Outcome::Findings, Outcome::Rejected] {
            let report = report(outcome);
            let mut diagnostics = Vec::new();
            assert!(
                publish_to(
                    &report,
                    OutputTarget::Stdout,
                    &mut FailAfter(2),
                    &mut diagnostics
                )
                .is_err()
            );
            assert_eq!(diagnostics, b"diagnostic");
            assert!(
                publish_to(
                    &report,
                    OutputTarget::Stdout,
                    &mut FlushFailure,
                    &mut Vec::new()
                )
                .is_err()
            );
        }
    }

    #[test]
    fn diagnostic_failure_prevents_stdout_publication() {
        let mut stdout = Vec::new();
        let report = report(Outcome::Clean);
        assert!(
            publish_to(
                &report,
                OutputTarget::Stdout,
                &mut stdout,
                &mut FailAfter(2)
            )
            .is_err()
        );
        assert!(stdout.is_empty(), "{stdout:?}");
        assert!(
            publish_to(
                &report,
                OutputTarget::Stdout,
                &mut stdout,
                &mut FlushFailure
            )
            .is_err()
        );
        assert!(stdout.is_empty(), "{stdout:?}");
    }

    fn assert_diagnostic_failure_preserves_file(mut writer: impl Write) {
        let file = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(file.path(), b"existing").unwrap();
        let mut stdout = Vec::new();
        assert!(
            publish_to(
                &report(Outcome::Clean),
                OutputTarget::File(file.path()),
                &mut stdout,
                &mut writer
            )
            .is_err()
        );
        assert_eq!(std::fs::read(file.path()).unwrap(), b"existing");
        assert!(stdout.is_empty(), "{stdout:?}");
    }

    #[test]
    fn diagnostic_write_and_flush_errors_preserve_destination() {
        assert_diagnostic_failure_preserves_file(FailAfter(2));
        assert_diagnostic_failure_preserves_file(FlushFailure);
    }

    #[test]
    fn successful_publication_retains_outcome_and_routes_body_once() {
        for outcome in [Outcome::Clean, Outcome::Findings, Outcome::Rejected] {
            let report = report(outcome);
            let file = tempfile::NamedTempFile::new().unwrap();
            let mut stdout = Vec::new();
            let mut stderr = Vec::new();
            assert_eq!(
                publish_to(
                    &report,
                    OutputTarget::File(file.path()),
                    &mut stdout,
                    &mut stderr
                )
                .unwrap(),
                outcome
            );
            assert!(stdout.is_empty(), "{stdout:?}");
            assert_eq!(stderr, b"diagnostic");
            assert_eq!(std::fs::read(file.path()).unwrap(), b"complete body");
            stderr.clear();
            assert_eq!(
                publish_to(&report, OutputTarget::Stdout, &mut stdout, &mut stderr).unwrap(),
                outcome
            );
            assert_eq!(stdout, b"complete body");
            assert_eq!(stderr, b"diagnostic");
        }
    }
}
