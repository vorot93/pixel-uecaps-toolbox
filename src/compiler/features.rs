use std::collections::BTreeSet;

use anyhow::{Context, ensure};

use crate::{
    proto::{
        ShannonFeatureSetDlPerCcNr, ShannonFeatureSetUlPerCcNr,
        combo_group::combo::SubBlock as ProtoSubBlock,
    },
    raw_nr::{
        LteDirection, NrDirection, PerCc, RawLteSubBlock, RawNrPayload, RawNrSubBlock, RawSubBlock,
        SubBlockKind, cc_count,
    },
    report::combos::band_label_for,
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

/// One sub-block as the KDL source spells it: proto field 6/7 as 1-based *references* into
/// `nr.kdl`'s global feature catalogs rather than resolved values, and no raw selector bytes
/// at all.
///
/// Exactly one representation per direction is populated, discriminated by [`kind`](Self::kind):
/// an `lte` node carries the scalar `dl_feature_index` (proto 4/5, `parseLteFeatureIndex`) and
/// never a catalog list; an `nr` node carries the per-CC `dl_feature` list and never an index
/// (NR derives proto 4/5 from its feature set on provision). The raw all-zero placeholder selector
/// is deliberately NOT a field: it is a pure function of `kind` + `bw_class`, so KDL omits it
/// and [`resolve`](Self::resolve) materializes it via [`placeholder_ids`]. Keeping it here
/// would let the type express a component with a catalog reference *and* a raw selector for
/// the same direction — a state the source format has no way to spell.
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

/// Every distinct DL/UL feature record actually referenced by a carrier's payloads, scanning
/// EVERY per-CC entry in `dl_features`/`ul_features` (not just CC0) so a non-uniform multi-CC
/// sub-block's second-and-later CCs are represented too. Paired with `reconstruct_sub_block`,
/// which emits one selector byte per entry — both must stay in lockstep or
/// `verify_compact_feature_list` (compiler/nr.rs) fails "unused/missing feature record" (a
/// per-CC1+ record referenced by reconstruct but absent from the plan) or leaves a plan record
/// unreferenced (present here but never emitted).
struct UsedFeatures {
    dl: BTreeSet<DlFeatureSource>,
    ul: BTreeSet<UlFeatureSource>,
}

impl UsedFeatures {
    fn scan(payloads: &[&RawNrPayload]) -> Self {
        let mut dl = BTreeSet::new();
        let mut ul = BTreeSet::new();
        for payload in payloads {
            for component in &payload.sub_blocks {
                for feature in component.dl_features() {
                    dl.insert(DlFeatureSource::from(feature));
                }
                for feature in component.ul_features() {
                    ul.insert(UlFeatureSource::from(feature));
                }
            }
        }
        Self { dl, ul }
    }
}

/// The global catalog's records that are `used`, in the catalog's own canonical order (not
/// `used`'s `BTreeSet` order) — this is what makes a local plan's indices stable across a
/// corpus that shares one global catalog. Errors if `used` names a record absent from the
/// catalog (a payload referencing a feature the catalog never saw), or if the filtered set
/// would overflow the local plan's 1-based `u8` index space (255 records).
fn local_catalog<T: Ord + Clone>(
    catalog: &[T],
    used: &BTreeSet<T>,
    direction: &str,
    basename: &str,
    sku: &str,
) -> anyhow::Result<Vec<T>> {
    let filtered: Vec<T> = catalog
        .iter()
        .filter(|feature| used.contains(*feature))
        .cloned()
        .collect();
    ensure!(
        filtered.len() == used.len(),
        "{basename} ({sku}) uses a {direction} feature absent from the global catalog"
    );
    ensure!(
        filtered.len() <= usize::from(u8::MAX),
        "{basename} ({sku}) uses {} distinct {direction} feature records; local limit is 255",
        filtered.len()
    );
    Ok(filtered)
}

impl LocalFeaturePlan {
    pub(crate) fn new(
        catalogs: &FeatureCatalogs,
        payloads: &[&RawNrPayload],
        basename: &str,
        sku: &str,
    ) -> anyhow::Result<Self> {
        let used = UsedFeatures::scan(payloads);
        let dl_source = local_catalog(&catalogs.dl, &used.dl, "DL", basename, sku)?;
        let ul_source = local_catalog(&catalogs.ul, &used.ul, "UL", basename, sku)?;

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
        // finds it directly. Falls back to the raw selector bytes when the sub-block
        // carries no resolved feature sets at all.
        let dl_feature_per_cc_ids = if component.dl_features().is_empty() {
            component.dl_selector().map(<[u8]>::to_vec)
        } else {
            Some(
                component
                    .dl_features()
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
        let ul_feature_per_cc_ids = if component.ul_features().is_empty() {
            component.ul_selector().map(<[u8]>::to_vec)
        } else {
            Some(
                component
                    .ul_features()
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
            dl_bw_class: component.dl_bw_class(),
            ul_bw_class: component.ul_bw_class(),
            dl_feature_index: component.dl_feature_index(),
            ul_feature_index: component.ul_feature_index(),
            dl_feature_per_cc_ids,
            ul_feature_per_cc_ids,
            srstxswitch: component.srs_tx_switch(),
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
                for feature in component.dl_features() {
                    dl.insert(DlFeatureSource::from(feature));
                }
                for feature in component.ul_features() {
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
            .dl_features()
            .iter()
            .map(|feature| {
                self.dl
                    .binary_search(&DlFeatureSource::from(feature))
                    .expect("canonical DL catalog contains every resolved component")
                    + 1
            })
            .collect();
        let ul_feature: Vec<usize> = component
            .ul_features()
            .iter()
            .map(|feature| {
                self.ul
                    .binary_search(&UlFeatureSource::from(feature))
                    .expect("canonical UL catalog contains every resolved component")
                    + 1
            })
            .collect();
        NrSourceSubBlock {
            kind: component.kind(),
            band: component.band(),
            dl_bw_class: component.dl_bw_class(),
            ul_bw_class: component.ul_bw_class(),
            dl_feature_index: component.source_dl_feature_index(),
            ul_feature_index: component.source_ul_feature_index(),
            // `dl_cc_ids`/`ul_cc_ids` are intentionally dropped: an unresolved direction only
            // ever carries the all-zero placeholder, which the source omits and `resolve`
            // re-derives from `bw_class`.
            dl_feature,
            ul_feature,
            srs_tx_switch: component.srs_tx_switch(),
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

/// The all-zero placeholder selector a direction carries when it references no catalog
/// record: `bw_class` supplies the CC count to fill with zero bytes. KDL omits these bytes
/// because they are fully derivable, so the provision path materializes them here instead of
/// storing them on [`NrSourceSubBlock`]. `None` when there is no bandwidth class to derive a
/// count from — an absent DL class, or UL disabled (the caller filters `ul_bw_class == 0`),
/// both of which mean the direction carries no proto field 6/7 at all.
fn placeholder_ids(kind: SubBlockKind, bw_class: Option<i32>) -> anyhow::Result<Option<Vec<u8>>> {
    match bw_class {
        Some(bw) => Ok(Some(vec![0u8; cc_count(kind, bw)?])),
        None => Ok(None),
    }
}

/// Builds the `lte`-kind resolved component. The source model is flat across both kinds, so
/// this is where an `lte` node carrying NR-only data (a resolved feature, or an SRS-TX-switch
/// value) is rejected instead of silently truncated — `RawLteSubBlock` has nowhere to put it.
/// Replaces the old `RawSubBlock::validate` "carries NR-only fields" check, which could only
/// run after the fields had already been stored.
fn resolve_lte(
    cc: &NrSourceSubBlock,
    dl: &[ShannonFeatureSetDlPerCcNr],
    ul: &[ShannonFeatureSetUlPerCcNr],
    ul_bw_class: Option<i32>,
) -> anyhow::Result<RawSubBlock> {
    ensure!(
        dl.is_empty() && ul.is_empty() && cc.srs_tx_switch.is_none(),
        "LTE component {} carries NR-only fields",
        band_label_for(SubBlockKind::Lte, cc.band)
    );
    Ok(RawLteSubBlock {
        band: cc.band,
        dl: LteDirection {
            bw_class: cc.dl_bw_class,
            feature_index: cc.dl_feature_index,
            selector: placeholder_ids(cc.kind, cc.dl_bw_class)?,
        },
        ul: LteDirection {
            bw_class: cc.ul_bw_class,
            feature_index: cc.ul_feature_index,
            // The stored `bw_class` field above keeps the raw value; only the placeholder
            // derivation needs the disabled-aware `ul_bw_class` (`Some(0)` -> `None`).
            selector: placeholder_ids(cc.kind, ul_bw_class)?,
        },
    }
    .into())
}

/// Builds the `nr`-kind resolved component: NR never stores a source feature index (it is
/// re-derived from the feature set on provision, downstream of this call), so a `dl`/`ul`
/// index present here means the source carries data only `lte` nodes should.
fn resolve_nr(
    cc: &NrSourceSubBlock,
    dl: Vec<ShannonFeatureSetDlPerCcNr>,
    ul: Vec<ShannonFeatureSetUlPerCcNr>,
    ul_bw_class: Option<i32>,
) -> anyhow::Result<RawSubBlock> {
    ensure!(
        cc.dl_feature_index.is_none() && cc.ul_feature_index.is_none(),
        "NR component {} stores a feature index; NR derives it from its feature set",
        band_label_for(SubBlockKind::Nr, cc.band)
    );
    Ok(RawNrSubBlock {
        band: cc.band,
        dl: NrDirection {
            bw_class: cc.dl_bw_class,
            features: nr_per_cc(dl, cc.dl_bw_class)?,
        },
        ul: NrDirection {
            // The stored `bw_class` field keeps the raw value; only the placeholder
            // derivation below needs the disabled-aware `ul_bw_class`.
            bw_class: cc.ul_bw_class,
            features: nr_per_cc(ul, ul_bw_class)?,
        },
        srs_tx_switch: cc.srs_tx_switch,
    }
    .into())
}

impl NrSourceSubBlock {
    pub(crate) fn resolve(&self, catalogs: &FeatureCatalogs) -> anyhow::Result<RawSubBlock> {
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
        // `ul_bw_class == 0` means UL disabled: no per-CC data at all, hence no placeholder
        // (DL has no such "disabled" class).
        let ul_bw_class = self.ul_bw_class.filter(|&bw| bw >= 1);
        let component: RawSubBlock = match self.kind {
            SubBlockKind::Lte => resolve_lte(self, &dl, &ul, ul_bw_class)?,
            SubBlockKind::Nr => resolve_nr(self, dl, ul, ul_bw_class)?,
        };
        component.validate()?;
        Ok(component)
    }
}

/// Resolved catalog references become values; a direction that references nothing
/// re-materializes the placeholder the source omitted (see [`placeholder_ids`]).
fn nr_per_cc<T: Copy>(features: Vec<T>, bw_class: Option<i32>) -> anyhow::Result<Option<PerCc<T>>> {
    if features.is_empty() {
        Ok(placeholder_ids(SubBlockKind::Nr, bw_class)?.map(PerCc::Selector))
    } else {
        Ok(Some(PerCc::Resolved(features)))
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
        // CC after the first). This exercises the compiler's `LocalFeaturePlan`, which
        // resolves each entry against a pre-scanned local catalog via `binary_search`.
        let a = ShannonFeatureSetDlPerCcNr {
            max_bw: Some(40),
            ..Default::default()
        };
        let b = ShannonFeatureSetDlPerCcNr {
            max_bw: Some(100),
            ..Default::default()
        };
        let sb: RawSubBlock = RawNrSubBlock {
            band: 48,
            dl: NrDirection::with_features(2, vec![a, b]),
            ..Default::default()
        }
        .into();
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
        let component: RawSubBlock = RawNrSubBlock {
            band: 78,
            dl: NrDirection::with_features(1, vec![ShannonFeatureSetDlPerCcNr::from(&high)]),
            ..Default::default()
        }
        .into();
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
        let component: RawSubBlock = RawNrSubBlock {
            band: 78,
            ul: NrDirection::with_features(1, vec![ShannonFeatureSetUlPerCcNr::from(&high)]),
            ..Default::default()
        }
        .into();
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
        let component: RawSubBlock = RawNrSubBlock {
            band: 78,
            dl: NrDirection::with_features(1, vec![ShannonFeatureSetDlPerCcNr::default()]),
            ..Default::default()
        }
        .into();
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
        let component: RawSubBlock = RawNrSubBlock {
            band: 78,
            ul: NrDirection::with_features(1, vec![ShannonFeatureSetUlPerCcNr::default()]),
            ..Default::default()
        }
        .into();
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
    fn local_plan_passes_through_the_all_zero_placeholder_selector() {
        // The all-zero placeholder is the ONLY unresolved selector that can reach
        // generation: decompose (`RawSubBlock::from_proto_sub_block`, via
        // `resolve_or_placeholder`) fails closed on a non-placeholder one. It must
        // survive `reconstruct_sub_block` verbatim -- LTE sub-blocks inside nr.kdl combos
        // and UL-disabled NR sub-blocks depend on this for byte-exact round-trip.
        let sb: RawSubBlock = RawLteSubBlock {
            band: 66,
            dl: LteDirection {
                // cc_count(Lte, 1) == 1, matching the 1-byte selector
                bw_class: Some(1),
                feature_index: None,
                selector: Some(vec![0]),
            },
            ul: LteDirection {
                bw_class: Some(1),
                ..Default::default()
            },
        }
        .into();
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
            out.dl_feature_per_cc_ids,
            Some(vec![0]),
            "the all-zero placeholder must survive generation byte-for-byte"
        );

        // The other half of the claim above: a UL-disabled NR sub-block carries the same
        // all-zero placeholder on the UL side (`validate_ul_cc_count` skips the length check
        // entirely when `ul_bw_class == 0`), and it must survive generation identically.
        let nr_sb: RawSubBlock = RawNrSubBlock {
            band: 78,
            // valid NR class; DL is not exercised by this assertion
            dl: NrDirection::bare(Some(1)),
            // UL disabled, but still carrying the placeholder
            ul: NrDirection::with_selector(0, vec![0]),
            ..Default::default()
        }
        .into();
        let nr_payload = RawNrPayload {
            power_class: None,
            bcs_nr: None,
            bcs_intra_endc: None,
            bcs_eutra: None,
            intra_band_en_dc_support: None,
            sub_blocks: vec![nr_sb.clone()],
        };
        let nr_catalogs = FeatureCatalogs::from_payloads([&nr_payload]);
        let nr_plan =
            LocalFeaturePlan::new(&nr_catalogs, &[&nr_payload], "A.binarypb", "legacy").unwrap();

        let nr_out = nr_plan.reconstruct_sub_block(&nr_sb).unwrap();

        assert_eq!(
            nr_out.ul_feature_per_cc_ids,
            Some(vec![0]),
            "the UL-disabled NR placeholder must survive generation byte-for-byte"
        );
    }

    #[test]
    fn referenced_all_absent_record_is_resolved_on_the_ingest_axis() {
        // Before Task 7, the compiler's `resolve()` (via `with_resolved_feature_sets`)
        // treated an all-absent referenced catalog record as genuinely resolved/present,
        // while the flat DTO ingest path's `RawSubBlock::from_sub_block` additionally gated
        // presence on "does the entry have any field set", collapsing the very same all-`None`
        // record to selector-only. Task 7 removed that ingest-only gate (a non-empty
        // `dl_features` vec IS presence, full stop), so both ingest paths now agree.
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
            resolved.dl_features().first().copied(),
            Some(ShannonFeatureSetDlPerCcNr::default())
        );
        assert_eq!(resolved.dl_selector(), None);
        assert_ne!(
            RawSubBlockKey::from(&resolved),
            RawSubBlockKey::from(&RawSubBlock::from(RawNrSubBlock {
                band: 78,
                ..Default::default()
            }))
        );

        let ingested = RawSubBlock::from_sub_block(&SubBlock {
            band: "n78".into(),
            dl_feature_per_cc_ids: Some(vec![7]),
            dl_features: vec![ShannonFeatureSetDlPerCcNr::default()],
            ..Default::default()
        });
        assert_eq!(
            ingested.dl_features().first().copied(),
            Some(ShannonFeatureSetDlPerCcNr::default())
        );
        // The flat DTO offered both a selector and resolved values; `PerCc` keeps only the
        // resolution, so the bytes are gone rather than merely masked from identity (they
        // used to survive on the struct and be filtered out by `RawSubBlockKey::from`).
        assert_eq!(ingested.dl_selector(), None);
        assert_eq!(
            RawSubBlockKey::from(&ingested),
            RawSubBlockKey::from(&RawSubBlock::from(RawNrSubBlock {
                band: 78,
                // The DTO under test carries no bandwidth class, so neither does this.
                dl: NrDirection {
                    bw_class: None,
                    features: Some(PerCc::Resolved(vec![ShannonFeatureSetDlPerCcNr::default()])),
                },
                ..Default::default()
            })),
            "a resolved direction has no selector left to disturb identity"
        );
    }

    #[test]
    fn resolve_derives_the_omitted_placeholder() {
        // The source format spells no raw selector bytes at all, so `resolve` is the single
        // place that puts proto field 6/7 back for a direction referencing no catalog
        // record: the all-zero placeholder, `cc_count(kind, bw_class)` bytes wide.
        let catalogs = FeatureCatalogs::new(vec![DlFeatureSource::default()], vec![]);

        // LTE never carries per-CC references; `cc_count(Lte, 1) == 1`.
        let lte = NrSourceSubBlock {
            kind: SubBlockKind::Lte,
            band: 66,
            dl_bw_class: Some(1),
            dl_feature_index: Some(3),
            ..Default::default()
        }
        .resolve(&catalogs)
        .unwrap();
        assert_eq!(lte.dl_selector(), Some([0].as_slice()));
        assert_eq!(
            lte.ul_selector(),
            None,
            "no ul_bw_class means no UL data at all"
        );

        // NR class 2 aggregates 2 CCs, so the placeholder is two bytes wide. `ul_bw_class`
        // 0 is UL-disabled — the one case that must stay absent rather than placeholdered.
        let nr = NrSourceSubBlock {
            kind: SubBlockKind::Nr,
            band: 48,
            dl_bw_class: Some(2),
            ul_bw_class: Some(0),
            ..Default::default()
        }
        .resolve(&catalogs)
        .unwrap();
        assert_eq!(nr.dl_selector(), Some([0, 0].as_slice()));
        assert_eq!(nr.ul_selector(), None, "ul_bw_class 0 means UL disabled");

        // A direction that does resolve carries values and no placeholder.
        let resolved = NrSourceSubBlock {
            kind: SubBlockKind::Nr,
            band: 78,
            dl_bw_class: Some(1),
            dl_feature: vec![1],
            ..Default::default()
        }
        .resolve(&catalogs)
        .unwrap();
        assert_eq!(resolved.dl_selector(), None);
        assert_eq!(resolved.dl_features().len(), 1);
    }

    #[test]
    fn resolve_rejects_a_bw_class_with_no_known_cc_count() {
        // Fail closed rather than mis-derive a placeholder length: the derivation is only
        // as safe as `cc_count`, so an unobserved class must error here too.
        let error = NrSourceSubBlock {
            kind: SubBlockKind::Nr,
            band: 78,
            dl_bw_class: Some(99),
            ..Default::default()
        }
        .resolve(&FeatureCatalogs::default())
        .unwrap_err()
        .to_string();
        assert!(error.contains("unknown Nr bw_class 99"), "{error}");
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
