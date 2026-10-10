//! Reports facade: retained inputs, observation-preserving comparison, shared folder validation.
mod check;
pub(crate) mod combos;
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
    let mut stderr = std::io::stderr().lock();
    stderr.write_all(diagnostics.as_bytes())?;
    stderr.flush()?;
    let mut stdout = std::io::stdout().lock();
    stdout.write_all(body.as_bytes())?;
    stdout.flush()?;
    Ok(())
}
