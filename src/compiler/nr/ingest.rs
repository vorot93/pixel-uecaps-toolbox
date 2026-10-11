use super::{InputLayout, LegacyNrFile, ProfiledNrFile};
use crate::{
    compiler::{
        additional_features::{ADDITIONAL_DL_FEATURES, ADDITIONAL_UL_FEATURES},
        feature_palette::{FeaturePalette, ObservedFeatureDomains},
        schema::{
            BitmaskFingerprint, CarrierSource, NrDocument, ProfileSource, nr_source_combo,
            validate_carrier_name,
        },
        selection::{NrDomain, Sku},
    },
    factor::gcd,
    mapping::{MappingRoot, map_to_root, root_to_map},
    model::{Profile, Tier, fingerprint_info, matching_anchors},
    proto::{ComboHeader, UeCaps},
    raw_nr::{NR_BAND_OFFSET, RawNrPayload, RawNrPayloadKey, SubBlockKind},
};
use anyhow::{Context, ensure};
use compact_str::CompactString;
use std::collections::{BTreeMap, BTreeSet};
/// The five cross-file accumulations `ingest_nr` builds up: which carriers came from the
/// bitmask folder, which fingerprints they carried, the per-carrier metadata, the applicability
/// domain, and the deduplicated payload table with each payload's relation.
#[derive(Default)]
struct NrIngest {
    bitmask_carriers: Vec<String>,
    fingerprints: BTreeMap<u64, Vec<String>>,
    carriers: BTreeMap<String, CarrierSource>,
    domain_members: BTreeSet<(CompactString, Sku)>,
    payloads: BTreeMap<RawNrPayloadKey, (RawNrPayload, BTreeSet<(CompactString, Sku)>)>,
}

impl NrIngest {
    /// Record one payload under `owners`, storing it the first time its identity key is seen
    /// and accumulating the relation every time.
    fn add_payload(
        &mut self,
        payload: RawNrPayload,
        owners: impl IntoIterator<Item = (CompactString, Sku)>,
    ) {
        let key = RawNrPayloadKey::from(&payload);
        let (_, relation) = self
            .payloads
            .entry(key)
            .or_insert_with(|| (payload, BTreeSet::new()));
        relation.extend(owners);
    }
}

/// Canonicalize the profiled legend and validate every carrier name in it, so downstream
/// ingest can assume canonical names. Round-tripping through `root_to_map`/`map_to_root` is
/// the canonicalization.
fn canonical_mapping(mapping: &MappingRoot) -> anyhow::Result<MappingRoot> {
    let mapping = map_to_root(&root_to_map(mapping).context("validating profiled PLMN mapping")?)
        .context("canonicalizing profiled PLMN mapping")?;
    for entry in &mapping.mappings {
        validate_carrier_name(&entry.name)
            .with_context(|| format!("validating mapping carrier `{}`", entry.name))?;
    }
    Ok(mapping)
}

/// Ingest one unnumbered bitmask-folder file: validate it, fold its payloads into the shared
/// tables, and record its carrier metadata.
fn ingest_legacy_file(ingest: &mut NrIngest, file: LegacyNrFile) -> anyhow::Result<()> {
    let LegacyNrFile { carrier, caps } = file;
    validate_carrier_name(&carrier)?;
    ensure!(
        !ingest.carriers.contains_key(&carrier),
        "duplicate legacy carrier `{carrier}`"
    );
    ensure!(
        caps.unknown == 0,
        "legacy carrier `{carrier}` has unsupported field 9 value {}",
        caps.unknown
    );

    for payload in canonical_payloads(&caps, InputLayout::Legacy, &carrier)? {
        ingest.add_payload(payload, [(carrier.as_str().into(), Sku::Legacy)]);
    }

    ingest
        .fingerprints
        .entry(caps.version)
        .or_default()
        .push(carrier.clone());
    ingest.bitmask_carriers.push(carrier.clone());
    ingest
        .domain_members
        .insert((carrier.as_str().into(), Sku::Legacy));
    ingest.carriers.insert(
        carrier,
        CarrierSource {
            bitmask_id: caps.id.map(i64::from),
            ..Default::default()
        },
    );
    Ok(())
}

/// Group the profiled files by carrier, validating each name once.
fn group_profiled_by_carrier(
    profiled: Vec<ProfiledNrFile>,
) -> anyhow::Result<BTreeMap<String, Vec<ProfiledNrFile>>> {
    let mut by_carrier = BTreeMap::<String, Vec<ProfiledNrFile>>::new();
    for file in profiled {
        validate_carrier_name(&file.carrier)?;
        by_carrier
            .entry(file.carrier.clone())
            .or_default()
            .push(file);
    }
    Ok(by_carrier)
}

/// One profiled file, resolved to the single registered profile its filename number selects.
struct ClassifiedProfiledFile {
    profile: &'static Profile,
    number: u64,
    caps: UeCaps,
}

/// Resolve every one of a carrier's files to exactly one registered anchor, rejecting an
/// unmatched, ambiguous, or duplicated anchor.
fn classify_profiled_files(
    carrier: &str,
    mut files: Vec<ProfiledNrFile>,
) -> anyhow::Result<Vec<ClassifiedProfiledFile>> {
    files.sort_by_key(|file| file.number);
    let mut classified = Vec::with_capacity(files.len());
    let mut seen_anchors = BTreeSet::new();
    for file in files {
        let number = file.number;
        let matches = matching_anchors(number);
        let [profile] = matches.as_slice() else {
            anyhow::bail!(
                "profiled carrier `{carrier}` number {number} must match exactly one registered anchor; matched {}",
                matches.len()
            );
        };
        ensure!(
            seen_anchors.insert(profile.anchor),
            "duplicate profiled carrier `{carrier}` anchor {}",
            profile.anchor
        );
        classified.push(ClassifiedProfiledFile {
            profile,
            number,
            caps: file.caps,
        });
    }
    Ok(classified)
}

/// Confirm one profiled file's fingerprint resolves to a family consistent with its matched
/// anchor, returning the tier it resolved to.
fn verify_fingerprint_family(carrier: &str, file: &ClassifiedProfiledFile) -> anyhow::Result<Tier> {
    let Some((fingerprint_family, tier)) = fingerprint_info(file.caps.version) else {
        anyhow::bail!(
            "profiled carrier `{carrier}` anchor {} has unknown fingerprint {}",
            file.profile.anchor,
            file.caps.version
        );
    };
    ensure!(
        fingerprint_family == file.profile.family,
        "profiled carrier `{carrier}` anchor {} fingerprint family {:?} differs from profile family {:?}",
        file.profile.anchor,
        fingerprint_family,
        file.profile.family
    );
    Ok(tier)
}

/// Reconstruct the multiplier that, combined with the carrier's shared filename signature,
/// exactly reproduces `number` -- the filename-encoding invariant every profiled file must
/// satisfy.
fn exact_multiplier(
    carrier: &str,
    profile: &Profile,
    number: u64,
    signature: u64,
) -> anyhow::Result<u64> {
    let multiplier = number / signature;
    let rebuilt = signature.checked_mul(multiplier).with_context(|| {
        format!(
            "filename product overflow for profiled carrier `{carrier}` anchor {}",
            profile.anchor
        )
    })?;
    ensure!(
        rebuilt == number,
        "filename multiplier does not exactly reconstruct {number} for profiled carrier `{carrier}` anchor {}",
        profile.anchor
    );
    Ok(multiplier)
}

/// Resolve one profile's applicable SKUs, record them in the applicability domain, and fold
/// every one of `caps`'s canonical payloads into the ingest under those SKUs.
fn ingest_profile(
    ingest: &mut NrIngest,
    carrier: &str,
    profile: &Profile,
    caps: &UeCaps,
) -> anyhow::Result<()> {
    let skus = Sku::for_nr_anchor(profile.anchor);
    for sku in &skus {
        ingest.domain_members.insert((carrier.into(), sku.clone()));
    }
    for payload in canonical_payloads(caps, InputLayout::Profiled, carrier)? {
        ingest.add_payload(
            payload,
            skus.iter().cloned().map(|sku| (carrier.into(), sku)),
        );
    }
    Ok(())
}

/// Ingest one profiled carrier: classify its files, derive its signature/id/tier metadata, and
/// fold every payload into the shared tables.
fn ingest_profiled_carrier(
    ingest: &mut NrIngest,
    carrier: &str,
    files: Vec<ProfiledNrFile>,
) -> anyhow::Result<()> {
    let classified = classify_profiled_files(carrier, files)?;
    let signature = classified.iter().map(|file| file.number).fold(0, gcd);
    ensure!(
        signature != 0,
        "profiled carrier `{carrier}` has zero filename signature"
    );

    let profiled_id = classified[0].caps.id;
    for file in &classified {
        ensure!(
            file.caps.id == profiled_id,
            "profiled carrier `{carrier}` has inconsistent field 2 IDs"
        );
    }

    let mut carrier_tier = None;
    let mut profiles = BTreeMap::new();
    for file in classified {
        let tier = verify_fingerprint_family(carrier, &file)?;
        if let Some(previous) = carrier_tier {
            ensure!(
                previous == tier,
                "profiled carrier `{carrier}` has inconsistent fingerprint tiers"
            );
        } else {
            carrier_tier = Some(tier);
        }

        let ClassifiedProfiledFile {
            profile,
            number,
            caps,
        } = file;
        let multiplier = exact_multiplier(carrier, profile, number, signature)?;
        ingest_profile(ingest, carrier, profile, &caps)?;

        profiles.insert(
            profile.anchor,
            ProfileSource {
                multiplier,
                unknown: caps.unknown,
            },
        );
    }

    let source = ingest.carriers.entry(carrier.to_string()).or_default();
    source.profiled_id = profiled_id.map(i64::from);
    source.signature = Some(signature);
    source.tier = carrier_tier;
    source.profiles = profiles;
    Ok(())
}

/// Turn the accumulated ingest and raw-input palette into the finished document: sort the bitmask
/// lists, build the domain, and resolve each payload's relation into a canonical selection.
fn finish_nr_document(
    ingest: NrIngest,
    mapping: MappingRoot,
    palette: FeaturePalette,
) -> anyhow::Result<NrDocument> {
    let NrIngest {
        mut bitmask_carriers,
        fingerprints,
        mut carriers,
        domain_members,
        payloads,
    } = ingest;

    for entry in mapping.mappings {
        let carrier = carriers.entry(entry.name).or_default();
        carrier.mapping_id = Some(entry.id);
        carrier.plmns = Some(entry.plmns);
    }

    bitmask_carriers.sort_unstable();
    let bitmask_fingerprints = fingerprints
        .into_iter()
        .map(|(fingerprint, mut carriers)| {
            carriers.sort_unstable();
            BitmaskFingerprint {
                fingerprint,
                carriers,
            }
        })
        .collect();
    let domain = NrDomain::new(domain_members)?;
    let combos = payloads
        .into_values()
        .enumerate()
        .map(|(combo_index, (payload, relation))| {
            let first_carrier = relation
                .first()
                .map(|(carrier, _)| carrier.clone())
                .unwrap_or_else(|| CompactString::new("<none>"));
            let owner_count = relation.len();
            let relation = domain.relation(&relation);
            nr_source_combo(&payload, &relation, &domain, &palette).with_context(|| {
                format!(
                    "building source NR combo {} selected by carrier `{first_carrier}` ({owner_count} owner pairs)",
                    combo_index + 1,
                )
            })
        })
        .collect::<anyhow::Result<Vec<_>>>()?;

    Ok(NrDocument {
        bitmask_carriers,
        bitmask_fingerprints,
        carriers,
        dl_features: palette.dl,
        ul_features: palette.ul,
        combos,
    })
}

/// Assemble both folder layouts into one normalized NR document, in four phases: canonicalize
/// the legend, ingest every legacy file, group the profiled files by carrier, then ingest each
/// carrier's group. [`NrIngest`] carries the accumulation across all four.
///
/// **Phase order is load-bearing.** Legacy files are fully ingested before any profiled group is
/// touched, and the legend is merged only in [`finish_nr_document`] — both are what make the
/// emitted document byte-stable across runs.
pub(crate) fn ingest_nr(
    legacy: Vec<LegacyNrFile>,
    profiled: Vec<ProfiledNrFile>,
    mapping: &MappingRoot,
) -> anyhow::Result<NrDocument> {
    let mut domains = ObservedFeatureDomains::default();
    for caps in legacy
        .iter()
        .map(|file| &file.caps)
        .chain(profiled.iter().map(|file| &file.caps))
    {
        domains.observe(caps);
    }
    let palette = domains.into_palette(ADDITIONAL_DL_FEATURES, ADDITIONAL_UL_FEATURES)?;
    let mapping = canonical_mapping(mapping)?;
    let mut ingest = NrIngest {
        bitmask_carriers: Vec::with_capacity(legacy.len()),
        ..Default::default()
    };
    for file in legacy {
        ingest_legacy_file(&mut ingest, file)?;
    }
    for (carrier, files) in group_profiled_by_carrier(profiled)? {
        ingest_profiled_carrier(&mut ingest, &carrier, files)?;
    }
    finish_nr_document(ingest, mapping, palette)
}

/// Rejects two malformed capture shapes before any combo is ingested: an empty combo group
/// whose header is still value-bearing (nothing left to attach it to once the group has no
/// combos), and any component whose raw protobuf band falls outside the plain E-UTRA or
/// shifted-NR range `NR_BAND_OFFSET` encodes (a plain band, `1..NR_BAND_OFFSET`, or a raw NR
/// band, `NR_BAND_OFFSET+1 ..= 2*NR_BAND_OFFSET-1`, i.e. plain `1..offset` shifted up). This
/// gate runs BEFORE the payload ingest in `canonical_payloads` so an invalid band (raw
/// `NR_BAND_OFFSET` / n0, 0, or out of range) is rejected here with a clear message — derived
/// from `NR_BAND_OFFSET` rather than bare 10_000/20_000 literals. The direct protobuf-to-raw
/// ingest path (`RawSubBlock::from_proto_sub_block`) no longer re-parses a band label, so the
/// old `from_sub_block`/`raw_band` panic surface is gone regardless.
fn validate_raw_bands(caps: &UeCaps, carrier: &str) -> anyhow::Result<()> {
    for (group_index, group) in caps.combo_groups.iter().enumerate() {
        ensure!(
            !group.combo.is_empty()
                || group
                    .combo_header
                    .as_ref()
                    .is_none_or(|header| header == &ComboHeader::default()),
            "{carrier} empty combo group {} has a value-bearing header that cannot be represented",
            group_index + 1
        );
        for (combo_index, combo) in group.combo.iter().enumerate() {
            for (component_index, component) in combo.sub_blocks.iter().enumerate() {
                let band = component.band;
                ensure!(
                    (1..NR_BAND_OFFSET).contains(&SubBlockKind::split_raw_band(band).1),
                    "{carrier} group {} combo {} component {} has invalid raw band {band}",
                    group_index + 1,
                    combo_index + 1,
                    component_index + 1
                );
            }
        }
    }
    Ok(())
}

pub(super) fn canonical_payloads(
    caps: &UeCaps,
    layout: InputLayout,
    carrier: &str,
) -> anyhow::Result<Vec<RawNrPayload>> {
    validate_raw_bands(caps, carrier)?;

    let mut seen = BTreeSet::new();
    let mut payloads = Vec::new();
    // Ingest each protobuf combo directly: walk the groups and convert each combo to a
    // raw payload without the report `Combo`/`SubBlock` DTO round-trip. `index` counts combos across
    // all groups, matching the flattened numbering the messages used before.
    let mut index = 0;
    for group in &caps.combo_groups {
        let header = group.combo_header.as_ref();
        for combo in &group.combo {
            index += 1;
            if matches!(layout, InputLayout::Profiled) {
                ensure!(
                    combo.bitmask.is_none_or(|value| value == 0),
                    "profiled carrier `{carrier}` combo {} has unsupported nonzero bitmask {}",
                    index,
                    combo.bitmask.unwrap_or_default()
                );
            }
            ensure!(
                !combo.sub_blocks.is_empty(),
                "{carrier} combo {} must contain at least one component",
                index
            );
            let payload = RawNrPayload::from_proto_combo(
                header,
                combo,
                &caps.dl_feature_per_cc_list,
                &caps.ul_feature_per_cc_list,
            )
            .with_context(|| format!("{carrier} combo {index}"))?;
            for component in &payload.sub_blocks {
                component
                    .validate()
                    .with_context(|| format!("validating {carrier} combo {index}"))?;
            }
            let key = RawNrPayloadKey::from(&payload);
            if !seen.insert(key) {
                ensure!(
                    layout == InputLayout::Legacy,
                    "duplicate canonical NR payload in profiled carrier `{carrier}`"
                );
                continue;
            }
            payloads.push(payload);
        }
    }
    payloads.sort_by_cached_key(|payload| RawNrPayloadKey::from(payload));
    Ok(payloads)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        compiler::{
            features::SourceSubBlock,
            nr::{NrTarget, generate_nr_files, ingest_nr, test_support::*},
        },
        proto::{ShannonFeatureSetDlPerCcNr, ShannonFeatureSetUlPerCcNr, UeCaps},
    };
    use prost::Message;
    use std::collections::BTreeMap;

    #[test]
    fn ingest_builds_exhaustive_raw_feature_palettes_and_remaps_references() {
        let mut caps = one_combo_caps(715_188_856, Some(1), 0, 10_078, Some(9));
        let component = &mut caps.combo_groups[0].combo[0].sub_blocks[0];
        // A single in-range byte: all-or-nothing resolution needs every byte in range
        // to resolve (an out-of-range trailing byte, e.g. `[2, 99]`, now stays raw
        // instead of resolving on the in-range prefix — see `resolve_all`).
        component.dl_feature_per_cc_ids = Some(vec![2]);
        component.ul_feature_per_cc_ids = Some(vec![2]);
        // Both selectors now resolve real records: FR1 DL and non-MIMO UL require
        // explicit index 1, including when max_mimo_cb is absent in the UL record.
        component.dl_feature_index = Some(1);
        component.ul_feature_index = Some(1);
        caps.dl_feature_per_cc_list = vec![
            ShannonFeatureSetDlPerCcNr {
                max_scs: Some(1),
                ..Default::default()
            },
            ShannonFeatureSetDlPerCcNr {
                max_scs: Some(3),
                max_bw: Some(100),
                ..Default::default()
            },
            ShannonFeatureSetDlPerCcNr {
                max_bw: Some(0),
                ..Default::default()
            },
        ];
        caps.ul_feature_per_cc_list = vec![
            ShannonFeatureSetUlPerCcNr {
                max_scs: Some(1),
                ..Default::default()
            },
            ShannonFeatureSetUlPerCcNr {
                max_scs: Some(4),
                max_bw: Some(50),
                ..Default::default()
            },
            ShannonFeatureSetUlPerCcNr {
                bw_90mhz_supported: Some(false),
                ..Default::default()
            },
        ];

        let nr = ingest_nr(vec![legacy_file("LEGACY", caps)], vec![], &empty_mapping()).unwrap();
        assert_eq!(nr.dl_features.len(), 10);
        assert_eq!(nr.ul_features.len(), 13);
        assert!(nr.dl_features.contains(&ShannonFeatureSetDlPerCcNr {
            max_scs: Some(4),
            max_mimo: Some(1),
            max_bw: Some(200),
            max_mod_order: Some(2),
            bw_90mhz_supported: Some(false),
        }));
        assert!(nr.ul_features.contains(&ShannonFeatureSetUlPerCcNr {
            max_scs: Some(4),
            max_mimo_cb: Some(2),
            max_bw: Some(200),
            max_mod_order: Some(1),
            bw_90mhz_supported: Some(false),
            max_mimo_non_cb: Some(2),
        }));
        assert!(nr.dl_features.contains(&ShannonFeatureSetDlPerCcNr {
            max_scs: Some(1),
            ..Default::default()
        }));
        assert!(nr.ul_features.contains(&ShannonFeatureSetUlPerCcNr {
            bw_90mhz_supported: Some(false),
            ..Default::default()
        }));
        assert!(nr.dl_features.contains(&ShannonFeatureSetDlPerCcNr {
            max_scs: Some(1),
            max_bw: Some(100),
            ..Default::default()
        }));
        assert!(nr.ul_features.contains(&ShannonFeatureSetUlPerCcNr {
            max_scs: Some(1),
            max_bw: Some(50),
            bw_90mhz_supported: Some(false),
            ..Default::default()
        }));
        let SourceSubBlock::Nr(cc) = &nr.combos[0].sub_blocks[0] else {
            panic!("expected an `nr` sub-block")
        };
        assert_eq!(cc.dl_feature_indices, vec![9]);
        assert_eq!(cc.ul_feature_indices, vec![11]);
    }

    #[test]
    fn ingest_legacy_preserves_all_fingerprint_partitions_and_discards_any_input_mask() {
        let legacy = vec![
            legacy_file(
                "VZW",
                one_combo_caps(715_188_856, Some(7), 0, 10_078, Some(1)),
            ),
            legacy_file("KDDI", one_combo_caps(702_152_537, None, 0, 10_041, None)),
            legacy_file(
                "ATT",
                one_combo_caps(548_015_020, Some(0), 0, 10_077, Some(u32::MAX)),
            ),
            legacy_file(
                "EMPTY",
                UeCaps {
                    version: 773_233_060,
                    id: Some(-1),
                    ..Default::default()
                },
            ),
        ];

        let nr = ingest_nr(legacy, vec![], &empty_mapping()).unwrap();

        assert_eq!(nr.bitmask_carriers, vec!["ATT", "EMPTY", "KDDI", "VZW"]);
        let groups: BTreeMap<_, _> = nr
            .bitmask_fingerprints
            .iter()
            .map(|group| (group.fingerprint, group.carriers.clone()))
            .collect();
        assert_eq!(groups[&548_015_020], vec!["ATT"]);
        assert_eq!(groups[&702_152_537], vec!["KDDI"]);
        assert_eq!(groups[&715_188_856], vec!["VZW"]);
        assert_eq!(groups[&773_233_060], vec!["EMPTY"]);
        assert_eq!(nr.carriers["ATT"].bitmask_id, Some(0));
        assert_eq!(nr.carriers["EMPTY"].bitmask_id, Some(-1));
        assert_eq!(nr.carriers["KDDI"].bitmask_id, None);
        assert_eq!(nr.carriers["VZW"].bitmask_id, Some(7));
        assert_eq!(nr.combos.len(), 3);
        assert!(nr.combos.iter().all(|combo| combo.selection.is_some()));
    }

    #[test]
    fn ingest_legacy_rejects_nonzero_field_nine() {
        let error = ingest_nr(
            vec![legacy_file(
                "VZW",
                one_combo_caps(715_188_856, None, 9, 10_078, Some(65_535)),
            )],
            vec![],
            &empty_mapping(),
        )
        .unwrap_err()
        .to_string();

        assert!(error.contains("legacy carrier `VZW` has unsupported field 9 value 9"));
    }

    #[test]
    fn ingest_merges_legacy_duplicates_after_mask_discard_but_rejects_profiled_duplicates() {
        let mut legacy = one_combo_caps(715_188_856, None, 0, 10_078, Some(12));
        let mut duplicate = legacy.combo_groups[0].clone();
        duplicate.combo[0].bitmask = Some(6_144);
        legacy.combo_groups.push(duplicate);
        let nr = ingest_nr(vec![legacy_file("DISH", legacy)], vec![], &empty_mapping()).unwrap();
        assert_eq!(nr.combos.len(), 1);

        let legacy_stub = legacy_file(
            "LEGACY",
            UeCaps {
                version: 715_188_856,
                ..Default::default()
            },
        );
        let mut profiled = one_combo_caps(862_505_271, None, 0, 10_078, Some(0));
        profiled.combo_groups.push(profiled.combo_groups[0].clone());
        let error = ingest_nr(
            vec![legacy_stub],
            vec![profiled_file("PROFILED", 66_813_533, profiled)],
            &empty_mapping(),
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("duplicate canonical NR payload in profiled carrier `PROFILED`"));
    }

    #[test]
    fn ingest_profiled_derives_signature_profiles_relations_and_mapping_merge() {
        const SIGNATURE: u64 = 5;
        const REAL_ANCHOR: u64 = 66_813_533;
        const SYNTHETIC_ANCHOR: u64 = 8_969;
        let legacy = vec![legacy_file(
            "CARRIER",
            one_combo_caps(715_188_856, Some(1), 0, 10_078, Some(17)),
        )];
        let profiled = vec![
            profiled_file(
                "CARRIER",
                SIGNATURE * REAL_ANCHOR,
                one_combo_caps(862_505_271, Some(7), 11, 10_077, None),
            ),
            profiled_file(
                "CARRIER",
                SIGNATURE * SYNTHETIC_ANCHOR,
                one_combo_caps(874_888_686, Some(7), 22, 10_041, Some(0)),
            ),
        ];
        let mapping = mapping(&[(7, "CARRIER", &["250-01", "250-01"]), (99, "MAP_ONLY", &[])]);

        let nr = ingest_nr(legacy, profiled, &mapping).unwrap();

        let carrier = &nr.carriers["CARRIER"];
        assert_eq!(carrier.bitmask_id, Some(1));
        assert_eq!(carrier.profiled_id, Some(7));
        assert_eq!(carrier.mapping_id, Some(7));
        assert_eq!(carrier.signature, Some(SIGNATURE));
        assert_eq!(carrier.tier, Some(Tier::Main));
        assert_eq!(
            carrier.plmns.as_deref(),
            Some(&["250-01".parse().unwrap(), "250-01".parse().unwrap()][..])
        );
        assert_eq!(carrier.profiles[&66_813_533].multiplier, REAL_ANCHOR);
        assert_eq!(carrier.profiles[&66_813_533].unknown, 11);
        assert_eq!(carrier.profiles[&8969].multiplier, SYNTHETIC_ANCHOR);
        assert_eq!(carrier.profiles[&8969].unknown, 22);

        let mapping_only = &nr.carriers["MAP_ONLY"];
        assert_eq!(mapping_only.profiled_id, None);
        assert_eq!(mapping_only.mapping_id, Some(99));
        assert_eq!(mapping_only.plmns, Some(vec![]));
        assert!(mapping_only.profiles.is_empty());
        assert_eq!(nr.combos.len(), 3);

        let real = nr
            .combos
            .iter()
            .find(|combo| combo.sub_blocks[0].band() == 77)
            .unwrap();
        assert_eq!(
            real.selection.as_ref().unwrap()[0].skus.as_deref(),
            Some(&["G2YBB".into()][..])
        );
        let synthetic = nr
            .combos
            .iter()
            .find(|combo| combo.sub_blocks[0].band() == 41)
            .unwrap();
        assert_eq!(
            synthetic.selection.as_ref().unwrap()[0].skus.as_deref(),
            Some(&["prime:8969".into()][..])
        );
    }

    #[test]
    fn profiled_and_mapping_ids_are_independent() {
        const ANCHOR: u64 = 66_813_533;
        let legacy = vec![legacy_file(
            "LEGACY",
            UeCaps {
                version: 715_188_856,
                ..Default::default()
            },
        )];
        let profiled = vec![
            profiled_file(
                "ABSENT",
                ANCHOR,
                UeCaps {
                    version: 862_505_271,
                    id: None,
                    ..Default::default()
                },
            ),
            profiled_file(
                "ZERO_A",
                2 * ANCHOR,
                UeCaps {
                    version: 862_505_271,
                    id: Some(0),
                    ..Default::default()
                },
            ),
            profiled_file(
                "ZERO_B",
                3 * ANCHOR,
                UeCaps {
                    version: 862_505_271,
                    id: Some(0),
                    ..Default::default()
                },
            ),
        ];
        let document = ingest_nr(
            legacy,
            profiled,
            &mapping(&[
                (59, "ABSENT", &[]),
                (60, "ZERO_A", &[]),
                (61, "ZERO_B", &[]),
            ]),
        )
        .unwrap();
        assert_eq!(document.carriers["ABSENT"].profiled_id, None);
        assert_eq!(document.carriers["ABSENT"].mapping_id, Some(59));
        assert_eq!(document.carriers["ZERO_A"].profiled_id, Some(0));
        assert_eq!(document.carriers["ZERO_A"].mapping_id, Some(60));
        assert_eq!(document.carriers["ZERO_B"].profiled_id, Some(0));
        assert_eq!(document.carriers["ZERO_B"].mapping_id, Some(61));

        let validated = validated_nr(document);
        let generated = generate_nr_files(
            &validated,
            NrTarget::Profile {
                anchor: ANCHOR,
                sku: Sku::Model("G2YBB".into()),
            },
        )
        .unwrap();
        let generated_id = |prefix: &str| {
            let file = generated
                .iter()
                .find(|file| file.basename.starts_with(prefix))
                .expect("profiled carrier file is generated");
            UeCaps::decode(file.bytes.as_slice()).unwrap().id
        };
        assert_eq!(generated_id("ABSENT_"), None);
        assert_eq!(generated_id("ZERO_A_"), Some(0));
        assert_eq!(generated_id("ZERO_B_"), Some(0));
    }

    #[test]
    fn ingest_profiled_rejects_nonzero_modern_bitmask() {
        let error = ingest_nr(
            vec![legacy_file(
                "LEGACY",
                one_combo_caps(715_188_856, None, 0, 10_078, None),
            )],
            vec![profiled_file(
                "CARRIER",
                66_813_533,
                one_combo_caps(862_505_271, Some(7), 0, 10_077, Some(1)),
            )],
            &empty_mapping(),
        )
        .unwrap_err()
        .to_string();

        assert!(error.contains("unsupported nonzero bitmask 1"));
    }

    #[test]
    fn profiled_metadata_rejects_unknown_ambiguous_and_wrong_family_numbers() {
        let legacy = || {
            vec![legacy_file(
                "LEGACY",
                UeCaps {
                    version: 715_188_856,
                    ..Default::default()
                },
            )]
        };

        let unknown = ingest_nr(
            legacy(),
            vec![profiled_file(
                "CARRIER",
                13,
                UeCaps {
                    version: 874_888_686,
                    ..Default::default()
                },
            )],
            &empty_mapping(),
        )
        .unwrap_err()
        .to_string();
        assert!(unknown.contains("must match exactly one registered anchor; matched 0"));

        let ambiguous_number = 167 * 1_847;
        let ambiguous = ingest_nr(
            legacy(),
            vec![profiled_file(
                "CARRIER",
                ambiguous_number,
                UeCaps {
                    version: 874_888_686,
                    ..Default::default()
                },
            )],
            &empty_mapping(),
        )
        .unwrap_err()
        .to_string();
        assert!(ambiguous.contains("must match exactly one registered anchor; matched 2"));

        let wrong_family = ingest_nr(
            legacy(),
            vec![profiled_file(
                "CARRIER",
                66_813_533,
                UeCaps {
                    version: 874_888_686,
                    ..Default::default()
                },
            )],
            &empty_mapping(),
        )
        .unwrap_err()
        .to_string();
        assert!(wrong_family.contains("fingerprint family A differs from profile family B"));
    }

    #[test]
    fn profiled_metadata_rejects_inconsistent_ids_and_tiers() {
        let legacy = || {
            vec![legacy_file(
                "LEGACY",
                UeCaps {
                    version: 715_188_856,
                    ..Default::default()
                },
            )]
        };
        let profiles = |second_id, second_fingerprint| {
            vec![
                profiled_file(
                    "CARRIER",
                    66_813_533,
                    UeCaps {
                        version: 862_505_271,
                        id: Some(7),
                        ..Default::default()
                    },
                ),
                profiled_file(
                    "CARRIER",
                    8_969,
                    UeCaps {
                        version: second_fingerprint,
                        id: second_id,
                        ..Default::default()
                    },
                ),
            ]
        };

        let ids = ingest_nr(legacy(), profiles(None, 874_888_686), &empty_mapping())
            .unwrap_err()
            .to_string();
        assert!(ids.contains("inconsistent field 2 IDs"));

        let tiers = ingest_nr(legacy(), profiles(Some(7), 707_802_847), &empty_mapping())
            .unwrap_err()
            .to_string();
        assert!(tiers.contains("inconsistent fingerprint tiers"));
    }

    #[test]
    fn ingestion_rejects_invalid_raw_bands_without_panicking() {
        for band in [i32::MIN, -1, 0, 10_000, 20_000, i32::MAX] {
            let error = ingest_nr(
                vec![legacy_file(
                    "LEGACY",
                    one_combo_caps(715_188_856, None, 0, band, None),
                )],
                vec![],
                &empty_mapping(),
            )
            .unwrap_err()
            .to_string();
            assert!(
                error.contains(&format!("invalid raw band {band}")),
                "{error}"
            );
        }
    }

    #[test]
    fn ingestion_rejects_noncanonical_mapping_names() {
        let legacy = || {
            vec![legacy_file(
                "LEGACY",
                UeCaps {
                    version: 715_188_856,
                    ..Default::default()
                },
            )]
        };
        let profiled = || {
            vec![profiled_file(
                "CARRIER",
                66_813_533,
                UeCaps {
                    version: 862_505_271,
                    id: Some(7),
                    ..Default::default()
                },
            )]
        };

        let bad_name = format!(
            "{:#}",
            ingest_nr(legacy(), profiled(), &mapping(&[(8, " BAD ", &[])]),).unwrap_err()
        );
        assert!(bad_name.contains("carrier name ` BAD ` is not canonical"));
    }
}
