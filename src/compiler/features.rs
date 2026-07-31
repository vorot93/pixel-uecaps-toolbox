use std::collections::BTreeSet;

use anyhow::{Context, ensure};

use crate::{
    proto::{ShannonFeatureSetDlPerCcNr, ShannonFeatureSetUlPerCcNr, SubBlock as ProtoSubBlock},
    raw_nr::{
        Direction, LteDirection, NrDirection, PerCc, RawLteSubBlock, RawNrPayload, RawNrSubBlock,
        RawSubBlock, SubBlockKind, cc_count,
    },
};

/// One sub-block as the KDL source spells it: proto field 6/7 as 1-based *references* into
/// the source document's global feature catalogs rather than resolved values, and no raw
/// selector bytes at all.
///
/// A closed sum over the two node kinds, mirroring [`RawSubBlock`] one layer down. The two
/// kinds carry genuinely different data — an `lte` node has the scalar `dl_feature_index`
/// (proto 4/5, `parseLteFeatureIndex`) and never a catalog list; an `nr` node has the per-CC
/// `dl_feature` list, never an index (NR derives proto 4/5 from its feature set on provision),
/// and is the only kind with `srs_tx_switch`. As a flat kind-tagged struct this needed two
/// runtime `ensure!`s to reject the illegal mixtures; neither variant can express them.
///
/// The raw all-zero placeholder selector is deliberately absent from both variants: it is a
/// pure function of kind + `bw_class`, so KDL omits it and [`resolve`](Self::resolve)
/// materializes it via [`placeholder_ids`]. Keeping it would let a component hold a catalog
/// reference *and* a raw selector for one direction — a state the source format cannot spell.
#[derive(Clone, Debug)]
pub(crate) enum NrSourceSubBlock {
    Lte(SourceLteSubBlock),
    Nr(SourceNrSubBlock),
}

/// The `lte` half of [`NrSourceSubBlock`] — an E-UTRA component inside an EN-DC combo.
#[derive(Clone, Debug, Default)]
pub(crate) struct SourceLteSubBlock {
    pub(crate) band: u16,
    pub(crate) dl_bw_class: Option<u8>,
    pub(crate) ul_bw_class: Option<u8>,
    pub(crate) dl_feature: Option<u16>,
    pub(crate) ul_feature: Option<u16>,
}

/// The `nr` half of [`NrSourceSubBlock`].
#[derive(Clone, Debug, Default)]
pub(crate) struct SourceNrSubBlock {
    pub(crate) band: u16,
    pub(crate) dl_bw_class: Option<u8>,
    pub(crate) ul_bw_class: Option<u8>,
    pub(crate) dl_feature: Vec<usize>,
    pub(crate) ul_feature: Vec<usize>,
    pub(crate) srs_tx_switch: Option<i32>,
}

impl From<SourceLteSubBlock> for NrSourceSubBlock {
    fn from(cc: SourceLteSubBlock) -> Self {
        Self::Lte(cc)
    }
}

impl From<SourceNrSubBlock> for NrSourceSubBlock {
    fn from(cc: SourceNrSubBlock) -> Self {
        Self::Nr(cc)
    }
}

/// The shared read surface for code that treats both kinds alike (the KDL emitter, and
/// the NR band-collection pass). Anything that *builds* a sub-block matches on the variant.
impl NrSourceSubBlock {
    pub(crate) const fn kind(&self) -> SubBlockKind {
        match self {
            Self::Lte(_) => SubBlockKind::Lte,
            Self::Nr(_) => SubBlockKind::Nr,
        }
    }

    pub(crate) const fn band(&self) -> u16 {
        match self {
            Self::Lte(cc) => cc.band,
            Self::Nr(cc) => cc.band,
        }
    }

    pub(crate) const fn dl_bw_class(&self) -> Option<u8> {
        match self {
            Self::Lte(cc) => cc.dl_bw_class,
            Self::Nr(cc) => cc.dl_bw_class,
        }
    }

    pub(crate) const fn ul_bw_class(&self) -> Option<u8> {
        match self {
            Self::Lte(cc) => cc.ul_bw_class,
            Self::Nr(cc) => cc.ul_bw_class,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct FeatureCatalogs {
    pub(crate) dl: Vec<ShannonFeatureSetDlPerCcNr>,
    pub(crate) ul: Vec<ShannonFeatureSetUlPerCcNr>,
}

#[derive(Debug)]
pub(crate) struct LocalFeaturePlan {
    dl_source: Vec<ShannonFeatureSetDlPerCcNr>,
    ul_source: Vec<ShannonFeatureSetUlPerCcNr>,
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
    dl: BTreeSet<ShannonFeatureSetDlPerCcNr>,
    ul: BTreeSet<ShannonFeatureSetUlPerCcNr>,
}

impl UsedFeatures {
    fn scan<'a>(payloads: impl IntoIterator<Item = &'a RawNrPayload>) -> Self {
        let mut dl = BTreeSet::new();
        let mut ul = BTreeSet::new();
        for payload in payloads {
            for component in &payload.sub_blocks {
                for feature in component.dl_features() {
                    dl.insert(*feature);
                }
                for feature in component.ul_features() {
                    ul.insert(*feature);
                }
            }
        }
        Self { dl, ul }
    }
}

/// The 1-based index of each resolved feature record in `catalog`, one per CC.
///
/// The `expect` is sound by construction at every call site: a catalog is only ever built by
/// [`UsedFeatures::scan`] over the very payloads whose records are looked up here, and the
/// local plan is that scan filtered down. `which` names the catalog in the panic message.
fn catalog_indices<'a, T: Ord>(
    features: &'a [T],
    catalog: &'a [T],
    which: &'static str,
) -> impl Iterator<Item = usize> + 'a {
    features.iter().map(move |feature| {
        catalog
            .binary_search(feature)
            .unwrap_or_else(|_| panic!("{which} contains every resolved component"))
            + 1
    })
}

/// The global catalog's records that are `used`, in the catalog's own canonical order (not
/// `used`'s `BTreeSet` order) — this is what makes a local plan's indices stable across a
/// corpus that shares one global catalog. Purely a filter: validating the result (an absent
/// record, or an over-255 local plan) is [`LocalFeaturePlan::new`]'s job, so that both
/// directions' filters resolve before either direction's checks run — see the ordering note
/// there.
fn local_catalog<T: Ord + Clone>(catalog: &[T], used: &BTreeSet<T>) -> Vec<T> {
    catalog
        .iter()
        .filter(|feature| used.contains(*feature))
        .cloned()
        .collect()
}

impl LocalFeaturePlan {
    pub(crate) fn new(
        catalogs: &FeatureCatalogs,
        payloads: &[&RawNrPayload],
        basename: &str,
        sku: &str,
    ) -> anyhow::Result<Self> {
        let used = UsedFeatures::scan(payloads.iter().copied());
        let dl_source = local_catalog(&catalogs.dl, &used.dl);
        let ul_source = local_catalog(&catalogs.ul, &used.ul);

        // Check order is DL-absent, UL-absent, DL-limit, UL-limit. Both directions' filtered
        // vectors resolve above before any check runs, so a DL-only over-limit input can no
        // longer shadow a simultaneous UL-absent one the way running DL's whole pair of checks
        // before UL's first check would.
        //
        // "absent" = a payload referenced a record the global catalog never saw; "limit" =
        // the local plan would overflow its 1-based `u8` index space.
        for (direction, local, used) in [
            (Direction::Dl, dl_source.len(), used.dl.len()),
            (Direction::Ul, ul_source.len(), used.ul.len()),
        ] {
            ensure!(
                local == used,
                "{basename} ({sku}) uses a {direction} feature absent from the global catalog"
            );
        }
        for (direction, local) in [
            (Direction::Dl, dl_source.len()),
            (Direction::Ul, ul_source.len()),
        ] {
            ensure!(
                local <= usize::from(u8::MAX),
                "{basename} ({sku}) uses {local} distinct {direction} feature records; local limit is 255"
            );
        }

        let dl = dl_source.to_vec();
        let ul = ul_source.to_vec();
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
        // `LocalFeaturePlan::new` already capped each plan at 255 records, so the narrowing
        // cannot fail.
        let selector_byte = |index: usize| u8::try_from(index).expect("local plan is at most 255");
        let dl_feature_per_cc_ids = if component.dl_features().is_empty() {
            component.dl_selector().map(<[u8]>::to_vec)
        } else {
            Some(
                catalog_indices(component.dl_features(), &self.dl_source, "local DL plan")
                    .map(selector_byte)
                    .collect(),
            )
        };
        let ul_feature_per_cc_ids = if component.ul_features().is_empty() {
            component.ul_selector().map(<[u8]>::to_vec)
        } else {
            Some(
                catalog_indices(component.ul_features(), &self.ul_source, "local UL plan")
                    .map(selector_byte)
                    .collect(),
            )
        };
        Ok(ProtoSubBlock {
            band: component.raw_band(),
            dl_bw_class: component.dl_bw_class().map(i32::from),
            ul_bw_class: component.ul_bw_class().map(i32::from),
            dl_feature_index: component.dl_feature_index(),
            ul_feature_index: component.ul_feature_index(),
            dl_feature_per_cc_ids,
            ul_feature_per_cc_ids,
            srstxswitch: component.srs_tx_switch(),
        })
    }
}

impl FeatureCatalogs {
    pub(crate) fn new(
        dl: Vec<ShannonFeatureSetDlPerCcNr>,
        ul: Vec<ShannonFeatureSetUlPerCcNr>,
    ) -> Self {
        Self { dl, ul }
    }

    /// The global catalog: every per-CC feature (not just CC0) any payload references, in
    /// sorted order. Shares [`UsedFeatures::scan`] with the per-carrier local plan, so the
    /// two cannot disagree about what "referenced" means.
    pub(crate) fn from_payloads<'a>(payloads: impl IntoIterator<Item = &'a RawNrPayload>) -> Self {
        let used = UsedFeatures::scan(payloads);
        Self {
            dl: used.dl.into_iter().collect(),
            ul: used.ul.into_iter().collect(),
        }
    }

    /// Per-CC global-catalog references: each entry in `dl_features`/`ul_features` maps to
    /// its own 1-based index into the canonical (global) catalog — one `usize` per CC.
    pub(crate) fn source_sub_block(&self, component: &RawSubBlock) -> NrSourceSubBlock {
        // The raw selectors are intentionally dropped: an unresolved direction only ever
        // carries the all-zero placeholder, which the source omits and `resolve` re-derives
        // from `bw_class`.
        match component {
            RawSubBlock::Lte(cc) => SourceLteSubBlock {
                band: cc.band,
                dl_bw_class: cc.dl.bw_class,
                ul_bw_class: cc.ul.bw_class,
                dl_feature: cc.dl.feature_index,
                ul_feature: cc.ul.feature_index,
            }
            .into(),
            RawSubBlock::Nr(cc) => SourceNrSubBlock {
                band: cc.band,
                dl_bw_class: cc.dl.bw_class,
                ul_bw_class: cc.ul.bw_class,
                dl_feature: catalog_indices(
                    component.dl_features(),
                    &self.dl,
                    "canonical DL catalog",
                )
                .collect(),
                ul_feature: catalog_indices(
                    component.ul_features(),
                    &self.ul,
                    "canonical UL catalog",
                )
                .collect(),
                srs_tx_switch: cc.srs_tx_switch,
            }
            .into(),
        }
    }
}

fn resolve_index<T: Clone>(index: usize, records: &[T], direction: Direction) -> anyhow::Result<T> {
    let direction = direction.lowercase();
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
fn placeholder_ids(kind: SubBlockKind, bw_class: Option<u8>) -> anyhow::Result<Option<Vec<u8>>> {
    match bw_class {
        Some(bw) => Ok(Some(vec![0u8; cc_count(kind, bw)?])),
        None => Ok(None),
    }
}

/// Builds the `lte`-kind resolved component. An `lte` node has no catalog references and no
/// SRS-TX-switch to carry, so the old "LTE component carries NR-only fields" check is gone:
/// [`SourceLteSubBlock`] has no field to put them in.
fn resolve_lte(cc: &SourceLteSubBlock, ul_bw_class: Option<u8>) -> anyhow::Result<RawSubBlock> {
    Ok(RawLteSubBlock {
        band: cc.band,
        dl: LteDirection {
            bw_class: cc.dl_bw_class,
            feature_index: cc.dl_feature,
            selector: placeholder_ids(SubBlockKind::Lte, cc.dl_bw_class)?,
        },
        ul: LteDirection {
            bw_class: cc.ul_bw_class,
            feature_index: cc.ul_feature,
            // The stored `bw_class` field above keeps the raw value; only the placeholder
            // derivation needs the disabled-aware `ul_bw_class` (`Some(0)` -> `None`).
            selector: placeholder_ids(SubBlockKind::Lte, ul_bw_class)?,
        },
    }
    .into())
}

/// Builds the `nr`-kind resolved component. NR stores no source feature index — it is
/// re-derived from the feature set on provision — and [`SourceNrSubBlock`] has no field for
/// one, so the old "NR component stores a feature index" check is gone too.
fn resolve_nr(
    cc: &SourceNrSubBlock,
    dl: Vec<ShannonFeatureSetDlPerCcNr>,
    ul: Vec<ShannonFeatureSetUlPerCcNr>,
    ul_bw_class: Option<u8>,
) -> anyhow::Result<RawSubBlock> {
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
        // `ul_bw_class == 0` means UL disabled: no per-CC data at all, hence no placeholder
        // (DL has no such "disabled" class).
        let ul_bw_class = self.ul_bw_class().filter(|&bw| bw >= 1);
        let component = match self {
            Self::Lte(cc) => resolve_lte(cc, ul_bw_class)?,
            Self::Nr(cc) => {
                let dl = cc
                    .dl_feature
                    .iter()
                    .map(|&index| resolve_index(index, &catalogs.dl, Direction::Dl))
                    .collect::<anyhow::Result<Vec<_>>>()?;
                let ul = cc
                    .ul_feature
                    .iter()
                    .map(|&index| resolve_index(index, &catalogs.ul, Direction::Ul))
                    .collect::<anyhow::Result<Vec<_>>>()?;
                resolve_nr(cc, dl, ul, ul_bw_class)?
            }
        };
        component.validate()?;
        Ok(component)
    }
}

/// Resolved catalog references become values; a direction that references nothing
/// re-materializes the placeholder the source omitted (see [`placeholder_ids`]).
fn nr_per_cc<T: Copy>(features: Vec<T>, bw_class: Option<u8>) -> anyhow::Result<Option<PerCc<T>>> {
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
        raw_nr::{RawNrPayload, RawSubBlock, RawSubBlockKey},
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
        let low = ShannonFeatureSetDlPerCcNr {
            max_scs: Some(1),
            ..Default::default()
        };
        let high = ShannonFeatureSetDlPerCcNr {
            max_scs: Some(3),
            ..Default::default()
        };
        let catalogs = FeatureCatalogs::new(vec![low, high], vec![]);
        let component: RawSubBlock = RawNrSubBlock {
            band: 78,
            dl: NrDirection::with_features(1, vec![high]),
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
        let low = ShannonFeatureSetUlPerCcNr {
            max_scs: Some(1),
            ..Default::default()
        };
        let high = ShannonFeatureSetUlPerCcNr {
            max_scs: Some(3),
            ..Default::default()
        };
        let catalogs = FeatureCatalogs::new(vec![], vec![low, high]);
        let component: RawSubBlock = RawNrSubBlock {
            band: 78,
            ul: NrDirection::with_features(1, vec![high]),
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
        assert_eq!(plan.ul_source, vec![high]);
        assert_eq!(plan.ul, vec![high]);
        assert!(plan.dl_source.is_empty());
        assert!(plan.dl.is_empty());
        let reconstructed = plan.reconstruct_sub_block(&component).unwrap();
        assert_eq!(reconstructed.ul_feature_per_cc_ids, Some(vec![1]));
        assert_eq!(reconstructed.dl_feature_per_cc_ids, None);
    }

    #[test]
    fn local_plan_emits_a_referenced_all_absent_dl_record() {
        let catalogs = FeatureCatalogs::new(vec![ShannonFeatureSetDlPerCcNr::default()], vec![]);
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
        let catalogs = FeatureCatalogs::new(vec![], vec![ShannonFeatureSetUlPerCcNr::default()]);
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
    fn local_plan_new_reports_ul_absent_before_dl_over_local_limit() {
        // Pins the original four-check order (DL-absent, UL-absent, DL-limit, UL-limit). A
        // `local_catalog` helper that bundled *one direction's* absent-check and limit-check
        // together would silently reorder this to DL-absent, DL-limit, UL-absent, UL-limit.
        // This input distinguishes the two: DL passes its absent-check but overflows the
        // 255-record local limit, while UL simultaneously references a feature absent from
        // the global catalog. The original order reports UL-absent; the bundled-per-direction
        // order reports DL-limit first and never reaches the UL check at all.
        let dl_sources: Vec<ShannonFeatureSetDlPerCcNr> = (1..=256)
            .map(|max_scs| ShannonFeatureSetDlPerCcNr {
                max_scs: Some(max_scs),
                ..Default::default()
            })
            .collect();
        let dl_features: Vec<ShannonFeatureSetDlPerCcNr> = dl_sources.to_vec();
        let missing_ul = ShannonFeatureSetUlPerCcNr {
            max_scs: Some(1),
            ..Default::default()
        };

        // The global catalog carries every DL record referenced below (so DL's absent-check
        // passes) but no UL records at all (so the one UL record referenced below fails UL's
        // absent-check).
        let catalogs = FeatureCatalogs::new(dl_sources, vec![]);
        let component: RawSubBlock = RawNrSubBlock {
            band: 78,
            dl: NrDirection::with_features(1, dl_features),
            ul: NrDirection::with_features(1, vec![missing_ul]),
            ..Default::default()
        }
        .into();
        let payload = RawNrPayload {
            power_class: None,
            bcs_nr: None,
            bcs_intra_endc: None,
            bcs_eutra: None,
            intra_band_en_dc_support: None,
            sub_blocks: vec![component],
        };

        let error = LocalFeaturePlan::new(&catalogs, &[&payload], "A.binarypb", "legacy")
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("uses a UL feature absent from the global catalog"),
            "{error}"
        );
        assert!(
            !error.contains("256 distinct"),
            "DL's over-limit check must not win the race ahead of UL's absent-check: {error}"
        );
    }

    #[test]
    fn local_plan_passes_through_the_all_zero_placeholder_selector() {
        // The all-zero placeholder is the ONLY unresolved selector that can reach
        // generation: decompose (`RawSubBlock::from_proto_sub_block`, via
        // `resolve_or_placeholder`) fails closed on a non-placeholder one. It must
        // survive `reconstruct_sub_block` verbatim -- LTE sub-blocks inside `n` combos
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
                feature_index: None,
                // A class implies its per-CC list; cc_count(Lte, 1) == 1.
                selector: Some(vec![0]),
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

        // The NR half. This used to assert that a *UL-disabled* NR sub-block carries an
        // all-zero placeholder on the UL side and that it "must survive generation
        // byte-for-byte" — a guarantee that was only ever true on a path `provision` never
        // takes, because the fixture was built directly instead of through `resolve`.
        // `resolve` filters `ul_bw_class` to `>= 1` before deriving presence, so it yields
        // `None` there, and the sibling test below (`resolve_derives_the_omitted_placeholder`)
        // asserted exactly the opposite. The corpus settles which is right: `ul_bw_class == 0`
        // never carries field 7, in any of 687 438 occurrences. So the placeholder is tested
        // where it actually lives — under a real class — and the UL-disabled direction carries
        // nothing, which is now also what `validate` enforces.
        let nr_sb: RawSubBlock = RawNrSubBlock {
            band: 78,
            // cc_count(Nr, 1) == 1: a one-byte all-zero placeholder under a live class.
            dl: NrDirection::with_selector(1, vec![0]),
            ul: NrDirection::bare(Some(0)),
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
            nr_out.dl_feature_per_cc_ids,
            Some(vec![0]),
            "an all-zero NR placeholder under a live class survives generation byte-for-byte"
        );
        assert_eq!(
            nr_out.ul_feature_per_cc_ids, None,
            "a UL-disabled direction carries no per-CC list — matching what `resolve` derives"
        );
    }

    #[test]
    fn referenced_all_absent_record_is_resolved_on_the_ingest_axis() {
        // An all-absent referenced catalog record is genuinely resolved/present: a non-empty
        // `dl_features` vec IS presence, full stop, with no "does the entry have any field
        // set" gate on top. So it must key differently from a component with no DL data.
        let catalogs = FeatureCatalogs::new(vec![ShannonFeatureSetDlPerCcNr::default()], vec![]);
        let source = NrSourceSubBlock::from(SourceNrSubBlock {
            band: 78,
            dl_bw_class: Some(1),
            dl_feature: vec![1],
            ..Default::default()
        });
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

        // A resolved direction has no selector bytes left to disturb identity: `PerCc` holds
        // one encoding or the other, never both, so identity follows from the resolution.
        assert_eq!(resolved.dl_selector(), None);
    }

    #[test]
    fn resolve_derives_the_omitted_placeholder() {
        // The source format spells no raw selector bytes at all, so `resolve` is the single
        // place that puts proto field 6/7 back for a direction referencing no catalog
        // record: the all-zero placeholder, `cc_count(kind, bw_class)` bytes wide.
        let catalogs = FeatureCatalogs::new(vec![ShannonFeatureSetDlPerCcNr::default()], vec![]);

        // LTE never carries per-CC references; `cc_count(Lte, 1) == 1`.
        let lte = NrSourceSubBlock::from(SourceLteSubBlock {
            band: 66,
            dl_bw_class: Some(1),
            dl_feature: Some(3),
            ..Default::default()
        })
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
        let nr = NrSourceSubBlock::from(SourceNrSubBlock {
            band: 48,
            dl_bw_class: Some(2),
            ul_bw_class: Some(0),
            ..Default::default()
        })
        .resolve(&catalogs)
        .unwrap();
        assert_eq!(nr.dl_selector(), Some([0, 0].as_slice()));
        assert_eq!(nr.ul_selector(), None, "ul_bw_class 0 means UL disabled");

        // A direction that does resolve carries values and no placeholder.
        let resolved = NrSourceSubBlock::from(SourceNrSubBlock {
            band: 78,
            dl_bw_class: Some(1),
            dl_feature: vec![1],
            ..Default::default()
        })
        .resolve(&catalogs)
        .unwrap();
        assert_eq!(resolved.dl_selector(), None);
        assert_eq!(resolved.dl_features().len(), 1);
    }

    #[test]
    fn resolve_rejects_a_bw_class_with_no_known_cc_count() {
        // Fail closed rather than mis-derive a placeholder length: the derivation is only
        // as safe as `cc_count`, so an unobserved class must error here too.
        let error = NrSourceSubBlock::from(SourceNrSubBlock {
            band: 78,
            dl_bw_class: Some(99),
            ..Default::default()
        })
        .resolve(&FeatureCatalogs::default())
        .unwrap_err()
        .to_string();
        assert!(error.contains("unknown Nr bw_class 99"), "{error}");
    }

    #[test]
    fn dl_feature_identity_orders_absence_before_explicit_zero() {
        let absent = ShannonFeatureSetDlPerCcNr::default();
        let zero = ShannonFeatureSetDlPerCcNr {
            max_scs: Some(0),
            ..Default::default()
        };
        assert!(absent < zero);
        assert_ne!(absent, zero);
    }

    #[test]
    fn ul_feature_identity_orders_absence_before_explicit_false() {
        let absent = ShannonFeatureSetUlPerCcNr::default();
        let explicit_false = ShannonFeatureSetUlPerCcNr {
            bw_90mhz_supported: Some(false),
            ..Default::default()
        };
        assert!(absent < explicit_false);
        assert_ne!(absent, explicit_false);
    }

    // `lte_component_rejects_even_an_all_absent_resolved_nr_feature` lived here. It fed an
    // `lte` sub-block carrying a resolved NR feature through `resolve` and asserted the
    // "LTE component carries NR-only fields" error. `SourceLteSubBlock` has no field for a
    // catalog reference or an SRS-TX-switch, so that input can no longer be written down —
    // the check it exercised is now the type, and there is nothing left to test.
}
