use super::{LegacyNrFile, ProfiledNrFile, ingest_nr};
use crate::{
    compiler::schema::{
        LteDocument, LteFileSource, NrDocument, SourceDocument, ValidatedNr, parse_sources, to_kdl,
    },
    mapping::{MappingEntry, MappingRoot},
    proto::{Combo, ComboGroup, ComboHeader, ShannonFeatureSetDlPerCcNr, SubBlock, UeCaps},
    raw_nr::RawNrPayload,
};
use std::collections::BTreeMap;
/// A component that differs from its siblings only in `srs_tx_switch`. (It used to vary
/// `dl_feature_index`, but an NR component no longer stores one — the index is derived,
/// so two otherwise-identical NR components can no longer differ in it.)
pub(crate) fn cc(band: i32, srs_tx_switch: i32) -> SubBlock {
    SubBlock {
        band,
        dl_bw_class: Some(1),
        ul_bw_class: Some(1),
        // A class-1 direction always carries a one-byte per-CC list; the biconditional in
        // `RawSubBlock::validate` requires it, and every real file has it.
        dl_feature_per_cc_ids: Some(vec![0]),
        ul_feature_per_cc_ids: Some(vec![0]),
        srstxswitch: Some(srs_tx_switch),
        ..Default::default()
    }
}

pub(crate) fn raw_payloads(caps: &UeCaps) -> Vec<RawNrPayload> {
    RawNrPayload::all_from_caps(caps).expect("fixture caps must be representable")
}

/// The four corpus-verified always-`Some` header fields. Ingest fails closed on a missing
/// one, so even a fixture that says nothing about the header has to carry a complete one —
/// real input always does.
pub(crate) fn full_header() -> Option<ComboHeader> {
    Some(ComboHeader {
        power_class: Some(0),
        bcs_nr: Some(0),
        bcs_intra_endc: None,
        bcs_eutra: Some(0),
        intra_band_en_dc_support: Some(0),
    })
}

pub(crate) fn one_combo_caps(
    version: u64,
    id: Option<i32>,
    unknown: u64,
    band: i32,
    bitmask: Option<u32>,
) -> UeCaps {
    UeCaps {
        version,
        id,
        unknown,
        combo_groups: vec![ComboGroup {
            combo_header: Some(ComboHeader {
                // The four corpus-verified always-`Some` header fields (all but
                // `bcs_intra_endc`) — the strict decode boundary
                // (`raw_nr::from_proto_combo`) fails closed on a missing one (Task 8).
                power_class: Some(0),
                bcs_nr: Some(0),
                bcs_intra_endc: None,
                bcs_eutra: Some(0),
                intra_band_en_dc_support: Some(0),
            }),
            combo: vec![Combo {
                sub_blocks: vec![cc(band, 0)],
                bitmask,
            }],
        }],
        ..Default::default()
    }
}

pub(crate) fn legacy_file(carrier: &str, caps: UeCaps) -> LegacyNrFile {
    LegacyNrFile {
        carrier: carrier.to_string(),
        caps,
    }
}

pub(crate) fn profiled_file(carrier: &str, number: u64, caps: UeCaps) -> ProfiledNrFile {
    ProfiledNrFile {
        carrier: carrier.to_string(),
        number,
        caps,
    }
}

pub(crate) fn empty_mapping() -> MappingRoot {
    MappingRoot { mappings: vec![] }
}

pub(crate) fn mapping(entries: &[(u64, &str, &[&str])]) -> MappingRoot {
    MappingRoot {
        mappings: entries
            .iter()
            .map(|(id, name, plmns)| MappingEntry {
                id: *id,
                name: (*name).into(),
                plmns: plmns.iter().map(|plmn| plmn.parse().unwrap()).collect(),
            })
            .collect(),
    }
}

pub(crate) fn validated_nr(document: NrDocument) -> ValidatedNr {
    let lte = LteDocument {
        files: BTreeMap::from([(
            1,
            LteFileSource {
                fingerprint: 0,
                bitmask: 0,
            },
        )]),
        combos: vec![],
    };
    let text = to_kdl(&SourceDocument { nr: document, lte }).unwrap();
    parse_sources(&text).unwrap().nr
}

pub(crate) fn generation_source() -> ValidatedNr {
    const SIGNATURE: u64 = 5;
    const REAL_ANCHOR: u64 = 66_813_533;
    const SYNTHETIC_ANCHOR: u64 = 8_969;
    let legacy = vec![
        legacy_file(
            "A",
            one_combo_caps(715_188_856, Some(1), 0, 10_078, Some(123)),
        ),
        legacy_file(
            "EMPTY",
            UeCaps {
                version: 773_233_060,
                ..Default::default()
            },
        ),
    ];

    let mut real = one_combo_caps(862_505_271, Some(7), 11, 10_077, None);
    real.combo_groups[0].combo[0].sub_blocks = vec![
        SubBlock {
            band: 10_077,
            dl_bw_class: Some(1),
            // DL-only: UL disabled but corpus-verified `Some(0)`, never absent.
            ul_bw_class: Some(0),
            dl_feature_index: Some(1),
            dl_feature_per_cc_ids: Some(vec![1]),
            ..Default::default()
        },
        SubBlock {
            band: 10_041,
            dl_bw_class: Some(1),
            ul_bw_class: Some(0),
            dl_feature_index: Some(1),
            dl_feature_per_cc_ids: Some(vec![1]),
            ..Default::default()
        },
    ];
    real.dl_feature_per_cc_list = vec![ShannonFeatureSetDlPerCcNr {
        max_scs: Some(0),
        ..Default::default()
    }];
    let profiled = vec![
        profiled_file("A", SIGNATURE * REAL_ANCHOR, real),
        profiled_file(
            "A",
            SIGNATURE * SYNTHETIC_ANCHOR,
            one_combo_caps(874_888_686, Some(7), 22, 10_003, Some(0)),
        ),
        profiled_file(
            "PROFILE_EMPTY",
            11 * REAL_ANCHOR,
            UeCaps {
                version: 862_505_271,
                id: Some(8),
                unknown: 33,
                ..Default::default()
            },
        ),
    ];
    validated_nr(ingest_nr(legacy, profiled, &mapping(&[(7, "A", &["250-01"])])).unwrap())
}
