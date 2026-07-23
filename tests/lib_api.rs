//! Integration tests against the public library API (compiled as a separate crate,
//! so they only see `pub` items — exactly what WASM bindings in Plan 2 will see).
use pixel_uecaps_toolbox::model::{
    CapabilityLayout, ModelInfo, PHONE_MODELS, device_model, device_model_layout, phone_model,
};
use std::path::Path;

#[test]
fn lib_exposes_outcome_instead_of_raw_exit_codes() {
    use pixel_uecaps_toolbox::outcome::Outcome;
    let _: fn(&Path, &Path, &Path) -> anyhow::Result<Outcome> =
        pixel_uecaps_toolbox::compiler::decompose;
    let _: fn(&str, &Path, &Path, Option<&str>) -> anyhow::Result<Outcome> =
        pixel_uecaps_toolbox::compiler::provision;
    // The three outcomes map onto the historical exit codes 0 / 1 / 2.
    assert_eq!(Outcome::Clean as u8, 0);
    assert_eq!(Outcome::Findings as u8, 1);
    assert_eq!(Outcome::Rejected as u8, 2);
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
    let info = ModelInfo::try_from(model).expect("GUL82 is a profiled model");
    assert_eq!(info.lte_id, 1_254_026_417);
    assert_eq!(info.nr_anchor, 3_616_442_437);
    assert_eq!(device_model(" gul82\n"), Some(info));

    assert_eq!(
        device_model_layout(" g0dzq\n"),
        Some(CapabilityLayout::Bitmask)
    );
    assert!(device_model("G0DZQ").is_none());
    // A bitmask model has no profiled selectors, so the conversion fails rather than panics.
    let bitmask = phone_model("G0DZQ").unwrap();
    assert!(ModelInfo::try_from(bitmask).is_err());
}
