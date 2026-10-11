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
mod output;
mod selftest;
mod validation;

pub use check::check_folder;
pub use compare::compare;
pub use detail::{Common, Detail};
pub use inspect::inspect;
pub use matrix::matrix;
pub use output::{OutputTarget, Report, publish};
pub use selftest::self_test;
