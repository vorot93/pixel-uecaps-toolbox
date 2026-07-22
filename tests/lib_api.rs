//! Integration tests against the public library API (compiled as a separate crate,
//! so they only see `pub` items — exactly what WASM bindings in Plan 2 will see).
use pixel_uecaps_toolbox::model::{
    CapabilityLayout, ModelInfo, PHONE_MODELS, device_model, device_model_layout, phone_model,
};
use std::path::Path;

#[test]
fn lib_exposes_folder_compiler_entry_points() {
    let _: fn(&Path, &Path, &Path) -> anyhow::Result<i32> =
        pixel_uecaps_toolbox::compiler::decompose;
    let _: fn(&str, &Path, &Path, Option<&str>) -> anyhow::Result<i32> =
        pixel_uecaps_toolbox::compiler::build;
}

#[test]
fn lib_exposes_the_one_file_decode_entry_point() {
    let _: fn(&Path, Option<pixel_uecaps_toolbox::decode::Kind>) -> anyhow::Result<i32> =
        pixel_uecaps_toolbox::decode::run;
}

#[test]
fn lib_exposes_the_mapping_legend_codec() {
    let _: fn(&[u8]) -> anyhow::Result<Vec<u8>> = pixel_uecaps_toolbox::mapping::decode_bytes;
    let _: fn(&[u8]) -> anyhow::Result<Vec<u8>> = pixel_uecaps_toolbox::mapping::encode_bytes;
}

#[test]
fn lib_exposes_phone_models() {
    assert_eq!(PHONE_MODELS.len(), 52);
    assert_eq!(
        PHONE_MODELS
            .iter()
            .filter(|model| model.layout == CapabilityLayout::Bitmask)
            .count(),
        34
    );
    assert_eq!(
        PHONE_MODELS
            .iter()
            .filter(|model| matches!(model.layout, CapabilityLayout::Profiled { .. }))
            .count(),
        18
    );
}

#[test]
fn lib_keeps_profiled_model_info_compatible() {
    let model = phone_model("GUL82").unwrap();
    let info = ModelInfo::from(model);
    assert_eq!(info.lte_id, 1_254_026_417);
    assert_eq!(info.nr_anchor, 3_616_442_437);
    assert_eq!(device_model(" gul82\n"), Some(info));

    assert_eq!(
        device_model_layout(" g0dzq\n"),
        Some(CapabilityLayout::Bitmask)
    );
    assert!(device_model("G0DZQ").is_none());
}
