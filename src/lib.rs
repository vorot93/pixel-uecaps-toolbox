//! Library API for pixel-uecaps-toolbox: decode/validate/patch Pixel UE-capability
//! files and provision flashable Magisk modules. Drives both the CLI (`src/main.rs`)
//! and, via Plan 2's wasm-bindgen wrapper, the pixel5g.vorotnikov.me web installer.

pub mod factor;
pub mod magisk;
pub mod mapping;
pub mod model;
pub mod patch;
pub mod proto;
pub mod provision;
pub mod report;
