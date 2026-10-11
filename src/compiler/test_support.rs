use crate::{
    mapping::Plmn,
    proto::{
        Carrier, Combo, ComboGroup, LteCaps, LteCombo, LteComponent, PlmnMap, SubBlock, UeCaps,
    },
};
use prost::Message;
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::Path,
};

pub(in crate::compiler) const REGISTERED_ANCHOR: u64 = 66_813_533;
pub(in crate::compiler) const SYNTHETIC_ANCHOR: u64 = 8_969;
pub(in crate::compiler) const FIRST_LTE_ID: u64 = 400_907_661;
pub(in crate::compiler) const SECOND_LTE_ID: u64 = 92;

const EXPECTED_SOURCE_KDL: &str = r#"version 1
bc ALPHA BETA
bf 702152537 {
    c BETA
}
bf 715188856 {
    c ALPHA
}
c ALPHA bi=1 pi=7 mi=7 sg=11 t=main {
    p mcc=250 mnc=1
    p mcc=250 mnc=1
    pf "66813533" x=66813533 u=11
    pf "8969" x=8969 u=22
}
c BETA bi=2 pi=8 mi=8 sg=13 t=main {
    ps
    pf "66813533" x=66813533 u=33
    pf "8969" x=8969 u=44
}
f "400907661" fp=101 bm=201
f "92" fp=102 bm=202
df s=4 m=1 b=200 o=2 w=#false
uf s=4 m=2 b=200 o=1 w=#false nc=2
n {
    s {
        m prime:8969
    }
    n3 A A
}
n {
    s {
        c BETA
        m legacy G2YBB
    }
    n41 A A
}
n {
    s {
        c ALPHA
        m legacy G2YBB
    }
    n78 A A
}
l b="" u1=0 u2=0 {
    s {
        m G2YBB GGX8B GR83Y
    }
    B1 A4
}
l b="" u1=0 u2=0 {
    B3 A4
}
l b="" u1=0 u2=0 {
    s {
        m lte:92
    }
    B5 A4
}
"#;

#[derive(Clone, Debug)]
pub(in crate::compiler) struct FixtureFile {
    pub(in crate::compiler) basename: String,
    pub(in crate::compiler) bytes: Vec<u8>,
}

#[derive(Clone, Debug)]
pub(in crate::compiler) struct MiniCorpus {
    pub(in crate::compiler) bitmask: Vec<FixtureFile>,
    pub(in crate::compiler) profiled: Vec<FixtureFile>,
    pub(in crate::compiler) expected: ExpectedMiniCorpus,
}

#[derive(Clone, Debug)]
pub(in crate::compiler) struct ExpectedMiniCorpus {
    pub(in crate::compiler) bitmask_carriers: Vec<String>,
    pub(in crate::compiler) profiles: BTreeMap<String, BTreeSet<u64>>,
    pub(in crate::compiler) plmns: BTreeMap<String, Vec<Plmn>>,
    pub(in crate::compiler) lte_ids: Vec<u64>,
    pub(in crate::compiler) nr_payloads: usize,
    pub(in crate::compiler) lte_payloads: usize,
    pub(in crate::compiler) source_kdl: String,
}

impl MiniCorpus {
    pub(in crate::compiler) fn new() -> Self {
        let bitmask = vec![
            encoded_file(
                "BETA.binarypb",
                &nr_caps(702_152_537, Some(2), 0, 10_041, Some(29)),
            ),
            encoded_file(
                "ALPHA.binarypb",
                &nr_caps(715_188_856, Some(1), 0, 10_078, Some(17)),
            ),
        ];

        let mut profiled = vec![
            encoded_file(
                &format!("BETA_{}.binarypb", 13 * SYNTHETIC_ANCHOR),
                &nr_caps(874_888_686, Some(8), 44, 10_003, Some(0)),
            ),
            encoded_file(
                &format!("ALPHA_{}.binarypb", 11 * REGISTERED_ANCHOR),
                &nr_caps(862_505_271, Some(7), 11, 10_078, Some(0)),
            ),
            FixtureFile {
                basename: "ap_plmn_mapping.binarypb".into(),
                bytes: mapping().encode_to_vec(),
            },
            encoded_file(
                &format!("BETA_{}.binarypb", 13 * REGISTERED_ANCHOR),
                &nr_caps(862_505_271, Some(8), 33, 10_041, Some(0)),
            ),
            FixtureFile {
                basename: format!("lte_{SECOND_LTE_ID}.binarypb"),
                bytes: second_lte().encode_to_vec(),
            },
            encoded_file(
                &format!("ALPHA_{}.binarypb", 11 * SYNTHETIC_ANCHOR),
                &nr_caps(874_888_686, Some(7), 22, 10_003, Some(0)),
            ),
            FixtureFile {
                basename: format!("lte_{FIRST_LTE_ID}.binarypb"),
                bytes: first_lte().encode_to_vec(),
            },
        ];

        // Deliberately avoid lexical fixture order so decompose determinism cannot accidentally
        // rely on the helper's construction sequence.
        profiled.rotate_left(2);
        Self {
            bitmask,
            profiled,
            expected: ExpectedMiniCorpus {
                bitmask_carriers: vec!["ALPHA".into(), "BETA".into()],
                profiles: BTreeMap::from([
                    (
                        "ALPHA".into(),
                        BTreeSet::from([REGISTERED_ANCHOR, SYNTHETIC_ANCHOR]),
                    ),
                    (
                        "BETA".into(),
                        BTreeSet::from([REGISTERED_ANCHOR, SYNTHETIC_ANCHOR]),
                    ),
                ]),
                plmns: BTreeMap::from([
                    (
                        "ALPHA".into(),
                        vec!["250-01".parse().unwrap(), "250-01".parse().unwrap()],
                    ),
                    ("BETA".into(), Vec::new()),
                ]),
                lte_ids: vec![FIRST_LTE_ID, SECOND_LTE_ID],
                nr_payloads: 3,
                lte_payloads: 3,
                source_kdl: EXPECTED_SOURCE_KDL.into(),
            },
        }
    }

    pub(in crate::compiler) fn write_to(
        &self,
        root: &Path,
        order: FileOrder,
    ) -> (std::path::PathBuf, std::path::PathBuf) {
        let bitmask_dir = root.join("bitmask");
        let profiled_dir = root.join("profiled");
        fs::create_dir_all(&bitmask_dir).unwrap();
        fs::create_dir_all(&profiled_dir).unwrap();

        write_files(&bitmask_dir, &self.bitmask, order);
        write_files(&profiled_dir, &self.profiled, order);
        (bitmask_dir, profiled_dir)
    }

    pub(in crate::compiler) fn rename_carrier(&mut self, old: &str, new: &str) {
        let mut bitmask_renamed = 0;
        for file in &mut self.bitmask {
            if file.basename == format!("{old}.binarypb") {
                file.basename = format!("{new}.binarypb");
                bitmask_renamed += 1;
            }
        }

        let mut profiled_renamed = 0;
        for file in &mut self.profiled {
            let number = file
                .basename
                .strip_suffix(".binarypb")
                .and_then(|stem| stem.rsplit_once('_'))
                .filter(|(carrier, _)| *carrier == old)
                .and_then(|(_, decimal)| {
                    let number = decimal.parse::<u64>().ok()?;
                    (number.to_string() == decimal).then_some(number)
                });
            if let Some(number) = number {
                file.basename = format!("{new}_{number}.binarypb");
                profiled_renamed += 1;
            }
        }
        let mapping = self
            .profiled
            .iter_mut()
            .find(|file| file.basename == "ap_plmn_mapping.binarypb")
            .expect("mini corpus has a mapping file");
        let mut decoded: PlmnMap = decode(mapping);
        let mapping_renamed = decoded
            .carriers
            .iter()
            .filter(|carrier| carrier.name == old)
            .count();
        decoded
            .carriers
            .iter_mut()
            .filter(|carrier| carrier.name == old)
            .for_each(|carrier| carrier.name = new.into());
        replace(mapping, &decoded);
        assert_eq!(
            (bitmask_renamed, profiled_renamed, mapping_renamed),
            (1, 2, 1),
            "renaming mini corpus carrier `{old}` to `{new}` must change exactly 1 bitmask file, 2 profiled files, and 1 mapping entry"
        );
    }

    pub(in crate::compiler) fn bitmask_file_mut(&mut self, basename: &str) -> &mut FixtureFile {
        self.bitmask
            .iter_mut()
            .find(|file| file.basename == basename)
            .unwrap()
    }

    pub(in crate::compiler) fn profiled_file_mut(&mut self, basename: &str) -> &mut FixtureFile {
        self.profiled
            .iter_mut()
            .find(|file| file.basename == basename)
            .unwrap()
    }

    pub(in crate::compiler) fn remove_profiled(
        &mut self,
        predicate: impl Fn(&FixtureFile) -> bool,
    ) {
        self.profiled.retain(|file| !predicate(file));
    }
}

pub(in crate::compiler) fn decode<M: Message + Default>(file: &FixtureFile) -> M {
    M::decode(file.bytes.as_slice()).unwrap()
}

pub(in crate::compiler) fn replace(file: &mut FixtureFile, message: &impl Message) {
    file.bytes = message.encode_to_vec();
}

/// Rewrite `file` into a non-canonical encoding of the *same* value, so the LTE byte-identity
/// self-check is what rejects it.
///
/// This deliberately uses an explicit zero for the bare `bitmask` scalar rather than the
/// reordered-fields trick it used before: `wire::scan` now rejects descending tag order (and
/// duplicates, and overlong varints), so a reordered encoding never reaches the byte comparison.
/// An explicit zero for a non-`optional` scalar is the one non-canonical form the scanner cannot
/// reject — prost drops it on re-encode — which makes it the right probe here.
pub(in crate::compiler) fn make_lte_encoding_noncanonical(file: &mut FixtureFile) {
    let mut caps = decode::<crate::proto::LteCaps>(file);
    caps.bitmask = 0;
    let mut bytes = caps.encode_to_vec();
    bytes.extend([0x18, 0x00]); // field 3 (bitmask), explicit zero, keeping ascending order
    assert_eq!(LteCaps::decode(bytes.as_slice()).unwrap(), caps);
    assert_ne!(bytes, caps.encode_to_vec());
    file.bytes = bytes;
}

pub(in crate::compiler) fn inject_unknown_nr_cc_field(file: &mut FixtureFile) {
    let caps = decode::<crate::proto::UeCaps>(file);
    let group = &caps.combo_groups[0];
    let combo = &group.combo[0];
    let mut cc = combo.sub_blocks[0].encode_to_vec();
    cc.extend(varint_field(15, 1));

    let mut combo_bytes = length_delimited_field(1, &cc);
    if let Some(bitmask) = combo.bitmask {
        combo_bytes.extend(varint_field(2, u64::from(bitmask)));
    }
    // Preserve the group's header bytes (field 1), if any — `nr_caps` now gives every
    // fixture combo a value-bearing header (Task 8), so it must survive this manual
    // re-encode rather than silently dropping it.
    let mut group_bytes = Vec::new();
    if let Some(header) = &group.combo_header {
        group_bytes.extend(length_delimited_field(1, &header.encode_to_vec()));
    }
    group_bytes.extend(length_delimited_field(2, &combo_bytes));

    let mut without_groups = caps.clone();
    without_groups.combo_groups.clear();
    let mut bytes = without_groups.encode_to_vec();
    bytes.extend(length_delimited_field(3, &group_bytes));
    file.bytes = bytes;
}

fn push_varint(mut value: u64, bytes: &mut Vec<u8>) {
    while value >= 0x80 {
        bytes.push((value as u8 & 0x7f) | 0x80);
        value >>= 7;
    }
    bytes.push(value as u8);
}

fn varint_field(field: u64, value: u64) -> Vec<u8> {
    let mut bytes = Vec::new();
    push_varint(field << 3, &mut bytes);
    push_varint(value, &mut bytes);
    bytes
}

fn length_delimited_field(field: u64, payload: &[u8]) -> Vec<u8> {
    let mut bytes = Vec::new();
    push_varint((field << 3) | 2, &mut bytes);
    push_varint(payload.len() as u64, &mut bytes);
    bytes.extend_from_slice(payload);
    bytes
}

#[derive(Clone, Copy)]
pub(in crate::compiler) enum FileOrder {
    Forward,
    Reverse,
}

fn write_files(dir: &Path, files: &[FixtureFile], order: FileOrder) {
    let mut ordered: Vec<_> = files.iter().collect();
    ordered.sort_by(|a, b| a.basename.cmp(&b.basename));
    if matches!(order, FileOrder::Reverse) {
        ordered.reverse();
    }
    for file in ordered {
        fs::write(dir.join(&file.basename), &file.bytes).unwrap();
    }
}

fn encoded_file(basename: &str, caps: &UeCaps) -> FixtureFile {
    FixtureFile {
        basename: basename.into(),
        bytes: caps.encode_to_vec(),
    }
}

fn nr_caps(
    fingerprint: u64,
    id: Option<i32>,
    unknown: u64,
    band: i32,
    bitmask: Option<u32>,
) -> UeCaps {
    UeCaps {
        version: fingerprint,
        id,
        combo_groups: vec![ComboGroup {
            combo_header: crate::raw_nr::test_support::full_header(),
            combo: vec![Combo {
                sub_blocks: vec![SubBlock {
                    band,
                    dl_bw_class: Some(1),
                    ul_bw_class: Some(1),
                    // Per-CC presence tracks `bw_class` in every real file, and
                    // `RawSubBlock::validate` now enforces that biconditional, so a class-1
                    // direction carries a one-byte all-zero placeholder (`cc_count == 1` for
                    // class 1 in both kinds). Omitting these made the fixture a shape the
                    // corpus never contains, and one regeneration would have silently added.
                    dl_feature_per_cc_ids: Some(vec![0]),
                    ul_feature_per_cc_ids: Some(vec![0]),
                    // Fields 4/5 are likewise never absent in a real file, and NR generation
                    // always emits them (the index is derived from the resolved feature sets).
                    // The all-zero placeholder resolves to no feature set, so the derived index
                    // is 0 — omitting these made regeneration add two bytes the fixture lacked,
                    // which is precisely what the new byte-identity check catches.
                    dl_feature_index: Some(0),
                    ul_feature_index: Some(0),
                    ..Default::default()
                }],
                bitmask,
            }],
        }],
        unknown,
        ..Default::default()
    }
}

fn mapping() -> PlmnMap {
    PlmnMap {
        carriers: vec![
            Carrier {
                plmns: vec![5_435_408, 5_435_408],
                index: 7,
                name: "ALPHA".into(),
            },
            Carrier {
                plmns: Vec::new(),
                index: 8,
                name: "BETA".into(),
            },
        ],
    }
}

fn lte_combo(band: i32) -> LteCombo {
    LteCombo {
        components: vec![LteComponent {
            band,
            // A real corpus bitfield (class A, 4x4). This was `32_768 + band`, which is only a
            // valid class+MIMO value for band 0 or 1 — the payloads are already distinguished by
            // `band`, so they do not need distinct classes too.
            dl_bw_class_mimo: 32_769,
            ul_bw_class_mimo: Some(0),
        }],
        bcs: Some(0),
        unknown1: Some(0),
        unknown2: Some(0),
    }
}

fn first_lte() -> LteCaps {
    LteCaps {
        fingerprint: 101,
        combos: vec![lte_combo(1), lte_combo(3)],
        bitmask: 201,
    }
}

fn second_lte() -> LteCaps {
    LteCaps {
        fingerprint: 102,
        combos: vec![lte_combo(3), lte_combo(5)],
        bitmask: 202,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rename_carrier_only_changes_the_exact_carrier_before_a_final_number() {
        let mut corpus = MiniCorpus::new();
        let template = corpus
            .profiled
            .iter()
            .find(|file| file.basename.starts_with("ALPHA_"))
            .unwrap()
            .clone();
        corpus.profiled.push(FixtureFile {
            basename: "ALPHA_PRIVATE_123.binarypb".into(),
            bytes: template.bytes.clone(),
        });
        corpus.profiled.push(FixtureFile {
            basename: "ALPHA_not-a-number.binarypb".into(),
            bytes: template.bytes,
        });

        corpus.rename_carrier("ALPHA", "RENAMED");

        let basenames = corpus
            .profiled
            .iter()
            .map(|file| file.basename.as_str())
            .collect::<Vec<_>>();
        assert!(basenames.contains(&"ALPHA_PRIVATE_123.binarypb"));
        assert!(basenames.contains(&"ALPHA_not-a-number.binarypb"));
        assert_eq!(
            basenames
                .iter()
                .filter(|basename| basename.starts_with("RENAMED_"))
                .count(),
            2
        );
    }

    #[test]
    #[should_panic(
        expected = "must change exactly 1 bitmask file, 2 profiled files, and 1 mapping entry"
    )]
    fn rename_carrier_asserts_the_expected_fixture_match_counts() {
        let mut corpus = MiniCorpus::new();
        corpus
            .bitmask
            .retain(|file| file.basename != "ALPHA.binarypb");

        corpus.rename_carrier("ALPHA", "RENAMED");
    }
}
