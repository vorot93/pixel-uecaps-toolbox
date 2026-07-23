//! Library API for pixel-uecaps-toolbox: decode/validate/patch Pixel UE-capability
//! files and provision flashable Magisk modules.

pub(crate) mod atomic;
pub mod compiler;
pub mod factor;
pub(crate) mod kdl_support;
pub mod magisk;
pub mod mapping;
pub mod model;
pub mod proto;
pub(crate) mod raw_nr;
pub mod report;
pub(crate) mod wire;
