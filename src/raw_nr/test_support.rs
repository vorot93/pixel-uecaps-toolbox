use super::RawNrPayload;
use crate::proto::{ComboHeader, SubBlock, UeCaps};

/// The four header fields required by strict ingestion; intra-ENDC remains absent.
pub(crate) fn full_header() -> Option<ComboHeader> {
    Some(ComboHeader {
        power_class: Some(0),
        bcs_nr: Some(0),
        bcs_intra_endc: None,
        bcs_eutra: Some(0),
        intra_band_en_dc_support: Some(0),
    })
}

/// Deliberately absent header fields for feature-only or presence-sensitive fixtures.
pub(crate) fn empty_payload() -> RawNrPayload {
    RawNrPayload {
        header: ComboHeader::default(),
        sub_blocks: Vec::new(),
    }
}

/// Class-1 placeholder directions with independently controllable SRS switching.
pub(crate) fn cc(band: i32, srs_tx_switch: i32) -> SubBlock {
    SubBlock {
        band,
        dl_bw_class: Some(1),
        ul_bw_class: Some(1),
        dl_feature_per_cc_ids: Some(vec![0]),
        ul_feature_per_cc_ids: Some(vec![0]),
        srstxswitch: Some(srs_tx_switch),
        ..Default::default()
    }
}

pub(crate) fn raw_payloads(caps: &UeCaps) -> Vec<RawNrPayload> {
    RawNrPayload::all_from_caps(caps).expect("fixture caps must be representable")
}
