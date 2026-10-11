//! Library API for pixel-uecaps-toolbox: inspect and audit Pixel UE-capability files, and
//! compile a complete offline `uecapconfig` folder into a flashable replacement module.

mod atomic;
pub mod compiler;
mod factor;
mod formatting;
mod kdl_support;
mod magisk;
mod mapping;
pub mod model;
pub mod outcome;
/// Raw `prost` message types for the on-disk protobuf formats.
///
/// Exposed for integration fixtures. The library follows current HEAD; these wire-shaped
/// types have no stable compatibility contract.
#[doc(hidden)]
pub mod proto;

/// NR wire-band offset, exposed so integration fixtures share the modeled encoding.
#[doc(hidden)]
pub use raw_nr::NR_BAND_OFFSET;
mod raw_nr;
pub mod report;
mod wire;
