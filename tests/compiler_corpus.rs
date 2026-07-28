use std::{
    collections::{BTreeMap, BTreeSet},
    env, fs,
    io::{Cursor, Read},
    path::Path,
};

use pixel_uecaps_toolbox::{
    NR_BAND_OFFSET,
    compiler::{decompose, load_sources, provision_from_sources},
    model::PHONE_MODELS,
    proto::{LteCaps, LteCombo, UeCaps},
};
use prost::Message;
use rayon::iter::{IndexedParallelIterator, IntoParallelRefIterator, ParallelIterator};
use zip::ZipArchive;

const BITMASK_CORPUS: &str = "UECAPS_BITMASK_CORPUS";
const PROFILED_CORPUS: &str = "UECAPS_PROFILED_CORPUS";

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

fn assert_module_nr_features(path: &Path, model_code: &str) {
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
        if basename == "ap_plmn_mapping.binarypb" || basename.starts_with("lte_") {
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
    let first_source = temp.path().join("source-a");
    let second_source = temp.path().join("source-b");

    // A successful decompose has already reparsed/reserialized both canonical documents and
    // self-verified every internal NR anchor, LTE ID, and mapping target. Repeating it also pins
    // directory-order-independent source bytes at the public boundary. The two decompose runs are
    // independent (distinct output dirs) and each is CPU-bound, so run them concurrently.
    rayon::join(
        || decompose(bitmask, profiled, &first_source).expect("decomposing both optional corpora"),
        || {
            decompose(bitmask, profiled, &second_source)
                .expect("re-decomposing both optional corpora")
        },
    );
    for document in ["nr.kdl", "lte.kdl"] {
        assert_eq!(
            fs::read(first_source.join(document)).expect("reading first canonical source"),
            fs::read(second_source.join(document)).expect("reading second canonical source"),
            "canonical source must be byte-idempotent"
        );
    }

    let sequences = read_lte_sequences(profiled);
    assert_lte_invariants(&sequences);
    // Parse and validate the ~19 MB canonical source ONCE, then generate every registered model
    // from the shared validated set instead of re-reading and re-parsing it per model (was 52×,
    // which dominated this test's wall-clock). The models are independent — each reads the shared
    // immutable `sources` and writes its own `model-{index}.zip` — so fan them across rayon's pool.
    let sources = load_sources(&first_source).expect("parsing the canonical corpus source once");
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
            assert_module_nr_features(&out, model.code);
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
/// `dl_feature_index`/`ul_feature_index` are always `Some` in the real corpus, so an absent
/// `dl-feature`/`ul-feature` in KDL unambiguously means "omitted zero", never a genuine `None`.
/// Env-gated exactly like the other corpus tests.
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
    let source_dir = temp.path().join("source");
    decompose(bitmask, profiled, &source_dir).expect("decoding the corpus for the regression test");
    let sources = load_sources(&source_dir).expect("parsing the decomposed canonical source");
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
