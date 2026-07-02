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
use std::{
    io::{Read, Write},
    path::Path,
};

use self::format::{Kind, NrPatch, Patch, PatchCombo, SetEntry};
use crate::{
    model::{Parsed, parse_name},
    proto::LteCaps,
    report::combos::{Cc, Combo, combo_key},
};
use std::collections::BTreeMap;

fn load_lte(path: &Path) -> anyhow::Result<LteCaps> {
    let bytes = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
    LteCaps::decode(&bytes[..]).with_context(|| format!("decoding {}", path.display()))
}

/// Full-field canonical form of one CC: resolved caps + modeled fields.
/// Selector IDs are part of raw combos, but ignored once their feature fields
/// have been resolved.
#[derive(PartialEq, Eq, PartialOrd, Ord, Clone)]
struct CanonCc {
    band: String,
    bw_class_dl: Option<i32>,
    bw_class_ul: Option<i32>,
    dl_feature_index: Option<i32>,
    ul_feature_index: Option<i32>,
    dl_feature_per_cc_ids: Option<Vec<u8>>,
    ul_feature_per_cc_ids: Option<Vec<u8>>,
    srs_tx_switch: Option<i32>,
    dl_max_scs: Option<i32>,
    dl_max_mimo: Option<i32>,
    dl_max_bw: Option<i32>,
    dl_max_mod_order: Option<i32>,
    dl_bw_90mhz_supported: Option<bool>,
    ul_max_scs: Option<i32>,
    ul_max_mimo_cb: Option<i32>,
    ul_max_bw: Option<i32>,
    ul_max_mod_order: Option<i32>,
    ul_bw_90mhz_supported: Option<bool>,
    ul_max_mimo_non_cb: Option<i32>,
}

/// Full-field canonical form of one combo: header + bitmask + sorted CCs.
#[derive(PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct CanonCombo {
    power_class: Option<i32>,
    bcs_nr: Option<u32>,
    bcs_intra_endc: Option<u32>,
    bcs_eutra: Option<u32>,
    intra_band_en_dc_support: Option<i32>,
    bit_mask: u32,
    cc: Vec<CanonCc>,
}

fn has_dl_feature_fields(x: &Cc) -> bool {
    has_dl_raw_values(x)
}

fn has_ul_feature_fields(x: &Cc) -> bool {
    has_ul_raw_values(x)
}

fn canon_dl_feature_per_cc_ids(x: &Cc) -> Option<Vec<u8>> {
    if has_dl_feature_fields(x) {
        None
    } else {
        x.dl_feature_per_cc_ids.clone()
    }
}

fn canon_ul_feature_per_cc_ids(x: &Cc) -> Option<Vec<u8>> {
    if has_ul_feature_fields(x) {
        None
    } else {
        x.ul_feature_per_cc_ids.clone()
    }
}

fn dl_raw(x: &Cc) -> Option<&crate::proto::ShannonFeatureSetDlPerCcNr> {
    x.dl_feature_per_cc.as_ref()
}

fn ul_raw(x: &Cc) -> Option<&crate::proto::ShannonFeatureSetUlPerCcNr> {
    x.ul_feature_per_cc.as_ref()
}

fn has_dl_raw_values(x: &Cc) -> bool {
    let Some(f) = x.dl_feature_per_cc.as_ref() else {
        return false;
    };
    f.max_scs.is_some()
        || f.max_mimo.is_some()
        || f.max_bw.is_some()
        || f.max_mod_order.is_some()
        || f.bw_90mhz_supported.is_some()
}

fn has_ul_raw_values(x: &Cc) -> bool {
    let Some(f) = x.ul_feature_per_cc.as_ref() else {
        return false;
    };
    f.max_scs.is_some()
        || f.max_mimo_cb.is_some()
        || f.max_bw.is_some()
        || f.max_mod_order.is_some()
        || f.bw_90mhz_supported.is_some()
        || f.max_mimo_non_cb.is_some()
}

fn canon_cc(x: &Cc) -> CanonCc {
    let dl = dl_raw(x);
    let ul = ul_raw(x);
    CanonCc {
        band: x.band.clone(),
        bw_class_dl: x.bw_class_dl,
        bw_class_ul: x.bw_class_ul,
        dl_feature_index: x.dl_feature_index,
        ul_feature_index: x.ul_feature_index,
        dl_feature_per_cc_ids: canon_dl_feature_per_cc_ids(x),
        ul_feature_per_cc_ids: canon_ul_feature_per_cc_ids(x),
        srs_tx_switch: x.srs_tx_switch,
        dl_max_scs: dl.and_then(|f| f.max_scs),
        dl_max_mimo: dl.and_then(|f| f.max_mimo),
        dl_max_bw: dl.and_then(|f| f.max_bw),
        dl_max_mod_order: dl.and_then(|f| f.max_mod_order),
        dl_bw_90mhz_supported: dl.and_then(|f| f.bw_90mhz_supported),
        ul_max_scs: ul.and_then(|f| f.max_scs),
        ul_max_mimo_cb: ul.and_then(|f| f.max_mimo_cb),
        ul_max_bw: ul.and_then(|f| f.max_bw),
        ul_max_mod_order: ul.and_then(|f| f.max_mod_order),
        ul_bw_90mhz_supported: ul.and_then(|f| f.bw_90mhz_supported),
        ul_max_mimo_non_cb: ul.and_then(|f| f.max_mimo_non_cb),
    }
}

fn canon_combo(c: &Combo) -> CanonCombo {
    let mut cc: Vec<CanonCc> = c.cc.iter().map(canon_cc).collect();
    cc.sort();
    CanonCombo {
        power_class: c.power_class,
        bcs_nr: c.bcs_nr,
        bcs_intra_endc: c.bcs_intra_endc,
        bcs_eutra: c.bcs_eutra,
        intra_band_en_dc_support: c.intra_band_en_dc_support,
        bit_mask: c.bit_mask,
        cc,
    }
}

/// Order-independent canonical form for all variants under one key.
pub(crate) fn canon_variants(combos: &[&Combo]) -> Vec<CanonCombo> {
    let mut v: Vec<CanonCombo> = combos.iter().map(|c| canon_combo(c)).collect();
    v.sort();
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
            None => (true, "add"),
            Some(a_combos) => (
                canon_variants(a_combos) != canon_variants(b_combos),
                "change",
            ),
        };
        if differs {
            set.push(SetEntry {
                kind: Some(kind.to_string()),
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

/// A path's file name as `&str`, or `"?"` if it has none / isn't UTF-8.
fn file_label(p: &Path) -> &str {
    p.file_name().and_then(|s| s.to_str()).unwrap_or("?")
}

/// Read a patch's TOML text from `input` (a file) or stdin.
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

/// `patch create <A> <B>`: diff A->B and write the patch TOML to `out` or stdout.
pub fn create(a: &Path, b: &Path, out: Option<&Path>) -> anyhow::Result<i32> {
    let na = file_label(a);
    let nb = file_label(b);
    let patch = match (parse_name(na), parse_name(nb)) {
        (Parsed::Carrier { .. }, Parsed::Carrier { .. }) => {
            let ca = crate::report::load_carrier_combos(a)?;
            let cb = crate::report::load_carrier_combos(b)?;
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
    let text = format::to_toml(&patch)?;
    match out {
        Some(path) => std::fs::write(path, text.as_bytes())
            .with_context(|| format!("writing patch {}", path.display()))?,
        None => {
            let mut handle = std::io::stdout().lock();
            handle
                .write_all(text.as_bytes())
                .context("writing patch to stdout")?;
            handle.flush().context("flushing stdout")?;
        }
    }
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
    let patch = format::from_toml(&patch_text)?;
    let base_bytes =
        std::fs::read(base).with_context(|| format!("reading base {}", base.display()))?;

    let (bytes, outcome) = match (patch, parse_name(filename)) {
        (Patch::Nr(fp), Parsed::Carrier { .. }) => {
            let caps = build::decode_base(&base_bytes)?;
            let (result, outcome) = build::apply_patch(&caps, &fp, strict)?;
            (result.encode_to_vec(), outcome)
        }
        (Patch::Lte(lp), Parsed::Lte(_)) => {
            let caps = LteCaps::decode(&base_bytes[..])
                .with_context(|| format!("decoding {}", base.display()))?;
            let (result, outcome) = lte::apply_lte_patch(&caps, &lp, strict)?;
            (result.encode_to_vec(), outcome)
        }
        (Patch::Nr(_), _) => {
            anyhow::bail!("{filename}: an nr/carrier patch needs a <CARRIER>_<NUMBER> base")
        }
        (Patch::Lte(_), _) => anyhow::bail!("{filename}: an lte patch needs an lte_* base"),
    };

    match out {
        Some(path) => {
            std::fs::write(path, &bytes).with_context(|| format!("writing {}", path.display()))?
        }
        None => {
            let mut handle = std::io::stdout().lock();
            handle.write_all(&bytes).context("writing to stdout")?;
            handle.flush().context("flushing stdout")?;
        }
    }
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
    use super::*; // Cc, Combo, build_patch, format come via the glob

    fn nr_combo(band_n: i32, dl_max_mimo: i32) -> Combo {
        let dl_feature_per_cc = crate::proto::ShannonFeatureSetDlPerCcNr {
            max_bw: Some(100),
            max_mimo: Some(dl_max_mimo),
            ..Default::default()
        };
        Combo {
            bit_mask: 0,
            cc: vec![Cc {
                band: format!("n{band_n}"),
                bw_class_dl: Some(1),
                bw_class_ul: Some(1),
                dl_feature_per_cc: Some(dl_feature_per_cc),
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
            cc: vec![Cc {
                band: format!("n{band_n}"),
                bw_class_dl: Some(1),
                bw_class_ul: Some(1),
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
        assert_eq!(by_key("n2A").kind.as_deref(), Some("add"));
        assert_eq!(by_key("n78A").kind.as_deref(), Some("change"));
        assert_eq!(by_key("n78A").combo[0].cc[0].dl_max_mimo, Some(3));
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
        a.cc[0].dl_feature_per_cc_ids = Some(vec![1]);
        b.cc[0].dl_feature_per_cc_ids = Some(vec![1]);

        let p = build_patch(&[a], &[b]);

        assert!(p.delete.is_empty());
        assert_eq!(p.set.len(), 1);
        assert_eq!(format::set_entry_key(&p.set[0]).unwrap(), "n78A");
        assert_eq!(p.set[0].combo[0].cc[0].dl_max_mimo, Some(3));
    }

    #[test]
    fn build_patch_detects_unknown_raw_scs_change_when_display_is_none() {
        let mut a = nr_combo(78, 2);
        let mut b = nr_combo(78, 2);
        a.cc[0].dl_feature_per_cc.as_mut().unwrap().max_scs = Some(7);
        b.cc[0].dl_feature_per_cc.as_mut().unwrap().max_scs = Some(8);
        a.cc[0].dl_scs_khz = None;
        b.cc[0].dl_scs_khz = None;

        let p = build_patch(&[a], &[b]);

        assert!(p.delete.is_empty());
        assert_eq!(p.set.len(), 1);
        assert_eq!(format::set_entry_key(&p.set[0]).unwrap(), "n78A");
        assert_eq!(p.set[0].combo[0].cc[0].dl_max_scs, Some(8));
    }

    #[test]
    fn build_patch_detects_raw_selector_only_change() {
        let a = nr_combo_with_raw_ids(78, vec![1], vec![1]);
        let b = nr_combo_with_raw_ids(78, vec![2], vec![1]);

        let p = build_patch(&[a], &[b]);

        assert!(p.delete.is_empty());
        assert_eq!(p.set.len(), 1);
        assert_eq!(format::set_entry_key(&p.set[0]).unwrap(), "n78A");
        assert_eq!(p.set[0].combo[0].cc[0].dl_feature_per_cc_ids, Some(vec![2]));
    }

    #[test]
    fn build_patch_detects_selector_change_when_resolved_feature_set_is_empty() {
        let mut a = nr_combo_with_raw_ids(78, vec![], vec![]);
        let mut b = nr_combo_with_raw_ids(78, vec![1], vec![]);
        a.cc[0].dl_feature_per_cc_ids = None;
        a.cc[0].ul_feature_per_cc_ids = None;
        b.cc[0].ul_feature_per_cc_ids = None;
        b.cc[0].dl_feature_per_cc = Some(crate::proto::ShannonFeatureSetDlPerCcNr::default());

        let p = build_patch(&[a], &[b]);

        assert!(p.delete.is_empty());
        assert_eq!(p.set.len(), 1);
        assert_eq!(format::set_entry_key(&p.set[0]).unwrap(), "n78A");
        assert_eq!(p.set[0].combo[0].cc[0].dl_feature_per_cc_ids, Some(vec![1]));
        assert_eq!(p.set[0].combo[0].cc[0].dl_feature_set(), None);
    }

    #[test]
    fn build_patch_ignores_dl_selector_change_when_dl_resolved_features_match() {
        let mut a = nr_combo(78, 2);
        a.cc[0].dl_feature_per_cc_ids = Some(vec![1]);
        a.cc[0].ul_feature_per_cc_ids = Some(vec![1]);
        let mut b = a.clone();
        b.cc[0].dl_feature_per_cc_ids = Some(vec![7]);

        let p = build_patch(&[a], &[b]);

        assert!(p.delete.is_empty());
        assert!(p.set.is_empty());
    }

    #[test]
    fn build_patch_detects_ul_selector_change_when_ul_features_absent() {
        let mut a = nr_combo(78, 2);
        a.cc[0].dl_feature_per_cc_ids = Some(vec![1]);
        a.cc[0].ul_feature_per_cc_ids = Some(vec![1]);
        let mut b = a.clone();
        b.cc[0].ul_feature_per_cc_ids = Some(vec![9]);

        let p = build_patch(&[a], &[b]);

        assert!(p.delete.is_empty());
        assert_eq!(p.set.len(), 1);
        assert_eq!(format::set_entry_key(&p.set[0]).unwrap(), "n78A");
        assert_eq!(p.set[0].combo[0].cc[0].ul_feature_per_cc_ids, Some(vec![9]));
    }

    #[test]
    fn build_patch_ignores_ul_selector_change_when_ul_resolved_features_match() {
        let mut a = nr_combo(78, 2);
        a.cc[0].ul_max_bw_mhz = Some(100);
        a.cc[0].ul_feature_per_cc = Some(crate::proto::ShannonFeatureSetUlPerCcNr {
            max_bw: Some(100),
            ..Default::default()
        });
        a.cc[0].ul_feature_per_cc_ids = Some(vec![1]);
        let mut b = a.clone();
        b.cc[0].ul_feature_per_cc_ids = Some(vec![9]);

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
        use crate::proto::{ComboGroup, UeCaps, combo_group, combo_group::nested2::ComboFeatures};
        use prost::Message;

        fn caps_with(band: i32) -> UeCaps {
            UeCaps {
                version: 874_888_686,
                combo_groups: vec![ComboGroup {
                    combo_header: None,
                    combo: vec![combo_group::Nested2 {
                        bitmask: Some(0),
                        cc: vec![ComboFeatures {
                            band,
                            bw_class_dl: Some(1),
                            bw_class_ul: Some(1),
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
        let patch_path = dir.join("p.toml");
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
                    bw_class_mimo_dl: 32768,
                    bw_class_mimo_ul: Some(ul),
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

        let patch_path = dir.join("lte_patch.toml");
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

        // B7A↓ field values survive create→toml→apply rebuild
        let b7 = result
            .combos
            .iter()
            .find(|c| lte_combo_key(c) == "B7A↓")
            .unwrap();
        assert_eq!(b7.bcs, Some(7));
        assert_eq!(b7.unknown1, Some(8));
        assert_eq!(b7.unknown2, Some(9));
        assert_eq!(b7.components[0].bw_class_mimo_ul, Some(0));
    }

    #[test]
    fn create_writes_patch_to_file() {
        use crate::proto::{
            ComboGroup, ShannonFeatureSetDlPerCcNr, UeCaps, combo_group,
            combo_group::nested2::ComboFeatures,
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
                    combo: vec![combo_group::Nested2 {
                        bitmask: Some(0),
                        cc: vec![ComboFeatures {
                            band,
                            bw_class_dl: Some(1),
                            bw_class_ul: Some(1),
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
        let outp = dir.join("p.toml");

        let code = create(&a, &b, Some(&outp)).unwrap();
        let text = std::fs::read_to_string(&outp).unwrap();
        std::fs::remove_dir_all(&dir).ok();

        assert_eq!(code, 0);
        let format::Patch::Nr(p) = format::from_toml(&text).unwrap() else {
            panic!("expected nr patch")
        };
        assert_eq!(p.delete, vec!["n78A".to_string()]);
        assert_eq!(p.set.len(), 1);
        assert_eq!(format::set_entry_key(&p.set[0]).unwrap(), "n2A");
    }
}
