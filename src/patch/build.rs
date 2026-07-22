//! The reconstruction engine: turn patch combos back into proto wire structures.

use super::format::{NrPatch, SetEntry, set_entry_combos, set_entry_key};
use crate::{
    proto::{
        ComboGroup, UeCaps,
        combo_group::{Combo as ProtoCombo, ComboHeader},
    },
    raw_nr::{FeatureLists, RawNrPayload, reconstruct_sub_block},
    report::combos::{Combo, build_combos, combo_key, resolve_all},
    wire::decode_uecaps,
};
use anyhow::Context;
use std::collections::BTreeSet;

/// Reconstruct every combo for one set entry into (header, proto combo) pairs.
/// On error the caller truncates `dl`/`ul` back to their pre-entry lengths.
pub(crate) fn reconstruct_set_entry(
    entry: &SetEntry,
    lists: &mut FeatureLists,
) -> anyhow::Result<Vec<(Option<ComboHeader>, ProtoCombo)>> {
    let key = set_entry_key(entry)?;
    let mut out = Vec::with_capacity(entry.combo.len());
    for patch_combo in &entry.combo {
        let combo_view = patch_combo.to_combo()?;
        let sub_blocks = patch_combo
            .sub_blocks
            .iter()
            .map(|c| reconstruct_sub_block(c, lists).with_context(|| format!("set {key:?}")))
            .collect::<anyhow::Result<Vec<_>>>()?;
        out.push((
            // Header comes from the neutral raw payload's shared builder (C-hdr); the
            // conversion's cc processing is irrelevant here — only the five header fields
            // are read.
            RawNrPayload::from(&combo_view).header(),
            ProtoCombo {
                sub_blocks,
                bitmask: Some(patch_combo.bit_mask),
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
pub(crate) fn append_grouped(caps: &mut UeCaps, combos: Vec<(Option<ComboHeader>, ProtoCombo)>) {
    let mut groups: Vec<(Option<ComboHeader>, Vec<ProtoCombo>)> = Vec::new();
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

/// Decode the base file. Byte-for-byte round-trip identity is a NON-GOAL: proto3
/// canonicalization (default-value omission, field ordering) makes real Google files
/// re-encode to different bytes with identical field values. The round-trip contract
/// is value-level — the decoded protobuf must have the same value in every field. The
/// shared wire validator rejects unmodeled fields and incorrect field encodings before
/// prost can silently discard or normalize them.
pub(crate) fn decode_base(bytes: &[u8]) -> anyhow::Result<UeCaps> {
    decode_uecaps(bytes, "base capability file")
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
) -> anyhow::Result<Vec<String>> {
    let resolved = build_combos(caps);
    let by_key = super::index_by_key(&resolved, combo_key);
    // A delete "failed" only if the key is still present AND no `set` entry legitimately
    // re-added it — a delete-then-re-add of the same key is a valid patch (R6).
    let mut failed: Vec<String> = applied_delete
        .iter()
        .filter(|k| by_key.contains_key(*k) && !applied_set.contains(*k))
        .cloned()
        .collect();
    for entry in &patch.set {
        let key = set_entry_key(entry)?;
        if !applied_set.contains(&key) {
            continue;
        }
        let mut want_combos = set_entry_combos(entry)?;
        // Resolve the want side's selector-only components against the applied file's
        // feature lists, exactly as `build_combos` resolved the got side. Otherwise a
        // selector-only component keeps its raw ids on the want side while the got side
        // carries the resolved values, so a correctly-applied entry compares unequal (R2).
        resolve_selectors_against(&mut want_combos, caps);
        let want = super::canon_variants(&want_combos.iter().collect::<Vec<_>>());
        let got = by_key
            .get(&key)
            .map(|v| super::canon_variants(v))
            .unwrap_or_default();
        if want != got {
            failed.push(key);
        }
    }
    Ok(failed)
}

/// Fill each combo's **selector-only** DL/UL feature set from `caps`'s top-level feature
/// lists, mirroring [`build_combos`]'s resolution. A component that already carries a
/// resolved feature set (from the patch's own values) is left untouched; only raw
/// selector bytes are resolved, and only when every byte is in range (`resolve_all`).
/// Used to canonicalize the self-verify "want" side.
fn resolve_selectors_against(combos: &mut [Combo], caps: &UeCaps) {
    for combo in combos {
        for cc in &mut combo.sub_blocks {
            if cc.dl_features.is_empty() {
                cc.dl_features = resolve_all(
                    cc.dl_feature_per_cc_ids.as_deref(),
                    &caps.dl_feature_per_cc_list,
                )
                .unwrap_or_default();
            }
            if cc.ul_features.is_empty() {
                cc.ul_features = resolve_all(
                    cc.ul_feature_per_cc_ids.as_deref(),
                    &caps.ul_feature_per_cc_list,
                )
                .unwrap_or_default();
            }
        }
    }
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
    let mut lists = FeatureLists {
        dl: std::mem::take(&mut caps.dl_feature_per_cc_list),
        ul: std::mem::take(&mut caps.ul_feature_per_cc_list),
    };
    let mut skipped = Vec::new();

    // 1. Reconstruct set entries; roll back feature-set appends on failure.
    let mut pending: Vec<(Option<ComboHeader>, ProtoCombo)> = Vec::new();
    let mut set_keys: BTreeSet<String> = BTreeSet::new();
    for entry in &patch.set {
        let key = set_entry_key(entry)?;
        if exclude.contains(&key) {
            continue;
        }
        let dl_mark = lists.dl.len();
        let ul_mark = lists.ul.len();
        match reconstruct_set_entry(entry, &mut lists) {
            Ok(combos) => {
                pending.extend(combos);
                set_keys.insert(key);
            }
            Err(e) => {
                lists.dl.truncate(dl_mark);
                lists.ul.truncate(ul_mark);
                let msg = format!("set {key:?}: {e:#}");
                if strict {
                    anyhow::bail!("{msg}");
                }
                skipped.push(msg);
            }
        }
    }
    caps.dl_feature_per_cc_list = lists.dl;
    caps.ul_feature_per_cc_list = lists.ul;

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
    let verify_failed = self_verify(&caps, patch, &set_keys, &delete_keys)?;

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
    use super::{
        super::format::{
            Kind, LteDirection, NrDirection, PatchCombo, PatchSubBlock, PerCc, RawLteSubBlock,
            RawNrSubBlock, SetKind,
        },
        *,
    };
    use crate::proto::{
        ComboGroup, ShannonFeatureSetDlPerCcNr, ShannonFeatureSetUlPerCcNr, UeCaps, combo_group,
    };
    use prost::Message;

    fn nr_patch_cc() -> PatchSubBlock {
        RawNrSubBlock {
            band: 78,
            dl: NrDirection {
                bw_class: Some(1),
                features: Some(PerCc::Resolved(vec![ShannonFeatureSetDlPerCcNr {
                    max_scs: Some(2),
                    max_mimo: Some(2),
                    max_bw: Some(100),
                    max_mod_order: Some(2),
                    ..Default::default()
                }])),
            },
            ul: NrDirection {
                bw_class: Some(1),
                features: Some(PerCc::Resolved(vec![ShannonFeatureSetUlPerCcNr {
                    max_scs: Some(2),
                    max_mimo_cb: Some(2),
                    max_mimo_non_cb: Some(1),
                    max_bw: Some(100),
                    max_mod_order: Some(2),
                    ..Default::default()
                }])),
            },
            srs_tx_switch: None,
        }
        .into()
    }

    #[test]
    fn reconstruct_sub_block_builds_and_dedups_feature_sets() {
        let mut lists = FeatureLists::default();
        let f1 = reconstruct_sub_block(&nr_patch_cc(), &mut lists).unwrap();
        // selector is 1-based into the per-direction list
        assert_eq!(f1.band, 10078);
        assert_eq!(f1.dl_feature_per_cc_ids, Some(vec![1]));
        assert_eq!(f1.ul_feature_per_cc_ids, Some(vec![1]));
        assert_eq!(lists.dl.len(), 1);
        assert_eq!(lists.dl[0].max_scs, Some(2));
        assert_eq!(lists.dl[0].max_mimo, Some(2));
        // a second identical CC dedups to the same index
        let f2 = reconstruct_sub_block(&nr_patch_cc(), &mut lists).unwrap();
        assert_eq!(f2.dl_feature_per_cc_ids, Some(vec![1]));
        assert_eq!(lists.dl.len(), 1);
        assert_eq!(lists.ul.len(), 1);
    }

    #[test]
    fn reconstruct_sub_block_without_feature_set_has_no_selector() {
        let cc = RawLteSubBlock {
            band: 1,
            dl: LteDirection {
                bw_class: None,
                feature_index: None,
                selector: None,
            },
            ul: LteDirection {
                bw_class: None,
                feature_index: None,
                selector: None,
            },
        }
        .into();
        let mut lists = FeatureLists::default();
        let f = reconstruct_sub_block(&cc, &mut lists).unwrap();
        assert_eq!(f.band, 1);
        assert_eq!(f.dl_feature_per_cc_ids, None);
        assert_eq!(f.ul_feature_per_cc_ids, None);
        assert!(lists.dl.is_empty() && lists.ul.is_empty());
    }

    #[test]
    fn reconstruct_sub_block_without_feature_set_preserves_raw_selector_ids() {
        let cc = RawNrSubBlock {
            band: 78,
            // 2 raw DL selector bytes need a DL class with cc_count 2.
            dl: NrDirection::with_selector(2, vec![3, 4]),
            ul: NrDirection::with_selector(1, vec![5]),
            srs_tx_switch: None,
        }
        .into();
        let mut lists = FeatureLists::default();

        let f = reconstruct_sub_block(&cc, &mut lists).unwrap();

        assert_eq!(f.band, 10078);
        assert_eq!(f.dl_feature_per_cc_ids, Some(vec![3, 4]));
        assert_eq!(f.ul_feature_per_cc_ids, Some(vec![5]));
        assert!(lists.dl.is_empty());
        assert!(lists.ul.is_empty());
    }

    // `reconstruct_sub_block_prefers_resolved_feature_set_over_raw_selector_ids` used to live
    // here: it built a component holding BOTH resolved values and raw selector bytes and
    // asserted the values won. `PerCc` cannot express that state, so there is nothing left to
    // prefer. The precedence itself is still covered where the two encodings can still meet —
    // the flat report DTO — by
    // `compiler::features::referenced_all_absent_record_is_resolved_on_both_the_compiler_and_patch_axes`.

    #[test]
    fn reconstruct_sub_block_accepts_unknown_raw_feature_codes() {
        let cc = RawNrSubBlock {
            band: 78,
            dl: NrDirection {
                bw_class: Some(1),
                features: Some(PerCc::Resolved(vec![ShannonFeatureSetDlPerCcNr {
                    max_scs: Some(9),
                    max_mimo: Some(7),
                    max_bw: Some(100),
                    max_mod_order: Some(8),
                    ..Default::default()
                }])),
            },
            ul: NrDirection {
                bw_class: Some(1),
                features: None,
            },
            srs_tx_switch: None,
        }
        .into();
        let mut lists = FeatureLists::default();

        let f = reconstruct_sub_block(&cc, &mut lists).unwrap();

        assert_eq!(f.band, 10078);
        assert_eq!(f.dl_feature_per_cc_ids, Some(vec![1]));
        assert_eq!(lists.dl.len(), 1);
        assert_eq!(lists.dl[0].max_scs, Some(9));
        assert_eq!(lists.dl[0].max_mimo, Some(7));
        assert_eq!(lists.dl[0].max_bw, Some(100));
        assert_eq!(lists.dl[0].max_mod_order, Some(8));
        assert!(lists.ul.is_empty());
    }

    #[test]
    fn reconstruct_derives_nr_feature_index_when_omitted() {
        let mut lists = FeatureLists::default();
        // NR component, index omitted in source, FR2 DL set + MIMO UL set.
        let cc = RawNrSubBlock {
            band: 78,
            dl: NrDirection {
                bw_class: Some(1),
                features: Some(PerCc::Resolved(vec![ShannonFeatureSetDlPerCcNr {
                    max_scs: Some(4),
                    ..Default::default()
                }])),
            },
            ul: NrDirection {
                bw_class: Some(1),
                features: Some(PerCc::Resolved(vec![ShannonFeatureSetUlPerCcNr {
                    max_mimo_cb: Some(2),
                    ..Default::default()
                }])),
            },
            srs_tx_switch: None,
        }
        .into();
        let out = reconstruct_sub_block(&cc, &mut lists).unwrap();
        assert_eq!(out.dl_feature_index, Some(2));
        assert_eq!(out.ul_feature_index, Some(2));

        // There is no "explicit NR override" case left to check: `RawNrSubBlock` has no
        // index field, so the derivation is the only possible answer. A component whose DL
        // resolves to nothing derives 0.
        let mut lists = FeatureLists::default();
        let bare: PatchSubBlock = RawNrSubBlock {
            band: 78,
            dl: NrDirection::bare(Some(1)),
            ul: NrDirection::bare(None),
            srs_tx_switch: None,
        }
        .into();
        assert_eq!(
            reconstruct_sub_block(&bare, &mut lists)
                .unwrap()
                .dl_feature_index,
            Some(0)
        );
    }

    fn base_caps() -> UeCaps {
        UeCaps {
            version: 874_888_686,
            combo_groups: vec![ComboGroup {
                combo_header: None,
                combo: vec![
                    combo_group::Combo {
                        bitmask: Some(0),
                        sub_blocks: vec![crate::proto::combo_group::combo::SubBlock {
                            band: 10078,
                            dl_bw_class: Some(1),
                            ul_bw_class: Some(1),
                            ..Default::default()
                        }],
                    },
                    combo_group::Combo {
                        bitmask: Some(0),
                        sub_blocks: vec![crate::proto::combo_group::combo::SubBlock {
                            band: 10041,
                            dl_bw_class: Some(1),
                            ul_bw_class: Some(1),
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
                kind: SetKind::Add,
                combo: vec![PatchCombo {
                    bit_mask: 0,
                    sub_blocks: vec![
                        RawNrSubBlock {
                            band: 2,
                            dl: NrDirection {
                                bw_class: Some(1),
                                features: None,
                            },
                            ul: NrDirection {
                                bw_class: Some(1),
                                features: None,
                            },
                            srs_tx_switch: None,
                        }
                        .into(),
                    ],
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
                kind: SetKind::Add,
                combo: vec![PatchCombo {
                    bit_mask: 0,
                    sub_blocks: vec![
                        RawNrSubBlock {
                            band: 2,
                            dl: NrDirection {
                                bw_class: Some(1),
                                features: None,
                            },
                            ul: NrDirection {
                                bw_class: Some(1),
                                features: None,
                            },
                            srs_tx_switch: None,
                        }
                        .into(),
                    ],
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
        let n2 = || combo_group::Combo {
            bitmask: Some(0),
            sub_blocks: vec![crate::proto::combo_group::combo::SubBlock {
                band: 10002,
                dl_bw_class: Some(1),
                ..Default::default()
            }],
        };
        let hdr_a = Some(combo_group::ComboHeader {
            power_class: Some(3),
            ..Default::default()
        });
        let hdr_b = Some(combo_group::ComboHeader {
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
                kind: SetKind::Change,
                combo: vec![PatchCombo {
                    bit_mask: 0,
                    sub_blocks: vec![
                        RawNrSubBlock {
                            band: 78,
                            dl: NrDirection {
                                bw_class: Some(1),
                                features: Some(PerCc::Resolved(vec![ShannonFeatureSetDlPerCcNr {
                                    max_scs: Some(2),
                                    max_mimo: Some(3),
                                    max_bw: Some(100),
                                    max_mod_order: Some(2),
                                    ..Default::default()
                                }])),
                            },
                            ul: NrDirection {
                                bw_class: Some(1),
                                features: None,
                            },
                            srs_tx_switch: None,
                        }
                        .into(),
                    ],
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
        let cc = &n78.sub_blocks[0];
        assert_eq!(cc.dl_mimo.as_deref(), Some("8x8"));
        assert_eq!(cc.dl_scs_khz, Some(30));
        assert_eq!(cc.dl_max_bw_mhz, Some(100));
        assert_eq!(cc.dl_mod_order.as_deref(), Some("QAM256"));
    }

    /// A non-uniform two-CC NR sub-block (distinct DL feature sets per CC) must apply and
    /// pass `self_verify` without being skipped: `reconstruct_sub_block` emits one selector
    /// byte per CC (Task 7), and `self_verify`'s `resolve_selectors_against` already
    /// resolves per-CC (`resolve_all`, since Task 4/5) — this pins that the two stay in
    /// lockstep end to end through a real `apply_patch` call, not just the unit-level
    /// `reconstruct_sub_block` test.
    #[test]
    fn apply_patch_multi_cc_non_uniform_features_pass_self_verify() {
        let base = base_caps(); // n78A, n41A
        let patch = NrPatch {
            kind: Kind::Nr,
            version: 1,
            delete: vec![],
            set: vec![SetEntry {
                kind: SetKind::Add,
                combo: vec![PatchCombo {
                    bit_mask: 0,
                    sub_blocks: vec![
                        RawNrSubBlock {
                            band: 48,
                            // class B, 2 CCs
                            dl: NrDirection::with_features(
                                2,
                                vec![
                                    ShannonFeatureSetDlPerCcNr {
                                        max_scs: Some(1),
                                        max_bw: Some(40),
                                        ..Default::default()
                                    },
                                    ShannonFeatureSetDlPerCcNr {
                                        max_scs: Some(2),
                                        max_bw: Some(100),
                                        ..Default::default()
                                    },
                                ],
                            ),
                            ul: NrDirection::bare(None),
                            srs_tx_switch: None,
                        }
                        .into(),
                    ],
                    ..Default::default()
                }],
            }],
        };

        let (result, outcome) = apply_patch(&base, &patch, false).unwrap();

        assert!(
            outcome.skipped.is_empty(),
            "self-verify must not skip a correctly-applied non-uniform multi-CC add: {:?}",
            outcome.skipped
        );
        assert_eq!(outcome.set, 1);
        assert_eq!(result.dl_feature_per_cc_list.len(), 2);
        let combos = build_combos(&result);
        let n48 = combos
            .iter()
            .find(|c| combo_key(c) == "n48B↓")
            .expect("n48B↓ must be present after add");
        assert_eq!(n48.sub_blocks[0].dl_features.len(), 2);
        assert_eq!(n48.sub_blocks[0].dl_features[0].max_scs, Some(1));
        assert_eq!(n48.sub_blocks[0].dl_features[1].max_scs, Some(2));
    }

    #[test]
    fn apply_patch_preserves_unknown_raw_feature_codes() {
        let base = base_caps();
        let patch = NrPatch {
            kind: Kind::Nr,
            version: 1,
            delete: vec![],
            set: vec![SetEntry {
                kind: SetKind::Add,
                combo: vec![PatchCombo {
                    bit_mask: 0,
                    sub_blocks: vec![
                        RawNrSubBlock {
                            band: 2,
                            dl: NrDirection {
                                bw_class: Some(1),
                                features: Some(PerCc::Resolved(vec![ShannonFeatureSetDlPerCcNr {
                                    max_scs: Some(9),
                                    max_mimo: Some(7),
                                    max_bw: Some(100),
                                    max_mod_order: Some(8),
                                    ..Default::default()
                                }])),
                            },
                            ul: NrDirection {
                                bw_class: Some(1),
                                features: None,
                            },
                            srs_tx_switch: None,
                        }
                        .into(),
                    ],
                    ..Default::default()
                }],
            }],
        };

        let (result, outcome) = apply_patch(&base, &patch, false).unwrap();

        assert!(outcome.skipped.is_empty());
        assert_eq!(outcome.set, 1);
        assert_eq!(result.dl_feature_per_cc_list.len(), 1);
        assert_eq!(result.dl_feature_per_cc_list[0].max_scs, Some(9));
        assert_eq!(result.dl_feature_per_cc_list[0].max_mimo, Some(7));
        assert_eq!(result.dl_feature_per_cc_list[0].max_mod_order, Some(8));
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

    /// Schema-aware count of explicit `ComboGroup.Combo.bitmask` (field 2) occurrences
    /// in a `UeCaps` wire buffer. Walks `UeCaps.combo_groups`(3) -> `ComboGroup.combo`(2)
    /// -> `Combo.bitmask`(2). Field 2 means different things in other messages, so this
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
        // field 2 -> Combo); 2 = Combo (count field 2 = bitmask).
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
            "fixture has no Combo.bitmask fields; pick a carrier file with combos"
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

    /// R2: a base with a **value-bearing** feature list plus a selector-only `set`
    /// component. Pass 1 applies the documented selector passthrough correctly, but the
    /// want-side key kept raw ids while the got side resolved them against the applied
    /// list, so self_verify wrongly reverted the entry. (base_caps has empty lists, so
    /// existing tests never hit this.)
    #[test]
    fn apply_patch_selector_only_over_value_bearing_base_is_not_reverted() {
        use crate::proto::{ShannonFeatureSetDlPerCcNr, combo_group::combo::SubBlock};
        let base = UeCaps {
            version: 874_888_686,
            dl_feature_per_cc_list: vec![ShannonFeatureSetDlPerCcNr {
                max_bw: Some(100),
                ..Default::default()
            }],
            combo_groups: vec![ComboGroup {
                combo_header: None,
                combo: vec![combo_group::Combo {
                    bitmask: Some(0),
                    sub_blocks: vec![SubBlock {
                        band: 10041, // n41A base combo (selector-only, no feature)
                        dl_bw_class: Some(1),
                        ul_bw_class: Some(1),
                        ..Default::default()
                    }],
                }],
            }],
            ..Default::default()
        };
        let patch = NrPatch {
            kind: Kind::Nr,
            version: 1,
            delete: vec![],
            set: vec![SetEntry {
                kind: SetKind::Add,
                combo: vec![PatchCombo {
                    bit_mask: 0,
                    sub_blocks: vec![
                        RawNrSubBlock {
                            band: 78,
                            // Selector-only: points at the base's dl_feature_per_cc_list[0].
                            dl: NrDirection::with_selector(1, vec![1]),
                            ul: NrDirection::bare(Some(1)),
                            srs_tx_switch: None,
                        }
                        .into(),
                    ],
                    ..Default::default()
                }],
            }],
        };

        let (result, outcome) = apply_patch(&base, &patch, false).unwrap();

        assert!(
            outcome.skipped.is_empty(),
            "selector-only add must not be reverted: {:?}",
            outcome.skipped
        );
        let combos = build_combos(&result);
        let n78 = combos
            .iter()
            .find(|c| combo_key(c) == "n78A")
            .expect("n78A must be present after a selector-only add");
        // The selector resolved against the base list, so the value shows through.
        assert_eq!(n78.sub_blocks[0].dl_max_bw_mhz, Some(100));
    }

    /// R6: a hand-authored patch that deletes `n78A` and re-adds it via a `set` entry.
    /// The union remove-then-append is correct, so self_verify must not flag the delete
    /// as failed merely because the key is present again (the set re-added it).
    #[test]
    fn apply_patch_delete_then_re_add_same_key_is_not_reverted() {
        let base = base_caps(); // n78A (no feature), n41A
        let patch = NrPatch {
            kind: Kind::Nr,
            version: 1,
            delete: vec!["n78A".to_string()],
            set: vec![SetEntry {
                kind: SetKind::Change,
                combo: vec![PatchCombo {
                    bit_mask: 0,
                    sub_blocks: vec![
                        RawNrSubBlock {
                            band: 78,
                            dl: NrDirection {
                                bw_class: Some(1),
                                features: Some(PerCc::Resolved(vec![ShannonFeatureSetDlPerCcNr {
                                    max_mimo: Some(3), // set's version differs from the base (no feature)
                                    max_bw: Some(100),
                                    ..Default::default()
                                }])),
                            },
                            ul: NrDirection {
                                bw_class: Some(1),
                                features: None,
                            },
                            srs_tx_switch: None,
                        }
                        .into(),
                    ],
                    ..Default::default()
                }],
            }],
        };

        let (result, outcome) = apply_patch(&base, &patch, false).unwrap();

        assert!(
            outcome.skipped.is_empty(),
            "delete + re-add of the same key must not warn: {:?}",
            outcome.skipped
        );
        let combos = build_combos(&result);
        let n78 = combos
            .iter()
            .find(|c| combo_key(c) == "n78A")
            .expect("n78A must be present (re-added by the set entry)");
        assert_eq!(n78.sub_blocks[0].dl_mimo.as_deref(), Some("8x8")); // the set's version won
        assert!(present_keys(&result).contains("n41A")); // untouched
    }
}
