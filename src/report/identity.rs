//! Observation-preserving identity. Never pass through compiler-derived defaults.
use super::combos::NR_BAND_OFFSET;
use crate::proto::{ComboHeader, ShannonFeatureSetDlPerCcNr, ShannonFeatureSetUlPerCcNr, UeCaps};

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum Selector<T> {
    Absent,
    Empty,
    Placeholder(usize),
    Resolved(Vec<T>),
    Eutra(Option<Vec<u8>>),
}
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(super) struct ComponentKey {
    pub band: i32,
    pub dl_bw_class: Option<i32>,
    pub ul_bw_class: Option<i32>,
    pub dl_feature_index: Option<i32>,
    pub ul_feature_index: Option<i32>,
    pub srstxswitch: Option<i32>,
    pub dl: Selector<ShannonFeatureSetDlPerCcNr>,
    pub ul: Selector<ShannonFeatureSetUlPerCcNr>,
}
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(super) struct CombinationKey {
    pub header: Option<ComboHeader>,
    pub bitmask: Option<u32>,
    pub components: Vec<ComponentKey>,
}
pub(super) type ShapeKey = Vec<(i32, Option<i32>, Option<i32>)>;
#[derive(Debug)]
pub(super) struct LocatedCombination {
    pub group: usize,
    pub index: usize,
    pub key: CombinationKey,
}

fn selector<T: Clone>(
    raw: &Option<Vec<u8>>,
    rows: &[T],
    nr: bool,
    context: &str,
    errors: &mut Vec<String>,
) -> Selector<T> {
    if !nr {
        return Selector::Eutra(raw.clone());
    }
    let Some(ids) = raw else {
        return Selector::Absent;
    };
    if ids.is_empty() {
        return Selector::Empty;
    }
    if ids.iter().all(|id| *id == 0) {
        return Selector::Placeholder(ids.len());
    }
    let mut resolved = Vec::new();
    for (position, id) in ids.iter().copied().enumerate() {
        match usize::from(id).checked_sub(1).and_then(|i| rows.get(i)) {
            Some(row) => resolved.push(row.clone()),
            None => errors.push(format!(
                "{context}[{}]: invalid NR reference {id} in selector {ids:?}",
                position + 1
            )),
        }
    }
    Selector::Resolved(resolved)
}

pub(super) fn project_combinations(caps: &UeCaps) -> Result<Vec<LocatedCombination>, Vec<String>> {
    let mut result = Vec::new();
    let mut errors = Vec::new();
    for (gi, group) in caps.combo_groups.iter().enumerate() {
        for (ci, combo) in group.combo.iter().enumerate() {
            let mut components = Vec::new();
            for (si, c) in combo.sub_blocks.iter().enumerate() {
                let context = format!("g{} c{} component {}", gi + 1, ci + 1, si + 1);
                components.push(ComponentKey {
                    band: c.band,
                    dl_bw_class: c.dl_bw_class,
                    ul_bw_class: c.ul_bw_class,
                    dl_feature_index: c.dl_feature_index,
                    ul_feature_index: c.ul_feature_index,
                    srstxswitch: c.srstxswitch,
                    dl: selector(
                        &c.dl_feature_per_cc_ids,
                        &caps.dl_feature_per_cc_list,
                        c.band >= NR_BAND_OFFSET,
                        &format!("{context} DL"),
                        &mut errors,
                    ),
                    ul: selector(
                        &c.ul_feature_per_cc_ids,
                        &caps.ul_feature_per_cc_list,
                        c.band >= NR_BAND_OFFSET,
                        &format!("{context} UL"),
                        &mut errors,
                    ),
                });
            }
            components.sort();
            result.push(LocatedCombination {
                group: gi + 1,
                index: ci + 1,
                key: CombinationKey {
                    header: group.combo_header,
                    bitmask: combo.bitmask,
                    components,
                },
            });
        }
    }
    if errors.is_empty() {
        Ok(result)
    } else {
        Err(errors)
    }
}
pub(super) fn shape(key: &CombinationKey) -> ShapeKey {
    let mut s: Vec<_> = key
        .components
        .iter()
        .map(|c| (c.band, c.dl_bw_class, c.ul_bw_class))
        .collect();
    s.sort();
    s
}
