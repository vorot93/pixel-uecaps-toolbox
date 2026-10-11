use crate::proto::{ShannonFeatureSetDlPerCcNr, ShannonFeatureSetUlPerCcNr};

pub(in crate::compiler) const ADDITIONAL_DL_FEATURES: &[ShannonFeatureSetDlPerCcNr] =
    &[ShannonFeatureSetDlPerCcNr {
        max_scs: Some(4),
        max_mimo: Some(1),
        max_bw: Some(200),
        max_mod_order: Some(2),
        bw_90mhz_supported: Some(false),
    }];

pub(in crate::compiler) const ADDITIONAL_UL_FEATURES: &[ShannonFeatureSetUlPerCcNr] =
    &[ShannonFeatureSetUlPerCcNr {
        max_scs: Some(4),
        max_mimo_cb: Some(2),
        max_bw: Some(200),
        max_mod_order: Some(1),
        bw_90mhz_supported: Some(false),
        max_mimo_non_cb: Some(2),
    }];
