use super::{InputLayout, NrFileIdentity, NrTarget, verify::verify_generated_file};
use crate::{
    compiler::{
        GeneratedFile, GeneratedRole, feature_palette::FeaturePalette, features::LocalFeaturePlan,
        schema::ValidatedNr, selection::Sku,
    },
    model::{profile, profile_model_codes},
    proto::{Combo, ComboGroup, UeCaps},
    raw_nr::RawNrPayload,
};
use anyhow::{Context, ensure};
use prost::Message;
pub(in crate::compiler) fn generate_nr_files(
    nr: &ValidatedNr,
    target: NrTarget,
) -> anyhow::Result<Vec<GeneratedFile>> {
    let mut files = match target {
        NrTarget::Legacy => generate_legacy_files(nr)?,
        NrTarget::Profile { anchor, sku } => generate_profiled_files(nr, anchor, &sku)?,
    };
    files.sort_by(|left, right| left.basename.cmp(&right.basename));
    Ok(files)
}

fn generate_legacy_files(nr: &ValidatedNr) -> anyhow::Result<Vec<GeneratedFile>> {
    let mut files = Vec::with_capacity(nr.source.bitmask_carriers.len());
    for carrier in &nr.source.bitmask_carriers {
        let fingerprint = *nr
            .bitmask_fingerprints
            .get(carrier)
            .with_context(|| format!("missing legacy fingerprint for carrier `{carrier}`"))?;
        let id = nr
            .carriers
            .get(carrier)
            .and_then(|source| source.bitmask_id);
        let payloads = selected_payloads(nr, carrier, &Sku::Legacy);
        files.push(build_generated_file(
            &nr.palette,
            &Sku::Legacy,
            NrFileIdentity {
                basename: format!("{carrier}.binarypb"),
                fingerprint,
                carrier_id: id,
                reference: 0,
            },
            &payloads,
            InputLayout::Legacy,
        )?);
    }
    Ok(files)
}

fn generate_profiled_files(
    nr: &ValidatedNr,
    anchor: u64,
    sku: &Sku,
) -> anyhow::Result<Vec<GeneratedFile>> {
    profile(anchor).with_context(|| format!("unknown NR profile anchor {anchor}"))?;
    validate_profile_target(anchor, sku)?;

    let mut files = Vec::new();
    for (carrier, source) in &nr.carriers {
        // A carrier without a profiled role has no profile for this anchor either; the two
        // used to be separate `Option`s that this loop re-checked one at a time.
        let Some(profiled) = &source.profiled else {
            continue;
        };
        let Some(profile_source) = profiled.profiles.get(&anchor) else {
            continue;
        };
        let number = profile_source.number;
        let id = source.profiled_id;
        let payloads = selected_payloads(nr, carrier, sku);
        files.push(build_generated_file(
            &nr.palette,
            sku,
            NrFileIdentity {
                basename: format!("{carrier}_{number}.binarypb"),
                fingerprint: profile_source.fingerprint,
                carrier_id: id,
                reference: profile_source.unknown,
            },
            &payloads,
            InputLayout::Profiled,
        )?);
    }
    Ok(files)
}

fn selected_payloads<'a>(nr: &'a ValidatedNr, carrier: &str, sku: &Sku) -> Vec<&'a RawNrPayload> {
    // Intern the (carrier, sku) probe once against the domain, then fetch the combos it selects
    // from the prebuilt inverted index — an O(1) lookup that replaces the former per-combo scan
    // (each combo used to cost its own O(log n) membership probe against the domain; the
    // prebuilt index removes that per-combo cost entirely). Generation calls this
    // once per carrier per target, so the scan was the dominant `decompose`/`provision` cost. A carrier or
    // sku outside the domain selects nothing. Stored indices are ascending, so payload order
    // matches the old `combo.iter().filter(..)` order and generated output stays byte-identical.
    let Some(target) = nr.domain.probe(carrier, sku) else {
        return Vec::new();
    };
    nr.selection_index
        .payload_indices(&target)
        .iter()
        .map(|&combo_index| &nr.combos[combo_index as usize].payload)
        .collect()
}

fn validate_profile_target(anchor: u64, sku: &Sku) -> anyhow::Result<()> {
    let model_codes = profile_model_codes(anchor);
    match sku {
        Sku::Model(code) => ensure!(
            model_codes.contains(&code.as_str()),
            "model `{code}` does not select NR profile anchor {anchor}"
        ),
        Sku::Prime(value) => ensure!(
            model_codes.is_empty() && *value == anchor,
            "synthetic profile token `prime:{value}` is not valid for anchor {anchor}"
        ),
        Sku::Legacy => anyhow::bail!("legacy is not a profiled NR target"),
        Sku::Lte(id) => anyhow::bail!("LTE token `lte:{id}` is not an NR profile target"),
    }
    Ok(())
}

fn build_generated_file(
    palette: &FeaturePalette,
    sku: &Sku,
    identity: NrFileIdentity,
    payloads: &[&RawNrPayload],
    layout: InputLayout,
) -> anyhow::Result<GeneratedFile> {
    let NrFileIdentity {
        basename,
        fingerprint,
        carrier_id,
        reference,
    } = identity;
    let bitmask = layout.bitmask();
    let plan = LocalFeaturePlan::new(palette, payloads, &basename, &sku.to_string())?;
    let mut combo_groups = Vec::with_capacity(payloads.len());
    for (payload_index, payload) in payloads.iter().enumerate() {
        // `payload.sub_blocks` is already sorted by `RawSubBlockKey` when the combo is
        // validated (`schema::validate_nr_combos`), so re-sorting a clone here was
        // redundant.
        let sub_blocks = payload
            .sub_blocks
            .iter()
            .enumerate()
            .map(|(component_index, component)| {
                plan.reconstruct_sub_block(component).with_context(|| {
                    format!(
                        "reconstructing {} payload {} component {}",
                        basename,
                        payload_index + 1,
                        component_index + 1
                    )
                })
            })
            .collect::<anyhow::Result<Vec<_>>>()?;
        combo_groups.push(ComboGroup {
            combo_header: Some(payload.header),
            combo: vec![Combo {
                sub_blocks,
                bitmask: Some(bitmask),
            }],
        });
    }
    let catalogs = plan.into_catalogs();
    let caps = UeCaps {
        version: fingerprint,
        id: carrier_id,
        combo_groups,
        dl_feature_per_cc_list: catalogs.dl,
        ul_feature_per_cc_list: catalogs.ul,
        unknown: reference,
    };
    let bytes = caps.encode_to_vec();
    verify_generated_file(&basename, &bytes, &caps, payloads, layout)?;
    let role = match layout {
        InputLayout::Legacy => GeneratedRole::LegacyNr,
        InputLayout::Profiled => GeneratedRole::ProfiledNr,
    };
    Ok(GeneratedFile {
        basename,
        bytes,
        role,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        compiler::{
            nr::{NrTarget, generate_nr_files, ingest_nr, test_support::*},
            schema::{ValidatedNr, ValidatedNrCombo},
        },
        proto::{ShannonFeatureSetDlPerCcNr, ShannonFeatureSetUlPerCcNr, UeCaps},
        raw_nr::{Direction, NrDirection, RawNrPayload, RawNrSubBlock},
    };
    use prost::Message;
    use std::collections::BTreeSet;

    #[test]
    fn generation_rebuilds_legacy_and_profiled_identity_with_canonical_features() {
        const REAL_ANCHOR: u64 = 66_813_533;
        let nr = generation_source();

        let legacy = generate_nr_files(&nr, NrTarget::Legacy).unwrap();
        assert_eq!(
            legacy
                .iter()
                .map(|file| file.basename.as_str())
                .collect::<Vec<_>>(),
            vec!["A.binarypb", "EMPTY.binarypb"]
        );
        let legacy_a = UeCaps::decode(legacy[0].bytes.as_slice()).unwrap();
        assert_eq!(legacy_a.version, 715_188_856);
        assert_eq!(legacy_a.id, Some(1));
        assert_eq!(legacy_a.unknown, 0);
        assert_eq!(
            legacy_a.combo_groups[0]
                .combo_header
                .as_ref()
                .unwrap()
                .bcs_nr,
            Some(0)
        );
        assert!(
            legacy_a
                .combo_groups
                .iter()
                .flat_map(|group| &group.combo)
                .all(|combo| combo.bitmask == Some(65_535))
        );
        let legacy_empty = UeCaps::decode(legacy[1].bytes.as_slice()).unwrap();
        assert!(legacy_empty.combo_groups.is_empty());

        let profiled = generate_nr_files(
            &nr,
            NrTarget::Profile {
                anchor: REAL_ANCHOR,
                sku: Sku::Model("G2YBB".into()),
            },
        )
        .unwrap();
        assert_eq!(
            profiled
                .iter()
                .map(|file| file.basename.as_str())
                .collect::<Vec<_>>(),
            vec!["A_334067665.binarypb", "PROFILE_EMPTY_734948863.binarypb",]
        );
        let profiled_a = UeCaps::decode(profiled[0].bytes.as_slice()).unwrap();
        assert_eq!(profiled_a.version, 862_505_271);
        assert_eq!(profiled_a.id, Some(7));
        assert_eq!(profiled_a.unknown, 11);
        assert_eq!(profiled_a.dl_feature_per_cc_list.len(), 1);
        assert_eq!(profiled_a.dl_feature_per_cc_list[0].max_scs, Some(0));
        let components = &profiled_a.combo_groups[0].combo[0].sub_blocks;
        assert_eq!(
            components.iter().map(|cc| cc.band).collect::<Vec<_>>(),
            vec![10_041, 10_077]
        );
        assert!(
            components
                .iter()
                .all(|cc| cc.dl_feature_per_cc_ids == Some(vec![1]))
        );
        assert_eq!(profiled_a.combo_groups[0].combo[0].bitmask, Some(0));
        let profiled_empty = UeCaps::decode(profiled[1].bytes.as_slice()).unwrap();
        assert_eq!(profiled_empty.id, Some(8));
        assert_eq!(profiled_empty.unknown, 33);
        assert!(profiled_empty.combo_groups.is_empty());
    }

    #[test]
    fn generation_compacts_real_features() {
        const ANCHOR: u64 = 66_813_533;
        let mut nr = generation_source();
        let payload = RawNrPayload {
            header: full_header().unwrap(),
            // A single resolved DL feature: generation compacts the global catalog down to
            // the one referenced record and rewrites the selector to its 1-based local index.
            // (A raw non-colliding selector-only sub-block — the dropped `dl-cc-id` fallback —
            // is no longer exercised: it cannot come from source and the strict self-verify
            // re-decode now rejects a non-placeholder unresolvable selector.)
            sub_blocks: vec![
                RawNrSubBlock {
                    band: 78,
                    dl: NrDirection::with_features(
                        1,
                        vec![ShannonFeatureSetDlPerCcNr {
                            max_scs: Some(3),
                            ..Default::default()
                        }],
                    ),
                    ul: NrDirection::bare(Some(0)),
                    srs_tx_switch: None,
                }
                .into(),
            ],
        };
        nr.set_combos(vec![ValidatedNrCombo {
            payload,
            relation: nr
                .domain
                .relation(&BTreeSet::from([("A".into(), Sku::Model("G2YBB".into()))])),
        }]);

        let generated = generate_nr_files(
            &nr,
            NrTarget::Profile {
                anchor: ANCHOR,
                sku: Sku::Model("G2YBB".into()),
            },
        )
        .unwrap();
        let file = generated
            .iter()
            .find(|file| file.basename.starts_with("A_"))
            .unwrap();
        let caps = UeCaps::decode(file.bytes.as_slice()).unwrap();
        assert_eq!(caps.dl_feature_per_cc_list.len(), 1);
        let components = &caps.combo_groups[0].combo[0].sub_blocks;
        assert_eq!(
            components
                .iter()
                .find(|component| component.band == 10_078)
                .unwrap()
                .dl_feature_per_cc_ids,
            Some(vec![1])
        );
    }

    fn feature_payload(
        values: std::ops::RangeInclusive<u16>,
        direction: Direction,
    ) -> RawNrPayload {
        RawNrPayload {
            header: full_header().unwrap(),
            sub_blocks: values
                .map(|value| {
                    // Single-CC (bw_class 1) so the generated compact-list selector
                    // stays a single byte for `verify_compact_feature_list`'s cc_count check.
                    // The direction *not* under test still has class 1, so it must carry a
                    // per-CC list: the all-zero placeholder, which is what a real file holds
                    // for a CC with no feature. `with_features(1, vec![])` would collapse to
                    // `features: None`, a class-without-a-list shape the corpus never contains
                    // and `RawSubBlock::validate` now rejects.
                    RawNrSubBlock {
                        band: value,
                        dl: if direction == Direction::Dl {
                            NrDirection::with_features(
                                1,
                                vec![ShannonFeatureSetDlPerCcNr {
                                    max_bw: Some(i32::from(value)),
                                    ..Default::default()
                                }],
                            )
                        } else {
                            NrDirection::with_selector(1, vec![0])
                        },
                        ul: if direction == Direction::Ul {
                            NrDirection::with_features(
                                1,
                                vec![ShannonFeatureSetUlPerCcNr {
                                    max_bw: Some(i32::from(value)),
                                    ..Default::default()
                                }],
                            )
                        } else {
                            NrDirection::with_selector(1, vec![0])
                        },
                        srs_tx_switch: None,
                    }
                    .into()
                })
                .collect(),
        }
    }

    fn install_profile_payload(nr: &mut ValidatedNr, payload: RawNrPayload) {
        nr.set_combos(vec![ValidatedNrCombo {
            payload,
            relation: nr
                .domain
                .relation(&BTreeSet::from([("A".into(), Sku::Model("G2YBB".into()))])),
        }]);
    }

    fn generated_profile_a_bytes(nr: &ValidatedNr) -> Vec<u8> {
        generate_nr_files(
            nr,
            NrTarget::Profile {
                anchor: 66_813_533,
                sku: Sku::Model("G2YBB".into()),
            },
        )
        .unwrap()
        .into_iter()
        .find(|file| file.basename.starts_with("A_"))
        .unwrap()
        .bytes
    }

    #[test]
    fn unused_palette_row_survives_combo_surgery_without_changing_generated_bytes() {
        let payload = feature_payload(1..=1, Direction::Dl);
        let mut baseline = generation_source();
        install_profile_payload(&mut baseline, payload.clone());
        let baseline_bytes = generated_profile_a_bytes(&baseline);

        let unused = ShannonFeatureSetDlPerCcNr {
            max_bw: Some(999),
            ..Default::default()
        };
        let mut with_unused = generation_source();
        with_unused.palette.dl.push(unused);
        with_unused.palette.dl.sort_unstable();
        with_unused.palette.dl.dedup();
        install_profile_payload(&mut with_unused, payload);

        assert!(with_unused.palette.dl.contains(&unused));
        assert!(with_unused.source.dl_features.contains(&unused));
        let bytes = generated_profile_a_bytes(&with_unused);
        assert_eq!(bytes, baseline_bytes);
        let caps = UeCaps::decode(bytes.as_slice()).unwrap();
        assert_eq!(caps.dl_feature_per_cc_list.len(), 1);
        assert!(!caps.dl_feature_per_cc_list.contains(&unused));
    }

    #[test]
    fn generation_keeps_255_and_256_local_limits_with_a_larger_global_palette() {
        const ANCHOR: u64 = 66_813_533;
        for direction in [Direction::Dl, Direction::Ul] {
            let mut nr = generation_source();
            if direction == Direction::Dl {
                nr.palette
                    .dl
                    .extend((1_001..=1_300).map(|max_bw| ShannonFeatureSetDlPerCcNr {
                        max_bw: Some(max_bw),
                        ..Default::default()
                    }));
                nr.palette.dl.sort_unstable();
                nr.palette.dl.dedup();
            } else {
                nr.palette
                    .ul
                    .extend((1_001..=1_300).map(|max_bw| ShannonFeatureSetUlPerCcNr {
                        max_bw: Some(max_bw),
                        ..Default::default()
                    }));
                nr.palette.ul.sort_unstable();
                nr.palette.ul.dedup();
            }
            install_profile_payload(&mut nr, feature_payload(1..=255, direction));
            let global_len = if direction == Direction::Dl {
                nr.palette.dl.len()
            } else {
                nr.palette.ul.len()
            };
            assert!(global_len > 255);
            let generated = generate_nr_files(
                &nr,
                NrTarget::Profile {
                    anchor: ANCHOR,
                    sku: Sku::Model("G2YBB".into()),
                },
            )
            .unwrap();
            let file = generated
                .iter()
                .find(|file| file.basename.starts_with("A_"))
                .unwrap();
            let caps = UeCaps::decode(file.bytes.as_slice()).unwrap();
            let len = if direction == Direction::Dl {
                caps.dl_feature_per_cc_list.len()
            } else {
                caps.ul_feature_per_cc_list.len()
            };
            assert_eq!(len, 255);

            install_profile_payload(&mut nr, feature_payload(1..=256, direction));
            let error = generate_nr_files(
                &nr,
                NrTarget::Profile {
                    anchor: ANCHOR,
                    sku: Sku::Model("G2YBB".into()),
                },
            )
            .unwrap_err()
            .to_string();
            assert!(error.contains(&direction.to_string()), "{error}");
            assert!(error.contains("256 distinct"), "{error}");
            assert!(error.contains("local limit is 255"), "{error}");
        }
    }

    #[test]
    fn global_catalog_over_255_is_valid_when_each_target_uses_a_smaller_subset() {
        const ANCHOR: u64 = 66_813_533;
        let mut nr = generation_source();
        nr.set_combos(vec![
            ValidatedNrCombo {
                payload: feature_payload(1..=150, Direction::Dl),
                relation: nr
                    .domain
                    .relation(&BTreeSet::from([("A".into(), Sku::Legacy)])),
            },
            ValidatedNrCombo {
                payload: feature_payload(151..=300, Direction::Dl),
                relation: nr
                    .domain
                    .relation(&BTreeSet::from([("A".into(), Sku::Model("G2YBB".into()))])),
            },
        ]);
        assert_eq!(
            nr.palette.dl.len(),
            302,
            "combo surgery keeps the source's pre-existing row and exact addition"
        );

        let legacy = generate_nr_files(&nr, NrTarget::Legacy).unwrap();
        let legacy_a = legacy
            .iter()
            .find(|file| file.basename == "A.binarypb")
            .unwrap();
        assert_eq!(
            UeCaps::decode(legacy_a.bytes.as_slice())
                .unwrap()
                .dl_feature_per_cc_list
                .len(),
            150
        );

        let profiled = generate_nr_files(
            &nr,
            NrTarget::Profile {
                anchor: ANCHOR,
                sku: Sku::Model("G2YBB".into()),
            },
        )
        .unwrap();
        let profiled_a = profiled
            .iter()
            .find(|file| file.basename.starts_with("A_"))
            .unwrap();
        assert_eq!(
            UeCaps::decode(profiled_a.bytes.as_slice())
                .unwrap()
                .dl_feature_per_cc_list
                .len(),
            150
        );
    }

    #[test]
    fn generation_supports_synthetic_anchor_tokens_and_rejects_mismatched_targets() {
        let nr = generation_source();

        let files = generate_nr_files(
            &nr,
            NrTarget::Profile {
                anchor: 8_969,
                sku: Sku::Prime(8_969),
            },
        )
        .unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].basename, "A_44845.binarypb");
        let caps = UeCaps::decode(files[0].bytes.as_slice()).unwrap();
        assert_eq!(caps.version, 874_888_686);
        assert_eq!(caps.id, Some(7));
        assert_eq!(caps.unknown, 22);
        assert_eq!(caps.combo_groups[0].combo[0].bitmask, Some(0));

        let error = generate_nr_files(
            &nr,
            NrTarget::Profile {
                anchor: 8_969,
                sku: Sku::Model("G2YBB".into()),
            },
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("model `G2YBB` does not select NR profile anchor 8969"));
    }

    #[test]
    fn generation_derives_both_alt_fingerprints_from_family_and_tier() {
        let legacy = vec![legacy_file(
            "LEGACY",
            UeCaps {
                version: 715_188_856,
                ..Default::default()
            },
        )];
        let profiled = vec![
            profiled_file(
                "A_ALT",
                167,
                UeCaps {
                    version: 707_802_847,
                    id: Some(1),
                    ..Default::default()
                },
            ),
            profiled_file(
                "B_ALT",
                1_847,
                UeCaps {
                    version: 627_223_094,
                    id: Some(2),
                    ..Default::default()
                },
            ),
        ];
        let nr = validated_nr(ingest_nr(legacy, profiled, &empty_mapping()).unwrap());

        let a_code = profile_model_codes(167)[0];
        let a = generate_nr_files(
            &nr,
            NrTarget::Profile {
                anchor: 167,
                sku: Sku::Model(a_code.into()),
            },
        )
        .unwrap();
        assert_eq!(
            UeCaps::decode(a[0].bytes.as_slice()).unwrap().version,
            707_802_847
        );

        let b_code = profile_model_codes(1_847)[0];
        let b = generate_nr_files(
            &nr,
            NrTarget::Profile {
                anchor: 1_847,
                sku: Sku::Model(b_code.into()),
            },
        )
        .unwrap();
        assert_eq!(
            UeCaps::decode(b[0].bytes.as_slice()).unwrap().version,
            627_223_094
        );
    }
}
