//! `patch` — create and apply band-combination patches between capability files.

pub(crate) mod build;
pub(crate) mod filter;
pub(crate) mod format;
pub(crate) mod lte;
pub(crate) mod show;

pub use filter::{FilterMode, filter};
pub use show::show;

use anyhow::Context;
use prost::Message;
use std::{io::Read, path::Path};

use self::format::{Kind, NrPatch, Patch, PatchCombo, SetEntry, SetKind};
use crate::{
    model::{Parsed, parse_name},
    proto::LteCaps,
    raw_nr::{RawNrPayload, RawNrPayloadKey},
    report::combos::{Combo, combo_key},
};
use std::collections::BTreeMap;

fn load_lte(path: &Path) -> anyhow::Result<LteCaps> {
    let bytes = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
    // Strict: LTE fallback files round-trip bit-for-bit, so an unmodeled field must
    // fail closed here rather than be silently dropped when `create` re-reads or
    // `apply` rewrites the file. See DESIGN.md "Invariants".
    crate::wire::decode_lte_caps(&bytes, &path.display().to_string())
}

/// Full-field canonical form of one combo: header + bitmask + sorted CCs.
#[derive(PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct CanonCombo {
    payload: RawNrPayloadKey,
    bit_mask: u32,
}

fn canon_combo(c: &Combo) -> CanonCombo {
    let payload = RawNrPayload::from(c);
    CanonCombo {
        payload: RawNrPayloadKey::from(&payload),
        bit_mask: c.bit_mask,
    }
}

/// Order-independent canonical form for all variants under one key.
pub(crate) fn canon_variants(combos: &[&Combo]) -> Vec<CanonCombo> {
    let mut v: Vec<CanonCombo> = combos.iter().map(|c| canon_combo(c)).collect();
    v.sort_unstable();
    v
}

fn index_by_key<T>(items: &[T], key: impl Fn(&T) -> String) -> BTreeMap<String, Vec<&T>> {
    let mut m: BTreeMap<String, Vec<&T>> = BTreeMap::new();
    for item in items {
        m.entry(key(item)).or_default().push(item);
    }
    m
}

/// Diff A -> B at full-field granularity into a patch.
pub(crate) fn build_patch(a: &[Combo], b: &[Combo]) -> NrPatch {
    let ia = index_by_key(a, combo_key);
    let ib = index_by_key(b, combo_key);

    let delete: Vec<String> = ia
        .keys()
        .filter(|k| !ib.contains_key(*k))
        .cloned()
        .collect();

    let mut set = Vec::new();
    for (k, b_combos) in &ib {
        let (differs, kind) = match ia.get(k) {
            None => (true, SetKind::Add),
            Some(a_combos) => (
                canon_variants(a_combos) != canon_variants(b_combos),
                SetKind::Change,
            ),
        };
        if differs {
            set.push(SetEntry {
                kind,
                combo: b_combos.iter().map(|c| PatchCombo::from_combo(c)).collect(),
            });
        }
    }
    NrPatch {
        kind: Kind::Nr,
        version: 1,
        delete,
        set,
    }
}

/// Reject combos whose component band cannot be represented as a plain band number,
/// *before* diffing — so `build_patch` never panics in `RawSubBlock::from_sub_block` on an
/// uninvertible band and the emitted patch never carries an out-of-range band the parser
/// would reject (R3/R12).
fn validate_nr_combo_bands(combos: &[Combo]) -> anyhow::Result<()> {
    for combo in combos {
        for cc in &combo.sub_blocks {
            crate::raw_nr::RawSubBlock::try_from_sub_block(cc)?;
        }
    }
    Ok(())
}

/// A path's file name as `&str`, or `"?"` if it has none / isn't UTF-8.
fn file_label(p: &Path) -> &str {
    p.file_name().and_then(|s| s.to_str()).unwrap_or("?")
}

/// Read a patch's KDL text from `input` (a file) or stdin.
fn read_patch_source(input: Option<&Path>) -> anyhow::Result<String> {
    match input {
        Some(p) => {
            std::fs::read_to_string(p).with_context(|| format!("reading patch {}", p.display()))
        }
        None => {
            let mut s = String::new();
            std::io::stdin()
                .lock()
                .read_to_string(&mut s)
                .context("reading patch from stdin")?;
            Ok(s)
        }
    }
}

/// `patch create <A> <B>`: diff A->B and write the patch KDL to `out` or stdout.
pub fn create(a: &Path, b: &Path, out: Option<&Path>) -> anyhow::Result<i32> {
    let na = file_label(a);
    let nb = file_label(b);
    let patch = match (parse_name(na), parse_name(nb)) {
        (Parsed::Carrier { .. }, Parsed::Carrier { .. }) => {
            // Strict load on this write path (mirrors NR `apply`'s `decode_base`): an
            // unmodeled field must fail closed, not be dropped in the emitted patch.
            let ca = crate::report::load_carrier_combos_strict(a)?;
            let cb = crate::report::load_carrier_combos_strict(b)?;
            // Reject uninvertible / out-of-range component bands before diffing, so the
            // diff cannot panic and the emitted patch is always re-parseable (R3/R12).
            validate_nr_combo_bands(&ca.combos).with_context(|| format!("in {na}"))?;
            validate_nr_combo_bands(&cb.combos).with_context(|| format!("in {nb}"))?;
            Patch::Nr(build_patch(&ca.combos, &cb.combos))
        }
        (Parsed::Lte(_), Parsed::Lte(_)) => {
            let la = load_lte(a)?;
            let lb = load_lte(b)?;
            Patch::Lte(lte::build_lte_patch(&la.combos, &lb.combos))
        }
        _ => anyhow::bail!(
            "patch create needs two files of the same kind (both <CARRIER>_<NUMBER> or both lte_*)"
        ),
    };
    // Never emit a patch our own parser would reject (R12) — e.g. an empty derived key.
    format::validate_patch(&patch)?;
    let text = format::to_kdl(&patch)?;
    crate::output::write_out(text.as_bytes(), out, "patch")?;
    Ok(0)
}

/// `patch apply <BASE>`: apply a patch to a base file -> new `.binarypb`.
pub fn apply(
    base: &Path,
    input: Option<&Path>,
    out: Option<&Path>,
    strict: bool,
) -> anyhow::Result<i32> {
    let filename = file_label(base);
    let patch_text = read_patch_source(input)?;
    let patch = format::from_kdl(&patch_text)?;
    let base_bytes =
        std::fs::read(base).with_context(|| format!("reading base {}", base.display()))?;

    let (bytes, outcome) = match (patch, parse_name(filename)) {
        (Patch::Nr(fp), Parsed::Carrier { .. }) => {
            let caps = build::decode_base(&base_bytes)?;
            let (result, outcome) = build::apply_patch(&caps, &fp, strict)?;
            (result.encode_to_vec(), outcome)
        }
        (Patch::Lte(lp), Parsed::Lte(_)) => {
            // Strict: the base is rewritten, so an unmodeled field must fail closed.
            let caps = crate::wire::decode_lte_caps(&base_bytes, &base.display().to_string())?;
            let (result, outcome) = lte::apply_lte_patch(&caps, &lp, strict)?;
            (result.encode_to_vec(), outcome)
        }
        (Patch::Nr(_), _) => {
            anyhow::bail!("{filename}: an nr/carrier patch needs a <CARRIER>_<NUMBER> base")
        }
        (Patch::Lte(_), _) => anyhow::bail!("{filename}: an lte patch needs an lte_* base"),
    };

    crate::output::write_out(&bytes, out, "")?;
    for s in &outcome.skipped {
        eprintln!("warning: {s}");
    }
    eprintln!(
        "applied {} entries ({} deleted, {} set), skipped {}",
        outcome.deleted + outcome.set,
        outcome.deleted,
        outcome.set,
        outcome.skipped.len(),
    );
    Ok(i32::from(!outcome.skipped.is_empty()))
}

#[cfg(test)]
mod tests {
    use super::*; // Combo, build_patch, and format come via the glob.
    use crate::report::combos::SubBlock;

    fn nr_combo(band_n: i32, dl_max_mimo: i32) -> Combo {
        let dl_feature_per_cc = crate::proto::ShannonFeatureSetDlPerCcNr {
            max_bw: Some(100),
            max_mimo: Some(dl_max_mimo),
            ..Default::default()
        };
        Combo {
            bit_mask: 0,
            sub_blocks: vec![SubBlock {
                band: format!("n{band_n}"),
                dl_bw_class: Some(1),
                ul_bw_class: Some(1),
                dl_features: vec![dl_feature_per_cc],
                dl_max_bw_mhz: Some(100),
                dl_mimo: Some(match dl_max_mimo {
                    1 => "2x2".to_string(),
                    2 => "4x4".to_string(),
                    3 => "8x8".to_string(),
                    n => format!("({n})"),
                }),
                ..Default::default()
            }],
            ..Default::default()
        }
    }

    fn nr_combo_with_raw_ids(band_n: i32, dl_ids: Vec<u8>, ul_ids: Vec<u8>) -> Combo {
        Combo {
            bit_mask: 0,
            sub_blocks: vec![SubBlock {
                band: format!("n{band_n}"),
                dl_bw_class: Some(1),
                ul_bw_class: Some(1),
                dl_feature_per_cc_ids: Some(dl_ids),
                ul_feature_per_cc_ids: Some(ul_ids),
                ..Default::default()
            }],
            ..Default::default()
        }
    }

    #[test]
    fn build_patch_classifies_add_change_delete() {
        let a = vec![nr_combo(78, 2), nr_combo(41, 2)];
        let b = vec![nr_combo(78, 3), nr_combo(2, 2)];
        let p = build_patch(&a, &b);
        assert_eq!(p.delete, vec!["n41A".to_string()]);
        let keys: Vec<String> = p
            .set
            .iter()
            .map(|s| format::set_entry_key(s).unwrap())
            .collect();
        assert_eq!(keys, vec!["n2A".to_string(), "n78A".to_string()]); // sorted
        let by_key = |k: &str| {
            p.set
                .iter()
                .find(|s| format::set_entry_key(s).unwrap() == k)
                .unwrap()
        };
        assert_eq!(by_key("n2A").kind, SetKind::Add);
        assert_eq!(by_key("n78A").kind, SetKind::Change);
        assert_eq!(
            by_key("n78A").combo[0].sub_blocks[0].dl_features[0].max_mimo,
            Some(3)
        );
    }

    #[test]
    fn build_patch_detects_bitmask_only_change() {
        // Same key, same caps signature, different bit_mask -> still a `change`.
        let mut a0 = nr_combo(78, 2);
        a0.bit_mask = 1;
        let b0 = nr_combo(78, 2); // bit_mask 0
        let p = build_patch(&[a0], &[b0]);
        assert!(p.delete.is_empty());
        assert_eq!(p.set.len(), 1);
        assert_eq!(format::set_entry_key(&p.set[0]).unwrap(), "n78A");
        assert_eq!(p.set[0].combo[0].bit_mask, 0);
    }

    #[test]
    fn build_patch_detects_raw_feature_value_change() {
        let mut a = nr_combo(78, 2);
        let mut b = nr_combo(78, 3);
        a.sub_blocks[0].dl_feature_per_cc_ids = Some(vec![1]);
        b.sub_blocks[0].dl_feature_per_cc_ids = Some(vec![1]);

        let p = build_patch(&[a], &[b]);

        assert!(p.delete.is_empty());
        assert_eq!(p.set.len(), 1);
        assert_eq!(format::set_entry_key(&p.set[0]).unwrap(), "n78A");
        assert_eq!(
            p.set[0].combo[0].sub_blocks[0].dl_features[0].max_mimo,
            Some(3)
        );
    }

    #[test]
    fn build_patch_detects_unknown_raw_scs_change_when_display_is_none() {
        let mut a = nr_combo(78, 2);
        let mut b = nr_combo(78, 2);
        a.sub_blocks[0].dl_features[0].max_scs = Some(7);
        b.sub_blocks[0].dl_features[0].max_scs = Some(8);
        a.sub_blocks[0].dl_scs_khz = None;
        b.sub_blocks[0].dl_scs_khz = None;

        let p = build_patch(&[a], &[b]);

        assert!(p.delete.is_empty());
        assert_eq!(p.set.len(), 1);
        assert_eq!(format::set_entry_key(&p.set[0]).unwrap(), "n78A");
        assert_eq!(
            p.set[0].combo[0].sub_blocks[0].dl_features[0].max_scs,
            Some(8)
        );
    }

    #[test]
    fn build_patch_detects_raw_selector_only_change() {
        let a = nr_combo_with_raw_ids(78, vec![1], vec![1]);
        let b = nr_combo_with_raw_ids(78, vec![2], vec![1]);

        let p = build_patch(&[a], &[b]);

        assert!(p.delete.is_empty());
        assert_eq!(p.set.len(), 1);
        assert_eq!(format::set_entry_key(&p.set[0]).unwrap(), "n78A");
        assert_eq!(p.set[0].combo[0].sub_blocks[0].dl_cc_ids, Some(vec![2]));
    }

    #[test]
    fn build_patch_detects_change_when_b_has_an_all_absent_resolved_wrapper() {
        // B's `dl_features` is a non-empty vec whose sole entry has every field `None` — a
        // legitimate all-absent catalog record, not "no feature set". Since Task 7 removed
        // the patch axis's old "does the entry have any field set" presence gate, this
        // counts as present (matching the compiler/protobuf-ingest axis), so the emitted
        // `cc` carries that resolved (empty-valued) feature set rather than falling back to
        // raw selector-byte comparison.
        let mut a = nr_combo_with_raw_ids(78, vec![], vec![]);
        let mut b = nr_combo_with_raw_ids(78, vec![1], vec![]);
        a.sub_blocks[0].dl_feature_per_cc_ids = None;
        a.sub_blocks[0].ul_feature_per_cc_ids = None;
        b.sub_blocks[0].ul_feature_per_cc_ids = None;
        b.sub_blocks[0].dl_features = vec![crate::proto::ShannonFeatureSetDlPerCcNr::default()];

        let p = build_patch(&[a], &[b]);

        assert!(p.delete.is_empty());
        assert_eq!(p.set.len(), 1);
        assert_eq!(format::set_entry_key(&p.set[0]).unwrap(), "n78A");
        // The raw ids are still carried on the struct (from_sub_block never clears them),
        // but the resolved (all-absent) feature set is what identity/the writer honor.
        assert_eq!(p.set[0].combo[0].sub_blocks[0].dl_cc_ids, Some(vec![1]));
        assert_eq!(
            p.set[0].combo[0].sub_blocks[0].dl_feature_set(),
            Some(crate::proto::ShannonFeatureSetDlPerCcNr::default())
        );
    }

    /// The patch-side half of the data-loss fix: `create` between two files differing
    /// ONLY in a non-first CC's resolved feature within one multi-CC sub-block must
    /// produce a `change` entry that carries both CCs' features. Before Task 7,
    /// `RawSubBlock::from_sub_block` truncated to CC0, so this second-CC-only difference
    /// was invisible to `canon_combo`/`build_patch` and silently collapsed to "identical".
    #[test]
    fn build_patch_detects_change_in_non_first_cc_feature_only() {
        let cc0 = crate::proto::ShannonFeatureSetDlPerCcNr {
            max_bw: Some(40),
            max_scs: Some(1),
            ..Default::default()
        };
        let make_combo = |cc1_scs: i32| Combo {
            bit_mask: 0,
            sub_blocks: vec![SubBlock {
                band: "n48".to_string(),
                dl_bw_class: Some(2), // class B, 2 CCs (NR_CC_COUNTS)
                dl_features: vec![
                    cc0,
                    crate::proto::ShannonFeatureSetDlPerCcNr {
                        max_bw: Some(100),
                        max_scs: Some(cc1_scs),
                        ..Default::default()
                    },
                ],
                ..Default::default()
            }],
            ..Default::default()
        };
        let a = make_combo(2); // CC1 max_scs=2
        let b = make_combo(3); // CC1 max_scs=3 — only this differs

        let p = build_patch(&[a], &[b]);

        assert!(p.delete.is_empty());
        assert_eq!(
            p.set.len(),
            1,
            "a second-CC-only feature change must produce a `set` entry, not collapse to no diff"
        );
        assert_eq!(format::set_entry_key(&p.set[0]).unwrap(), "n48B↓");
        let cc = &p.set[0].combo[0].sub_blocks[0];
        assert_eq!(cc.dl_features.len(), 2, "both CCs must survive the diff");
        assert_eq!(cc.dl_features[0].max_scs, Some(1));
        assert_eq!(cc.dl_features[1].max_scs, Some(3));
    }

    #[test]
    fn build_patch_ignores_dl_selector_change_when_dl_resolved_features_match() {
        let mut a = nr_combo(78, 2);
        a.sub_blocks[0].dl_feature_per_cc_ids = Some(vec![1]);
        a.sub_blocks[0].ul_feature_per_cc_ids = Some(vec![1]);
        let mut b = a.clone();
        b.sub_blocks[0].dl_feature_per_cc_ids = Some(vec![7]);

        let p = build_patch(&[a], &[b]);

        assert!(p.delete.is_empty());
        assert!(p.set.is_empty());
    }

    #[test]
    fn build_patch_detects_ul_selector_change_when_ul_features_absent() {
        let mut a = nr_combo(78, 2);
        a.sub_blocks[0].dl_feature_per_cc_ids = Some(vec![1]);
        a.sub_blocks[0].ul_feature_per_cc_ids = Some(vec![1]);
        let mut b = a.clone();
        b.sub_blocks[0].ul_feature_per_cc_ids = Some(vec![9]);

        let p = build_patch(&[a], &[b]);

        assert!(p.delete.is_empty());
        assert_eq!(p.set.len(), 1);
        assert_eq!(format::set_entry_key(&p.set[0]).unwrap(), "n78A");
        assert_eq!(p.set[0].combo[0].sub_blocks[0].ul_cc_ids, Some(vec![9]));
    }

    #[test]
    fn build_patch_ignores_ul_selector_change_when_ul_resolved_features_match() {
        let mut a = nr_combo(78, 2);
        a.sub_blocks[0].ul_max_bw_mhz = Some(100);
        a.sub_blocks[0].ul_features = vec![crate::proto::ShannonFeatureSetUlPerCcNr {
            max_bw: Some(100),
            ..Default::default()
        }];
        a.sub_blocks[0].ul_feature_per_cc_ids = Some(vec![1]);
        let mut b = a.clone();
        b.sub_blocks[0].ul_feature_per_cc_ids = Some(vec![9]);

        let p = build_patch(&[a], &[b]);

        assert!(p.delete.is_empty());
        assert!(p.set.is_empty());
    }

    #[test]
    fn build_patch_identical_is_empty() {
        let a = vec![nr_combo(78, 2)];
        let p = build_patch(&a, &a);
        assert!(p.delete.is_empty());
        assert!(p.set.is_empty());
    }

    #[test]
    fn create_then_apply_reproduces_b_combos() {
        use crate::proto::{ComboGroup, UeCaps, combo_group, combo_group::combo::SubBlock};
        use prost::Message;

        fn caps_with(band: i32) -> UeCaps {
            UeCaps {
                version: 874_888_686,
                combo_groups: vec![ComboGroup {
                    combo_header: None,
                    combo: vec![combo_group::Combo {
                        bitmask: Some(0),
                        sub_blocks: vec![SubBlock {
                            band,
                            dl_bw_class: Some(1),
                            ul_bw_class: Some(1),
                            ..Default::default()
                        }],
                    }],
                }],
                ..Default::default()
            }
        }

        let dir = std::env::temp_dir().join(format!("uecaps-e2e-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let a = dir.join("AAA_1.binarypb");
        let b = dir.join("BBB_2.binarypb");
        std::fs::write(&a, caps_with(10078).encode_to_vec()).unwrap(); // n78A
        std::fs::write(&b, caps_with(10002).encode_to_vec()).unwrap(); // n2A
        let patch_path = dir.join("p.kdl");
        let outp = dir.join("out.binarypb");

        create(&a, &b, Some(&patch_path)).unwrap();
        let code = apply(&a, Some(&patch_path), Some(&outp), false).unwrap();

        let result = UeCaps::decode(&std::fs::read(&outp).unwrap()[..]).unwrap();
        std::fs::remove_dir_all(&dir).ok();

        assert_eq!(code, 0);
        assert_eq!(
            build::present_keys(&result),
            build::present_keys(&caps_with(10002))
        );
        assert_eq!(result.version, 874_888_686); // base identity preserved
    }

    #[test]
    fn create_then_apply_lte_reproduces_b_combos() {
        use crate::{
            proto::{LteCaps, LteCombo, LteComponent},
            report::lte::lte_combo_key,
        };
        use prost::Message;
        use std::collections::BTreeSet;

        fn make_lte_combo(band: i32, ul: i32, bcs: u64, unknown1: u64, unknown2: u64) -> LteCombo {
            LteCombo {
                components: vec![LteComponent {
                    band,
                    dl_bw_class_mimo: 32768,
                    ul_bw_class_mimo: Some(ul),
                }],
                bcs: Some(bcs),
                unknown1: Some(unknown1),
                unknown2: Some(unknown2),
            }
        }

        fn make_lte_caps(combos: Vec<LteCombo>) -> LteCaps {
            LteCaps {
                fingerprint: 874_888_686,
                combos,
                bitmask: 42,
            }
        }

        let dir = std::env::temp_dir().join(format!("uecaps-lte-e2e-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();

        // A: B1A + B5A↓   B: B1A + B7A↓ (non-zero bcs/unknown to prove field survival)
        let caps_a = make_lte_caps(vec![
            make_lte_combo(1, 32768, 0, 0, 0), // B1A
            make_lte_combo(5, 0, 0, 0, 0),     // B5A↓
        ]);
        let caps_b = make_lte_caps(vec![
            make_lte_combo(1, 32768, 0, 0, 0), // B1A
            make_lte_combo(7, 0, 7, 8, 9),     // B7A↓ with non-zero bcs/unknown
        ]);

        let a = dir.join("lte_400907661.binarypb");
        let b = dir.join("lte_2160127815.binarypb");
        std::fs::write(&a, caps_a.encode_to_vec()).unwrap();
        std::fs::write(&b, caps_b.encode_to_vec()).unwrap();

        let patch_path = dir.join("lte_patch.kdl");
        let outp = dir.join("lte_out.binarypb");

        create(&a, &b, Some(&patch_path)).unwrap();
        let code = apply(&a, Some(&patch_path), Some(&outp), false).unwrap();
        assert_eq!(code, 0);

        let result = LteCaps::decode(&std::fs::read(&outp).unwrap()[..]).unwrap();
        std::fs::remove_dir_all(&dir).ok();

        // Key set matches B
        let got_keys: BTreeSet<String> = result.combos.iter().map(lte_combo_key).collect();
        let want_keys: BTreeSet<String> = caps_b.combos.iter().map(lte_combo_key).collect();
        assert_eq!(got_keys, want_keys);

        // Base identity preserved
        assert_eq!(result.fingerprint, 874_888_686);
        assert_eq!(result.bitmask, 42);

        // B7A↓ field values survive create→kdl→apply rebuild
        let b7 = result
            .combos
            .iter()
            .find(|c| lte_combo_key(c) == "B7A↓")
            .unwrap();
        assert_eq!(b7.bcs, Some(7));
        assert_eq!(b7.unknown1, Some(8));
        assert_eq!(b7.unknown2, Some(9));
        assert_eq!(b7.components[0].ul_bw_class_mimo, Some(0));
    }

    #[test]
    fn create_writes_patch_to_file() {
        use crate::proto::{
            ComboGroup, ShannonFeatureSetDlPerCcNr, UeCaps, combo_group,
            combo_group::combo::SubBlock,
        };
        use prost::Message;

        fn caps_with(band: i32) -> Vec<u8> {
            UeCaps {
                version: 874_888_686,
                dl_feature_per_cc_list: vec![ShannonFeatureSetDlPerCcNr {
                    max_bw: Some(100),
                    max_mimo: Some(2),
                    ..Default::default()
                }],
                combo_groups: vec![ComboGroup {
                    combo_header: None,
                    combo: vec![combo_group::Combo {
                        bitmask: Some(0),
                        sub_blocks: vec![SubBlock {
                            band,
                            dl_bw_class: Some(1),
                            ul_bw_class: Some(1),
                            dl_feature_per_cc_ids: Some(vec![1]),
                            ..Default::default()
                        }],
                    }],
                }],
                ..Default::default()
            }
            .encode_to_vec()
        }

        let dir = std::env::temp_dir().join(format!("uecaps-create-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let a = dir.join("AAA_1.binarypb");
        let b = dir.join("BBB_2.binarypb");
        std::fs::write(&a, caps_with(10078)).unwrap(); // n78A
        std::fs::write(&b, caps_with(10002)).unwrap(); // n2A
        let outp = dir.join("p.kdl");

        let code = create(&a, &b, Some(&outp)).unwrap();
        let text = std::fs::read_to_string(&outp).unwrap();
        std::fs::remove_dir_all(&dir).ok();

        assert_eq!(code, 0);
        let format::Patch::Nr(p) = format::from_kdl(&text).unwrap() else {
            panic!("expected nr patch")
        };
        assert_eq!(p.delete, vec!["n78A".to_string()]);
        assert_eq!(p.set.len(), 1);
        assert_eq!(format::set_entry_key(&p.set[0]).unwrap(), "n2A");
    }

    // R1: the re-encoding write paths must reject a file carrying a field number
    // outside the modeled schema, instead of silently dropping those bytes on rewrite.
    // The tamper `[0x20, 0x05]` is field #4 (varint) — unmodeled for both LteCaps and
    // UeCaps; prost decodes past it, so only the strict wire validator catches it.

    fn lte_bytes(band: i32) -> Vec<u8> {
        use crate::proto::{LteCaps, LteCombo, LteComponent};
        LteCaps {
            fingerprint: 874_888_686,
            combos: vec![LteCombo {
                components: vec![LteComponent {
                    band,
                    dl_bw_class_mimo: 32768,
                    ul_bw_class_mimo: Some(0),
                }],
                bcs: Some(0),
                unknown1: Some(0),
                unknown2: Some(0),
            }],
            bitmask: 0,
        }
        .encode_to_vec()
    }

    fn nr_bytes(band: i32) -> Vec<u8> {
        use crate::proto::{ComboGroup, UeCaps, combo_group, combo_group::combo::SubBlock};
        UeCaps {
            version: 874_888_686,
            combo_groups: vec![ComboGroup {
                combo_header: None,
                combo: vec![combo_group::Combo {
                    bitmask: Some(0),
                    sub_blocks: vec![SubBlock {
                        band,
                        dl_bw_class: Some(1),
                        ul_bw_class: Some(1),
                        ..Default::default()
                    }],
                }],
            }],
            ..Default::default()
        }
        .encode_to_vec()
    }

    #[test]
    fn create_lte_rejects_unmodeled_field() {
        let dir = std::env::temp_dir().join(format!("uecaps-r1-lc-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut a_bytes = lte_bytes(1);
        a_bytes.extend_from_slice(&[0x20, 0x05]); // LteCaps field #4 — not modeled
        let a = dir.join("lte_400907661.binarypb");
        let b = dir.join("lte_2160127815.binarypb");
        std::fs::write(&a, &a_bytes).unwrap();
        std::fs::write(&b, lte_bytes(7)).unwrap();

        let r = create(&a, &b, Some(&dir.join("p.kdl")));
        std::fs::remove_dir_all(&dir).ok();
        assert!(
            r.is_err(),
            "create must fail closed on an unmodeled LTE field"
        );
    }

    #[test]
    fn create_nr_rejects_unmodeled_field() {
        let dir = std::env::temp_dir().join(format!("uecaps-r1-nc-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut a_bytes = nr_bytes(10078); // n78A
        a_bytes.extend_from_slice(&[0x20, 0x05]); // UeCaps field #4 — not modeled
        let a = dir.join("AAA_1.binarypb");
        let b = dir.join("BBB_2.binarypb");
        std::fs::write(&a, &a_bytes).unwrap();
        std::fs::write(&b, nr_bytes(10002)).unwrap();

        let r = create(&a, &b, Some(&dir.join("p.kdl")));
        std::fs::remove_dir_all(&dir).ok();
        assert!(
            r.is_err(),
            "create must fail closed on an unmodeled NR field"
        );
    }

    #[test]
    fn apply_lte_rejects_unmodeled_base() {
        let dir = std::env::temp_dir().join(format!("uecaps-r1-la-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut base = lte_bytes(1);
        base.extend_from_slice(&[0x20, 0x05]); // LteCaps field #4 — not modeled
        let base_path = dir.join("lte_400907661.binarypb");
        std::fs::write(&base_path, &base).unwrap();
        let patch_path = dir.join("p.kdl");
        std::fs::write(&patch_path, "kind lte\nversion 1\n").unwrap();

        let r = apply(
            &base_path,
            Some(&patch_path),
            Some(&dir.join("out.binarypb")),
            false,
        );
        std::fs::remove_dir_all(&dir).ok();
        assert!(
            r.is_err(),
            "apply must fail closed on an unmodeled LTE field in the base"
        );
    }

    #[test]
    fn create_nr_rejects_uninvertible_band() {
        // R3: a combo whose SubBlock.band is 0 (absent int32) renders as "B0",
        // which raw_band cannot invert — RawSubBlock::from_sub_block used to panic mid-diff. create
        // must reject it with a clean error instead.
        let dir = std::env::temp_dir().join(format!("uecaps-r3-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let a = dir.join("AAA_1.binarypb");
        let b = dir.join("BBB_2.binarypb");
        std::fs::write(&a, nr_bytes(10078)).unwrap(); // n78A (valid)
        std::fs::write(&b, nr_bytes(0)).unwrap(); // band 0 -> "B0" (uninvertible)

        let r = create(&a, &b, Some(&dir.join("p.kdl")));
        std::fs::remove_dir_all(&dir).ok();
        assert!(
            r.is_err(),
            "create must reject an uninvertible band, not panic"
        );
    }

    #[test]
    fn create_nr_rejects_out_of_range_band() {
        // R12: raw band >= 20000 renders as "n10078"; from_sub_block succeeds but yields
        // band 10078, which the patch parser (RawSubBlock::validate) rejects. create must not
        // emit a patch its own parser would reject.
        let dir = std::env::temp_dir().join(format!("uecaps-r12b-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let a = dir.join("AAA_1.binarypb");
        let b = dir.join("BBB_2.binarypb");
        std::fs::write(&a, nr_bytes(10078)).unwrap();
        std::fs::write(&b, nr_bytes(20078)).unwrap(); // "n10078" — out of range

        let r = create(&a, &b, Some(&dir.join("p.kdl")));
        std::fs::remove_dir_all(&dir).ok();
        assert!(
            r.is_err(),
            "create must reject an out-of-range band, not emit an invalid patch"
        );
    }

    #[test]
    fn create_nr_rejects_patch_with_empty_derived_key() {
        // R12: a base combo with no components derives an empty key; create must run
        // validate_patch and reject rather than write a patch its parser rejects.
        use crate::proto::{ComboGroup, UeCaps, combo_group};
        use prost::Message;
        let empty_combo = UeCaps {
            version: 874_888_686,
            combo_groups: vec![ComboGroup {
                combo_header: None,
                combo: vec![combo_group::Combo {
                    bitmask: Some(0),
                    sub_blocks: vec![],
                }],
            }],
            ..Default::default()
        }
        .encode_to_vec();

        let dir = std::env::temp_dir().join(format!("uecaps-r12e-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let a = dir.join("AAA_1.binarypb");
        let b = dir.join("BBB_2.binarypb");
        std::fs::write(&a, nr_bytes(10078)).unwrap();
        std::fs::write(&b, &empty_combo).unwrap();

        let r = create(&a, &b, Some(&dir.join("p.kdl")));
        std::fs::remove_dir_all(&dir).ok();
        assert!(
            r.is_err(),
            "create must reject a patch whose set entry derives an empty key"
        );
    }

    #[test]
    fn create_nr_rejects_selector_only_unresolved_component() {
        // Final review, Fix 1: `patch create` must fail closed on a corpus-impossible NR
        // component whose per-CC selector is present, non-placeholder (a non-zero byte), and
        // unresolved (no matching feature set) — symmetric with the proto decode boundary's
        // `RawSubBlock::from_proto_sub_block` -> `resolve_or_placeholder` guard. `create` reads
        // through the lenient report-DTO path (`build_combos`), which resolves selectors
        // leniently: an empty `dl_feature_per_cc_list` means selector byte 5 below can never
        // resolve, so without the new guard `create` used to silently drop this component
        // instead of erroring (`sub_block_to_node` emits nothing for an unresolved direction).
        use crate::proto::{ComboGroup, UeCaps, combo_group, combo_group::combo::SubBlock};
        use prost::Message;

        fn selector_only_bytes(band: i32) -> Vec<u8> {
            UeCaps {
                version: 874_888_686,
                dl_feature_per_cc_list: Vec::new(), // empty: byte 5 below can never resolve
                combo_groups: vec![ComboGroup {
                    combo_header: None,
                    combo: vec![combo_group::Combo {
                        bitmask: Some(0),
                        sub_blocks: vec![SubBlock {
                            band,
                            dl_bw_class: Some(1),
                            ul_bw_class: Some(1),
                            dl_feature_per_cc_ids: Some(vec![5]), // non-zero, unresolvable
                            ..Default::default()
                        }],
                    }],
                }],
                ..Default::default()
            }
            .encode_to_vec()
        }

        let dir = std::env::temp_dir().join(format!("uecaps-selector-only-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let a = dir.join("AAA_1.binarypb");
        let b = dir.join("BBB_2.binarypb");
        std::fs::write(&a, nr_bytes(10078)).unwrap(); // n78A, clean
        std::fs::write(&b, selector_only_bytes(10002)).unwrap(); // n2A, selector-only

        let r = create(&a, &b, Some(&dir.join("p.kdl")));
        std::fs::remove_dir_all(&dir).ok();

        let err =
            r.expect_err("create must fail closed on a selector-only unresolved NR component");
        let msg = format!("{err:#}");
        assert!(
            msg.contains("selector") && msg.contains("placeholder"),
            "error should explain the unresolved, non-placeholder selector: {msg}"
        );
    }
}
