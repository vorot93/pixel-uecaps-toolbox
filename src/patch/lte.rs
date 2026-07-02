//! LTE-fallback combo patches: diff/apply for `patch create`/`apply` on `lte_*.binarypb`.

use super::{
    build::Outcome,
    format::{
        self, Kind, LtePatch, LtePatchCombo, LtePatchComponent, LteSetEntry, lte_set_entry_key,
    },
};
use crate::{
    proto::{LteCaps, LteCombo},
    report::lte::lte_combo_key,
};
use std::collections::BTreeSet;

/// Full-field canonical form of one LTE combo: sorted components + bcs/unknown.
#[derive(PartialEq, Eq, PartialOrd, Ord)]
struct CanonLteCombo {
    components: Vec<(i32, i32, i32)>, // (band, dl, ul)
    bcs: u64,
    unknown1: u64,
    unknown2: u64,
}

fn canon(c: &LteCombo) -> CanonLteCombo {
    let mut components: Vec<(i32, i32, i32)> = c
        .components
        .iter()
        .map(|x| (x.band, x.bw_class_mimo_dl, x.bw_class_mimo_ul.unwrap_or(0)))
        .collect();
    components.sort_unstable();
    CanonLteCombo {
        components,
        bcs: c.bcs.unwrap_or(0),
        unknown1: c.unknown1.unwrap_or(0),
        unknown2: c.unknown2.unwrap_or(0),
    }
}

fn canon_variants(combos: &[&LteCombo]) -> Vec<CanonLteCombo> {
    let mut v: Vec<CanonLteCombo> = combos.iter().map(|c| canon(c)).collect();
    v.sort();
    v
}

fn to_patch_combo(c: &LteCombo) -> LtePatchCombo {
    LtePatchCombo {
        components: c
            .components
            .iter()
            .map(|x| LtePatchComponent {
                band: x.band,
                bw_class_mimo_dl: x.bw_class_mimo_dl,
                bw_class_mimo_ul: x.bw_class_mimo_ul.unwrap_or(0),
            })
            .collect(),
        bcs: c.bcs.unwrap_or(0),
        unknown1: c.unknown1.unwrap_or(0),
        unknown2: c.unknown2.unwrap_or(0),
    }
}

/// Diff A -> B LTE combos into an LTE patch.
pub(crate) fn build_lte_patch(a: &[LteCombo], b: &[LteCombo]) -> LtePatch {
    let ia = super::index_by_key(a, lte_combo_key);
    let ib = super::index_by_key(b, lte_combo_key);
    let delete: Vec<String> = ia
        .keys()
        .filter(|k| !ib.contains_key(*k))
        .cloned()
        .collect();
    let mut set = Vec::new();
    for (k, bc) in &ib {
        let (differs, kind) = match ia.get(k) {
            None => (true, "add"),
            Some(ac) => (canon_variants(ac) != canon_variants(bc), "change"),
        };
        if differs {
            set.push(LteSetEntry {
                kind: Some(kind.to_string()),
                combo: bc.iter().map(|c| to_patch_combo(c)).collect(),
            });
        }
    }
    LtePatch {
        kind: Kind::Lte,
        version: 1,
        delete,
        set,
    }
}

/// Apply an LTE patch to a base `LteCaps` -> new `LteCaps` (best-effort; `strict` fails on the
/// first delete key absent from the base). Preserves `fingerprint`/`bitmask`.
pub(crate) fn apply_lte_patch(
    base: &LteCaps,
    patch: &LtePatch,
    strict: bool,
) -> anyhow::Result<(LteCaps, Outcome)> {
    let base_keys: BTreeSet<String> = base.combos.iter().map(lte_combo_key).collect();
    let del: BTreeSet<&str> = patch.delete.iter().map(String::as_str).collect();
    let set_keys: BTreeSet<String> = patch
        .set
        .iter()
        .map(lte_set_entry_key)
        .collect::<anyhow::Result<_>>()?;

    let mut skipped = Vec::new();
    let mut deleted = 0;
    for k in &patch.delete {
        if base_keys.contains(k) {
            deleted += 1;
        } else {
            let msg = format!("delete key not present in base: {k}");
            if strict {
                anyhow::bail!(msg);
            }
            skipped.push(msg);
        }
    }

    // Keep base combos that aren't deleted or replaced by a set entry, then append
    // the set entries' combos.
    let mut combos: Vec<LteCombo> = base
        .combos
        .iter()
        .filter(|c| {
            let k = lte_combo_key(c);
            !del.contains(k.as_str()) && !set_keys.contains(k.as_str())
        })
        .cloned()
        .collect();
    combos.extend(
        patch
            .set
            .iter()
            .flat_map(|e| e.combo.iter().map(format::lte_combo_from_patch)),
    );
    let result = LteCaps {
        fingerprint: base.fingerprint,
        combos,
        bitmask: base.bitmask,
    };
    Ok((
        result,
        Outcome {
            deleted,
            set: patch.set.len(),
            skipped,
        },
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proto::LteComponent;

    fn combo(band: i32, ul: i32) -> LteCombo {
        LteCombo {
            components: vec![LteComponent {
                band,
                bw_class_mimo_dl: 32768,
                bw_class_mimo_ul: Some(ul),
            }],
            bcs: Some(0),
            unknown1: Some(0),
            unknown2: Some(0),
        }
    }

    #[test]
    fn build_lte_patch_classifies_add_change_delete() {
        // A: B1A (bcs 0), B5A↓   B: B1A (bcs 9 → change), B7A↓ (add)
        let a1 = combo(1, 32768);
        let a2 = combo(5, 0); // B5A↓
        let mut b1 = combo(1, 32768);
        b1.bcs = Some(9); // same key B1A, different canon (bcs) -> change
        let b2 = combo(7, 0); // B7A↓ -> add
        let p = build_lte_patch(&[a1, a2], &[b1, b2]);
        assert_eq!(p.delete, vec!["B5A↓".to_string()]);
        let keys: Vec<String> = p
            .set
            .iter()
            .map(|e| lte_set_entry_key(e).unwrap())
            .collect();
        assert_eq!(keys, vec!["B1A".to_string(), "B7A↓".to_string()]); // sorted
        assert_eq!(
            p.set
                .iter()
                .find(|e| lte_set_entry_key(e).unwrap() == "B1A")
                .unwrap()
                .kind
                .as_deref(),
            Some("change")
        );
        assert_eq!(
            p.set
                .iter()
                .find(|e| lte_set_entry_key(e).unwrap() == "B7A↓")
                .unwrap()
                .kind
                .as_deref(),
            Some("add")
        );
    }

    #[test]
    fn apply_lte_patch_reproduces_b_and_preserves_identity() {
        let base = LteCaps {
            fingerprint: 874_888_686,
            combos: vec![combo(1, 32768), combo(5, 0)],
            bitmask: 42,
        };
        let target = vec![combo(1, 32768), combo(7, 0)];
        let patch = build_lte_patch(&base.combos, &target);
        let (result, outcome) = apply_lte_patch(&base, &patch, false).unwrap();
        let got: BTreeSet<String> = result.combos.iter().map(lte_combo_key).collect();
        let want: BTreeSet<String> = target.iter().map(lte_combo_key).collect();
        assert_eq!(got, want);
        assert_eq!(result.fingerprint, 874_888_686); // identity preserved
        assert_eq!(result.bitmask, 42);
        assert!(outcome.skipped.is_empty());
    }

    #[test]
    fn apply_strict_fails_on_missing_delete_key() {
        let base = LteCaps {
            fingerprint: 1,
            combos: vec![combo(1, 32768)],
            bitmask: 0,
        };
        let patch = LtePatch {
            kind: Kind::Lte,
            version: 1,
            delete: vec!["B99A".to_string()], // not in base
            set: vec![],
        };
        assert!(apply_lte_patch(&base, &patch, true).is_err());
        let (_, outcome) = apply_lte_patch(&base, &patch, false).unwrap();
        assert_eq!(outcome.skipped.len(), 1);
    }
}
