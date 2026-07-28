use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Context, ensure};
use compact_str::CompactString;
use prost::Message;

use crate::{
    compiler::{
        GeneratedFile,
        features::{FeatureCatalogs, LocalFeaturePlan},
        schema::{
            BitmaskFingerprint, CarrierSource, CarrierTier, DecimalU64, NrDocument, ProfileSource,
            ValidatedNr, modern_fingerprint, nr_source_combo, validate_carrier_name,
        },
        selection::{NrDomain, Sku},
    },
    factor::gcd,
    mapping::{MappingRoot, Plmn, map_to_root, root_to_map},
    model::{PROFILES, Profile, fp_info, matching_anchors, profile_model_codes},
    proto::{Combo, ComboGroup, ComboHeader, SubBlock as ProtoSubBlock, UeCaps},
    raw_nr::{Direction, RawNrPayload, RawNrPayloadKey, SubBlockKind, cc_count},
    report::combos::{NR_BAND_OFFSET, feature_index},
};

/// An unnumbered `<CARRIER>.binarypb` from the bitmask folder. Legacy files have no filename
/// number *by construction*, so there is nothing to validate downstream.
pub(crate) struct LegacyNrFile {
    pub(crate) carrier: String,
    pub(crate) caps: UeCaps,
}

/// A numbered `<CARRIER>_<NUMBER>.binarypb` from the profiled folder. The number is not
/// optional: `decompose`'s classifier already parsed it out of the filename to decide this
/// file belongs here at all, so the type records what the classifier proved.
pub(crate) struct ProfiledNrFile {
    pub(crate) carrier: String,
    pub(crate) number: u64,
    pub(crate) caps: UeCaps,
}

pub(crate) enum NrTarget {
    Legacy,
    Profile { anchor: u64, sku: Sku },
}

pub(crate) fn generate_nr_files(
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
            &nr.features,
            &Sku::Legacy,
            format!("{carrier}.binarypb"),
            fingerprint,
            id,
            0,
            payloads,
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
    let profile = PROFILES
        .iter()
        .find(|profile| profile.anchor == anchor)
        .with_context(|| format!("unknown NR profile anchor {anchor}"))?;
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
        let number = profiled
            .signature
            .checked_mul(profile_source.multiplier)
            .with_context(|| {
                format!("filename product overflow for carrier `{carrier}` profile {anchor}")
            })?;
        ensure!(
            number == profile_source.number,
            "filename product changed for carrier `{carrier}` profile {anchor}"
        );
        let fingerprint = modern_fingerprint(profile.family, profiled.tier);
        ensure!(
            fingerprint == profile_source.fingerprint,
            "derived fingerprint changed for carrier `{carrier}` profile {anchor}"
        );
        let id = source.profiled_id;
        let payloads = selected_payloads(nr, carrier, sku);
        files.push(build_generated_file(
            &nr.features,
            sku,
            format!("{carrier}_{number}.binarypb"),
            fingerprint,
            id,
            profile_source.unknown,
            payloads,
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
        .map(|&combo_index| &nr.combo[combo_index as usize].payload)
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

#[allow(clippy::too_many_arguments)]
fn build_generated_file(
    features: &FeatureCatalogs,
    sku: &Sku,
    basename: String,
    version: u64,
    id: Option<i32>,
    unknown: u64,
    payloads: Vec<&RawNrPayload>,
    layout: InputLayout,
) -> anyhow::Result<GeneratedFile> {
    let bitmask = layout.bitmask();
    let plan = LocalFeaturePlan::new(features, &payloads, &basename, &sku.to_string())?;
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
            combo_header: payload.header(),
            combo: vec![Combo {
                sub_blocks,
                bitmask: Some(bitmask),
            }],
        });
    }
    let caps = UeCaps {
        version,
        id,
        combo_groups,
        dl_feature_per_cc_list: plan.dl.clone(),
        ul_feature_per_cc_list: plan.ul.clone(),
        unknown,
    };
    let bytes = caps.encode_to_vec();
    verify_generated_file(&basename, &bytes, &caps, &payloads, layout)?;
    Ok(GeneratedFile { basename, bytes })
}

/// `entries` yields `(raw_band, bw_class, selector)` per component — `dl_bw_class`/DL or
/// `ul_bw_class`/UL depending on `direction`. A selector counts as a resolved catalog
/// reference (checked against `cc_count`, and its bytes fold into the coverage set) only
/// when its FIRST byte resolves against `records` — the same leading-byte gate the report path
/// uses. By construction (`LocalFeaturePlan::reconstruct_sub_block`), a
/// resolved reference's bytes are ALL catalog indices, never a mix with raw selector-only
/// data, so it is enough to gate on the first byte and then trust every byte.
fn verify_compact_feature_list<T>(
    records: &[T],
    entries: impl IntoIterator<Item = (i32, Option<i32>, Option<Vec<u8>>)>,
    direction: Direction,
    basename: &str,
) -> anyhow::Result<()> {
    let mut referenced = BTreeSet::new();
    for (raw_band, bw_class, ids) in entries {
        let Some(ids) = ids else { continue };
        if feature_index(Some(&ids), records.len()).is_none() {
            continue;
        }
        let (kind, _) = SubBlockKind::split_raw_band(raw_band);
        let bw_class = bw_class.with_context(|| {
            format!(
                "generated {basename} has a resolved {direction} selector with no bw_class to derive its CC count"
            )
        })?;
        // `kind` is dead-in-practice as `Lte` here: an LTE per-CC selector is always the
        // all-zero placeholder, which never passes the `feature_index` gate above (per-CC
        // selectors that resolve against a compact list are NR-only, corpus-verified — see
        // DESIGN.md), so this branch only ever runs for `Nr` on real data.
        let cc = u8::try_from(bw_class).with_context(|| {
            format!("generated {basename} {direction} bw_class {bw_class} is out of range")
        })?;
        // `cc_count`'s contract excludes 0 ("callers gate on it first"), and this caller did
        // not — a class-0 direction reached it and produced a bare "unknown Nr bw_class 0"
        // naming neither the file nor the band. Reaching here with 0 also means the generated
        // file broke the selector-presence biconditional `RawSubBlock::validate` enforces, so
        // say that rather than bottoming out in a CC-count lookup.
        ensure!(
            cc >= 1,
            "generated {basename} band {raw_band} carries a resolved {direction} selector but \
             its bw_class is 0; per-CC data and a live bandwidth class imply each other"
        );
        let expected = cc_count(kind, cc)?;
        ensure!(
            ids.len() == expected,
            "generated {basename} resolved {direction} selector has {} byte(s), expected {expected} for {kind:?} bw_class {bw_class}",
            ids.len()
        );
        for &b in &ids {
            let k = b as usize;
            ensure!(
                (1..=records.len()).contains(&k),
                "generated {basename} resolved {direction} selector byte {b} is out of the compact {direction} list bounds 1..={}",
                records.len()
            );
            referenced.insert(k - 1);
        }
    }
    ensure!(
        referenced.len() == records.len() && referenced.iter().copied().eq(0..records.len()),
        "generated {basename} has an unused or missing {direction} feature record"
    );
    Ok(())
}

/// Confirms every sub-block's resolved per-CC selector fully references the file's compact
/// DL/UL feature catalogs exactly once each (no unused or missing record) — both directions,
/// since a per-CC selector references only its own direction's catalog.
fn verify_compact_feature_lists(
    decoded: &UeCaps,
    components: &[&ProtoSubBlock],
    basename: &str,
) -> anyhow::Result<()> {
    verify_compact_feature_list(
        &decoded.dl_feature_per_cc_list,
        components.iter().map(|component| {
            (
                component.band,
                component.dl_bw_class,
                component.dl_feature_per_cc_ids.clone(),
            )
        }),
        Direction::Dl,
        basename,
    )?;
    verify_compact_feature_list(
        &decoded.ul_feature_per_cc_list,
        components.iter().map(|component| {
            (
                component.band,
                component.ul_bw_class,
                component.ul_feature_per_cc_ids.clone(),
            )
        }),
        Direction::Ul,
        basename,
    )?;
    Ok(())
}

/// Confirms the generated file's canonical payload set exactly matches what generation
/// intended, order-independent (both sides are sorted before comparison) — the last line of
/// defense against a generation bug that emits the wrong SET of payloads even when every
/// individual payload otherwise self-verifies.
fn verify_canonical_payloads(
    decoded: &UeCaps,
    layout: InputLayout,
    basename: &str,
    expected_payloads: &[&RawNrPayload],
) -> anyhow::Result<()> {
    let actual = canonical_payloads(decoded, layout, basename)?;
    let actual: Vec<_> = actual.iter().map(RawNrPayloadKey::from).collect();
    let mut expected: Vec<_> = expected_payloads
        .iter()
        .map(|payload| RawNrPayloadKey::from(*payload))
        .collect();
    expected.sort_unstable();
    ensure!(
        actual == expected,
        "generated NR canonical payload self-check failed for {basename}"
    );
    Ok(())
}

fn verify_generated_file(
    basename: &str,
    bytes: &[u8],
    expected_caps: &UeCaps,
    expected_payloads: &[&RawNrPayload],
    layout: InputLayout,
) -> anyhow::Result<()> {
    let expected_bitmask = layout.bitmask();
    let decoded = UeCaps::decode(bytes)
        .with_context(|| format!("self-verifying generated NR file {basename}"))?;
    ensure!(
        decoded.version == expected_caps.version
            && decoded.id == expected_caps.id
            && decoded.unknown == expected_caps.unknown,
        "generated NR identity self-check failed for {basename}"
    );
    // Read the bitmask straight off the decoded message. This used to call
    // `build_combos_with_bitmasks` and use only the tuple's second element, allocating and
    // discarding the entire report display tree — a `Vec<SubBlock>` per combo plus a band
    // `CompactString`, two `resolve_all` vectors and a `render_component` `String` per
    // component, then a joined `bands` string — on the order of 7-8M throwaway allocations per
    // `decompose`, to check one integer per combo.
    ensure!(
        decoded
            .combo_groups
            .iter()
            .flat_map(|group| &group.combo)
            .all(|combo| combo.bitmask == Some(expected_bitmask)),
        "generated NR bitmask self-check failed for {basename}"
    );
    ensure!(
        decoded.dl_feature_per_cc_list == expected_caps.dl_feature_per_cc_list,
        "generated NR DL feature-list self-check failed for {basename}"
    );
    ensure!(
        decoded.ul_feature_per_cc_list == expected_caps.ul_feature_per_cc_list,
        "generated NR UL feature-list self-check failed for {basename}"
    );
    let components = decoded
        .combo_groups
        .iter()
        .flat_map(|group| &group.combo)
        .flat_map(|combo| &combo.sub_blocks)
        .collect::<Vec<_>>();
    verify_compact_feature_lists(&decoded, &components, basename)?;
    verify_canonical_payloads(&decoded, layout, basename, expected_payloads)?;
    Ok(())
}

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
        ensure!(
            matches.len() == 1,
            "profiled carrier `{carrier}` number {number} must match exactly one registered anchor; matched {}",
            matches.len()
        );
        let profile = matches[0];
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
fn verify_fingerprint_family(
    carrier: &str,
    file: &ClassifiedProfiledFile,
) -> anyhow::Result<CarrierTier> {
    let Some((fingerprint_family, tier)) = fp_info(file.caps.version) else {
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
    Ok(tier.into())
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
    let skus = profile_model_codes(profile.anchor);
    let skus: Vec<Sku> = if skus.is_empty() {
        vec![Sku::Prime(profile.anchor)]
    } else {
        skus.into_iter()
            .map(|code| Sku::Model(code.into()))
            .collect()
    };
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
            profile.anchor.to_string(),
            ProfileSource {
                multiplier: DecimalU64(multiplier),
                unknown: DecimalU64(caps.unknown),
            },
        );
    }

    let source = ingest.carriers.entry(carrier.to_string()).or_default();
    source.profiled_id = profiled_id.map(i64::from);
    source.signature = Some(DecimalU64(signature));
    source.tier = carrier_tier;
    source.profiles = profiles;
    Ok(())
}

/// Turn the accumulated ingest into the finished document: sort the bitmask lists, build the
/// domain and feature catalogs, and resolve each payload's relation into a canonical selection.
fn finish_nr_document(ingest: NrIngest, mapping: MappingRoot) -> anyhow::Result<NrDocument> {
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
        carrier.plmns = Some(entry.plmns.iter().map(Plmn::to_string).collect());
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
    let domain = NrDomain::new(domain_members);
    let features = FeatureCatalogs::from_payloads(payloads.values().map(|(payload, _)| payload));
    let combo = payloads
        .into_values()
        .map(|(payload, relation)| {
            let relation = domain.relation(relation);
            nr_source_combo(&payload, &relation, &domain, &features)
        })
        .collect::<anyhow::Result<Vec<_>>>()?;

    Ok(NrDocument {
        version: 1,
        bitmask_carriers,
        bitmask_fingerprints,
        carriers,
        dl_features: features.dl,
        ul_features: features.ul,
        combo,
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
    finish_nr_document(ingest, mapping)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum InputLayout {
    Legacy,
    Profiled,
}

impl InputLayout {
    /// The single bitmask a generated combo carries for this layout: the legacy all-ones sentinel
    /// vs. the profiled zero. Previously threaded in as a redundant argument beside `layout`.
    fn bitmask(self) -> u32 {
        match self {
            InputLayout::Legacy => 65_535,
            InputLayout::Profiled => 0,
        }
    }
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
                    (1..NR_BAND_OFFSET).contains(&band)
                        || ((NR_BAND_OFFSET + 1)..(2 * NR_BAND_OFFSET)).contains(&band),
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

fn canonical_payloads(
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
    use std::collections::{BTreeMap, BTreeSet};

    use prost::Message;

    use super::*;
    use crate::{
        compiler::{
            features::NrSourceSubBlock,
            schema::{
                CarrierTier, DecimalU64, LteDocument, LteFileSource, ValidatedNr, ValidatedNrCombo,
                parse_sources, to_kdl,
            },
        },
        mapping::{MappingEntry, MappingRoot},
        proto::{
            Combo, ComboGroup, ComboHeader, ShannonFeatureSetDlPerCcNr, ShannonFeatureSetUlPerCcNr,
            SubBlock, UeCaps,
        },
        raw_nr::{NrDirection, RawNrPayload, RawNrSubBlock, RawSubBlock, RawSubBlockKey},
        report::combos::build_combos,
    };

    /// A component that differs from its siblings only in `srs_tx_switch`. (It used to vary
    /// `dl_feature_index`, but an NR component no longer stores one — the index is derived,
    /// so two otherwise-identical NR components can no longer differ in it.)
    fn cc(band: i32, srs_tx_switch: i32) -> SubBlock {
        SubBlock {
            band,
            dl_bw_class: Some(1),
            ul_bw_class: Some(1),
            // A class-1 direction always carries a one-byte per-CC list; the biconditional in
            // `RawSubBlock::validate` requires it, and every real file has it.
            dl_feature_per_cc_ids: Some(vec![0]),
            ul_feature_per_cc_ids: Some(vec![0]),
            srstxswitch: Some(srs_tx_switch),
            ..Default::default()
        }
    }

    fn raw_payloads(caps: &UeCaps) -> Vec<RawNrPayload> {
        RawNrPayload::all_from_caps(caps).expect("fixture caps carry a complete combo header")
    }

    /// The four corpus-verified always-`Some` header fields. Ingest fails closed on a missing
    /// one, so even a fixture that says nothing about the header has to carry a complete one —
    /// real input always does.
    fn full_header() -> Option<ComboHeader> {
        Some(ComboHeader {
            power_class: Some(0),
            bcs_nr: Some(0),
            bcs_intra_endc: None,
            bcs_eutra: Some(0),
            intra_band_en_dc_support: Some(0),
        })
    }

    fn one_combo_caps(
        version: u64,
        id: Option<i32>,
        unknown: u64,
        band: i32,
        bitmask: Option<u32>,
    ) -> UeCaps {
        UeCaps {
            version,
            id,
            unknown,
            combo_groups: vec![ComboGroup {
                combo_header: Some(ComboHeader {
                    // The four corpus-verified always-`Some` header fields (all but
                    // `bcs_intra_endc`) — the strict decode boundary
                    // (`raw_nr::from_proto_combo`) fails closed on a missing one (Task 8).
                    power_class: Some(0),
                    bcs_nr: Some(0),
                    bcs_intra_endc: None,
                    bcs_eutra: Some(0),
                    intra_band_en_dc_support: Some(0),
                }),
                combo: vec![Combo {
                    sub_blocks: vec![cc(band, 0)],
                    bitmask,
                }],
            }],
            ..Default::default()
        }
    }

    fn legacy_file(carrier: &str, caps: UeCaps) -> LegacyNrFile {
        LegacyNrFile {
            carrier: carrier.to_string(),
            caps,
        }
    }

    fn profiled_file(carrier: &str, number: u64, caps: UeCaps) -> ProfiledNrFile {
        ProfiledNrFile {
            carrier: carrier.to_string(),
            number,
            caps,
        }
    }

    fn empty_mapping() -> MappingRoot {
        MappingRoot { mappings: vec![] }
    }

    fn mapping(entries: &[(u64, &str, &[&str])]) -> MappingRoot {
        MappingRoot {
            mappings: entries
                .iter()
                .map(|(id, name, plmns)| MappingEntry {
                    id: *id,
                    name: (*name).into(),
                    plmns: plmns.iter().map(|plmn| plmn.parse().unwrap()).collect(),
                })
                .collect(),
        }
    }

    fn validated_nr(document: NrDocument) -> ValidatedNr {
        let lte = LteDocument {
            version: 1,
            files: BTreeMap::from([(
                "1".into(),
                LteFileSource {
                    fingerprint: 0,
                    bitmask: 0,
                },
            )]),
            combo: vec![],
        };
        let (nr_text, lte_text) = to_kdl(&document, &lte).unwrap();
        parse_sources(&nr_text, &lte_text).unwrap().nr
    }

    fn generation_source() -> ValidatedNr {
        const SIGNATURE: u64 = 5;
        const REAL_ANCHOR: u64 = 66_813_533;
        const SYNTHETIC_ANCHOR: u64 = 8_969;
        let legacy = vec![
            legacy_file(
                "A",
                one_combo_caps(715_188_856, Some(1), 0, 10_078, Some(123)),
            ),
            legacy_file(
                "EMPTY",
                UeCaps {
                    version: 773_233_060,
                    ..Default::default()
                },
            ),
        ];

        let mut real = one_combo_caps(862_505_271, Some(7), 11, 10_077, None);
        real.combo_groups[0].combo[0].sub_blocks = vec![
            SubBlock {
                band: 10_077,
                dl_bw_class: Some(1),
                // DL-only: UL disabled but corpus-verified `Some(0)`, never absent.
                ul_bw_class: Some(0),
                dl_feature_per_cc_ids: Some(vec![1]),
                ..Default::default()
            },
            SubBlock {
                band: 10_041,
                dl_bw_class: Some(1),
                ul_bw_class: Some(0),
                dl_feature_per_cc_ids: Some(vec![1]),
                ..Default::default()
            },
        ];
        real.dl_feature_per_cc_list = vec![ShannonFeatureSetDlPerCcNr {
            max_scs: Some(0),
            ..Default::default()
        }];
        let profiled = vec![
            profiled_file("A", SIGNATURE * REAL_ANCHOR, real),
            profiled_file(
                "A",
                SIGNATURE * SYNTHETIC_ANCHOR,
                one_combo_caps(874_888_686, Some(7), 22, 10_003, Some(0)),
            ),
            profiled_file(
                "PROFILE_EMPTY",
                11 * REAL_ANCHOR,
                UeCaps {
                    version: 862_505_271,
                    id: Some(8),
                    unknown: 33,
                    ..Default::default()
                },
            ),
        ];
        validated_nr(ingest_nr(legacy, profiled, &mapping(&[(7, "A", &["250-01"])])).unwrap())
    }

    #[test]
    fn raw_conversion_flattens_group_packing_and_sorts_components_by_full_key() {
        let caps = UeCaps {
            combo_groups: vec![
                ComboGroup {
                    combo_header: full_header(),
                    combo: vec![Combo {
                        sub_blocks: vec![cc(10_078, 9), cc(10_078, 2)],
                        bitmask: Some(7),
                    }],
                },
                ComboGroup {
                    combo_header: full_header(),
                    combo: vec![Combo {
                        sub_blocks: vec![cc(10_041, 1)],
                        bitmask: None,
                    }],
                },
            ],
            ..Default::default()
        };

        let report = build_combos(&caps);
        assert_eq!((report[0].group, report[0].index), (1, 1));
        assert_eq!((report[1].group, report[1].index), (2, 1));

        let payloads = raw_payloads(&caps);

        assert_eq!(payloads.len(), 2, "group packing must flatten away");
        assert_eq!(payloads[0].power_class, Some(0));
        assert_eq!(
            payloads[0]
                .sub_blocks
                .iter()
                .map(RawSubBlock::srs_tx_switch)
                .collect::<Vec<_>>(),
            vec![Some(2), Some(9)],
            "all raw component fields participate in canonical ordering"
        );
    }

    #[test]
    fn raw_conversion_preserves_optional_zero_values_and_suppresses_resolved_selectors() {
        let caps = UeCaps {
            combo_groups: vec![ComboGroup {
                combo_header: Some(ComboHeader {
                    bcs_nr: Some(0),
                    bcs_intra_endc: Some(0),
                    bcs_eutra: Some(0),
                    power_class: Some(0),
                    intra_band_en_dc_support: Some(0),
                }),
                combo: vec![Combo {
                    sub_blocks: vec![SubBlock {
                        band: 10_078,
                        // Both classes are 1, not 0: per-CC presence and `bw_class` imply each
                        // other, so a direction carrying a selector must have a real class.
                        // The resolution behaviour under test is unaffected — it depends on the
                        // selector bytes, not on which class they sit under.
                        dl_bw_class: Some(1),
                        ul_bw_class: Some(1),
                        // A single in-range byte, so all-or-nothing resolution still
                        // resolves (the out-of-range multi-byte case is covered by
                        // `raw_conversion_preserves_every_selector_only_presence_shape`
                        // and `resolve_all_keeps_every_cc_or_none`).
                        dl_feature_per_cc_ids: Some(vec![1]),
                        // The all-zero placeholder: resolves to nothing, so it survives
                        // verbatim rather than being superseded like DL's.
                        ul_feature_per_cc_ids: Some(vec![0]),
                        srstxswitch: Some(0),
                        ..Default::default()
                    }],
                    bitmask: Some(0),
                }],
            }],
            dl_feature_per_cc_list: vec![ShannonFeatureSetDlPerCcNr {
                max_scs: Some(0),
                max_mimo: Some(0),
                max_bw: Some(0),
                max_mod_order: Some(0),
                bw_90mhz_supported: Some(false),
            }],
            ..Default::default()
        };

        let payload = raw_payloads(&caps).pop().unwrap();
        assert_eq!(payload.bcs_nr, Some(0));
        assert_eq!(payload.power_class, Some(0));
        let component = &payload.sub_blocks[0];
        // The explicit-zero *class* case moved to `ul_bw_class_zero_is_preserved_as_explicit`
        // below: NR DL is never class 0 in any real file, and a class-0 direction may not carry
        // a selector, so the old `dl_bw_class: Some(0)` + DL selector fixture was a shape the
        // corpus does not contain. The remaining explicit zeros here are all realistic.
        assert_eq!(component.srs_tx_switch(), Some(0));
        assert_eq!(component.dl_features().len(), 1);
        assert_eq!(component.dl_features()[0].max_scs, Some(0));
        assert_eq!(component.dl_features()[0].bw_90mhz_supported, Some(false));
        assert_eq!(
            component.dl_selector(),
            None,
            "resolved raw values must suppress the source selector"
        );
        assert_eq!(component.ul_selector(), Some([0].as_slice()));
    }

    /// The realistic explicit-zero class: UL disabled. `ul_bw_class = 0` occurs 687 438 times
    /// in the corpus and always with field 7 absent, so this is the shape that must survive
    /// ingest as `Some(0)` rather than being normalized to `None`.
    #[test]
    fn ul_bw_class_zero_is_preserved_as_explicit() {
        let caps = UeCaps {
            combo_groups: vec![ComboGroup {
                combo_header: full_header(),
                combo: vec![Combo {
                    sub_blocks: vec![SubBlock {
                        band: 10_078,
                        dl_bw_class: Some(1),
                        dl_feature_per_cc_ids: Some(vec![0]),
                        ul_bw_class: Some(0),
                        ..Default::default()
                    }],
                    bitmask: Some(0),
                }],
            }],
            ..Default::default()
        };

        let payload = raw_payloads(&caps).pop().unwrap();
        let component = &payload.sub_blocks[0];

        assert_eq!(component.ul_bw_class(), Some(0));
        assert_eq!(component.ul_selector(), None);
    }

    // The former `raw_conversion_preserves_every_selector_only_presence_shape` lived here. It
    // fed arbitrary selectors (`[1, 9]`, `[2]`) and a mismatched NR `dl_feature_index` through
    // the report-DTO ingest path, which applied neither the unresolvable-selector nor the
    // index-derivation guard. That path is gone, and the guards it bypassed are covered
    // directly by `raw_nr`'s `from_proto_rejects_non_placeholder_unresolvable_selector`,
    // `from_proto_accepts_the_all_zero_placeholder_selector`,
    // `from_proto_rejects_nr_feature_index_mismatch`, and
    // `from_proto_accepts_a_matching_nr_feature_index`.

    #[test]
    fn compiler_conversion_preserves_a_referenced_default_feature_record() {
        let caps = UeCaps {
            combo_groups: vec![ComboGroup {
                combo_header: full_header(),
                combo: vec![Combo {
                    sub_blocks: vec![SubBlock {
                        band: 10_078,
                        // A DL selector requires a DL class to sit under — the biconditional
                        // `validate` now enforces, and the shape every real file has.
                        dl_bw_class: Some(1),
                        ul_bw_class: Some(0),
                        // A single in-range byte: all-or-nothing resolution needs every
                        // byte in range to resolve, unlike the old first-byte rule.
                        dl_feature_per_cc_ids: Some(vec![1]),
                        ..Default::default()
                    }],
                    bitmask: Some(0),
                }],
            }],
            dl_feature_per_cc_list: vec![ShannonFeatureSetDlPerCcNr::default()],
            ..Default::default()
        };
        let payload = raw_payloads(&caps).pop().unwrap();
        let component = &payload.sub_blocks[0];
        assert_eq!(
            component.dl_features().first().copied(),
            Some(ShannonFeatureSetDlPerCcNr::default())
        );
        assert_eq!(component.dl_selector(), None);
        assert_ne!(
            RawSubBlockKey::from(component),
            RawSubBlockKey::from(&RawSubBlock::from(RawNrSubBlock {
                band: 78,
                ..Default::default()
            }))
        );
    }

    #[test]
    fn ingest_prunes_unreferenced_records_and_canonicalizes_referenced_dl_and_ul() {
        let mut caps = one_combo_caps(715_188_856, Some(1), 0, 10_078, Some(9));
        let component = &mut caps.combo_groups[0].combo[0].sub_blocks[0];
        // A single in-range byte: all-or-nothing resolution needs every byte in range
        // to resolve (an out-of-range trailing byte, e.g. `[2, 99]`, now stays raw
        // instead of resolving on the in-range prefix — see `resolve_all`).
        component.dl_feature_per_cc_ids = Some(vec![2]);
        component.ul_feature_per_cc_ids = Some(vec![2]);
        // `cc()` seeds `dl_feature_index = Some(0)`, but selector `[2]` resolves to the
        // max_scs=3 (FR1) record whose derived index is 1. NR no longer carries a source
        // index override, so the strict decode boundary rejects a stored≠derived index —
        // clear it (the derived 1 is materialized on provision) so decompose is about pruning, not
        // the dropped override.
        component.dl_feature_index = None;
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
        assert_eq!(nr.dl_features.len(), 1);
        assert_eq!(nr.ul_features.len(), 1);
        assert_eq!(nr.dl_features[0].max_scs, Some(3));
        assert_eq!(nr.ul_features[0].max_scs, Some(4));
        let NrSourceSubBlock::Nr(cc) = &nr.combo[0].sub_blocks[0] else {
            panic!("expected an `nr` sub-block")
        };
        assert_eq!(cc.dl_feature, vec![1]);
        assert_eq!(cc.ul_feature, vec![1]);
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
        assert_eq!(nr.combo.len(), 3);
        assert!(nr.combo.iter().all(|combo| combo.selection.is_some()));
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
        assert_eq!(nr.combo.len(), 1);

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
        assert_eq!(carrier.signature, Some(DecimalU64(SIGNATURE)));
        assert_eq!(carrier.tier, Some(CarrierTier::Main));
        assert_eq!(
            carrier.plmns.as_deref(),
            Some(&["250-01".into(), "250-01".into()][..])
        );
        assert_eq!(
            carrier.profiles["66813533"].multiplier,
            DecimalU64(REAL_ANCHOR)
        );
        assert_eq!(carrier.profiles["66813533"].unknown, DecimalU64(11));
        assert_eq!(
            carrier.profiles["8969"].multiplier,
            DecimalU64(SYNTHETIC_ANCHOR)
        );
        assert_eq!(carrier.profiles["8969"].unknown, DecimalU64(22));

        let mapping_only = &nr.carriers["MAP_ONLY"];
        assert_eq!(mapping_only.profiled_id, None);
        assert_eq!(mapping_only.mapping_id, Some(99));
        assert_eq!(mapping_only.plmns, Some(vec![]));
        assert!(mapping_only.profiles.is_empty());
        assert_eq!(nr.combo.len(), 3);

        let real = nr
            .combo
            .iter()
            .find(|combo| combo.sub_blocks[0].band() == 77)
            .unwrap();
        assert_eq!(
            real.selection.as_ref().unwrap()[0].skus.as_deref(),
            Some(&["G2YBB".into()][..])
        );
        let synthetic = nr
            .combo
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
            // The four corpus-verified always-`Some` header fields (all but
            // `bcs_intra_endc`) — `verify_generated_file`'s self-check re-decodes the
            // generated bytes through the strict `raw_nr::from_proto_combo` boundary,
            // which fails closed on a missing one (Task 8).
            power_class: Some(0),
            bcs_nr: Some(0),
            bcs_intra_endc: None,
            bcs_eutra: Some(0),
            intra_band_en_dc_support: Some(0),
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
                .relation(BTreeSet::from([("A".into(), Sku::Model("G2YBB".into()))])),
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

    fn feature_payload(values: std::ops::RangeInclusive<u16>, direction: &str) -> RawNrPayload {
        RawNrPayload {
            // See `generation_compacts_real_features`'s `payload` above:
            // `verify_generated_file`'s self-check needs these four fields.
            power_class: Some(0),
            bcs_nr: Some(0),
            bcs_intra_endc: None,
            bcs_eutra: Some(0),
            intra_band_en_dc_support: Some(0),
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
                        dl: if direction == "DL" {
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
                        ul: if direction == "UL" {
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
                .relation(BTreeSet::from([("A".into(), Sku::Model("G2YBB".into()))])),
        }]);
    }

    #[test]
    fn generation_accepts_255_and_rejects_256_local_records_per_direction() {
        const ANCHOR: u64 = 66_813_533;
        for direction in ["DL", "UL"] {
            let mut nr = generation_source();
            install_profile_payload(&mut nr, feature_payload(1..=255, direction));
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
            let len = if direction == "DL" {
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
            assert!(error.contains(direction), "{error}");
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
                payload: feature_payload(1..=150, "DL"),
                relation: nr
                    .domain
                    .relation(BTreeSet::from([("A".into(), Sku::Legacy)])),
            },
            ValidatedNrCombo {
                payload: feature_payload(151..=300, "DL"),
                relation: nr
                    .domain
                    .relation(BTreeSet::from([("A".into(), Sku::Model("G2YBB".into()))])),
            },
        ]);
        assert_eq!(nr.features.dl.len(), 300);

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
