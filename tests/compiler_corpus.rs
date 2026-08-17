use kdl::{KdlDocument, KdlNode};
use pixel_uecaps_toolbox::{
    NR_BAND_OFFSET,
    compiler::{decompose, load_sources, provision_from_sources},
    model::{CapabilityLayout, PHONE_MODELS, PhoneModel},
    proto::{LteCaps, LteCombo, ShannonFeatureSetDlPerCcNr, ShannonFeatureSetUlPerCcNr, UeCaps},
};
use prost::Message;
use rayon::iter::{IndexedParallelIterator, IntoParallelRefIterator, ParallelIterator};
use std::{
    collections::{BTreeMap, BTreeSet},
    env, fs,
    io::{Cursor, Read, Write},
    path::Path,
};
use zip::{ZipArchive, ZipWriter, write::SimpleFileOptions};

const BITMASK_CORPUS: &str = "UECAPS_BITMASK_CORPUS";
const PROFILED_CORPUS: &str = "UECAPS_PROFILED_CORPUS";

fn is_profiled_lte_basename(name: &str) -> bool {
    let Some(decimal) = name
        .strip_prefix("lte_")
        .and_then(|name| name.strip_suffix(".binarypb"))
    else {
        return false;
    };
    !decimal.is_empty()
        && decimal.bytes().all(|byte| byte.is_ascii_digit())
        && decimal
            .parse::<u64>()
            .is_ok_and(|id| id.to_string() == decimal)
}

fn is_uecaps_basename(layout: CapabilityLayout, name: &str) -> bool {
    name.ends_with(".binarypb")
        && name != "ap_plmn_mapping.binarypb"
        && match layout {
            CapabilityLayout::Bitmask => true,
            CapabilityLayout::Profiled { .. } => !is_profiled_lte_basename(name),
        }
}

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
    for (directory, layout) in [
        (bitmask, CapabilityLayout::Bitmask),
        (
            profiled,
            CapabilityLayout::Profiled {
                nr_anchor: 0,
                lte_id: 0,
            },
        ),
    ] {
        for entry in fs::read_dir(directory).expect("reading an optional NR corpus directory") {
            let entry = entry.expect("reading an optional NR corpus entry");
            let name = entry.file_name();
            let Some(name) = name.to_str() else {
                continue;
            };
            if !is_uecaps_basename(layout, name) {
                continue;
            }
            let bytes = fs::read(entry.path()).expect("reading an optional NR corpus file");
            let caps = UeCaps::decode(bytes.as_slice()).unwrap_or_else(|error| {
                panic!("prost-decoding raw NR corpus file {name}: {error}")
            });
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
    for component in caps
        .combo_groups
        .iter()
        .flat_map(|group| &group.combo)
        .flat_map(|combo| &combo.sub_blocks)
    {
        if let Some(ids) = &component.dl_feature_per_cc_ids
            && let Some(first) = ids.first().copied()
            && (1..=caps.dl_feature_per_cc_list.len()).contains(&usize::from(first))
        {
            let expected = nr_cc_count(component.dl_bw_class).unwrap_or_else(|| {
                panic!(
                    "model {model_code} NR entry {basename} resolved DL selector has unknown bw_class {:?}",
                    component.dl_bw_class
                )
            });
            assert_eq!(
                ids.len(),
                expected,
                "model {model_code} NR entry {basename} has a resolved DL selector whose length disagrees with its bw_class CC count"
            );
            for &id in ids {
                dl_refs.insert(usize::from(id) - 1);
            }
        }
        if let Some(ids) = &component.ul_feature_per_cc_ids
            && let Some(first) = ids.first().copied()
            && (1..=caps.ul_feature_per_cc_list.len()).contains(&usize::from(first))
        {
            let expected = nr_cc_count(component.ul_bw_class).unwrap_or_else(|| {
                panic!(
                    "model {model_code} NR entry {basename} resolved UL selector has unknown bw_class {:?}",
                    component.ul_bw_class
                )
            });
            assert_eq!(
                ids.len(),
                expected,
                "model {model_code} NR entry {basename} has a resolved UL selector whose length disagrees with its bw_class CC count"
            );
            for &id in ids {
                ul_refs.insert(usize::from(id) - 1);
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
        caps.dl_feature_per_cc_list.windows(2).all(|pair| {
            let left = (
                pair[0].max_scs,
                pair[0].max_mimo,
                pair[0].max_bw,
                pair[0].max_mod_order,
                pair[0].bw_90mhz_supported,
            );
            let right = (
                pair[1].max_scs,
                pair[1].max_mimo,
                pair[1].max_bw,
                pair[1].max_mod_order,
                pair[1].bw_90mhz_supported,
            );
            left < right
        }),
        "model {model_code} NR entry {basename} DL list is not strictly canonical"
    );
    assert!(
        caps.ul_feature_per_cc_list.windows(2).all(|pair| {
            let left = (
                pair[0].max_scs,
                pair[0].max_mimo_cb,
                pair[0].max_bw,
                pair[0].max_mod_order,
                pair[0].bw_90mhz_supported,
                pair[0].max_mimo_non_cb,
            );
            let right = (
                pair[1].max_scs,
                pair[1].max_mimo_cb,
                pair[1].max_bw,
                pair[1].max_mod_order,
                pair[1].bw_90mhz_supported,
                pair[1].max_mimo_non_cb,
            );
            left < right
        }),
        "model {model_code} NR entry {basename} UL list is not strictly canonical"
    );
}

fn assert_module_nr_features(path: &Path, model: PhoneModel) {
    let model_code = model.code;
    let bytes = fs::read(path).unwrap_or_else(|error| {
        panic!("reading generated corpus module for model {model_code}: {error}")
    });
    let mut zip = ZipArchive::new(Cursor::new(bytes)).unwrap_or_else(|error| {
        panic!("opening generated corpus module for model {model_code}: {error}")
    });
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
    }
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
    let noncompact = UeCaps {
        dl_feature_per_cc_list: vec![ShannonFeatureSetDlPerCcNr {
            max_scs: Some(17),
            ..Default::default()
        }],
        ..Default::default()
    };

    let bitmask_module = temp.path().join("bitmask.zip");
    write_test_module_entry(&bitmask_module, "lte_FOO.binarypb", &noncompact);
    let bitmask_model = PHONE_MODELS
        .iter()
        .copied()
        .find(|model| model.is_bitmask())
        .expect("registered models must include a bitmask layout");
    let inspected = std::panic::catch_unwind(|| {
        assert_module_nr_features(&bitmask_module, bitmask_model);
    });
    assert!(
        inspected.is_err(),
        "generated bitmask lte_FOO.binarypb must be decoded and checked for compact catalogs"
    );

    let profiled_module = temp.path().join("profiled.zip");
    write_test_module_entry(&profiled_module, "lte_7.binarypb", &noncompact);
    let profiled_model = PHONE_MODELS
        .iter()
        .copied()
        .find(|model| matches!(model.layout, CapabilityLayout::Profiled { .. }))
        .expect("registered models must include a profiled layout");
    assert_module_nr_features(&profiled_module, profiled_model);
}

#[test]
fn optional_corpora_decompose_and_provision_every_registered_target() {
    let (Some(bitmask), Some(profiled)) =
        (env::var_os(BITMASK_CORPUS), env::var_os(PROFILED_CORPUS))
    else {
        eprintln!("skipping optional compiler corpus test: set both corpus variables");
        return;
    };

    let bitmask = Path::new(&bitmask);
    let profiled = Path::new(&profiled);
    let temp = tempfile::tempdir().expect("creating optional corpus test workspace");
    let first_source =
        tempfile::NamedTempFile::new_in(temp.path()).expect("creating the first source file");
    let second_source =
        tempfile::NamedTempFile::new_in(temp.path()).expect("creating the second source file");

    // A successful decompose has already reparsed/reserialized the canonical document and
    // self-verified every internal NR anchor, LTE ID, and mapping target. Repeating it also pins
    // directory-order-independent source bytes at the public boundary. The two decompose runs are
    // independent (distinct output files) and each is CPU-bound, so run them concurrently.
    rayon::join(
        || {
            decompose(bitmask, profiled, Some(first_source.path()))
                .expect("decomposing both optional corpora")
        },
        || {
            decompose(bitmask, profiled, Some(second_source.path()))
                .expect("re-decomposing both optional corpora")
        },
    );
    assert_eq!(
        fs::read(first_source.path()).expect("reading first canonical source"),
        fs::read(second_source.path()).expect("reading second canonical source"),
        "canonical source must be byte-idempotent"
    );
    let source = fs::read(first_source.path()).expect("reading canonical source for palette");
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
    // Parse and validate the multi-megabyte canonical source ONCE, then generate every registered
    // model from the shared validated set instead of re-reading and re-parsing it per model (was 52×,
    // which dominated this test's wall-clock). The models are independent — each reads the shared
    // immutable `sources` and writes its own `model-{index}.zip` — so fan them across rayon's pool.
    let sources =
        load_sources(first_source.path()).expect("parsing the canonical corpus source once");
    PHONE_MODELS
        .par_iter()
        .enumerate()
        .for_each(|(index, model)| {
            let out = temp.path().join(format!("model-{index}.zip"));
            provision_from_sources(&sources, model.code, &out, None).unwrap_or_else(|error| {
                panic!("provisioning registered model {}: {error:#}", model.code)
            });
            let module_len = fs::metadata(&out)
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
            assert_module_nr_features(&out, *model);
        });
}

/// Independently verify (against the real corpus) that every NR component's stored
/// `dl/ul_feature_index` equals what we derive from its per-CC feature set. This is the
/// invariant that lets `decompose` omit the field and `provision` re-derive it.
/// Env-gated exactly like the other corpus test.
#[test]
fn nr_feature_index_matches_derivation_formula() {
    let (Some(bitmask), Some(profiled)) =
        (env::var_os(BITMASK_CORPUS), env::var_os(PROFILED_CORPUS))
    else {
        eprintln!("skipping NR feature-index formula test: set both corpus variables");
        return;
    };

    /// All-or-nothing per-CC resolution (mirrors `report::combos::resolve_all`): every
    /// selector byte must be a valid 1-based catalog index, or none of them resolve.
    fn resolved_indices(ids: &Option<Vec<u8>>, len: usize) -> Option<Vec<usize>> {
        let ids = ids.as_ref()?;
        if ids.is_empty() {
            return None;
        }
        ids.iter()
            .map(|&b| {
                let k = b as usize;
                (1..=len).contains(&k).then(|| k - 1)
            })
            .collect()
    }

    let mut checked = 0u64;
    for dir in [Path::new(&bitmask), Path::new(&profiled)] {
        for entry in fs::read_dir(dir).expect("reading corpus dir") {
            let path = entry.expect("corpus entry").path();
            if path.extension().and_then(|x| x.to_str()) != Some("binarypb") {
                continue;
            }
            let bytes = fs::read(&path).expect("reading corpus file");
            let Ok(caps) = UeCaps::decode(&bytes[..]) else {
                continue; // non-UeCaps file (e.g. an LTE config)
            };
            let dl_list = &caps.dl_feature_per_cc_list;
            let ul_list = &caps.ul_feature_per_cc_list;
            for cc in caps
                .combo_groups
                .iter()
                .flat_map(|g| &g.combo)
                .flat_map(|c| &c.sub_blocks)
                .filter(|cc| cc.band >= NR_BAND_OFFSET)
            {
                let derived_dl = match resolved_indices(&cc.dl_feature_per_cc_ids, dl_list.len()) {
                    None => 0,
                    Some(indices) => {
                        let per_cc: Vec<i32> = indices
                            .iter()
                            .map(|&i| {
                                let scs = dl_list[i].max_scs.unwrap_or(0);
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
                let derived_ul = match resolved_indices(&cc.ul_feature_per_cc_ids, ul_list.len()) {
                    None => 0,
                    Some(indices) => {
                        let per_cc: Vec<i32> = indices
                            .iter()
                            .map(|&i| {
                                let cb = ul_list[i].max_mimo_cb.unwrap_or(0);
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
    let (Some(bitmask), Some(profiled)) =
        (env::var_os(BITMASK_CORPUS), env::var_os(PROFILED_CORPUS))
    else {
        eprintln!("skipping LTE feature-index always-Some test: set both corpus variables");
        return;
    };

    let mut checked = 0u64;
    for dir in [Path::new(&bitmask), Path::new(&profiled)] {
        for entry in fs::read_dir(dir).expect("reading corpus dir") {
            let path = entry.expect("corpus entry").path();
            if path.extension().and_then(|x| x.to_str()) != Some("binarypb") {
                continue;
            }
            let bytes = fs::read(&path).expect("reading corpus file");
            let Ok(caps) = UeCaps::decode(&bytes[..]) else {
                continue; // non-UeCaps file (e.g. an LTE-only config)
            };
            for cc in caps
                .combo_groups
                .iter()
                .flat_map(|g| &g.combo)
                .flat_map(|c| &c.sub_blocks)
                .filter(|cc| cc.band > 0 && cc.band < NR_BAND_OFFSET)
            {
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
    }
    assert!(
        checked > 0,
        "no LTE components checked — are the corpus dirs populated?"
    );
    eprintln!("verified LTE feature-index always-Some on {checked} LTE components");
}

/// Resolve a selector's bytes against a per-CC feature catalog the same way the compiler
/// does: all-or-nothing, 1-based, in order. `None` means "not fully resolved" (out-of-range
/// byte, missing selector, or empty selector) — mirrors `report::combos::resolve_all`
/// (`pub(crate)`, unreachable from this integration test crate).
fn resolve_selector<T: Clone>(ids: Option<&[u8]>, list: &[T]) -> Option<Vec<T>> {
    let ids = ids?;
    if ids.is_empty() {
        return None;
    }
    ids.iter()
        .map(|&byte| {
            let index = usize::from(byte);
            (1..=list.len())
                .contains(&index)
                .then(|| list[index - 1].clone())
        })
        .collect()
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
    let (Some(bitmask), Some(profiled)) =
        (env::var_os(BITMASK_CORPUS), env::var_os(PROFILED_CORPUS))
    else {
        eprintln!("skipping ATT n48 non-uniform sub-block regression: set both corpus variables");
        return;
    };
    const ATT_N48_BAND: i32 = NR_BAND_OFFSET + 48;

    let bitmask = Path::new(&bitmask);
    let profiled = Path::new(&profiled);

    // Capture ground truth directly from the raw corpus file — no compiler code involved.
    let att_path = bitmask.join("ATT.binarypb");
    let att_bytes = fs::read(&att_path)
        .unwrap_or_else(|error| panic!("reading {}: {error}", att_path.display()));
    let att_caps = UeCaps::decode(att_bytes.as_slice())
        .unwrap_or_else(|error| panic!("decoding {}: {error}", att_path.display()));
    let original = att_caps
        .combo_groups
        .iter()
        .flat_map(|group| &group.combo)
        .flat_map(|combo| &combo.sub_blocks)
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
    let temp = tempfile::tempdir().expect("creating regression test workspace");
    let source =
        tempfile::NamedTempFile::new_in(temp.path()).expect("creating the regression source file");
    decompose(bitmask, profiled, Some(source.path()))
        .expect("decoding the corpus for the regression test");
    let sources = load_sources(source.path()).expect("parsing the decomposed canonical source");
    let model = PHONE_MODELS
        .iter()
        .find(|model| model.is_bitmask())
        .expect("the registered model list must include at least one bitmask-layout model");
    let module_path = temp.path().join("module.zip");
    provision_from_sources(&sources, model.code, &module_path, None).unwrap_or_else(|error| {
        panic!(
            "provisioning bitmask-layout model {}: {error:#}",
            model.code
        )
    });

    // Extract the rebuilt ATT.binarypb from the generated module and resolve its n48
    // sub-block(s) the same way.
    let module_bytes = fs::read(&module_path).expect("reading generated module");
    let mut zip = ZipArchive::new(Cursor::new(module_bytes)).expect("opening generated module");
    let mut rebuilt_att = None;
    for index in 0..zip.len() {
        let mut entry = zip.by_index(index).expect("reading generated module entry");
        if entry.name() == "system/vendor/firmware/uecapconfig/ATT.binarypb" {
            let mut payload = Vec::new();
            entry
                .read_to_end(&mut payload)
                .expect("reading rebuilt ATT.binarypb from the generated module");
            rebuilt_att = Some(payload);
            break;
        }
    }
    let rebuilt_att = rebuilt_att.expect("generated bitmask module must contain ATT.binarypb");
    let rebuilt_caps =
        UeCaps::decode(rebuilt_att.as_slice()).expect("decoding rebuilt ATT.binarypb");

    let rebuilt_matches = rebuilt_caps
        .combo_groups
        .iter()
        .flat_map(|group| &group.combo)
        .flat_map(|combo| &combo.sub_blocks)
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
