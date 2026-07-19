use std::collections::BTreeSet;

use anyhow::{Context, ensure};

use crate::{
    proto::{
        ShannonFeatureSetDlPerCcNr, ShannonFeatureSetUlPerCcNr,
        combo_group::combo::SubBlock as ProtoSubBlock,
    },
    raw_nr::{RawNrPayload, RawSubBlock, SubBlockKind},
    report::combos::feature_index,
};

#[derive(Clone, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct DlFeatureSource {
    pub(crate) max_scs: Option<i32>,
    pub(crate) max_mimo: Option<i32>,
    pub(crate) max_bw: Option<i32>,
    pub(crate) max_mod_order: Option<i32>,
    pub(crate) bw_90mhz_supported: Option<bool>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct UlFeatureSource {
    pub(crate) max_scs: Option<i32>,
    pub(crate) max_mimo_cb: Option<i32>,
    pub(crate) max_bw: Option<i32>,
    pub(crate) max_mod_order: Option<i32>,
    pub(crate) bw_90mhz_supported: Option<bool>,
    pub(crate) max_mimo_non_cb: Option<i32>,
}

impl From<&ShannonFeatureSetDlPerCcNr> for DlFeatureSource {
    fn from(value: &ShannonFeatureSetDlPerCcNr) -> Self {
        Self {
            max_scs: value.max_scs,
            max_mimo: value.max_mimo,
            max_bw: value.max_bw,
            max_mod_order: value.max_mod_order,
            bw_90mhz_supported: value.bw_90mhz_supported,
        }
    }
}

impl From<&DlFeatureSource> for ShannonFeatureSetDlPerCcNr {
    fn from(value: &DlFeatureSource) -> Self {
        Self {
            max_scs: value.max_scs,
            max_mimo: value.max_mimo,
            max_bw: value.max_bw,
            max_mod_order: value.max_mod_order,
            bw_90mhz_supported: value.bw_90mhz_supported,
        }
    }
}

impl From<&ShannonFeatureSetUlPerCcNr> for UlFeatureSource {
    fn from(value: &ShannonFeatureSetUlPerCcNr) -> Self {
        Self {
            max_scs: value.max_scs,
            max_mimo_cb: value.max_mimo_cb,
            max_bw: value.max_bw,
            max_mod_order: value.max_mod_order,
            bw_90mhz_supported: value.bw_90mhz_supported,
            max_mimo_non_cb: value.max_mimo_non_cb,
        }
    }
}

impl From<&UlFeatureSource> for ShannonFeatureSetUlPerCcNr {
    fn from(value: &UlFeatureSource) -> Self {
        Self {
            max_scs: value.max_scs,
            max_mimo_cb: value.max_mimo_cb,
            max_bw: value.max_bw,
            max_mod_order: value.max_mod_order,
            bw_90mhz_supported: value.bw_90mhz_supported,
            max_mimo_non_cb: value.max_mimo_non_cb,
        }
    }
}

#[derive(Clone, Debug, Default)]
pub(crate) struct NrSourceSubBlock {
    pub(crate) kind: SubBlockKind,
    pub(crate) band: i32,
    pub(crate) dl_bw_class: Option<i32>,
    pub(crate) ul_bw_class: Option<i32>,
    pub(crate) dl_feature_index: Option<i32>,
    pub(crate) ul_feature_index: Option<i32>,
    pub(crate) dl_feature: Vec<usize>,
    pub(crate) ul_feature: Vec<usize>,
    pub(crate) dl_feature_per_cc_ids: Option<Vec<u8>>,
    pub(crate) ul_feature_per_cc_ids: Option<Vec<u8>>,
    pub(crate) srs_tx_switch: Option<i32>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct FeatureCatalogs {
    pub(crate) dl: Vec<DlFeatureSource>,
    pub(crate) ul: Vec<UlFeatureSource>,
}

#[derive(Debug)]
pub(crate) struct LocalFeaturePlan {
    pub(crate) dl_source: Vec<DlFeatureSource>,
    pub(crate) ul_source: Vec<UlFeatureSource>,
    pub(crate) dl: Vec<ShannonFeatureSetDlPerCcNr>,
    pub(crate) ul: Vec<ShannonFeatureSetUlPerCcNr>,
}

fn ensure_selector_only_unresolved(
    ids: &Option<Vec<u8>>,
    local_len: usize,
    direction: &str,
    basename: &str,
    sku: &str,
) -> anyhow::Result<()> {
    if let Some(index) = ids
        .as_deref()
        .and_then(|values| values.first())
        .copied()
        .filter(|index| *index != 0)
    {
        // Complement of the resolution rule: a nonzero selector-only leading byte must NOT
        // resolve against the compact local list (else its meaning would change). `> len` is
        // exactly `feature_index(..).is_none()` for a nonzero byte — route through the authority.
        ensure!(
            feature_index(ids.as_deref(), local_len).is_none(),
            "{basename} ({sku}) {direction} selector-only leading byte {index} would resolve against compact local list length {local_len}"
        );
    }
    Ok(())
}

impl LocalFeaturePlan {
    pub(crate) fn new(
        catalogs: &FeatureCatalogs,
        payloads: &[&RawNrPayload],
        basename: &str,
        sku: &str,
    ) -> anyhow::Result<Self> {
        let mut used_dl = BTreeSet::new();
        let mut used_ul = BTreeSet::new();
        // Scan EVERY per-CC entry in `dl_features`/`ul_features`, not just CC0, so a
        // non-uniform multi-CC sub-block's second-and-later CCs are represented in the
        // local plan too. Paired with `reconstruct_sub_block` below, which emits one
        // selector byte per entry — both must stay in lockstep or
        // `verify_compact_feature_list` (compiler/nr.rs) fails "unused/missing feature
        // record" (a per-CC1+ record referenced by reconstruct but absent from the plan)
        // or leaves a plan record unreferenced (present here but never emitted).
        for payload in payloads {
            for component in &payload.sub_blocks {
                for feature in &component.dl_features {
                    used_dl.insert(DlFeatureSource::from(feature));
                }
                for feature in &component.ul_features {
                    used_ul.insert(UlFeatureSource::from(feature));
                }
            }
        }

        let dl_source = catalogs
            .dl
            .iter()
            .filter(|feature| used_dl.contains(*feature))
            .cloned()
            .collect::<Vec<_>>();
        let ul_source = catalogs
            .ul
            .iter()
            .filter(|feature| used_ul.contains(*feature))
            .cloned()
            .collect::<Vec<_>>();
        ensure!(
            dl_source.len() == used_dl.len(),
            "{basename} ({sku}) uses a DL feature absent from the global catalog"
        );
        ensure!(
            ul_source.len() == used_ul.len(),
            "{basename} ({sku}) uses a UL feature absent from the global catalog"
        );
        ensure!(
            dl_source.len() <= usize::from(u8::MAX),
            "{basename} ({sku}) uses {} distinct DL feature records; local limit is 255",
            dl_source.len()
        );
        ensure!(
            ul_source.len() <= usize::from(u8::MAX),
            "{basename} ({sku}) uses {} distinct UL feature records; local limit is 255",
            ul_source.len()
        );

        for payload in payloads {
            for component in &payload.sub_blocks {
                if !component.dl_feature_set_is_present() {
                    ensure_selector_only_unresolved(
                        &component.dl_cc_ids,
                        dl_source.len(),
                        "DL",
                        basename,
                        sku,
                    )?;
                }
                if !component.ul_feature_set_is_present() {
                    ensure_selector_only_unresolved(
                        &component.ul_cc_ids,
                        ul_source.len(),
                        "UL",
                        basename,
                        sku,
                    )?;
                }
            }
        }

        let dl = dl_source
            .iter()
            .map(ShannonFeatureSetDlPerCcNr::from)
            .collect();
        let ul = ul_source
            .iter()
            .map(ShannonFeatureSetUlPerCcNr::from)
            .collect();
        Ok(Self {
            dl_source,
            ul_source,
            dl,
            ul,
        })
    }

    pub(crate) fn reconstruct_sub_block(
        &self,
        component: &RawSubBlock,
    ) -> anyhow::Result<ProtoSubBlock> {
        component.validate()?;
        // One selector byte per `dl_features`/`ul_features` entry — a CC-count-long array,
        // not a single byte. Paired with `LocalFeaturePlan::new`'s used_dl/used_ul scan
        // above: every per-CC record referenced here was inserted there, so `binary_search`
        // (not find_or_append) is enough. Falls back to the raw selector bytes when the
        // sub-block carries no resolved feature sets at all.
        let dl_feature_per_cc_ids = if component.dl_features.is_empty() {
            component.dl_cc_ids.clone()
        } else {
            Some(
                component
                    .dl_features
                    .iter()
                    .map(|feature| {
                        let index = self
                            .dl_source
                            .binary_search(&DlFeatureSource::from(feature))
                            .expect("local DL plan contains every resolved component")
                            + 1;
                        u8::try_from(index).expect("local DL plan is at most 255")
                    })
                    .collect(),
            )
        };
        let ul_feature_per_cc_ids = if component.ul_features.is_empty() {
            component.ul_cc_ids.clone()
        } else {
            Some(
                component
                    .ul_features
                    .iter()
                    .map(|feature| {
                        let index = self
                            .ul_source
                            .binary_search(&UlFeatureSource::from(feature))
                            .expect("local UL plan contains every resolved component")
                            + 1;
                        u8::try_from(index).expect("local UL plan is at most 255")
                    })
                    .collect(),
            )
        };
        Ok(ProtoSubBlock {
            band: component.raw_band(),
            dl_bw_class: component.dl_bw_class,
            ul_bw_class: component.ul_bw_class,
            dl_feature_index: component.materialized_dl_feature_index(),
            ul_feature_index: component.materialized_ul_feature_index(),
            dl_feature_per_cc_ids,
            ul_feature_per_cc_ids,
            srstxswitch: component.srs_tx_switch,
        })
    }
}

impl FeatureCatalogs {
    pub(crate) fn new(dl: Vec<DlFeatureSource>, ul: Vec<UlFeatureSource>) -> Self {
        Self { dl, ul }
    }

    /// Inserts every per-CC feature (not just CC0) into the global catalog, so a
    /// non-uniform sub-block's second-and-later CCs are represented too.
    pub(crate) fn from_payloads<'a>(payloads: impl IntoIterator<Item = &'a RawNrPayload>) -> Self {
        let mut dl = BTreeSet::new();
        let mut ul = BTreeSet::new();
        for payload in payloads {
            for component in &payload.sub_blocks {
                for feature in &component.dl_features {
                    dl.insert(DlFeatureSource::from(feature));
                }
                for feature in &component.ul_features {
                    ul.insert(UlFeatureSource::from(feature));
                }
            }
        }
        Self {
            dl: dl.into_iter().collect(),
            ul: ul.into_iter().collect(),
        }
    }

    /// Per-CC global-catalog references: each entry in `dl_features`/`ul_features` maps to
    /// its own 1-based index into the canonical (global) catalog — one `usize` per CC.
    pub(crate) fn source_sub_block(&self, component: &RawSubBlock) -> NrSourceSubBlock {
        let dl_feature: Vec<usize> = component
            .dl_features
            .iter()
            .map(|feature| {
                self.dl
                    .binary_search(&DlFeatureSource::from(feature))
                    .expect("canonical DL catalog contains every resolved component")
                    + 1
            })
            .collect();
        let ul_feature: Vec<usize> = component
            .ul_features
            .iter()
            .map(|feature| {
                self.ul
                    .binary_search(&UlFeatureSource::from(feature))
                    .expect("canonical UL catalog contains every resolved component")
                    + 1
            })
            .collect();
        NrSourceSubBlock {
            kind: component.kind,
            band: component.band,
            dl_bw_class: component.dl_bw_class,
            ul_bw_class: component.ul_bw_class,
            dl_feature_index: component.source_dl_feature_index(),
            ul_feature_index: component.source_ul_feature_index(),
            dl_feature_per_cc_ids: dl_feature
                .is_empty()
                .then(|| component.dl_cc_ids.clone())
                .flatten(),
            ul_feature_per_cc_ids: ul_feature
                .is_empty()
                .then(|| component.ul_cc_ids.clone())
                .flatten(),
            dl_feature,
            ul_feature,
            srs_tx_switch: component.srs_tx_switch,
        }
    }
}

fn resolve_index<T: Clone>(index: usize, records: &[T], direction: &str) -> anyhow::Result<T> {
    ensure!(
        index != 0,
        "{direction}_feature index must be 1-based, not 0"
    );
    records.get(index - 1).cloned().with_context(|| {
        format!(
            "{direction}_feature index {index} exceeds the {direction} catalog length {}",
            records.len()
        )
    })
}

impl NrSourceSubBlock {
    pub(crate) fn resolve(&self, catalogs: &FeatureCatalogs) -> anyhow::Result<RawSubBlock> {
        ensure!(
            self.dl_feature.is_empty() || self.dl_feature_per_cc_ids.is_none(),
            "component has both dl_feature and dl_feature_per_cc_ids"
        );
        ensure!(
            self.ul_feature.is_empty() || self.ul_feature_per_cc_ids.is_none(),
            "component has both ul_feature and ul_feature_per_cc_ids"
        );
        let dl = self
            .dl_feature
            .iter()
            .map(|&index| {
                resolve_index(index, &catalogs.dl, "dl")
                    .map(|source| ShannonFeatureSetDlPerCcNr::from(&source))
            })
            .collect::<anyhow::Result<Vec<_>>>()?;
        let ul = self
            .ul_feature
            .iter()
            .map(|&index| {
                resolve_index(index, &catalogs.ul, "ul")
                    .map(|source| ShannonFeatureSetUlPerCcNr::from(&source))
            })
            .collect::<anyhow::Result<Vec<_>>>()?;
        let component = RawSubBlock {
            kind: self.kind,
            band: self.band,
            dl_bw_class: self.dl_bw_class,
            ul_bw_class: self.ul_bw_class,
            dl_feature_index: self.dl_feature_index,
            ul_feature_index: self.ul_feature_index,
            dl_cc_ids: self.dl_feature_per_cc_ids.clone(),
            ul_cc_ids: self.ul_feature_per_cc_ids.clone(),
            srs_tx_switch: self.srs_tx_switch,
            ..Default::default()
        }
        .with_resolved_feature_sets(dl, ul);
        component.validate()?;
        Ok(component)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        proto::{ShannonFeatureSetDlPerCcNr, ShannonFeatureSetUlPerCcNr},
        raw_nr::{RawNrPayload, RawSubBlock, RawSubBlockKey, SubBlockKind},
        report::combos::SubBlock,
    };

    #[test]
    fn local_plan_reconstruct_sub_block_emits_one_selector_per_cc() {
        // A class-2 NR n48 sub-block with two *different* resolved DL features must
        // reconstruct a 2-byte `dl_feature_per_cc_ids`, not a single byte (the bug this
        // per-CC catalog model fixes: the old CC0-only projection silently dropped every
        // CC after the first). Note: this exercises the COMPILER's `LocalFeaturePlan`,
        // which resolves against a pre-scanned local catalog (`binary_search`); the free
        // `raw_nr::reconstruct_sub_block` (the patch-build path) also emits one selector
        // per CC as of Task 7, but grows its lists on the fly (`find_or_append`) instead.
        let a = ShannonFeatureSetDlPerCcNr {
            max_bw: Some(40),
            ..Default::default()
        };
        let b = ShannonFeatureSetDlPerCcNr {
            max_bw: Some(100),
            ..Default::default()
        };
        let sb = RawSubBlock {
            kind: SubBlockKind::Nr,
            band: 48,
            dl_bw_class: Some(2),
            dl_features: vec![a, b],
            ..Default::default()
        };
        let payload = RawNrPayload {
            power_class: None,
            bcs_nr: None,
            bcs_intra_endc: None,
            bcs_eutra: None,
            intra_band_en_dc_support: None,
            sub_blocks: vec![sb.clone()],
        };
        let catalogs = FeatureCatalogs::from_payloads([&payload]);
        let plan = LocalFeaturePlan::new(&catalogs, &[&payload], "A.binarypb", "legacy").unwrap();
        let out = plan.reconstruct_sub_block(&sb).unwrap();
        assert_eq!(
            out.dl_feature_per_cc_ids.as_ref().map(Vec::len),
            Some(2),
            "one selector byte per CC, not a single transitional CC0 byte"
        );
        assert_eq!(
            plan.dl.len(),
            2,
            "two distinct features deduped into the list"
        );
    }

    #[test]
    fn local_plan_filters_dl_global_order_and_rewrites_resolved_selectors() {
        let low = DlFeatureSource {
            max_scs: Some(1),
            ..Default::default()
        };
        let high = DlFeatureSource {
            max_scs: Some(3),
            ..Default::default()
        };
        let catalogs = FeatureCatalogs::new(vec![low, high.clone()], vec![]);
        let component = RawSubBlock {
            kind: SubBlockKind::Nr,
            band: 78,
            dl_bw_class: Some(1),
            ..Default::default()
        }
        .with_resolved_feature_sets(vec![ShannonFeatureSetDlPerCcNr::from(&high)], vec![]);
        let payload = RawNrPayload {
            power_class: None,
            bcs_nr: None,
            bcs_intra_endc: None,
            bcs_eutra: None,
            intra_band_en_dc_support: None,
            sub_blocks: vec![component.clone()],
        };
        let plan = LocalFeaturePlan::new(&catalogs, &[&payload], "A.binarypb", "legacy").unwrap();
        assert_eq!(plan.dl_source, vec![high]);
        assert_eq!(plan.dl.len(), 1);
        assert_eq!(
            plan.reconstruct_sub_block(&component)
                .unwrap()
                .dl_feature_per_cc_ids,
            Some(vec![1])
        );
    }

    #[test]
    fn local_plan_filters_ul_global_order_and_rewrites_resolved_selectors() {
        let low = UlFeatureSource {
            max_scs: Some(1),
            ..Default::default()
        };
        let high = UlFeatureSource {
            max_scs: Some(3),
            ..Default::default()
        };
        let catalogs = FeatureCatalogs::new(vec![], vec![low, high.clone()]);
        let component = RawSubBlock {
            kind: SubBlockKind::Nr,
            band: 78,
            ul_bw_class: Some(1),
            ..Default::default()
        }
        .with_resolved_feature_sets(vec![], vec![ShannonFeatureSetUlPerCcNr::from(&high)]);
        let payload = RawNrPayload {
            power_class: None,
            bcs_nr: None,
            bcs_intra_endc: None,
            bcs_eutra: None,
            intra_band_en_dc_support: None,
            sub_blocks: vec![component.clone()],
        };
        let plan = LocalFeaturePlan::new(&catalogs, &[&payload], "A.binarypb", "legacy").unwrap();
        assert_eq!(plan.ul_source, vec![high.clone()]);
        assert_eq!(plan.ul, vec![ShannonFeatureSetUlPerCcNr::from(&high)]);
        assert!(plan.dl_source.is_empty());
        assert!(plan.dl.is_empty());
        let reconstructed = plan.reconstruct_sub_block(&component).unwrap();
        assert_eq!(reconstructed.ul_feature_per_cc_ids, Some(vec![1]));
        assert_eq!(reconstructed.dl_feature_per_cc_ids, None);
    }

    #[test]
    fn local_plan_emits_a_referenced_all_absent_dl_record() {
        let catalogs = FeatureCatalogs::new(vec![DlFeatureSource::default()], vec![]);
        let component = RawSubBlock {
            kind: SubBlockKind::Nr,
            band: 78,
            dl_bw_class: Some(1),
            ..Default::default()
        }
        .with_resolved_feature_sets(vec![ShannonFeatureSetDlPerCcNr::default()], vec![]);
        let payload = RawNrPayload {
            power_class: None,
            bcs_nr: None,
            bcs_intra_endc: None,
            bcs_eutra: None,
            intra_band_en_dc_support: None,
            sub_blocks: vec![component.clone()],
        };
        let plan = LocalFeaturePlan::new(&catalogs, &[&payload], "A.binarypb", "legacy").unwrap();
        assert_eq!(plan.dl, vec![ShannonFeatureSetDlPerCcNr::default()]);
        assert_eq!(
            plan.reconstruct_sub_block(&component)
                .unwrap()
                .dl_feature_per_cc_ids,
            Some(vec![1])
        );
    }

    #[test]
    fn local_plan_emits_a_referenced_all_absent_ul_record() {
        let catalogs = FeatureCatalogs::new(vec![], vec![UlFeatureSource::default()]);
        let component = RawSubBlock {
            kind: SubBlockKind::Nr,
            band: 78,
            ul_bw_class: Some(1),
            ..Default::default()
        }
        .with_resolved_feature_sets(vec![], vec![ShannonFeatureSetUlPerCcNr::default()]);
        let payload = RawNrPayload {
            power_class: None,
            bcs_nr: None,
            bcs_intra_endc: None,
            bcs_eutra: None,
            intra_band_en_dc_support: None,
            sub_blocks: vec![component.clone()],
        };
        let plan = LocalFeaturePlan::new(&catalogs, &[&payload], "A.binarypb", "legacy").unwrap();
        assert_eq!(plan.ul, vec![ShannonFeatureSetUlPerCcNr::default()]);
        assert_eq!(
            plan.reconstruct_sub_block(&component)
                .unwrap()
                .ul_feature_per_cc_ids,
            Some(vec![1])
        );
    }

    #[test]
    fn local_plan_rejects_selector_only_collision_without_filler() {
        let catalogs = FeatureCatalogs::new(
            vec![DlFeatureSource {
                max_scs: Some(3),
                ..Default::default()
            }],
            vec![],
        );
        let resolved = RawSubBlock {
            kind: SubBlockKind::Nr,
            band: 78,
            dl_features: vec![ShannonFeatureSetDlPerCcNr {
                max_scs: Some(3),
                ..Default::default()
            }],
            ..Default::default()
        };
        let selector_only = RawSubBlock {
            kind: SubBlockKind::Nr,
            band: 41,
            dl_cc_ids: Some(vec![1, 9]),
            ..Default::default()
        };
        let payload = RawNrPayload {
            power_class: None,
            bcs_nr: None,
            bcs_intra_endc: None,
            bcs_eutra: None,
            intra_band_en_dc_support: None,
            sub_blocks: vec![resolved, selector_only],
        };
        let error = LocalFeaturePlan::new(&catalogs, &[&payload], "A_1.binarypb", "G2YBB")
            .unwrap_err()
            .to_string();
        assert!(error.contains("DL selector-only leading byte 1"), "{error}");
        assert!(error.contains("local list length 1"), "{error}");
        assert!(error.contains("G2YBB"), "{error}");
    }

    #[test]
    fn referenced_all_absent_record_is_resolved_on_both_the_compiler_and_patch_axes() {
        // Before Task 7, the compiler's `resolve()` (via `with_resolved_feature_sets`)
        // treated an all-absent referenced catalog record as genuinely resolved/present,
        // while the patch axis's `RawSubBlock::from_sub_block` additionally gated presence
        // on "does the entry have any field set", collapsing the very same all-`None`
        // record to selector-only. Task 7 removed that patch-only gate (a non-empty
        // `dl_features` vec IS presence, full stop), so the two axes now agree.
        let catalogs = FeatureCatalogs::new(vec![DlFeatureSource::default()], vec![]);
        let source = NrSourceSubBlock {
            kind: SubBlockKind::Nr,
            band: 78,
            dl_bw_class: Some(1),
            dl_feature: vec![1],
            ..Default::default()
        };
        let resolved = source.resolve(&catalogs).unwrap();
        assert_eq!(
            resolved.dl_feature_set(),
            Some(ShannonFeatureSetDlPerCcNr::default())
        );
        assert_eq!(resolved.dl_cc_ids, None);
        assert_ne!(
            RawSubBlockKey::from(&resolved),
            RawSubBlockKey::from(&RawSubBlock {
                kind: SubBlockKind::Nr,
                band: 78,
                ..Default::default()
            })
        );

        let patch = RawSubBlock::from_sub_block(&SubBlock {
            band: "n78".into(),
            dl_feature_per_cc_ids: Some(vec![7]),
            dl_features: vec![ShannonFeatureSetDlPerCcNr::default()],
            ..Default::default()
        });
        assert_eq!(
            patch.dl_feature_set(),
            Some(ShannonFeatureSetDlPerCcNr::default())
        );
        // The raw selector bytes are still carried on the struct (`from_sub_block` never
        // clears `dl_cc_ids` itself), but identity/writer logic ignore it once
        // `dl_feature_set_is_present()` is true — see `RawSubBlockKey::from` and
        // `patch::format::sub_block_to_node`.
        assert_eq!(patch.dl_cc_ids, Some(vec![7]));
        assert_eq!(
            RawSubBlockKey::from(&patch),
            RawSubBlockKey::from(&RawSubBlock {
                kind: SubBlockKind::Nr,
                band: 78,
                dl_features: vec![ShannonFeatureSetDlPerCcNr::default()],
                ..Default::default()
            }),
            "raw dl_cc_ids must be masked from identity once the feature set is present"
        );
    }

    #[test]
    fn dl_feature_identity_orders_absence_before_explicit_zero() {
        let absent = DlFeatureSource::default();
        let zero = DlFeatureSource {
            max_scs: Some(0),
            ..Default::default()
        };
        assert!(absent < zero);
        assert_ne!(absent, zero);
    }

    #[test]
    fn ul_feature_identity_orders_absence_before_explicit_false() {
        let absent = UlFeatureSource::default();
        let explicit_false = UlFeatureSource {
            bw_90mhz_supported: Some(false),
            ..Default::default()
        };
        assert!(absent < explicit_false);
        assert_ne!(absent, explicit_false);
    }

    #[test]
    fn lte_component_rejects_even_an_all_absent_resolved_nr_feature() {
        let catalogs = FeatureCatalogs::new(vec![DlFeatureSource::default()], vec![]);
        let error = NrSourceSubBlock {
            kind: SubBlockKind::Lte,
            band: 1,
            dl_feature: vec![1],
            ..Default::default()
        }
        .resolve(&catalogs)
        .unwrap_err()
        .to_string();
        assert!(
            error.contains("LTE component B1 carries NR-only fields"),
            "{error}"
        );
    }
}
