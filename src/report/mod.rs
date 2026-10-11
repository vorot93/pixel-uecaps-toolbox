//! Reports facade: retained inputs, observation-preserving comparison, shared folder validation.
mod check;
mod combos;
mod compare;
mod detail;
mod folder;
mod identity;
mod input;
mod inspect;
mod lte;
mod matrix;
mod selftest;
mod validation;

pub use check::check_folder;
pub use compare::compare;
pub use detail::{Common, Detail};
pub use inspect::inspect;
pub use matrix::matrix;
pub use selftest::self_test;
use std::io::Write;

fn write_report(body: &str, diagnostics: &str) -> anyhow::Result<()> {
    write_report_to(
        &mut std::io::stdout().lock(),
        &mut std::io::stderr().lock(),
        body,
        diagnostics,
    )
}
fn write_report_to(
    stdout: &mut impl Write,
    stderr: &mut impl Write,
    body: &str,
    diagnostics: &str,
) -> anyhow::Result<()> {
    stderr.write_all(diagnostics.as_bytes())?;
    stderr.flush()?;
    stdout.write_all(body.as_bytes())?;
    stdout.flush()?;
    Ok(())
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
    #[test]
    fn report_output_errors_are_returned() {
        let mut sink = Vec::new();
        assert!(write_report_to(&mut FailAfter(2), &mut sink, "complete body", "").is_err());
        assert!(write_report_to(&mut sink, &mut FailAfter(0), "body", "error").is_err());
        assert!(
            sink.is_empty(),
            "diagnostic failure must prevent body publication"
        );
        assert!(write_report_to(&mut FlushFailure, &mut sink, "body", "").is_err());
        assert!(write_report_to(&mut sink, &mut FlushFailure, "body", "error").is_err());
        assert!(sink.is_empty());
    }
}
