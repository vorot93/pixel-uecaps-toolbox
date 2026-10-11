//! Observation-preserving identity. Never pass through compiler-derived defaults.
use crate::{
    proto::{ComboHeader, ShannonFeatureSetDlPerCcNr, ShannonFeatureSetUlPerCcNr, UeCaps},
    raw_nr::{Direction, SubBlockKind},
};

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
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(super) struct ShapeComponent {
    pub band: i32,
    pub dl: Option<i32>,
    pub ul: Option<i32>,
}
pub(super) type ShapeKey = Vec<ShapeComponent>;
#[derive(Debug)]
pub(super) struct LocatedCombination {
    pub group: usize,
    pub index: usize,
    pub key: CombinationKey,
}

#[derive(Debug, thiserror::Error)]
#[error(
    "g{group} c{combo} component {component} {direction}[{position}]: invalid NR reference {value} in selector {selector:?}"
)]
pub(super) struct ProjectionError {
    pub group: usize,
    pub combo: usize,
    pub component: usize,
    pub direction: Direction,
    pub position: usize,
    pub value: u8,
    pub selector: Vec<u8>,
}

#[derive(Clone, Copy)]
struct ComponentLocation {
    group: usize,
    combo: usize,
    component: usize,
}

fn selector<T: Clone>(
    raw: Option<&[u8]>,
    rows: &[T],
    kind: SubBlockKind,
    location: ComponentLocation,
    direction: Direction,
    errors: &mut Vec<ProjectionError>,
) -> Selector<T> {
    if kind == SubBlockKind::Lte {
        return Selector::Eutra(raw.map(<[u8]>::to_vec));
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
            None => errors.push(ProjectionError {
                group: location.group,
                combo: location.combo,
                component: location.component,
                direction,
                position: position + 1,
                value: id,
                selector: ids.to_vec(),
            }),
        }
    }
    Selector::Resolved(resolved)
}

pub(super) fn project_combinations(
    caps: &UeCaps,
) -> Result<Vec<LocatedCombination>, Vec<ProjectionError>> {
    let mut result = Vec::new();
    let mut errors = Vec::new();
    for (gi, group) in caps.combo_groups.iter().enumerate() {
        for (ci, combo) in group.combo.iter().enumerate() {
            let mut components = Vec::new();
            for (si, c) in combo.sub_blocks.iter().enumerate() {
                let location = ComponentLocation {
                    group: gi + 1,
                    combo: ci + 1,
                    component: si + 1,
                };
                let kind = SubBlockKind::split_raw_band(c.band).0;
                components.push(ComponentKey {
                    band: c.band,
                    dl_bw_class: c.dl_bw_class,
                    ul_bw_class: c.ul_bw_class,
                    dl_feature_index: c.dl_feature_index,
                    ul_feature_index: c.ul_feature_index,
                    srstxswitch: c.srstxswitch,
                    dl: selector(
                        c.dl_feature_per_cc_ids.as_deref(),
                        &caps.dl_feature_per_cc_list,
                        kind,
                        location,
                        Direction::Dl,
                        &mut errors,
                    ),
                    ul: selector(
                        c.ul_feature_per_cc_ids.as_deref(),
                        &caps.ul_feature_per_cc_list,
                        kind,
                        location,
                        Direction::Ul,
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
    // These fields are ComponentKey's ordering prefix, so projection retains sorted order.
    key.components
        .iter()
        .map(|c| ShapeComponent {
            band: c.band,
            dl: c.dl_bw_class,
            ul: c.ul_bw_class,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        proto::{Combo, ComboGroup, SubBlock},
        raw_nr::Direction,
    };

    fn caps(dl: Option<Vec<u8>>, ul: Option<Vec<u8>>) -> UeCaps {
        UeCaps {
            dl_feature_per_cc_list: vec![
                ShannonFeatureSetDlPerCcNr::default(),
                ShannonFeatureSetDlPerCcNr {
                    max_bw: Some(100),
                    ..Default::default()
                },
            ],
            ul_feature_per_cc_list: vec![ShannonFeatureSetUlPerCcNr::default()],
            combo_groups: vec![ComboGroup {
                combo: vec![Combo {
                    sub_blocks: vec![SubBlock {
                        band: crate::raw_nr::NR_BAND_OFFSET + 78,
                        dl_feature_per_cc_ids: dl,
                        ul_feature_per_cc_ids: ul,
                        ..Default::default()
                    }],
                    ..Default::default()
                }],
                ..Default::default()
            }],
            ..Default::default()
        }
    }

    #[test]
    fn invalid_positions_retain_typed_coordinates_in_traversal_order() {
        let errors = project_combinations(&caps(Some(vec![1, 99]), Some(vec![0, 1]))).unwrap_err();
        assert_eq!(errors.len(), 2);
        let first = &errors[0];
        assert_eq!(
            (first.group, first.combo, first.component, first.position),
            (1, 1, 1, 2)
        );
        assert_eq!(first.direction, Direction::Dl);
        assert_eq!(first.value, 99);
        assert_eq!(first.selector, [1, 99]);
        assert_eq!(
            first.to_string(),
            "g1 c1 component 1 DL[2]: invalid NR reference 99 in selector [1, 99]"
        );
        assert_eq!(errors[1].direction, Direction::Ul);
        assert_eq!(errors[1].position, 1);
        assert_eq!(errors[1].value, 0);
    }

    #[test]
    fn selectors_preserve_absent_empty_placeholder_and_real_empty_record() {
        let cases = [
            (None, Selector::Absent),
            (Some(vec![]), Selector::Empty),
            (Some(vec![0, 0]), Selector::Placeholder(2)),
            (
                Some(vec![1]),
                Selector::Resolved(vec![ShannonFeatureSetDlPerCcNr::default()]),
            ),
        ];
        for (raw, expected) in cases {
            let projected = project_combinations(&caps(raw, None)).unwrap();
            assert_eq!(projected[0].key.components[0].dl, expected);
        }
    }

    #[test]
    fn later_cc_identity_changes_but_component_permutation_does_not() {
        let mut original = caps(Some(vec![1, 2]), Some(vec![1]));
        original.combo_groups[0].combo[0].sub_blocks.push(SubBlock {
            band: 3,
            ..Default::default()
        });
        let key = project_combinations(&original).unwrap().remove(0).key;
        original.combo_groups[0].combo[0].sub_blocks.reverse();
        assert_eq!(project_combinations(&original).unwrap()[0].key, key);
        original.dl_feature_per_cc_list[1].max_bw = Some(200);
        let changed = project_combinations(&original).unwrap().remove(0).key;
        assert_ne!(changed, key);
        assert_eq!(shape(&changed), shape(&key));
    }
}
