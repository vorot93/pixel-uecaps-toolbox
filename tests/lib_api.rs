//! Integration tests against the public library API (compiled as a separate crate,
//! so they only see `pub` items — exactly what WASM bindings in Plan 2 will see).
use pixel_uecaps_toolbox::model::PHONE_MODELS;

#[test]
fn lib_exposes_phone_models() {
    assert_eq!(PHONE_MODELS.len(), 18);
}
