use crate::proto::{ShannonFeatureSetDlPerCcNr, ShannonFeatureSetUlPerCcNr, UeCaps};
use anyhow::{Context, Result};
use std::collections::BTreeSet;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct FeaturePalette {
    pub(crate) dl: Vec<ShannonFeatureSetDlPerCcNr>,
    pub(crate) ul: Vec<ShannonFeatureSetUlPerCcNr>,
}

impl FeaturePalette {
    pub(crate) fn from_rows(
        dl: Vec<ShannonFeatureSetDlPerCcNr>,
        ul: Vec<ShannonFeatureSetUlPerCcNr>,
    ) -> Self {
        Self { dl, ul }
    }

    pub(crate) fn canonicalized(mut self) -> Self {
        self.dl.sort_unstable();
        self.dl.dedup();
        self.ul.sort_unstable();
        self.ul.dedup();
        self
    }
}

#[derive(Default)]
pub(crate) struct ObservedFeatureDomains {
    dl: DlFeatureDomains,
    ul: UlFeatureDomains,
}

#[derive(Default)]
struct DlFeatureDomains {
    max_scs: BTreeSet<Option<i32>>,
    max_mimo: BTreeSet<Option<i32>>,
    max_bw: BTreeSet<Option<i32>>,
    max_mod_order: BTreeSet<Option<i32>>,
    bw_90mhz_supported: BTreeSet<Option<bool>>,
}

#[derive(Default)]
struct UlFeatureDomains {
    max_scs: BTreeSet<Option<i32>>,
    max_mimo_cb: BTreeSet<Option<i32>>,
    max_bw: BTreeSet<Option<i32>>,
    max_mod_order: BTreeSet<Option<i32>>,
    bw_90mhz_supported: BTreeSet<Option<bool>>,
    max_mimo_non_cb: BTreeSet<Option<i32>>,
}

impl ObservedFeatureDomains {
    pub(crate) fn observe(&mut self, caps: &UeCaps) {
        for row in &caps.dl_feature_per_cc_list {
            self.dl.max_scs.insert(row.max_scs);
            self.dl.max_mimo.insert(row.max_mimo);
            self.dl.max_bw.insert(row.max_bw);
            self.dl.max_mod_order.insert(row.max_mod_order);
            self.dl.bw_90mhz_supported.insert(row.bw_90mhz_supported);
        }
        for row in &caps.ul_feature_per_cc_list {
            self.ul.max_scs.insert(row.max_scs);
            self.ul.max_mimo_cb.insert(row.max_mimo_cb);
            self.ul.max_bw.insert(row.max_bw);
            self.ul.max_mod_order.insert(row.max_mod_order);
            self.ul.bw_90mhz_supported.insert(row.bw_90mhz_supported);
            self.ul.max_mimo_non_cb.insert(row.max_mimo_non_cb);
        }
    }

    pub(crate) fn into_palette(
        self,
        additional_dl: &[ShannonFeatureSetDlPerCcNr],
        additional_ul: &[ShannonFeatureSetUlPerCcNr],
    ) -> Result<FeaturePalette> {
        Ok(FeaturePalette::from_rows(
            self.dl.into_rows(additional_dl)?,
            self.ul.into_rows(additional_ul)?,
        ))
    }
}

impl DlFeatureDomains {
    fn into_rows(
        self,
        additions: &[ShannonFeatureSetDlPerCcNr],
    ) -> Result<Vec<ShannonFeatureSetDlPerCcNr>> {
        let row_count = checked_row_count(
            "DL",
            &[
                self.max_scs.len(),
                self.max_mimo.len(),
                self.max_bw.len(),
                self.max_mod_order.len(),
                self.bw_90mhz_supported.len(),
            ],
            additions.len(),
        )?;
        let mut rows = Vec::new();
        rows.try_reserve_exact(row_count)
            .context("DL feature palette allocation failed")?;
        for max_scs in self.max_scs {
            for max_mimo in &self.max_mimo {
                for max_bw in &self.max_bw {
                    for max_mod_order in &self.max_mod_order {
                        for bw_90mhz_supported in &self.bw_90mhz_supported {
                            rows.push(ShannonFeatureSetDlPerCcNr {
                                max_scs,
                                max_mimo: *max_mimo,
                                max_bw: *max_bw,
                                max_mod_order: *max_mod_order,
                                bw_90mhz_supported: *bw_90mhz_supported,
                            });
                        }
                    }
                }
            }
        }
        rows.extend_from_slice(additions);
        rows.sort_unstable();
        rows.dedup();
        Ok(rows)
    }
}

impl UlFeatureDomains {
    fn into_rows(
        self,
        additions: &[ShannonFeatureSetUlPerCcNr],
    ) -> Result<Vec<ShannonFeatureSetUlPerCcNr>> {
        let row_count = checked_row_count(
            "UL",
            &[
                self.max_scs.len(),
                self.max_mimo_cb.len(),
                self.max_bw.len(),
                self.max_mod_order.len(),
                self.bw_90mhz_supported.len(),
                self.max_mimo_non_cb.len(),
            ],
            additions.len(),
        )?;
        let mut rows = Vec::new();
        rows.try_reserve_exact(row_count)
            .context("UL feature palette allocation failed")?;
        for max_scs in self.max_scs {
            for max_mimo_cb in &self.max_mimo_cb {
                for max_bw in &self.max_bw {
                    for max_mod_order in &self.max_mod_order {
                        for bw_90mhz_supported in &self.bw_90mhz_supported {
                            for max_mimo_non_cb in &self.max_mimo_non_cb {
                                rows.push(ShannonFeatureSetUlPerCcNr {
                                    max_scs,
                                    max_mimo_cb: *max_mimo_cb,
                                    max_bw: *max_bw,
                                    max_mod_order: *max_mod_order,
                                    bw_90mhz_supported: *bw_90mhz_supported,
                                    max_mimo_non_cb: *max_mimo_non_cb,
                                });
                            }
                        }
                    }
                }
            }
        }
        rows.extend_from_slice(additions);
        rows.sort_unstable();
        rows.dedup();
        Ok(rows)
    }
}

fn checked_cardinality(direction: &str, lengths: &[usize]) -> Result<usize> {
    lengths.iter().try_fold(1usize, |product, &length| {
        product
            .checked_mul(length)
            .with_context(|| format!("{direction} feature Cartesian-product cardinality overflow"))
    })
}

fn checked_row_count(direction: &str, lengths: &[usize], additions: usize) -> Result<usize> {
    checked_cardinality(direction, lengths)?
        .checked_add(additions)
        .with_context(|| format!("{direction} feature row count overflow after additions"))
}

#[cfg(test)]
mod tests {
    use super::{FeaturePalette, ObservedFeatureDomains, checked_row_count};
    use crate::{
        compiler::additional_features::{ADDITIONAL_DL_FEATURES, ADDITIONAL_UL_FEATURES},
        proto::{ShannonFeatureSetDlPerCcNr, ShannonFeatureSetUlPerCcNr, UeCaps},
    };

    #[test]
    fn observed_values_form_presence_sensitive_cartesian_products() {
        let mut domains = ObservedFeatureDomains::default();
        domains.observe(&UeCaps {
            dl_feature_per_cc_list: vec![
                ShannonFeatureSetDlPerCcNr {
                    max_scs: None,
                    max_mimo: Some(0),
                    max_bw: Some(100),
                    max_mod_order: Some(2),
                    bw_90mhz_supported: Some(false),
                },
                ShannonFeatureSetDlPerCcNr {
                    max_scs: Some(0),
                    max_mimo: Some(0),
                    max_bw: Some(200),
                    max_mod_order: Some(2),
                    bw_90mhz_supported: Some(true),
                },
            ],
            ..Default::default()
        });

        let palette = domains.into_palette(&[], &[]).unwrap();
        assert_eq!(palette.dl.len(), 8);
        assert!(palette.dl.iter().any(|row| row.max_scs.is_none()));
        assert!(palette.dl.iter().any(|row| row.max_scs == Some(0)));
        assert!(
            palette
                .dl
                .iter()
                .any(|row| row.bw_90mhz_supported == Some(false))
        );
        assert!(
            palette
                .dl
                .iter()
                .any(|row| row.bw_90mhz_supported == Some(true))
        );
    }

    #[test]
    fn observed_ul_values_form_presence_sensitive_cartesian_products_across_all_axes() {
        let mut domains = ObservedFeatureDomains::default();
        domains.observe(&UeCaps {
            ul_feature_per_cc_list: vec![
                ShannonFeatureSetUlPerCcNr {
                    max_scs: None,
                    max_mimo_cb: Some(0),
                    max_bw: Some(100),
                    max_mod_order: Some(1),
                    bw_90mhz_supported: Some(false),
                    max_mimo_non_cb: Some(1),
                },
                ShannonFeatureSetUlPerCcNr {
                    max_scs: Some(0),
                    max_mimo_cb: Some(1),
                    max_bw: Some(200),
                    max_mod_order: Some(2),
                    bw_90mhz_supported: Some(true),
                    max_mimo_non_cb: Some(2),
                },
            ],
            ..Default::default()
        });

        let palette = domains.into_palette(&[], &[]).unwrap();
        assert_eq!(palette.ul.len(), 64);
        assert!(palette.ul.iter().any(|row| {
            row.max_scs.is_none()
                && row.max_mimo_cb == Some(1)
                && row.max_bw == Some(200)
                && row.max_mod_order == Some(2)
                && row.bw_90mhz_supported == Some(true)
                && row.max_mimo_non_cb == Some(2)
        }));
    }

    #[test]
    fn exact_additions_do_not_seed_observed_axes() {
        let mut domains = ObservedFeatureDomains::default();
        domains.observe(&UeCaps {
            dl_feature_per_cc_list: vec![ShannonFeatureSetDlPerCcNr {
                max_scs: Some(1),
                max_bw: Some(100),
                ..Default::default()
            }],
            ..Default::default()
        });
        let addition = ShannonFeatureSetDlPerCcNr {
            max_scs: Some(4),
            max_mimo: Some(1),
            max_bw: Some(200),
            max_mod_order: Some(2),
            bw_90mhz_supported: Some(false),
        };

        let palette = domains.into_palette(&[addition], &[]).unwrap();
        assert_eq!(
            palette.dl,
            vec![
                ShannonFeatureSetDlPerCcNr {
                    max_scs: Some(1),
                    max_bw: Some(100),
                    ..Default::default()
                },
                addition,
            ]
        );
    }

    #[test]
    fn empty_observed_directions_keep_only_exact_additions() {
        let palette = ObservedFeatureDomains::default()
            .into_palette(ADDITIONAL_DL_FEATURES, ADDITIONAL_UL_FEATURES)
            .unwrap();

        assert_eq!(palette.dl, ADDITIONAL_DL_FEATURES);
        assert_eq!(palette.ul, ADDITIONAL_UL_FEATURES);
    }

    #[test]
    fn duplicate_additions_collapse_after_product_construction() {
        let addition = ShannonFeatureSetDlPerCcNr {
            max_scs: Some(4),
            max_mimo: Some(1),
            max_bw: Some(200),
            max_mod_order: Some(2),
            bw_90mhz_supported: Some(false),
        };

        let palette = ObservedFeatureDomains::default()
            .into_palette(&[addition, addition], &[])
            .unwrap();

        assert_eq!(palette.dl, vec![addition]);
    }

    #[test]
    fn observation_order_does_not_change_the_canonical_palette() {
        let first = UeCaps {
            dl_feature_per_cc_list: vec![ShannonFeatureSetDlPerCcNr {
                max_scs: None,
                max_mimo: Some(1),
                ..Default::default()
            }],
            ..Default::default()
        };
        let second = UeCaps {
            dl_feature_per_cc_list: vec![ShannonFeatureSetDlPerCcNr {
                max_scs: Some(2),
                max_mimo: Some(2),
                ..Default::default()
            }],
            ..Default::default()
        };
        let mut forward = ObservedFeatureDomains::default();
        forward.observe(&first);
        forward.observe(&second);
        let mut reverse = ObservedFeatureDomains::default();
        reverse.observe(&second);
        reverse.observe(&first);

        assert_eq!(
            forward.into_palette(&[], &[]).unwrap(),
            reverse.into_palette(&[], &[]).unwrap()
        );
    }

    #[test]
    fn reference_stub_rows_contribute_without_combo_groups() {
        let stub = UeCaps {
            combo_groups: vec![],
            dl_feature_per_cc_list: vec![ShannonFeatureSetDlPerCcNr {
                max_scs: Some(1),
                max_mimo: Some(2),
                max_bw: Some(100),
                max_mod_order: Some(2),
                bw_90mhz_supported: Some(false),
            }],
            ..Default::default()
        };
        let mut domains = ObservedFeatureDomains::default();
        domains.observe(&stub);

        assert_eq!(
            domains.into_palette(&[], &[]).unwrap().dl,
            stub.dl_feature_per_cc_list
        );
    }

    #[test]
    fn from_rows_preserves_positions_for_selector_resolution() {
        let first = ShannonFeatureSetDlPerCcNr {
            max_scs: Some(2),
            ..Default::default()
        };
        let second = ShannonFeatureSetDlPerCcNr {
            max_scs: Some(1),
            ..Default::default()
        };

        let palette = FeaturePalette::from_rows(vec![first, second], vec![]);

        assert_eq!(palette.dl, vec![first, second]);
    }

    #[test]
    fn canonicalized_sorts_and_deduplicates_complete_rows() {
        let first = ShannonFeatureSetDlPerCcNr {
            max_scs: Some(2),
            ..Default::default()
        };
        let second = ShannonFeatureSetDlPerCcNr {
            max_scs: Some(1),
            ..Default::default()
        };
        let ul = ShannonFeatureSetUlPerCcNr {
            max_scs: Some(1),
            ..Default::default()
        };

        let palette =
            FeaturePalette::from_rows(vec![first, second, first], vec![ul, ul]).canonicalized();

        assert_eq!(palette.dl, vec![second, first]);
        assert_eq!(palette.ul, vec![ul]);
    }

    #[test]
    fn cardinality_and_addition_count_overflow_before_reservation() {
        let cardinality = checked_row_count("DL", &[usize::MAX, 2], 0).unwrap_err();
        assert!(
            cardinality
                .to_string()
                .contains("DL feature Cartesian-product cardinality overflow")
        );

        let additions = checked_row_count("UL", &[1], usize::MAX).unwrap_err();
        assert!(
            additions
                .to_string()
                .contains("UL feature row count overflow after additions")
        );
    }

    #[test]
    fn exact_additions_are_canonical_sets() {
        assert_eq!(
            ADDITIONAL_DL_FEATURES,
            &[ShannonFeatureSetDlPerCcNr {
                max_scs: Some(4),
                max_mimo: Some(1),
                max_bw: Some(200),
                max_mod_order: Some(2),
                bw_90mhz_supported: Some(false),
            }]
        );
        assert_eq!(
            ADDITIONAL_UL_FEATURES,
            &[ShannonFeatureSetUlPerCcNr {
                max_scs: Some(4),
                max_mimo_cb: Some(2),
                max_bw: Some(200),
                max_mod_order: Some(1),
                bw_90mhz_supported: Some(false),
                max_mimo_non_cb: Some(2),
            }]
        );
        assert!(
            ADDITIONAL_DL_FEATURES
                .windows(2)
                .all(|rows| rows[0] < rows[1])
        );
        assert!(
            ADDITIONAL_UL_FEATURES
                .windows(2)
                .all(|rows| rows[0] < rows[1])
        );
    }
}
