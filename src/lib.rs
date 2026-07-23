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
pub mod proto;
pub(crate) mod raw_nr;
pub mod report;
pub(crate) mod wire;
