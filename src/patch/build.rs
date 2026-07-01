//! The reconstruction engine: turn patch combos back into proto wire structures.

use super::format::{NrPatch, SetEntry};
use crate::{
    proto::{
        ComboGroup, ShannonFeatureSetDlPerCcNr, ShannonFeatureSetUlPerCcNr, UeCaps,
        combo_group::{Nested1, Nested2, nested2::ComboFeatures},
    },
    report::combos::{
        Cc, Combo, build_combos, combo_key, dl_mimo_code, mod_order_code, scs_code, ul_mimo_cb_code,
    },
};
use anyhow::Context;
use prost::Message;
use std::collections::BTreeSet;

/// Find an equal entry or append `item`; return its 0-based index.
fn find_or_append<T: PartialEq>(list: &mut Vec<T>, item: T) -> usize {
    if let Some(i) = list.iter().position(|x| *x == item) {
        i
    } else {
        list.push(item);
        list.len() - 1
    }
}

/// Invert an optional resolved value via `f`, erroring (with a field name) when a
/// present value has no inverse.
fn invert<V: Copy + std::fmt::Debug>(
    value: Option<V>,
    f: impl Fn(V) -> Option<i32>,
    field: &str,
) -> anyhow::Result<Option<i32>> {
    match value {
        None => Ok(None),
        Some(v) => f(v)
            .map(Some)
            .with_context(|| format!("{field} value {v:?} has no encoding")),
    }
}

fn dl_feature_of(cc: &Cc) -> anyhow::Result<Option<ShannonFeatureSetDlPerCcNr>> {
    let present = cc.dl_max_bw_mhz.is_some()
        || cc.dl_scs_khz.is_some()
        || cc.dl_mimo.is_some()
        || cc.dl_mod_order.is_some()
        || cc.dl_bw90mhz.is_some();
    if !present {
        return Ok(None);
    }
    Ok(Some(ShannonFeatureSetDlPerCcNr {
        max_scs: invert(cc.dl_scs_khz, scs_code, "dl_scs_khz")?,
        max_mimo: invert(cc.dl_mimo.as_deref(), dl_mimo_code, "dl_mimo")?,
        max_bw: cc.dl_max_bw_mhz,
        max_mod_order: invert(cc.dl_mod_order.as_deref(), mod_order_code, "dl_mod_order")?,
        bw_90mhz_supported: cc.dl_bw90mhz,
    }))
}

fn ul_feature_of(cc: &Cc) -> anyhow::Result<Option<ShannonFeatureSetUlPerCcNr>> {
    let present = cc.ul_max_bw_mhz.is_some()
        || cc.ul_scs_khz.is_some()
        || cc.ul_mimo_cb.is_some()
        || cc.ul_mimo_non_cb.is_some()
        || cc.ul_mod_order.is_some()
        || cc.ul_bw90mhz.is_some();
    if !present {
        return Ok(None);
    }
    Ok(Some(ShannonFeatureSetUlPerCcNr {
        max_scs: invert(cc.ul_scs_khz, scs_code, "ul_scs_khz")?,
        max_mimo_cb: invert(cc.ul_mimo_cb.as_deref(), ul_mimo_cb_code, "ul_mimo_cb")?,
        max_bw: cc.ul_max_bw_mhz,
        max_mod_order: invert(cc.ul_mod_order.as_deref(), mod_order_code, "ul_mod_order")?,
        bw_90mhz_supported: cc.ul_bw90mhz,
        max_mimo_non_cb: cc.ul_mimo_non_cb,
    }))
}

/// Header (`Nested1`) for a patch combo, or `None` when every header field is absent.
const fn build_header(combo: &Combo) -> Option<Nested1> {
    if combo.power_class.is_none()
        && combo.bcs_nr.is_none()
        && combo.bcs_intra_endc.is_none()
        && combo.bcs_eutra.is_none()
        && combo.intra_band_en_dc_support.is_none()
    {
        None
    } else {
        Some(Nested1 {
            bcs_nr: combo.bcs_nr,
            bcs_intra_endc: combo.bcs_intra_endc,
            bcs_eutra: combo.bcs_eutra,
            power_class: combo.power_class,
            intra_band_en_dc_support: combo.intra_band_en_dc_support,
        })
    }
}

/// Reconstruct every combo for one set entry into (header, proto combo) pairs.
/// On error the caller truncates `dl`/`ul` back to their pre-entry lengths.
pub(crate) fn reconstruct_set_entry(
    entry: &SetEntry,
    dl: &mut Vec<ShannonFeatureSetDlPerCcNr>,
    ul: &mut Vec<ShannonFeatureSetUlPerCcNr>,
) -> anyhow::Result<Vec<(Option<Nested1>, Nested2)>> {
    let mut out = Vec::with_capacity(entry.combo.len());
    for combo in &entry.combo {
        let cc = combo
            .cc
            .iter()
            .map(|c| reconstruct_cc(c, dl, ul).with_context(|| format!("set {:?}", entry.key)))
            .collect::<anyhow::Result<Vec<_>>>()?;
        out.push((
            build_header(combo),
            Nested2 {
                cc,
                bitmask: Some(combo.bit_mask),
            },
        ));
    }
    Ok(out)
}

/// Every combo key currently present in `caps`.
pub(crate) fn present_keys(caps: &UeCaps) -> BTreeSet<String> {
    build_combos(caps).iter().map(combo_key).collect()
}

/// Drop every combo whose key is in `keys`; drop any group left empty.
pub(crate) fn remove_keys(caps: &mut UeCaps, keys: &BTreeSet<String>) {
    let resolved = build_combos(caps);
    let to_drop: BTreeSet<(usize, usize)> = resolved
        .iter()
        .filter(|&c| keys.contains(&combo_key(c)))
        .map(|c| (c.group - 1, c.index - 1))
        .collect();
    for (gi, group) in caps.combo_groups.iter_mut().enumerate() {
        let mut ci = 0usize;
        group.combo.retain(|_| {
            let keep = !to_drop.contains(&(gi, ci));
            ci += 1;
            keep
        });
    }
    caps.combo_groups.retain(|g| !g.combo.is_empty());
}

/// Append reconstructed combos, grouping by identical header into `ComboGroup`s.
pub(crate) fn append_grouped(caps: &mut UeCaps, combos: Vec<(Option<Nested1>, Nested2)>) {
    let mut groups: Vec<(Option<Nested1>, Vec<Nested2>)> = Vec::new();
    for (hdr, n2) in combos {
        match groups.iter_mut().find(|(h, _)| *h == hdr) {
            Some(g) => g.1.push(n2),
            None => groups.push((hdr, vec![n2])),
        }
    }
    for (hdr, combo) in groups {
        caps.combo_groups.push(ComboGroup {
            combo_header: hdr,
            combo,
        });
    }
}

/// Resolve one optional feature set against `list` (append or dedup), returning its
/// 1-based selector bytes, or `None` when there is no feature set.
fn selector<T: PartialEq>(list: &mut Vec<T>, fs: Option<T>) -> anyhow::Result<Option<Vec<u8>>> {
    let Some(fs) = fs else { return Ok(None) };
    let idx = find_or_append(list, fs) + 1;
    if idx > u8::MAX as usize {
        anyhow::bail!("feature-set list exceeds 255 entries");
    }
    Ok(Some(vec![idx as u8]))
}

/// Build a proto CC from a resolved `Cc`, appending/deduping any referenced feature
/// set into `dl`/`ul` and pointing the 1-based selector byte at it.
pub(crate) fn reconstruct_cc(
    cc: &Cc,
    dl: &mut Vec<ShannonFeatureSetDlPerCcNr>,
    ul: &mut Vec<ShannonFeatureSetUlPerCcNr>,
) -> anyhow::Result<ComboFeatures> {
    let dl_ids = selector(dl, dl_feature_of(cc)?)?;
    let ul_ids = selector(ul, ul_feature_of(cc)?)?;
    Ok(ComboFeatures {
        band: cc.band,
        bw_class_dl: cc.bw_class_dl,
        bw_class_ul: cc.bw_class_ul,
        dl_feature_index: cc.dl_feature_index,
        ul_feature_index: cc.ul_feature_index,
        dl_feature_per_cc_ids: dl_ids,
        ul_feature_per_cc_ids: ul_ids,
        srstxswitch: cc.srs_tx_switch,
    })
}

/// Decode the base file. Byte-for-byte round-trip identity is a NON-GOAL: proto3
/// canonicalization (default-value omission, field ordering) makes real Google files
/// re-encode to different bytes with identical field values. The round-trip contract
/// is value-level — the decoded protobuf must have the same value in every field, and
/// no more. The only way that can break is a field number the proto does not model,
/// which prost silently drops; `ensure_modeled` scans the wire format and refuses
/// exactly those.
pub(crate) fn decode_base(bytes: &[u8]) -> anyhow::Result<UeCaps> {
    ensure_modeled(bytes)?;
    UeCaps::decode(bytes).context("decoding base capability file")
}

/// Messages of the UE-caps schema, for the field-presence scan.
#[derive(Clone, Copy)]
enum Msg {
    UeCaps,
    ComboGroup,
    Header,
    Combo,
    Cc,
    DlFs,
    UlFs,
}

const fn msg_name(m: Msg) -> &'static str {
    match m {
        Msg::UeCaps => "UeCaps",
        Msg::ComboGroup => "ComboGroup",
        Msg::Header => "ComboGroup.combo_header",
        Msg::Combo => "ComboGroup.combo",
        Msg::Cc => "combo.cc",
        Msg::DlFs => "dl_feature_per_cc_list",
        Msg::UlFs => "ul_feature_per_cc_list",
    }
}

/// What a (message, field number) pair denotes: a leaf value, a nested message of a
/// known type, or `None` if the field number is not modeled by the proto.
enum Field {
    Leaf,
    Sub(Msg),
}

const fn field_kind(msg: Msg, field: u64) -> Option<Field> {
    use Field::{Leaf, Sub};
    use Msg::*;
    Some(match (msg, field) {
        (UeCaps, 1 | 2 | 9) => Leaf,
        (UeCaps, 3) => Sub(ComboGroup),
        (UeCaps, 6) => Sub(DlFs),
        (UeCaps, 7) => Sub(UlFs),
        (ComboGroup, 1) => Sub(Header),
        (ComboGroup, 2) => Sub(Combo),
        (Header, 1..=5) => Leaf,
        (Combo, 1) => Sub(Cc),
        (Combo, 2) => Leaf,
        (Cc, 1..=8) => Leaf,
        (DlFs, 1..=5) => Leaf,
        (UlFs, 1..=6) => Leaf,
        _ => return None,
    })
}

fn read_varint(bytes: &[u8], i: &mut usize) -> anyhow::Result<u64> {
    let mut shift = 0u32;
    let mut out = 0u64;
    loop {
        let byte = *bytes.get(*i).context("truncated varint")?;
        *i += 1;
        // On the 10th byte (shift == 63) only the low bit fits in a u64.
        anyhow::ensure!(shift < 63 || byte & 0x7f <= 1, "varint overflows u64");
        out |= u64::from(byte & 0x7f) << shift;
        if byte & 0x80 == 0 {
            return Ok(out);
        }
        shift += 7;
        anyhow::ensure!(shift < 64, "varint too long");
    }
}

/// Walk the protobuf wire format of `bytes` (a `msg`-typed message) and error on the
/// first field number not in the schema, recursing into modeled sub-messages.
fn scan(bytes: &[u8], msg: Msg) -> anyhow::Result<()> {
    let mut i = 0usize;
    while i < bytes.len() {
        let key = read_varint(bytes, &mut i)?;
        let field = key >> 3;
        let wire = key & 7;
        let payload = match wire {
            0 => {
                read_varint(bytes, &mut i)?;
                None
            }
            1 => {
                i = i.checked_add(8).context("64-bit field length overflow")?;
                anyhow::ensure!(i <= bytes.len(), "truncated 64-bit field");
                None
            }
            5 => {
                i = i.checked_add(4).context("32-bit field length overflow")?;
                anyhow::ensure!(i <= bytes.len(), "truncated 32-bit field");
                None
            }
            2 => {
                let len = read_varint(bytes, &mut i)? as usize;
                let end = i
                    .checked_add(len)
                    .context("length-delimited length overflow")?;
                let p = bytes
                    .get(i..end)
                    .context("truncated length-delimited field")?;
                i = end;
                Some(p)
            }
            other => anyhow::bail!("unsupported protobuf wire type {other}"),
        };
        match field_kind(msg, field) {
            None => anyhow::bail!(
                "base file carries field #{field} in {}, which this tool does not model; \
                 cannot guarantee a value-preserving round-trip",
                msg_name(msg)
            ),
            Some(Field::Sub(child)) => {
                if let Some(p) = payload {
                    scan(p, child)?;
                }
            }
            Some(Field::Leaf) => {}
        }
    }
    Ok(())
}

/// Refuse the base only if it carries a field the proto does not model (prost would
/// drop it silently, breaking the value-preservation contract).
fn ensure_modeled(bytes: &[u8]) -> anyhow::Result<()> {
    scan(bytes, Msg::UeCaps)
}

/// Summary of an apply: counts plus a human-readable skip list.
pub(crate) struct Outcome {
    pub(crate) deleted: usize,
    pub(crate) set: usize,
    pub(crate) skipped: Vec<String>,
}

struct ApplyPass {
    caps: UeCaps,
    deleted: usize,
    set: usize,
    skipped: Vec<String>,
    verify_failed: Vec<String>,
}

impl ApplyPass {
    /// Consume the pass into its `(caps, outcome)` result, dropping verify bookkeeping.
    fn finish(self) -> (UeCaps, Outcome) {
        (
            self.caps,
            Outcome {
                deleted: self.deleted,
                set: self.set,
                skipped: self.skipped,
            },
        )
    }
}

/// Re-decode the result and confirm each applied entry took: deletes absent, set
/// keys matching the patch at canonical (full-field) granularity. Returns failed keys.
fn self_verify(
    caps: &UeCaps,
    patch: &NrPatch,
    applied_set: &BTreeSet<String>,
    applied_delete: &BTreeSet<String>,
) -> Vec<String> {
    let resolved = build_combos(caps);
    let by_key = super::index_by_key(&resolved, combo_key);
    let mut failed: Vec<String> = applied_delete
        .iter()
        .filter(|k| by_key.contains_key(*k))
        .cloned()
        .collect();
    for entry in &patch.set {
        if !applied_set.contains(&entry.key) {
            continue;
        }
        let want = super::canon_variants(&entry.combo.iter().collect::<Vec<_>>());
        let got = by_key
            .get(&entry.key)
            .map(|v| super::canon_variants(v))
            .unwrap_or_default();
        if want != got {
            failed.push(entry.key.clone());
        }
    }
    failed
}

/// One apply attempt, skipping `exclude` keys. Best-effort unless `strict`.
fn apply_once(
    base: &UeCaps,
    patch: &NrPatch,
    exclude: &BTreeSet<String>,
    strict: bool,
) -> anyhow::Result<ApplyPass> {
    let mut caps = base.clone();
    let present = present_keys(&caps);
    let mut skipped = Vec::new();

    // 1. Reconstruct set entries; roll back feature-set appends on failure.
    let mut pending: Vec<(Option<Nested1>, Nested2)> = Vec::new();
    let mut set_keys: BTreeSet<String> = BTreeSet::new();
    for entry in &patch.set {
        if exclude.contains(&entry.key) {
            continue;
        }
        let dl_mark = caps.dl_feature_per_cc_list.len();
        let ul_mark = caps.ul_feature_per_cc_list.len();
        match reconstruct_set_entry(
            entry,
            &mut caps.dl_feature_per_cc_list,
            &mut caps.ul_feature_per_cc_list,
        ) {
            Ok(combos) => {
                pending.extend(combos);
                set_keys.insert(entry.key.clone());
            }
            Err(e) => {
                caps.dl_feature_per_cc_list.truncate(dl_mark);
                caps.ul_feature_per_cc_list.truncate(ul_mark);
                let msg = format!("set {:?}: {e:#}", entry.key);
                if strict {
                    anyhow::bail!("{msg}");
                }
                skipped.push(msg);
            }
        }
    }

    // 2. Deletes: present -> remove; absent -> warn/skip.
    let mut delete_keys: BTreeSet<String> = BTreeSet::new();
    for key in &patch.delete {
        if exclude.contains(key) {
            continue;
        }
        if present.contains(key) {
            delete_keys.insert(key.clone());
        } else {
            let msg = format!("delete {key:?}: not present in base");
            if strict {
                anyhow::bail!("{msg}");
            }
            skipped.push(msg);
        }
    }

    // 3. Remove (deletes ∪ set keys), then append reconstructed set combos.
    let remove = &delete_keys | &set_keys;
    remove_keys(&mut caps, &remove);
    append_grouped(&mut caps, pending);

    // 4. Self-verify.
    let verify_failed = self_verify(&caps, patch, &set_keys, &delete_keys);

    Ok(ApplyPass {
        caps,
        deleted: delete_keys.len(),
        set: set_keys.len(),
        skipped,
        verify_failed,
    })
}

/// Apply a patch to a decoded base. Best-effort by default; `strict` turns any skip
/// or verify failure into an error. A verify failure triggers one re-apply that
/// excludes the failing keys (at most two passes).
pub(crate) fn apply_patch(
    base: &UeCaps,
    patch: &NrPatch,
    strict: bool,
) -> anyhow::Result<(UeCaps, Outcome)> {
    let pass1 = apply_once(base, patch, &BTreeSet::new(), strict)?;
    if pass1.verify_failed.is_empty() {
        return Ok(pass1.finish());
    }
    if strict {
        anyhow::bail!("self-verify failed for: {}", pass1.verify_failed.join(", "));
    }
    let exclude: BTreeSet<String> = pass1.verify_failed.iter().cloned().collect();
    let mut pass2 = apply_once(base, patch, &exclude, false)?;
    if !pass2.verify_failed.is_empty() {
        anyhow::bail!(
            "self-verify still failing after re-apply: {}",
            pass2.verify_failed.join(", ")
        );
    }
    for k in &pass1.verify_failed {
        pass2
            .skipped
            .push(format!("set {k:?}: self-verify failed; left unchanged"));
    }
    Ok(pass2.finish())
}

#[cfg(test)]
mod tests {
    use super::super::format::Kind;
    use super::*; // Cc and the reconstruction fns come via the glob
    use crate::proto::{ComboGroup, UeCaps, combo_group};

    fn nr_cc() -> Cc {
        Cc {
            band: 10078,
            bw_class_dl: Some(1),
            bw_class_ul: Some(1),
            dl_max_bw_mhz: Some(100),
            dl_mimo: Some("4x4".to_string()),
            dl_scs_khz: Some(30),
            dl_mod_order: Some("QAM256".to_string()),
            ul_max_bw_mhz: Some(100),
            ul_mimo_cb: Some("Yes".to_string()),
            ul_mimo_non_cb: Some(1),
            ul_mod_order: Some("QAM256".to_string()),
            ..Default::default()
        }
    }

    #[test]
    fn reconstruct_cc_builds_and_dedups_feature_sets() {
        let mut dl = Vec::new();
        let mut ul = Vec::new();
        let f1 = reconstruct_cc(&nr_cc(), &mut dl, &mut ul).unwrap();
        // selector is 1-based into the per-direction list
        assert_eq!(f1.band, 10078);
        assert_eq!(f1.dl_feature_per_cc_ids, Some(vec![1]));
        assert_eq!(f1.ul_feature_per_cc_ids, Some(vec![1]));
        assert_eq!(dl.len(), 1);
        assert_eq!(dl[0].max_scs, Some(2)); // 30 kHz -> code 2
        assert_eq!(dl[0].max_mimo, Some(2)); // 4x4 -> 2
        // a second identical CC dedups to the same index
        let f2 = reconstruct_cc(&nr_cc(), &mut dl, &mut ul).unwrap();
        assert_eq!(f2.dl_feature_per_cc_ids, Some(vec![1]));
        assert_eq!(dl.len(), 1);
        assert_eq!(ul.len(), 1);
    }

    #[test]
    fn reconstruct_cc_without_feature_set_has_no_selector() {
        let cc = Cc {
            band: 1,
            ..Default::default()
        }; // E-UTRA, no resolved caps
        let mut dl = Vec::new();
        let mut ul = Vec::new();
        let f = reconstruct_cc(&cc, &mut dl, &mut ul).unwrap();
        assert_eq!(f.band, 1);
        assert_eq!(f.dl_feature_per_cc_ids, None);
        assert_eq!(f.ul_feature_per_cc_ids, None);
        assert!(dl.is_empty() && ul.is_empty());
    }

    #[test]
    fn reconstruct_cc_rejects_uninvertible_label() {
        let cc = Cc {
            band: 10078,
            dl_max_bw_mhz: Some(100),
            dl_mimo: Some("(7)".to_string()), // not invertible
            ..Default::default()
        };
        let mut dl = Vec::new();
        let mut ul = Vec::new();
        assert!(reconstruct_cc(&cc, &mut dl, &mut ul).is_err());
    }

    fn base_caps() -> UeCaps {
        UeCaps {
            version: 874_888_686,
            combo_groups: vec![ComboGroup {
                combo_header: None,
                combo: vec![
                    combo_group::Nested2 {
                        bitmask: Some(0),
                        cc: vec![crate::proto::combo_group::nested2::ComboFeatures {
                            band: 10078,
                            bw_class_dl: Some(1),
                            bw_class_ul: Some(1),
                            ..Default::default()
                        }],
                    },
                    combo_group::Nested2 {
                        bitmask: Some(0),
                        cc: vec![crate::proto::combo_group::nested2::ComboFeatures {
                            band: 10041,
                            bw_class_dl: Some(1),
                            bw_class_ul: Some(1),
                            ..Default::default()
                        }],
                    },
                ],
            }],
            ..Default::default()
        }
    }

    #[test]
    fn decode_base_accepts_clean_rejects_unmodeled_field() {
        let bytes = base_caps().encode_to_vec();
        assert!(decode_base(&bytes).is_ok());
        let mut tampered = bytes;
        tampered.extend_from_slice(&[0x78, 0x01]); // field 15 varint — not in the proto
        assert!(decode_base(&tampered).is_err());
    }

    #[test]
    fn decode_base_accepts_benign_reencoding() {
        // Modeled fields in non-canonical order: field 9 (unknown=7) BEFORE field 1
        // (version=300). prost re-encodes in field-number order, so this is NOT
        // byte-identical to its re-encode — the old byte-identity guard wrongly
        // rejected it. The value-preserving guard accepts it (every field is modeled).
        let bytes = [0x48, 0x07, 0x08, 0xAC, 0x02];
        let caps = decode_base(&bytes).expect("benign re-encoding must be accepted");
        assert_eq!(caps.version, 300);
        assert_eq!(caps.unknown, 7);
        assert_ne!(
            caps.encode_to_vec(),
            bytes.to_vec(),
            "this input must differ from its prost re-encode (proving byte-identity was the wrong test)"
        );
    }

    #[test]
    fn apply_patch_transplants_combos() {
        let base = base_caps(); // n78A, n41A
        let patch = NrPatch {
            kind: Kind::Nr,
            version: 1,
            delete: vec!["n41A".to_string()],
            set: vec![SetEntry {
                key: "n2A".to_string(),
                kind: Some("add".to_string()),
                combo: vec![Combo {
                    bit_mask: 0,
                    cc: vec![Cc {
                        band: 10002,
                        bw_class_dl: Some(1),
                        bw_class_ul: Some(1),
                        ..Default::default()
                    }],
                    ..Default::default()
                }],
            }],
        };
        let (result, outcome) = apply_patch(&base, &patch, false).unwrap();
        let keys = present_keys(&result);
        assert!(keys.contains("n2A"));
        assert!(keys.contains("n78A"));
        assert!(!keys.contains("n41A"));
        assert_eq!(result.version, base.version);
        assert!(outcome.skipped.is_empty());
        assert_eq!(outcome.deleted, 1);
        assert_eq!(outcome.set, 1);
    }

    #[test]
    fn apply_patch_best_effort_skips_absent_delete() {
        let base = base_caps();
        let patch = NrPatch {
            kind: Kind::Nr,
            version: 1,
            delete: vec!["n99A".to_string()],
            set: vec![],
        };
        let (_r, outcome) = apply_patch(&base, &patch, false).unwrap();
        assert_eq!(outcome.skipped.len(), 1);
        assert!(outcome.skipped[0].contains("n99A"));
        assert!(apply_patch(&base, &patch, true).is_err()); // strict
    }

    #[test]
    fn apply_once_excludes_listed_keys() {
        let base = base_caps();
        let patch = NrPatch {
            kind: Kind::Nr,
            version: 1,
            delete: vec![],
            set: vec![SetEntry {
                key: "n2A".to_string(),
                kind: None,
                combo: vec![Combo {
                    bit_mask: 0,
                    cc: vec![Cc {
                        band: 10002,
                        bw_class_dl: Some(1),
                        bw_class_ul: Some(1),
                        ..Default::default()
                    }],
                    ..Default::default()
                }],
            }],
        };
        let exclude: BTreeSet<String> = ["n2A".to_string()].into_iter().collect();
        let pass = apply_once(&base, &patch, &exclude, false).unwrap();
        assert!(!present_keys(&pass.caps).contains("n2A"));
        assert_eq!(pass.set, 0);
    }

    #[test]
    fn present_keys_lists_all() {
        let keys = present_keys(&base_caps());
        assert!(keys.contains("n78A"));
        assert!(keys.contains("n41A"));
    }

    #[test]
    fn remove_keys_drops_matching_and_empty_groups() {
        let mut caps = base_caps();
        remove_keys(&mut caps, &["n78A".to_string()].into_iter().collect());
        let keys = present_keys(&caps);
        assert!(!keys.contains("n78A"));
        assert!(keys.contains("n41A"));
        // n41A still occupies its group; group not dropped
        assert_eq!(caps.combo_groups.len(), 1);
        // removing the last one empties and drops the group
        remove_keys(&mut caps, &["n41A".to_string()].into_iter().collect());
        assert!(caps.combo_groups.is_empty());
    }

    #[test]
    fn append_grouped_splits_by_header() {
        let mut caps = UeCaps::default();
        let n2 = || combo_group::Nested2 {
            bitmask: Some(0),
            cc: vec![crate::proto::combo_group::nested2::ComboFeatures {
                band: 10002,
                bw_class_dl: Some(1),
                ..Default::default()
            }],
        };
        let hdr_a = Some(combo_group::Nested1 {
            power_class: Some(3),
            ..Default::default()
        });
        let hdr_b = Some(combo_group::Nested1 {
            power_class: Some(2),
            ..Default::default()
        });
        append_grouped(&mut caps, vec![(hdr_a, n2()), (hdr_a, n2()), (hdr_b, n2())]);
        // two distinct headers -> two groups; the first holds two combos
        assert_eq!(caps.combo_groups.len(), 2);
        assert_eq!(caps.combo_groups[0].combo.len(), 2);
    }

    #[test]
    fn apply_patch_change_preserves_resolved_caps() {
        // Drive a CHANGE over an existing key (n78A) with full resolved caps and confirm
        // they survive the reconstruct + self_verify round-trip.
        let base = base_caps(); // n78A (no feature set), n41A (no feature set)
        let patch = NrPatch {
            kind: Kind::Nr,
            version: 1,
            delete: vec![],
            set: vec![SetEntry {
                key: "n78A".to_string(),
                kind: Some("change".to_string()),
                combo: vec![Combo {
                    bit_mask: 0,
                    cc: vec![Cc {
                        band: 10078,
                        bw_class_dl: Some(1),
                        bw_class_ul: Some(1),
                        dl_max_bw_mhz: Some(100),
                        dl_mimo: Some("8x8".into()),
                        dl_scs_khz: Some(30),
                        dl_mod_order: Some("QAM256".into()),
                        ..Default::default()
                    }],
                    ..Default::default()
                }],
            }],
        };
        let (result, outcome) = apply_patch(&base, &patch, false).unwrap();
        assert!(outcome.skipped.is_empty());
        assert_eq!(outcome.set, 1);
        let combos = build_combos(&result);
        let n78 = combos
            .iter()
            .find(|c| combo_key(c) == "n78A")
            .expect("n78A must be present after change");
        let cc = &n78.cc[0];
        assert_eq!(cc.dl_mimo.as_deref(), Some("8x8"));
        assert_eq!(cc.dl_scs_khz, Some(30));
        assert_eq!(cc.dl_max_bw_mhz, Some(100));
        assert_eq!(cc.dl_mod_order.as_deref(), Some("QAM256"));
    }

    #[test]
    fn apply_patch_skips_uninvertible_label_best_effort_errors_strict() {
        // A CC with dl_mimo="(7)" has no inverse encoding; reconstruction must fail.
        let base = base_caps();
        let patch = NrPatch {
            kind: Kind::Nr,
            version: 1,
            delete: vec![],
            set: vec![SetEntry {
                key: "n2A".to_string(),
                kind: Some("add".to_string()),
                combo: vec![Combo {
                    bit_mask: 0,
                    cc: vec![Cc {
                        band: 10002,
                        bw_class_dl: Some(1),
                        bw_class_ul: Some(1),
                        dl_max_bw_mhz: Some(100),
                        dl_mimo: Some("(7)".into()), // no inverse
                        ..Default::default()
                    }],
                    ..Default::default()
                }],
            }],
        };
        // Best-effort: succeeds but skips the bad entry; n2A absent from result.
        let (result, outcome) = apply_patch(&base, &patch, false).unwrap();
        assert!(!outcome.skipped.is_empty(), "expected a skip entry");
        let keys: std::collections::BTreeSet<String> =
            build_combos(&result).iter().map(combo_key).collect();
        assert!(!keys.contains("n2A"), "n2A must not be present after skip");
        // Strict: fails immediately.
        assert!(
            apply_patch(&base, &patch, true).is_err(),
            "strict mode must return Err for uninvertible label"
        );
    }

    #[test]
    fn real_file_decode_base_passes() {
        // Opt-in: set UECAPS_FIXTURE=/path/to/some_carrier.binarypb
        let Ok(path) = std::env::var("UECAPS_FIXTURE") else {
            return;
        };
        let bytes = std::fs::read(&path).expect("reading fixture");
        assert!(
            decode_base(&bytes).is_ok(),
            "a real carrier file must pass the value-preservation guard"
        );
    }

    /// Schema-aware count of explicit `ComboGroup.Nested2.bitmask` (field 2) occurrences
    /// in a `UeCaps` wire buffer. Walks `UeCaps.combo_groups`(3) -> `ComboGroup.combo`(2)
    /// -> `Nested2.bitmask`(2). Field 2 means different things in other messages, so this
    /// descends only the bitmask path rather than counting raw `0x10` tags.
    fn count_bitmask_fields(buf: &[u8]) -> usize {
        fn varint(b: &[u8], i: &mut usize) -> u64 {
            let (mut shift, mut v) = (0u32, 0u64);
            loop {
                let byte = b[*i];
                *i += 1;
                v |= ((byte & 0x7f) as u64) << shift;
                if byte & 0x80 == 0 {
                    return v;
                }
                shift += 7;
            }
        }
        // depth 0 = UeCaps (descend field 3 -> ComboGroup); 1 = ComboGroup (descend
        // field 2 -> Nested2); 2 = Nested2 (count field 2 = bitmask).
        fn walk(buf: &[u8], depth: u8) -> usize {
            let (mut i, mut n) = (0usize, 0usize);
            while i < buf.len() {
                let tag = varint(buf, &mut i);
                let (fno, wt) = ((tag >> 3) as u32, (tag & 7) as u8);
                match wt {
                    0 => {
                        let _ = varint(buf, &mut i);
                        if depth == 2 && fno == 2 {
                            n += 1;
                        }
                    }
                    2 => {
                        let len = varint(buf, &mut i) as usize;
                        let seg = &buf[i..i + len];
                        i += len;
                        if (depth == 0 && fno == 3) || (depth == 1 && fno == 2) {
                            n += walk(seg, depth + 1);
                        }
                    }
                    5 => i += 4,
                    1 => i += 8,
                    // wire types 3/4 (groups) never occur in proto3 output; stop if seen
                    _ => break,
                }
            }
            n
        }
        walk(buf, 0)
    }

    #[test]
    fn real_file_bitmask_presence_survives_reencode() {
        // Opt-in: UECAPS_FIXTURE=/path/to/mustang/<carrier>.binarypb (a file WITH combos).
        let Ok(path) = std::env::var("UECAPS_FIXTURE") else {
            return;
        };
        let original = std::fs::read(&path).expect("reading fixture");
        let before = count_bitmask_fields(&original);
        assert!(
            before > 0,
            "fixture has no Nested2.bitmask fields; pick a carrier file with combos"
        );
        let caps = UeCaps::decode(&original[..]).expect("decode fixture");
        let reencoded = caps.encode_to_vec();
        let after = count_bitmask_fields(&reencoded);
        assert_eq!(
            before, after,
            "explicit bitmask fields dropped on re-encode: {before} -> {after}"
        );
    }

    #[test]
    fn real_file_create_apply_reproduces_target() {
        // Opt-in: UECAPS_FIXTURE_A and UECAPS_FIXTURE_B = two real carrier files.
        // The headline guarantee on real data: apply(create(A,B), A) reproduces B's combos.
        let (Ok(pa), Ok(pb)) = (
            std::env::var("UECAPS_FIXTURE_A"),
            std::env::var("UECAPS_FIXTURE_B"),
        ) else {
            return;
        };
        let caps_a = decode_base(&std::fs::read(pa).expect("read A")).expect("A passes guard");
        let caps_b = decode_base(&std::fs::read(pb).expect("read B")).expect("B passes guard");
        let patch = crate::patch::build_patch(&build_combos(&caps_a), &build_combos(&caps_b));
        let (result, outcome) = apply_patch(&caps_a, &patch, false).expect("apply");
        assert!(
            outcome.skipped.is_empty(),
            "real-file apply skipped entries: {:?}",
            outcome.skipped
        );
        let canon = |caps: &UeCaps| {
            crate::patch::canon_variants(&build_combos(caps).iter().collect::<Vec<_>>())
        };
        assert!(
            canon(&result) == canon(&caps_b),
            "applied result's combos must equal B's at full-field granularity"
        );
    }
}
