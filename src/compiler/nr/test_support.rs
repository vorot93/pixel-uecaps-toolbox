use super::{LegacyNrFile, ProfiledNrFile, ingest_nr};
pub(in crate::compiler) use crate::raw_nr::test_support::{cc, full_header};
use crate::{
    compiler::schema::{
        LteDocument, LteFileSource, NrDocument, SourceDocument, ValidatedNr, parse_sources, to_kdl,
    },
    mapping::{MappingEntry, MappingRoot},
    proto::{Combo, ComboGroup, ShannonFeatureSetDlPerCcNr, SubBlock, UeCaps},
};
use std::collections::BTreeMap;

pub(in crate::compiler) fn one_combo_caps(
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
            combo_header: full_header(),
            combo: vec![Combo {
                sub_blocks: vec![cc(band, 0)],
                bitmask,
            }],
        }],
        ..Default::default()
    }
}

pub(in crate::compiler) fn legacy_file(carrier: &str, caps: UeCaps) -> LegacyNrFile {
    LegacyNrFile {
        carrier: carrier.to_string(),
        caps,
    }
}

pub(in crate::compiler) fn profiled_file(
    carrier: &str,
    number: u64,
    caps: UeCaps,
) -> ProfiledNrFile {
    ProfiledNrFile {
        carrier: carrier.to_string(),
        number,
        caps,
    }
}

pub(in crate::compiler) fn empty_mapping() -> MappingRoot {
    MappingRoot { mappings: vec![] }
}

pub(in crate::compiler) fn mapping(entries: &[(u64, &str, &[&str])]) -> MappingRoot {
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

pub(in crate::compiler) fn validated_nr(document: NrDocument) -> ValidatedNr {
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

pub(in crate::compiler) fn generation_source() -> ValidatedNr {
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
