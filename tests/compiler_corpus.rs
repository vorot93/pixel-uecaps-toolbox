mod corpus;
use corpus::{components, corpora, is_uecaps_basename, raw_carriers, resolve_selector};
use kdl::{KdlDocument, KdlNode};
use pixel_uecaps_toolbox::{
    NR_BAND_OFFSET,
    compiler::{decompose, load_sources, provision_from_sources},
    model::{CapabilityLayout, PHONE_MODELS, PhoneModel},
    proto::{LteCaps, LteCombo, ShannonFeatureSetDlPerCcNr, ShannonFeatureSetUlPerCcNr, UeCaps},
};
use prost::Message;
use rayon::iter::{IntoParallelRefIterator, ParallelIterator};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::{Cursor, Read, Write},
    path::Path,
};
use zip::{ZipArchive, ZipWriter, write::SimpleFileOptions};

#[derive(Default)]
struct ExpectedDlDomains {
    max_scs: BTreeSet<Option<i32>>,
    max_mimo: BTreeSet<Option<i32>>,
    max_bw: BTreeSet<Option<i32>>,
    max_mod_order: BTreeSet<Option<i32>>,
    bw_90mhz_supported: BTreeSet<Option<bool>>,
}

#[derive(Default)]
struct ExpectedUlDomains {
    max_scs: BTreeSet<Option<i32>>,
    max_mimo_cb: BTreeSet<Option<i32>>,
    max_bw: BTreeSet<Option<i32>>,
    max_mod_order: BTreeSet<Option<i32>>,
    bw_90mhz_supported: BTreeSet<Option<bool>>,
    max_mimo_non_cb: BTreeSet<Option<i32>>,
}

fn expected_raw_feature_palettes(
    bitmask: &Path,
    profiled: &Path,
) -> (
    Vec<ShannonFeatureSetDlPerCcNr>,
    Vec<ShannonFeatureSetUlPerCcNr>,
) {
    let mut dl = ExpectedDlDomains::default();
    let mut ul = ExpectedUlDomains::default();
    let mut decoded_files = 0_usize;
    for (_, caps) in raw_carriers(bitmask, profiled) {
        decoded_files += 1;
        for row in caps.dl_feature_per_cc_list {
            dl.max_scs.insert(row.max_scs);
            dl.max_mimo.insert(row.max_mimo);
            dl.max_bw.insert(row.max_bw);
            dl.max_mod_order.insert(row.max_mod_order);
            dl.bw_90mhz_supported.insert(row.bw_90mhz_supported);
        }
        for row in caps.ul_feature_per_cc_list {
            ul.max_scs.insert(row.max_scs);
            ul.max_mimo_cb.insert(row.max_mimo_cb);
            ul.max_bw.insert(row.max_bw);
            ul.max_mod_order.insert(row.max_mod_order);
            ul.bw_90mhz_supported.insert(row.bw_90mhz_supported);
            ul.max_mimo_non_cb.insert(row.max_mimo_non_cb);
        }
    }
    assert!(decoded_files > 0, "no raw NR corpus files were decoded");

    let mut expected_dl = Vec::new();
    for &max_scs in &dl.max_scs {
        for &max_mimo in &dl.max_mimo {
            for &max_bw in &dl.max_bw {
                for &max_mod_order in &dl.max_mod_order {
                    for &bw_90mhz_supported in &dl.bw_90mhz_supported {
                        expected_dl.push(ShannonFeatureSetDlPerCcNr {
                            max_scs,
                            max_mimo,
                            max_bw,
                            max_mod_order,
                            bw_90mhz_supported,
                        });
                    }
                }
            }
        }
    }
    expected_dl.push(ShannonFeatureSetDlPerCcNr {
        max_scs: Some(4),
        max_mimo: Some(1),
        max_bw: Some(200),
        max_mod_order: Some(2),
        bw_90mhz_supported: Some(false),
    });
    expected_dl.sort_unstable();
    expected_dl.dedup();

    let mut expected_ul = Vec::new();
    for &max_scs in &ul.max_scs {
        for &max_mimo_cb in &ul.max_mimo_cb {
            for &max_bw in &ul.max_bw {
                for &max_mod_order in &ul.max_mod_order {
                    for &bw_90mhz_supported in &ul.bw_90mhz_supported {
                        for &max_mimo_non_cb in &ul.max_mimo_non_cb {
                            expected_ul.push(ShannonFeatureSetUlPerCcNr {
                                max_scs,
                                max_mimo_cb,
                                max_bw,
                                max_mod_order,
                                bw_90mhz_supported,
                                max_mimo_non_cb,
                            });
                        }
                    }
                }
            }
        }
    }
    expected_ul.push(ShannonFeatureSetUlPerCcNr {
        max_scs: Some(4),
        max_mimo_cb: Some(2),
        max_bw: Some(200),
        max_mod_order: Some(1),
        bw_90mhz_supported: Some(false),
        max_mimo_non_cb: Some(2),
    });
    expected_ul.sort_unstable();
    expected_ul.dedup();
    (expected_dl, expected_ul)
}

fn optional_i32(node: &KdlNode, key: &str) -> Option<i32> {
    node.get(key).map(|value| {
        value
            .as_integer()
            .unwrap_or_else(|| panic!("feature property `{key}` must be an integer"))
            .try_into()
            .unwrap_or_else(|_| panic!("feature property `{key}` must fit i32"))
    })
}

fn optional_bool(node: &KdlNode, key: &str) -> Option<bool> {
    node.get(key).map(|value| {
        value
            .as_bool()
            .unwrap_or_else(|| panic!("feature property `{key}` must be a boolean"))
    })
}

fn emitted_feature_palettes(
    source: &[u8],
) -> (
    Vec<ShannonFeatureSetDlPerCcNr>,
    Vec<ShannonFeatureSetUlPerCcNr>,
) {
    let text = std::str::from_utf8(source).expect("canonical source must be UTF-8");
    let document: KdlDocument = text.parse().expect("canonical source must parse as KDL");
    let mut dl = Vec::new();
    let mut ul = Vec::new();
    for node in document.nodes() {
        match node.name().value() {
            "df" => dl.push(ShannonFeatureSetDlPerCcNr {
                max_scs: optional_i32(node, "s"),
                max_mimo: optional_i32(node, "m"),
                max_bw: optional_i32(node, "b"),
                max_mod_order: optional_i32(node, "o"),
                bw_90mhz_supported: optional_bool(node, "w"),
            }),
            "uf" => ul.push(ShannonFeatureSetUlPerCcNr {
                max_scs: optional_i32(node, "s"),
                max_mimo_cb: optional_i32(node, "m"),
                max_bw: optional_i32(node, "b"),
                max_mod_order: optional_i32(node, "o"),
                bw_90mhz_supported: optional_bool(node, "w"),
                max_mimo_non_cb: optional_i32(node, "nc"),
            }),
            _ => {}
        }
    }
    (dl, ul)
}

fn read_lte_sequences(dir: &Path) -> BTreeMap<u64, Vec<Vec<u8>>> {
    let mut sequences = BTreeMap::new();
    for entry in fs::read_dir(dir).expect("reading the optional profiled corpus") {
        let entry = entry.expect("reading an optional profiled corpus entry");
        if !entry
            .file_type()
            .expect("reading an optional corpus entry type")
            .is_file()
        {
            continue;
        }
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        let Some(decimal) = name
            .strip_prefix("lte_")
            .and_then(|name| name.strip_suffix(".binarypb"))
        else {
            continue;
        };
        let id = decimal
            .parse::<u64>()
            .expect("an LTE corpus filename must contain a decimal ID");
        assert_eq!(id.to_string(), decimal, "LTE corpus ID must be canonical");
        let bytes = fs::read(entry.path()).expect("reading an optional LTE corpus file");
        let caps = LteCaps::decode(bytes.as_slice()).expect("decoding an optional LTE corpus file");
        let sequence = caps.combos.iter().map(LteCombo::encode_to_vec).collect();
        assert!(sequences.insert(id, sequence).is_none(), "duplicate LTE ID");
    }
    sequences
}

fn assert_lte_invariants(sequences: &BTreeMap<u64, Vec<Vec<u8>>>) {
    assert_eq!(sequences.len(), 8, "unexpected LTE file count");

    let mut distinct = BTreeSet::new();
    let mut edges = BTreeMap::<Vec<u8>, BTreeSet<Vec<u8>>>::new();
    for sequence in sequences.values() {
        let mut within_file = BTreeSet::new();
        for payload in sequence {
            assert!(
                within_file.insert(payload.clone()),
                "duplicate LTE payload within one file"
            );
            distinct.insert(payload.clone());
            edges.entry(payload.clone()).or_default();
        }
        for adjacent in sequence.windows(2) {
            edges
                .entry(adjacent[0].clone())
                .or_default()
                .insert(adjacent[1].clone());
        }
    }
    assert_eq!(
        distinct.len(),
        3_878,
        "unexpected distinct LTE payload count"
    );

    let mut indegree = edges
        .keys()
        .cloned()
        .map(|payload| (payload, 0_usize))
        .collect::<BTreeMap<_, _>>();
    for successors in edges.values() {
        for successor in successors {
            *indegree
                .get_mut(successor)
                .expect("every LTE successor must be a node") += 1;
        }
    }
    let mut ready = indegree
        .iter()
        .filter(|(_, count)| **count == 0)
        .map(|(payload, _)| payload.clone())
        .collect::<BTreeSet<_>>();
    let mut visited = 0;
    while let Some(payload) = ready.pop_first() {
        visited += 1;
        for successor in &edges[&payload] {
            let count = indegree
                .get_mut(successor)
                .expect("every LTE successor retains its indegree");
            *count -= 1;
            if *count == 0 {
                ready.insert(successor.clone());
            }
        }
    }
    assert_eq!(visited, edges.len(), "LTE global ordering must be acyclic");

    assert_eq!(
        sequences
            .get(&2_160_127_815)
            .expect("first LTE sequence pair must be present"),
        sequences
            .get(&4_210_990_300)
            .expect("first LTE sequence pair must be present"),
        "first observed LTE pair must retain equal sequences"
    );
    assert_eq!(
        sequences
            .get(&2_306_930_561)
            .expect("second LTE sequence pair must be present"),
        sequences
            .get(&4_017_061_044)
            .expect("second LTE sequence pair must be present"),
        "second observed LTE pair must retain equal sequences"
    );
}

/// Samsung Shannon `bw_class` -> aggregated NR CC count. Mirrors `raw_nr::NR_CC_COUNTS`
/// (duplicated here: that table is `pub(crate)`, unreachable from this integration test
/// crate) — exception-free across 3.46M corpus sub-blocks per that module's doc comment.
const NR_CC_COUNTS: &[(i32, usize)] = &[
    (1, 1),
    (2, 2),
    (3, 2),
    (7, 2),
    (8, 3),
    (9, 4),
    (10, 5),
    (11, 6),
    (12, 7),
    (13, 8),
];

fn nr_cc_count(bw_class: Option<i32>) -> Option<usize> {
    NR_CC_COUNTS
        .iter()
        .find(|(class, _)| Some(*class) == bw_class)
        .map(|(_, n)| *n)
}

fn assert_compact_nr_features(caps: &UeCaps, model_code: &str, basename: &str) {
    let mut dl_refs = BTreeSet::new();
    let mut ul_refs = BTreeSet::new();
    for component in components(caps) {
        for (direction, selector, class, len, refs) in [
            (
                "DL",
                component.dl_feature_per_cc_ids.as_deref(),
                component.dl_bw_class,
                caps.dl_feature_per_cc_list.len(),
                &mut dl_refs,
            ),
            (
                "UL",
                component.ul_feature_per_cc_ids.as_deref(),
                component.ul_bw_class,
                caps.ul_feature_per_cc_list.len(),
                &mut ul_refs,
            ),
        ] {
            if component.band >= NR_BAND_OFFSET
                && let Some(ids) = selector
                && ids.iter().any(|&id| id != 0)
            {
                assert!(
                    ids.iter().all(|&id| (1..=len).contains(&usize::from(id))),
                    "model {model_code} NR entry {basename} has an invalid {direction} selector {ids:?}"
                );
            }
            if let Some(ids) = selector.filter(|ids| {
                !ids.is_empty() && ids.iter().all(|&id| (1..=len).contains(&usize::from(id)))
            }) {
                let expected = nr_cc_count(class).unwrap_or_else(|| panic!(
                    "model {model_code} NR entry {basename} resolved {direction} selector has unknown bw_class {class:?}"
                ));
                assert_eq!(
                    ids.len(),
                    expected,
                    "model {model_code} NR entry {basename} has a resolved {direction} selector whose length disagrees with its bw_class CC count"
                );
                refs.extend(ids.iter().map(|&id| usize::from(id) - 1));
            }
        }
    }
    assert_eq!(
        dl_refs,
        (0..caps.dl_feature_per_cc_list.len()).collect(),
        "model {model_code} NR entry {basename} has an unused DL feature record"
    );
    assert_eq!(
        ul_refs,
        (0..caps.ul_feature_per_cc_list.len()).collect(),
        "model {model_code} NR entry {basename} has an unused UL feature record"
    );
    assert!(
        caps.dl_feature_per_cc_list
            .windows(2)
            .all(|pair| pair[0] < pair[1]),
        "model {model_code} NR entry {basename} DL list is not strictly canonical"
    );
    assert!(
        caps.ul_feature_per_cc_list
            .windows(2)
            .all(|pair| pair[0] < pair[1]),
        "model {model_code} NR entry {basename} UL list is not strictly canonical"
    );
}

#[test]
fn compact_catalog_oracle_rejects_mixed_invalid_nr_selectors_even_when_rows_are_used() {
    let valid = pixel_uecaps_toolbox::proto::SubBlock {
        band: NR_BAND_OFFSET + 78,
        dl_bw_class: Some(1),
        dl_feature_per_cc_ids: Some(vec![1]),
        ..Default::default()
    };
    for ids in [vec![1, 0], vec![1, 99]] {
        let mut invalid = valid.clone();
        invalid.dl_bw_class = Some(2);
        invalid.dl_feature_per_cc_ids = Some(ids);
        let caps = UeCaps {
            combo_groups: vec![pixel_uecaps_toolbox::proto::ComboGroup {
                combo: vec![pixel_uecaps_toolbox::proto::Combo {
                    sub_blocks: vec![valid.clone(), invalid],
                    ..Default::default()
                }],
                ..Default::default()
            }],
            dl_feature_per_cc_list: vec![Default::default()],
            ..Default::default()
        };
        assert!(
            std::panic::catch_unwind(|| assert_compact_nr_features(
                &caps,
                "fixture",
                "fixture.binarypb"
            ))
            .is_err()
        );
    }
}

fn assert_module_nr_features(path: &Path, model: PhoneModel) -> BTreeSet<String> {
    let model_code = model.code;
    let bytes = fs::read(path).unwrap_or_else(|error| {
        panic!("reading generated corpus module for model {model_code}: {error}")
    });
    let mut zip = ZipArchive::new(Cursor::new(bytes)).unwrap_or_else(|error| {
        panic!("opening generated corpus module for model {model_code}: {error}")
    });
    let mut inspected = BTreeSet::new();
    for index in 0..zip.len() {
        let mut entry = zip.by_index(index).unwrap_or_else(|error| {
            panic!("reading generated module entry {index} for model {model_code}: {error}")
        });
        let name = entry.name().to_owned();
        let Some(basename) = name
            .strip_prefix("system/vendor/firmware/uecapconfig/")
            .filter(|name| name.ends_with(".binarypb"))
        else {
            continue;
        };
        if !is_uecaps_basename(model.layout, basename) {
            continue;
        }
        let mut payload = Vec::new();
        entry.read_to_end(&mut payload).unwrap_or_else(|error| {
            panic!("reading generated NR entry {basename} for model {model_code}: {error}")
        });
        let caps = UeCaps::decode(payload.as_slice()).unwrap_or_else(|error| {
            panic!("decoding generated NR entry {basename} for model {model_code}: {error}")
        });
        assert_compact_nr_features(&caps, model_code, basename);
        inspected.insert(basename.to_owned());
    }
    inspected
}

fn write_test_module_entry(path: &Path, basename: &str, caps: &UeCaps) {
    let file = fs::File::create(path).expect("creating test module");
    let mut zip = ZipWriter::new(file);
    zip.start_file(
        format!("system/vendor/firmware/uecapconfig/{basename}"),
        SimpleFileOptions::default(),
    )
    .expect("starting test module entry");
    zip.write_all(&caps.encode_to_vec())
        .expect("writing test module entry");
    zip.finish().expect("finishing test module");
}

#[test]
fn layout_aware_oracle_observes_bitmask_lte_carrier_but_skips_profiled_lte_id() {
    let temp = tempfile::tempdir().expect("creating layout-aware oracle workspace");
    let bitmask = temp.path().join("bitmask");
    let profiled = temp.path().join("profiled");
    fs::create_dir(&bitmask).expect("creating bitmask corpus directory");
    fs::create_dir(&profiled).expect("creating profiled corpus directory");

    let caps_with_scs = |max_scs| UeCaps {
        dl_feature_per_cc_list: vec![ShannonFeatureSetDlPerCcNr {
            max_scs: Some(max_scs),
            ..Default::default()
        }],
        ..Default::default()
    };
    fs::write(
        bitmask.join("BASE.binarypb"),
        caps_with_scs(1).encode_to_vec(),
    )
    .expect("writing baseline bitmask UeCaps");
    fs::write(
        bitmask.join("lte_FOO.binarypb"),
        caps_with_scs(17).encode_to_vec(),
    )
    .expect("writing lte_FOO bitmask UeCaps");
    fs::write(
        profiled.join("BASE_1.binarypb"),
        caps_with_scs(2).encode_to_vec(),
    )
    .expect("writing baseline profiled UeCaps");
    fs::write(
        profiled.join("lte_7.binarypb"),
        caps_with_scs(99).encode_to_vec(),
    )
    .expect("writing profiled LTE-role fixture");

    let (dl, _) = expected_raw_feature_palettes(&bitmask, &profiled);
    assert!(
        dl.iter().any(|row| row.max_scs == Some(17)),
        "bitmask lte_FOO.binarypb must contribute raw UeCaps feature axes"
    );
    assert!(
        dl.iter().all(|row| row.max_scs != Some(99)),
        "profiled lte_<id>.binarypb must remain excluded as an LTE-role file"
    );
}

#[test]
fn layout_aware_oracle_inspects_generated_bitmask_lte_carrier_but_skips_profiled_lte_id() {
    let temp = tempfile::tempdir().expect("creating generated-module oracle workspace");
    let compact = UeCaps::default();

    let bitmask_module = temp.path().join("bitmask.zip");
    write_test_module_entry(&bitmask_module, "lte_FOO.binarypb", &compact);
    let bitmask_model = PHONE_MODELS
        .iter()
        .copied()
        .find(|model| model.is_bitmask())
        .expect("registered models must include a bitmask layout");
    assert_eq!(
        assert_module_nr_features(&bitmask_module, bitmask_model),
        BTreeSet::from(["lte_FOO.binarypb".to_owned()])
    );

    let profiled_module = temp.path().join("profiled.zip");
    write_test_module_entry(&profiled_module, "lte_7.binarypb", &compact);
    let profiled_model = PHONE_MODELS
        .iter()
        .copied()
        .find(|model| matches!(model.layout, CapabilityLayout::Profiled { .. }))
        .expect("registered models must include a profiled layout");
    assert_eq!(
        assert_module_nr_features(&profiled_module, profiled_model),
        BTreeSet::new()
    );
}

#[test]
fn optional_corpora_decompose_and_provision_every_registered_target() {
    let Some(corpora) = corpora() else { return };
    let (bitmask, profiled) = (corpora.bitmask.as_path(), corpora.profiled.as_path());
    let root = corpus::output_root().expect("authorized UECAPS_CORPUS_OUTPUT_ROOT");
    let source_file = tempfile::NamedTempFile::new_in(&root).expect("source fixture");

    // One active source per product; repeat serially at the same owned path.
    decompose(bitmask, profiled, Some(source_file.path()))
        .expect("decomposing both optional corpora");
    let source = fs::read(source_file.path()).expect("reading first canonical source");
    decompose(bitmask, profiled, Some(source_file.path()))
        .expect("re-decomposing both optional corpora");
    assert_eq!(
        source,
        fs::read(source_file.path()).expect("reading second canonical source"),
        "canonical source must be byte-idempotent"
    );
    let expected = expected_raw_feature_palettes(bitmask, profiled);
    let actual = emitted_feature_palettes(&source);
    assert_eq!(actual.0, expected.0, "emitted DL feature palette differs");
    assert_eq!(actual.1, expected.1, "emitted UL feature palette differs");
    eprintln!(
        "exhaustive source palette: DL={}, UL={}, KDL={} bytes",
        actual.0.len(),
        actual.1.len(),
        source.len()
    );

    let sequences = read_lte_sequences(profiled);
    assert_lte_invariants(&sequences);
    // Parse once; independent models share immutable sources and own temporary ZIPs.
    let sources =
        load_sources(source_file.path()).expect("parsing the canonical corpus source once");
    PHONE_MODELS.par_iter().for_each(|model| {
        let out = tempfile::NamedTempFile::new_in(&root).expect("module fixture");
        provision_from_sources(&sources, model.code, out.path(), None).unwrap_or_else(|error| {
            panic!("provisioning registered model {}: {error:#}", model.code)
        });
        let module_len = fs::metadata(out.path())
            .unwrap_or_else(|error| {
                panic!(
                    "reading corpus module metadata for registered model {}: {error}",
                    model.code
                )
            })
            .len();
        assert!(
            module_len > 0,
            "registered model {} generated an empty corpus module",
            model.code
        );
        let _validated_basenames = assert_module_nr_features(out.path(), *model);
    });
}

/// Independently verify (against the real corpus) that every NR component's stored
/// `dl/ul_feature_index` equals what we derive from its per-CC feature set. This is the
/// invariant that lets `decompose` omit the field and `provision` re-derive it.
/// Env-gated exactly like the other corpus test.
#[test]
fn nr_feature_index_matches_derivation_formula() {
    let Some(corpora) = corpora() else { return };
    let mut checked = 0u64;
    for (path, caps) in raw_carriers(&corpora.bitmask, &corpora.profiled) {
        let dl_list = &caps.dl_feature_per_cc_list;
        let ul_list = &caps.ul_feature_per_cc_list;
        for cc in components(&caps).filter(|cc| cc.band >= NR_BAND_OFFSET) {
            let derived_dl = match resolve_selector(cc.dl_feature_per_cc_ids.as_deref(), dl_list) {
                None => 0,
                Some(indices) => {
                    let per_cc: Vec<i32> = indices
                        .iter()
                        .map(|row| {
                            let scs = row.max_scs.unwrap_or(0);
                            if scs >= 4 { 2 } else { 1 }
                        })
                        .collect();
                    assert!(
                        per_cc.windows(2).all(|pair| pair[0] == pair[1]),
                        "NR DL per-CC derived feature-index disagrees across CCs in {}",
                        path.display()
                    );
                    per_cc[0]
                }
            };
            assert_eq!(
                cc.dl_feature_index.unwrap_or(0),
                derived_dl,
                "NR DL feature-index != derived in {}",
                path.display()
            );
            let derived_ul = match resolve_selector(cc.ul_feature_per_cc_ids.as_deref(), ul_list) {
                None => 0,
                Some(indices) => {
                    let per_cc: Vec<i32> = indices
                        .iter()
                        .map(|row| {
                            let cb = row.max_mimo_cb.unwrap_or(0);
                            if cb == 2 { 2 } else { 1 }
                        })
                        .collect();
                    assert!(
                        per_cc.windows(2).all(|pair| pair[0] == pair[1]),
                        "NR UL per-CC derived feature-index disagrees across CCs in {}",
                        path.display()
                    );
                    per_cc[0]
                }
            };
            assert_eq!(
                cc.ul_feature_index.unwrap_or(0),
                derived_ul,
                "NR UL feature-index != derived in {}",
                path.display()
            );
            checked += 1;
        }
    }
    assert!(
        checked > 0,
        "no NR components checked — are the corpus dirs populated?"
    );
    eprintln!("verified NR feature-index derivation on {checked} NR components");
}

/// Guard the omit-when-0 + reader-default invariant for LTE (E-UTRA) combo sub-blocks: their
/// `dl_feature_index`/`ul_feature_index` are always `Some` in the real corpus, so an omitted UL
/// positional-argument value in the sub-block's KDL spelling unambiguously means the explicit
/// zero, never a genuine `None`. Env-gated exactly like the other corpus tests.
#[test]
fn lte_feature_index_is_always_some_in_corpus() {
    let Some(corpora) = corpora() else { return };

    let mut checked = 0u64;
    for (path, caps) in raw_carriers(&corpora.bitmask, &corpora.profiled) {
        for cc in components(&caps).filter(|cc| cc.band > 0 && cc.band < NR_BAND_OFFSET) {
            assert!(
                cc.dl_feature_index.is_some(),
                "LTE sub-block with absent dl_feature_index in {}",
                path.display()
            );
            assert!(
                cc.ul_feature_index.is_some(),
                "LTE sub-block with absent ul_feature_index in {}",
                path.display()
            );
            checked += 1;
        }
    }
    assert!(
        checked > 0,
        "no LTE components checked — are the corpus dirs populated?"
    );
    eprintln!("verified LTE feature-index always-Some on {checked} LTE components");
}

/// The bug this regression pins was invisible to a plain decompose->provision round trip: `decompose`
/// collapsed a non-uniform multi-CC NR sub-block (one whose per-CC DL feature selector bytes
/// resolve to *different* records for different CCs) down to its first CC, and `provision`
/// faithfully reproduced that same collapsed value. Comparing rebuilt-vs-rebuilt therefore
/// compared collapsed-vs-collapsed and passed, silently dropping the second (and any further)
/// CC's feature set. The only way to catch that class of bug is to compare the pipeline's
/// output against the ORIGINAL raw corpus file's per-CC features, captured independently of
/// `decompose`/`provision` — which is what this test does.
///
/// Ground truth (verified against the real corpus, see task-10 report): `ATT.binarypb`'s NR
/// band `n48` (`band == NR_BAND_OFFSET + 48`) carries a class-B (`dl_bw_class == 2`, 2-CC)
/// sub-block whose `dl_feature_per_cc_ids` is the 2-byte selector `[22, 23]`, resolving to two
/// DISTINCT `ShannonFeatureSetDlPerCcNr` records (`max_bw` 40 vs. 50). A collapse-to-first-CC
/// bug would instead resolve both CCs to record 22 (or drop the selector to length 1) — this
/// test fails either way, because it demands two records AND that they differ AND that they
/// equal the two original records in original order.
#[test]
fn att_n48_non_uniform_subblock_preserves_distinct_per_cc_dl_features() {
    let Some(corpora) = corpora() else { return };
    const ATT_N48_BAND: i32 = NR_BAND_OFFSET + 48;

    let (bitmask, profiled) = (corpora.bitmask.as_path(), corpora.profiled.as_path());

    // Capture ground truth directly from the raw corpus file — no compiler code involved.
    let att_path = bitmask.join("ATT.binarypb");
    let att_bytes = fs::read(&att_path)
        .unwrap_or_else(|error| panic!("reading {}: {error}", att_path.display()));
    let att_caps = UeCaps::decode(att_bytes.as_slice())
        .unwrap_or_else(|error| panic!("decoding {}: {error}", att_path.display()));
    let original = components(&att_caps)
        .filter(|cc| cc.band == ATT_N48_BAND)
        .find_map(|cc| {
            let resolved = resolve_selector(
                cc.dl_feature_per_cc_ids.as_deref(),
                &att_caps.dl_feature_per_cc_list,
            )?;
            (resolved.len() >= 2).then_some(resolved)
        })
        .expect(
            "ATT.binarypb must contain a non-uniform (>=2 CC) n48 DL sub-block for this \
             regression to mean anything; if the reference corpus changed, repoint this test \
             at another verified non-uniform sub-block",
        );
    assert!(
        original.windows(2).any(|pair| pair[0] != pair[1]),
        "the chosen ATT n48 sub-block's per-CC DL records must actually differ, or this test \
         cannot distinguish a preserved round trip from a collapsed one"
    );

    // Run the real pipeline: decompose the full corpus into canonical sources, then provision a
    // legacy (bitmask-layout) model, which regenerates ATT.binarypb from those sources.
    let root = corpus::output_root().expect("authorized UECAPS_CORPUS_OUTPUT_ROOT");
    let source =
        tempfile::NamedTempFile::new_in(&root).expect("creating the regression source file");
    decompose(bitmask, profiled, Some(source.path()))
        .expect("decoding the corpus for the regression test");
    let sources = load_sources(source.path()).expect("parsing the decomposed canonical source");
    let model = PHONE_MODELS
        .iter()
        .find(|model| model.is_bitmask())
        .expect("the registered model list must include at least one bitmask-layout model");
    let module = tempfile::NamedTempFile::new_in(&root).expect("module fixture");
    provision_from_sources(&sources, model.code, module.path(), None).unwrap_or_else(|error| {
        panic!(
            "provisioning bitmask-layout model {}: {error:#}",
            model.code
        )
    });

    // Extract the rebuilt ATT.binarypb from the generated module and resolve its n48
    // sub-block(s) the same way.
    let module_bytes = fs::read(module.path()).expect("reading generated module");
    let mut zip = ZipArchive::new(Cursor::new(module_bytes)).expect("opening generated module");
    let mut rebuilt_att = Vec::new();
    zip.by_name("system/vendor/firmware/uecapconfig/ATT.binarypb")
        .expect("generated bitmask module must contain ATT.binarypb")
        .read_to_end(&mut rebuilt_att)
        .expect("reading rebuilt ATT.binarypb from the generated module");
    let rebuilt_caps =
        UeCaps::decode(rebuilt_att.as_slice()).expect("decoding rebuilt ATT.binarypb");

    let rebuilt_matches = components(&rebuilt_caps)
        .filter(|cc| cc.band == ATT_N48_BAND)
        .filter_map(|cc| {
            resolve_selector(
                cc.dl_feature_per_cc_ids.as_deref(),
                &rebuilt_caps.dl_feature_per_cc_list,
            )
        })
        .filter(|resolved| resolved.len() >= 2)
        .collect::<Vec<_>>();
    assert!(
        !rebuilt_matches.is_empty(),
        "rebuilt ATT.binarypb has no non-uniform (>=2 CC) n48 DL sub-block at all — the pipeline \
         dropped the multi-CC selector down to a single CC (the collapse bug this test guards \
         against), or the selector no longer resolves"
    );
    for rebuilt in &rebuilt_matches {
        assert_eq!(
            rebuilt, &original,
            "rebuilt ATT n48 sub-block's resolved per-CC DL features must equal the original \
             file's — a mismatch (in particular, both CCs equal to the original's first record) \
             means the pipeline collapsed the non-uniform sub-block to a single CC"
        );
    }
}
