//! Library API for pixel-uecaps-toolbox: inspect and audit Pixel UE-capability files, and
//! compile a complete offline `uecapconfig` folder into a flashable replacement module.

pub(crate) mod atomic;
pub mod compiler;
pub(crate) mod factor;
pub(crate) mod kdl_support;
pub(crate) mod magisk;
pub(crate) mod mapping;
pub mod model;
pub mod outcome;
/// Raw `prost` message types for the on-disk protobuf formats.
///
/// **Not a stable API.** These mirror the wire layout, so their module paths, field names and
/// nesting change whenever the reverse-engineered schema does — the `combo_group::ComboHeader`
/// → `ComboHeader` flattening was one such change, shipped as a `refactor:` against a 1.0.0
/// version because the surface was public by accident rather than by intent. `#[doc(hidden)]`
/// says what was meant all along: this is a command-line tool, and the generated types are an
/// implementation detail. It stays `pub` only because the integration tests in `tests/` consume
/// it from outside the crate.
#[doc(hidden)]
pub mod proto;

/// NR bands are stored offset by this base on the wire. Re-exported for the integration tests
/// in `tests/`, which previously kept a hand-copied literal that could drift from the real
/// constant with no compile error. **Not a stable API**, same as [`proto`].
#[doc(hidden)]
pub use report::combos::NR_BAND_OFFSET;
pub(crate) mod raw_nr;
pub mod report;
pub(crate) mod wire;
