use std::collections::{BTreeMap, BTreeSet, HashMap};

use anyhow::{Context, bail, ensure};
use compact_str::CompactString;

use super::{
    features::{DlFeatureSource, FeatureCatalogs, NrSourceSubBlock, UlFeatureSource},
    selection::{
        CarrierId, LteDomain, LteRelation, NrDomain, NrRelation, SelectionRect, Sku, SkuId,
    },
};
use crate::{
    mapping::{MappingEntry, MappingRoot, Plmn, map_to_root, root_to_map},
    model::{Family, PROFILES, Tier, lte_model_codes, matching_anchors, profile_model_codes},
    raw_nr::{RawNrPayload, RawNrPayloadKey, RawSubBlockKey},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct DecimalU64(pub(crate) u64);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CarrierTier {
    Main,
    Alt,
}

impl CarrierTier {
    /// The model-level [`Tier`] this compiler tier corresponds to.
    pub(crate) const fn to_model(self) -> Tier {
        match self {
            Self::Main => Tier::Main,
            Self::Alt => Tier::Alt,
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct BitmaskFingerprint {
    pub(crate) fingerprint: u64,
    pub(crate) carriers: Vec<String>,
}

#[derive(Clone, Debug)]
pub(crate) struct ProfileSource {
    pub(crate) multiplier: DecimalU64,
    pub(crate) unknown: DecimalU64,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct CarrierSource {
    pub(crate) bitmask_id: Option<i64>,
    pub(crate) profiled_id: Option<i64>,
    pub(crate) mapping_id: Option<u64>,
    pub(crate) plmns: Option<Vec<String>>,
    pub(crate) signature: Option<DecimalU64>,
    pub(crate) tier: Option<CarrierTier>,
    pub(crate) profiles: BTreeMap<String, ProfileSource>,
}

#[derive(Clone, Debug)]
pub(crate) struct NrSourceCombo {
    pub(crate) selection: Option<Vec<SelectionRect>>,
    pub(crate) power_class: Option<i32>,
    pub(crate) bcs_nr: Option<u32>,
    pub(crate) bcs_intra_endc: Option<u32>,
    pub(crate) bcs_eutra: Option<u32>,
    pub(crate) intra_band_en_dc_support: Option<i32>,
    pub(crate) sub_blocks: Vec<NrSourceSubBlock>,
}

#[derive(Clone, Debug)]
pub(crate) struct NrDocument {
    pub(crate) version: u32,
    pub(crate) bitmask_carriers: Vec<String>,
    pub(crate) bitmask_fingerprints: Vec<BitmaskFingerprint>,
    pub(crate) carriers: BTreeMap<String, CarrierSource>,
    pub(crate) dl_features: Vec<DlFeatureSource>,
    pub(crate) ul_features: Vec<UlFeatureSource>,
    pub(crate) combo: Vec<NrSourceCombo>,
}

#[derive(Clone, Debug)]
pub(crate) struct LteFileSource {
    pub(crate) fingerprint: u64,
    pub(crate) bitmask: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct LteSourceComponent {
    pub(crate) band: i32,
    pub(crate) dl_bw_class_mimo: i32,
    pub(crate) ul_bw_class_mimo: Option<i32>,
}

#[derive(Clone, Debug)]
pub(crate) struct LteSourceCombo {
    pub(crate) selection: Option<Vec<SelectionRect>>,
    pub(crate) bcs: Option<u64>,
    pub(crate) unknown1: Option<u64>,
    pub(crate) unknown2: Option<u64>,
    pub(crate) components: Vec<LteSourceComponent>,
}

#[derive(Clone, Debug)]
pub(crate) struct LteDocument {
    pub(crate) version: u32,
    pub(crate) files: BTreeMap<String, LteFileSource>,
    pub(crate) combo: Vec<LteSourceCombo>,
}

#[derive(Clone, Debug)]
pub(crate) struct ValidatedNr {
    pub(crate) source: NrDocument,
    pub(crate) bitmask_fingerprints: BTreeMap<String, u64>,
    pub(crate) carriers: BTreeMap<String, ValidatedCarrier>,
    pub(crate) domain: NrDomain,
    pub(crate) features: FeatureCatalogs,
    pub(crate) combo: Vec<ValidatedNrCombo>,
    pub(crate) selection_index: NrSelectionIndex,
}

#[derive(Clone, Debug)]
pub(crate) struct ValidatedNrCombo {
    pub(crate) payload: RawNrPayload,
    pub(crate) relation: NrRelation,
}

/// Inverted index from an interned `(carrier_id, sku_id)` target to the `combo` indices whose
/// relation contains it, built once per [`ValidatedNr`]. It replaces `selected_payloads`' former
/// per-target linear scan over every combo (each an O(log n) [`NrRelation::contains`] probe) with
/// one O(1) lookup — the dominant `decompose`/`provision` cost, since generation runs `selected_payloads`
/// once per carrier per target. Indices are stored ascending, so a lookup yields payloads in the
/// exact order the old `combo.iter().filter(..)` produced them, keeping generated output
/// byte-identical. Combo order is fixed once [`validate_nr_combos`] returns
/// ([`canonicalize_sources`] never reorders `combo`), so the indices stay valid through generation.
#[derive(Clone, Debug, Default)]
pub(crate) struct NrSelectionIndex(HashMap<(CarrierId, SkuId), Vec<u32>>);

impl NrSelectionIndex {
    fn build(combos: &[ValidatedNrCombo]) -> Self {
        let mut index: HashMap<(CarrierId, SkuId), Vec<u32>> = HashMap::new();
        for (combo_index, combo) in combos.iter().enumerate() {
            let combo_index = u32::try_from(combo_index).expect("combo count fits in u32");
            for member in combo.relation.members() {
                index.entry(member).or_default().push(combo_index);
            }
        }
        Self(index)
    }

    /// The `combo` indices selected by `target`, ascending; empty when the target selects nothing.
    pub(crate) fn payload_indices(&self, target: &(CarrierId, SkuId)) -> &[u32] {
        self.0.get(target).map_or(&[], Vec::as_slice)
    }
}

#[cfg(test)]
impl ValidatedNr {
    /// Test-only combo surgery: replace `combo` and rebuild every field derived from it
    /// (`features` and `selection_index`) so they stay consistent. Production sets all three once
    /// in [`validate_documents`]; because `selection_index` points at `combo` positions, any
    /// post-construction combo replacement must go through here or those indices dangle.
    pub(crate) fn set_combos(&mut self, combo: Vec<ValidatedNrCombo>) {
        self.features = FeatureCatalogs::from_payloads(combo.iter().map(|combo| &combo.payload));
        self.selection_index = NrSelectionIndex::build(&combo);
        self.combo = combo;
    }
}

#[derive(Clone, Debug)]
pub(crate) struct ValidatedCarrier {
    pub(crate) bitmask_id: Option<i32>,
    pub(crate) profiled_id: Option<i32>,
    pub(crate) mapping_id: Option<u64>,
    pub(crate) plmns: Option<Vec<u64>>,
    pub(crate) signature: Option<u64>,
    pub(crate) tier: Option<CarrierTier>,
    pub(crate) profiles: BTreeMap<u64, ValidatedProfile>,
}

#[derive(Clone, Debug)]
pub(crate) struct ValidatedProfile {
    pub(crate) multiplier: u64,
    pub(crate) number: u64,
    pub(crate) unknown: u64,
    pub(crate) fingerprint: u64,
}

#[derive(Clone, Debug)]
pub(crate) struct ValidatedLte {
    pub(crate) source: LteDocument,
    pub(crate) files: BTreeMap<u64, LteFileSource>,
    pub(crate) domain: LteDomain,
    pub(crate) combo: Vec<ValidatedLteCombo>,
}

#[derive(Clone, Debug)]
pub(crate) struct ValidatedLteCombo {
    pub(crate) source: LteSourceCombo,
    pub(crate) relation: LteRelation,
}

/// A fully parsed, cross-referenced, canonicalized source set. Produced once by
/// [`load_sources`](crate::compiler::load_sources) and consumed by
/// [`provision_from_sources`](crate::compiler::provision_from_sources) for every model, so a
/// batch `provision` run parses the ~19 MB source a single time. Fields are crate-internal; externally it
/// is an opaque handle.
#[derive(Clone, Debug)]
pub struct ValidatedSources {
    pub(crate) nr: ValidatedNr,
    pub(crate) lte: ValidatedLte,
}

impl ValidatedSources {
    /// Serialize these already-validated, canonical sources to `(nr.kdl, lte.kdl)` **without**
    /// re-validating. `validate_documents` leaves `nr.source`/`lte.source` canonical, so serializing
    /// them directly reproduces exactly what [`to_kdl`] would — letting `decompose` drop a redundant
    /// third `validate_documents` pass while its byte-idempotence assertion still proves the emitted
    /// documents are a fixed point.
    pub(crate) fn to_kdl(&self) -> anyhow::Result<(String, String)> {
        Ok((
            super::nr_to_kdl(&self.nr.source)?,
            super::lte_to_kdl(&self.lte.source),
        ))
    }
}

pub(crate) fn parse_sources(nr_text: &str, lte_text: &str) -> anyhow::Result<ValidatedSources> {
    let nr = super::nr_from_kdl(nr_text).context("parsing nr.kdl")?;
    let lte = super::lte_from_kdl(lte_text).context("parsing lte.kdl")?;
    validate_documents(nr, lte)
}

pub(crate) fn validate_documents(
    nr: NrDocument,
    lte: LteDocument,
) -> anyhow::Result<ValidatedSources> {
    ensure!(nr.version == 1, "unsupported nr.kdl version {}", nr.version);
    ensure!(
        lte.version == 1,
        "unsupported lte.kdl version {}",
        lte.version
    );
    let bitmask_fingerprints = validate_fingerprint_partition(&nr)?;
    let carriers = validate_carriers(&nr)?;
    validate_mapping_projection(&carriers)?;
    let nr_domain = build_nr_domain(&nr, &carriers);
    let input_features = FeatureCatalogs::new(nr.dl_features.clone(), nr.ul_features.clone());
    let nr_combo = validate_nr_combos(&nr.combo, &nr_domain, &input_features)?;
    let selection_index = NrSelectionIndex::build(&nr_combo);
    let features = FeatureCatalogs::from_payloads(nr_combo.iter().map(|combo| &combo.payload));
    let (lte_files, lte_domain) = validate_lte_files(&lte)?;
    let lte_combo = validate_lte_combos(&lte.combo, &lte_domain)?;
    let mut validated = ValidatedSources {
        nr: ValidatedNr {
            source: nr,
            bitmask_fingerprints,
            carriers,
            domain: nr_domain,
            features,
            combo: nr_combo,
            selection_index,
        },
        lte: ValidatedLte {
            source: lte,
            files: lte_files,
            domain: lte_domain,
            combo: lte_combo,
        },
    };
    canonicalize_sources(&mut validated)?;
    Ok(validated)
}

/// Validate + canonicalize + serialize in one step. Now used only by tests (the `decompose`/`provision`
/// paths validate once and reuse via [`ValidatedSources::to_kdl`]); kept as a convenient fixture
/// helper.
#[cfg(test)]
pub(crate) fn to_kdl(nr: &NrDocument, lte: &LteDocument) -> anyhow::Result<(String, String)> {
    validate_documents(nr.clone(), lte.clone())?.to_kdl()
}

/// Re-derives a carrier's canonical PLMN strings from their packed encoded form — the inverse
/// of `validate_carriers`' PLMN encode, applied post-validation so `nr.kdl` always stores the
/// human-readable `mcc-mnc` spelling, never the packed integer.
fn canonical_plmn_strings(encoded: &[u64]) -> Vec<String> {
    encoded
        .iter()
        .map(|value| {
            Plmn::from_encoded(*value)
                .expect("validated PLMN remains within 24 bits")
                .to_string()
        })
        .collect()
}

/// Rebuilds `nr.kdl`'s `combo` list from validated data: each combo's selection re-derived to
/// its canonical rectangle form, and its per-component catalog references re-derived via
/// `source_sub_block` — the write-side inverse of `validate_nr_combos`/`resolve`.
fn nr_source_combos(
    combo: &[ValidatedNrCombo],
    domain: &NrDomain,
    features: &FeatureCatalogs,
) -> anyhow::Result<Vec<NrSourceCombo>> {
    combo
        .iter()
        .map(|combo| {
            Ok(NrSourceCombo {
                selection: combo.relation.canonical_selection(domain)?,
                power_class: combo.payload.power_class,
                bcs_nr: combo.payload.bcs_nr,
                bcs_intra_endc: combo.payload.bcs_intra_endc,
                bcs_eutra: combo.payload.bcs_eutra,
                intra_band_en_dc_support: combo.payload.intra_band_en_dc_support,
                sub_blocks: combo
                    .payload
                    .sub_blocks
                    .iter()
                    .map(|component| features.source_sub_block(component))
                    .collect(),
            })
        })
        .collect()
}

/// Rebuilds `lte.kdl`'s `combo` list from validated data: only `selection` is re-derived (to
/// its canonical rectangle form); every other field is a straight clone of the validated
/// source combo, since LTE combos carry no catalog references to re-derive.
fn lte_source_combos(
    combo: &[ValidatedLteCombo],
    domain: &LteDomain,
) -> anyhow::Result<Vec<LteSourceCombo>> {
    combo
        .iter()
        .map(|combo| {
            let mut source = combo.source.clone();
            source.selection = combo.relation.canonical_selection(domain)?;
            Ok(source)
        })
        .collect()
}

fn canonicalize_sources(validated: &mut ValidatedSources) -> anyhow::Result<()> {
    validated.nr.source.bitmask_carriers.sort_unstable();
    for group in &mut validated.nr.source.bitmask_fingerprints {
        group.carriers.sort_unstable();
    }
    validated
        .nr
        .source
        .bitmask_fingerprints
        .sort_by(|left, right| {
            (left.fingerprint, &left.carriers).cmp(&(right.fingerprint, &right.carriers))
        });

    for (carrier, source) in &mut validated.nr.source.carriers {
        let parsed = &validated.nr.carriers[carrier];
        if let Some(plmns) = &parsed.plmns {
            source.plmns = Some(canonical_plmn_strings(plmns));
        }
    }

    validated.nr.source.dl_features = validated.nr.features.dl.clone();
    validated.nr.source.ul_features = validated.nr.features.ul.clone();
    validated.nr.source.combo = nr_source_combos(
        &validated.nr.combo,
        &validated.nr.domain,
        &validated.nr.features,
    )?;

    validated.lte.source.combo = lte_source_combos(&validated.lte.combo, &validated.lte.domain)?;
    Ok(())
}

pub(crate) fn one_trailing_newline(mut text: String) -> String {
    while text.ends_with('\n') {
        text.pop();
    }
    text.push('\n');
    text
}

fn build_nr_domain(nr: &NrDocument, carriers: &BTreeMap<String, ValidatedCarrier>) -> NrDomain {
    let mut members: BTreeSet<(CompactString, Sku)> = nr
        .bitmask_carriers
        .iter()
        .map(|carrier| (carrier.as_str().into(), Sku::Legacy))
        .collect();
    for (carrier, source) in carriers {
        for anchor in source.profiles.keys().copied() {
            let model_codes = profile_model_codes(anchor);
            if model_codes.is_empty() {
                members.insert((carrier.as_str().into(), Sku::Prime(anchor)));
            } else {
                members.extend(
                    model_codes
                        .into_iter()
                        .map(|code| (carrier.as_str().into(), Sku::Model(code.into()))),
                );
            }
        }
    }
    NrDomain::new(members)
}

fn validate_nr_combos(
    sources: &[NrSourceCombo],
    domain: &NrDomain,
    features: &FeatureCatalogs,
) -> anyhow::Result<Vec<ValidatedNrCombo>> {
    let mut seen = BTreeSet::new();
    let mut validated = Vec::with_capacity(sources.len());
    for (index, source) in sources.iter().enumerate() {
        ensure!(
            !source.sub_blocks.is_empty(),
            "NR combo {} must contain at least one component",
            index + 1
        );
        let mut sub_blocks = source
            .sub_blocks
            .iter()
            .enumerate()
            .map(|(component_index, component)| {
                component.resolve(features).map_err(|error| {
                    let context = format!(
                        "validating NR combo {} component {}: {error}",
                        index + 1,
                        component_index + 1,
                    );
                    error.context(context)
                })
            })
            .collect::<anyhow::Result<Vec<_>>>()?;
        sub_blocks.sort_by_cached_key(|component| RawSubBlockKey::from(component));
        let payload = RawNrPayload {
            power_class: source.power_class,
            bcs_nr: source.bcs_nr,
            bcs_intra_endc: source.bcs_intra_endc,
            bcs_eutra: source.bcs_eutra,
            intra_band_en_dc_support: source.intra_band_en_dc_support,
            sub_blocks,
        };
        ensure!(
            seen.insert(RawNrPayloadKey::from(&payload)),
            "duplicate canonical NR payload record at combo {}",
            index + 1
        );
        let relation = NrRelation::from_selection(domain, source.selection.as_deref())
            .with_context(|| format!("validating NR combo {} selection", index + 1))?;
        validated.push(ValidatedNrCombo { payload, relation });
    }
    validated.sort_by_cached_key(|combo| RawNrPayloadKey::from(&combo.payload));
    Ok(validated)
}

fn validate_lte_files(
    lte: &LteDocument,
) -> anyhow::Result<(BTreeMap<u64, LteFileSource>, LteDomain)> {
    ensure!(!lte.files.is_empty(), "LTE files map must be nonempty");
    let mut files = BTreeMap::new();
    let mut members = BTreeSet::new();
    for (key, source) in &lte.files {
        let id = parse_decimal_key(key, &format!("LTE file key `{key}`"))?;
        files.insert(id, source.clone());
        let model_codes = lte_model_codes(id);
        if model_codes.is_empty() {
            members.insert(Sku::Lte(id));
        } else {
            members.extend(model_codes.into_iter().map(|code| Sku::Model(code.into())));
        }
    }
    Ok((files, LteDomain::new(members)))
}

#[derive(PartialEq, Eq, PartialOrd, Ord)]
struct LtePayloadKey {
    components: Vec<LteSourceComponent>,
    bcs: Option<u64>,
    unknown1: Option<u64>,
    unknown2: Option<u64>,
}

impl From<&LteSourceCombo> for LtePayloadKey {
    fn from(source: &LteSourceCombo) -> Self {
        Self {
            components: source.components.clone(),
            bcs: source.bcs,
            unknown1: source.unknown1,
            unknown2: source.unknown2,
        }
    }
}

fn validate_lte_combos(
    sources: &[LteSourceCombo],
    domain: &LteDomain,
) -> anyhow::Result<Vec<ValidatedLteCombo>> {
    let mut seen = BTreeSet::new();
    let mut validated = Vec::with_capacity(sources.len());
    for (index, source) in sources.iter().enumerate() {
        ensure!(
            !source.components.is_empty(),
            "LTE combo {} must contain at least one component",
            index + 1
        );
        for component in &source.components {
            ensure!(
                component.band > 0,
                "LTE combo {} component band must be positive",
                index + 1
            );
        }
        ensure!(
            seen.insert(LtePayloadKey::from(source)),
            "duplicate canonical LTE payload record at combo {}",
            index + 1
        );
        let relation = LteRelation::from_selection(domain, source.selection.as_deref())
            .with_context(|| format!("validating LTE combo {} selection", index + 1))?;
        validated.push(ValidatedLteCombo {
            source: source.clone(),
            relation,
        });
    }
    Ok(validated)
}

/// Range-checks an optional raw id down to `i32`, naming `field` and `carrier` in the error so
/// a too-large `bitmask_id`/`profiled_id` is traceable to its source. Both fields share this
/// exact check; only the field name in the message differs.
fn checked_i32_id(id: Option<i64>, field: &str, carrier: &str) -> anyhow::Result<Option<i32>> {
    id.map(|id| {
        i32::try_from(id).with_context(|| format!("{field} for carrier `{carrier}` must fit int32"))
    })
    .transpose()
}

/// Enforces the structural rules a carrier's profile/signature/tier/mapping/bitmask fields
/// must jointly satisfy: a profiled carrier needs signature+tier and vice versa, `profiled_id`
/// implies profiles, `mapping_id` and `plmns` imply each other, and every carrier must have at
/// least one role (bitmask membership, profiles, or a PLMN mapping).
fn validate_carrier_role(
    carrier: &str,
    source: &CarrierSource,
    bitmask_carriers: &BTreeSet<&str>,
) -> anyhow::Result<()> {
    let has_profiles = !source.profiles.is_empty();
    if has_profiles {
        ensure!(
            source.signature.is_some(),
            "profiled carrier `{carrier}` requires signature"
        );
        ensure!(
            source.tier.is_some(),
            "profiled carrier `{carrier}` requires tier"
        );
    } else {
        ensure!(
            source.signature.is_none() && source.tier.is_none(),
            "carrier `{carrier}` has signature or tier without profiles"
        );
    }
    ensure!(
        has_profiles || source.profiled_id.is_none(),
        "carrier `{carrier}` has profiled_id but no profiled NR files"
    );
    ensure!(
        source.mapping_id.is_some() == source.plmns.is_some(),
        "carrier `{carrier}` must provide mapping_id and plmns together"
    );
    ensure!(
        bitmask_carriers.contains(carrier) || has_profiles || source.plmns.is_some(),
        "carrier `{carrier}` has no bitmask, profile, or mapping-only role"
    );
    Ok(())
}

/// Encodes a carrier's PLMN list to its packed form, or `None` when the carrier has no PLMN
/// list at all. Each entry's parse error names the offending PLMN and carrier.
fn parse_carrier_plmns(
    plmns: Option<&[String]>,
    carrier: &str,
) -> anyhow::Result<Option<Vec<u64>>> {
    plmns
        .map(|plmns| {
            plmns
                .iter()
                .map(|plmn| {
                    plmn.parse::<Plmn>()
                        .with_context(|| format!("invalid PLMN `{plmn}` for carrier `{carrier}`"))
                        .map(Plmn::to_encoded)
                })
                .collect::<anyhow::Result<Vec<_>>>()
        })
        .transpose()
}

/// Builds one carrier's `anchor -> ValidatedProfile` table: each profile key's filename
/// product (`signature * multiplier`) must round-trip through `matching_anchors` to the SAME
/// anchor it was declared under, or the profile is rejected as ambiguous/mismatched.
/// `signature`/`tier` are pre-validated `Some` by `validate_carrier_role` for any carrier that
/// reaches here with a non-empty `source_profiles`.
fn validated_profiles(
    source_profiles: &BTreeMap<String, ProfileSource>,
    carrier: &str,
    signature: Option<u64>,
    tier: Option<CarrierTier>,
) -> anyhow::Result<BTreeMap<u64, ValidatedProfile>> {
    let mut profiles = BTreeMap::new();
    for (key, profile_source) in source_profiles {
        let anchor =
            parse_decimal_key(key, &format!("profile key `{key}` for carrier `{carrier}`"))?;
        let Some(profile) = PROFILES.iter().find(|profile| profile.anchor == anchor) else {
            bail!("unknown profile anchor {anchor} for carrier `{carrier}`");
        };
        let number = signature
            .expect("profiled carrier signature checked above")
            .checked_mul(profile_source.multiplier.0)
            .with_context(|| {
                format!("filename product overflow for carrier `{carrier}` profile {anchor}")
            })?;
        let matches = matching_anchors(number);
        if matches.len() > 1 {
            bail!(
                "filename product {number} for carrier `{carrier}` profile {anchor} is ambiguous"
            );
        }
        ensure!(
            matches.len() == 1 && matches[0].anchor == anchor,
            "filename product {number} for carrier `{carrier}` has wrong profile anchor; expected {anchor}"
        );
        profiles.insert(
            anchor,
            ValidatedProfile {
                multiplier: profile_source.multiplier.0,
                number,
                unknown: profile_source.unknown.0,
                fingerprint: modern_fingerprint(profile.family, tier.unwrap()),
            },
        );
    }
    Ok(profiles)
}

fn validate_carriers(nr: &NrDocument) -> anyhow::Result<BTreeMap<String, ValidatedCarrier>> {
    let bitmask_carriers: BTreeSet<_> = nr.bitmask_carriers.iter().map(String::as_str).collect();
    let mut normalized_names = BTreeSet::new();
    let mut mapping_ids = BTreeMap::<u64, &str>::new();
    let mut validated = BTreeMap::new();

    for (carrier, source) in &nr.carriers {
        validate_carrier_name(carrier)?;
        ensure!(
            normalized_names.insert(carrier.trim()),
            "duplicate carrier name `{carrier}` after normalization"
        );

        let bitmask_id = checked_i32_id(source.bitmask_id, "bitmask_id", carrier)?;
        if bitmask_id.is_some() {
            ensure!(
                bitmask_carriers.contains(carrier.as_str()),
                "carrier `{carrier}` has bitmask_id but is absent from bitmask_carriers"
            );
        }

        validate_carrier_role(carrier, source, &bitmask_carriers)?;

        let profiled_id = checked_i32_id(source.profiled_id, "profiled_id", carrier)?;

        if let Some(id) = source.mapping_id
            && let Some(previous) = mapping_ids.insert(id, carrier)
        {
            bail!("mapping_id {id} is used by both carrier `{previous}` and `{carrier}`");
        }

        let plmns = parse_carrier_plmns(source.plmns.as_deref(), carrier)?;
        let signature = source.signature.map(|value| value.0);
        let profiles = validated_profiles(&source.profiles, carrier, signature, source.tier)?;

        validated.insert(
            carrier.clone(),
            ValidatedCarrier {
                bitmask_id,
                profiled_id,
                mapping_id: source.mapping_id,
                plmns,
                signature,
                tier: source.tier,
                profiles,
            },
        );
    }
    Ok(validated)
}

fn validate_mapping_projection(
    carriers: &BTreeMap<String, ValidatedCarrier>,
) -> anyhow::Result<()> {
    let mappings = carriers
        .iter()
        .filter_map(|(name, carrier)| {
            carrier.plmns.as_ref().map(|plmns| MappingEntry {
                id: carrier
                    .mapping_id
                    .expect("validated PLMN carrier has mapping_id"),
                name: name.clone(),
                plmns: plmns
                    .iter()
                    .map(|value| {
                        Plmn::from_encoded(*value)
                            .expect("validated PLMN remains within 24 bits")
                            .to_string()
                    })
                    .collect(),
            })
        })
        .collect();
    let root = MappingRoot { mappings };
    let map = root_to_map(&root).context("validating source mapping projection")?;
    map_to_root(&map).context("round-tripping source mapping projection")?;
    Ok(())
}

/// The modern (profiled) fingerprint for a `(family, tier)` pair. Thin wrapper over the one
/// source `model::fingerprint_for` (from `model::FINGERPRINTS`), shared by the NR generation
/// and schema-validation paths so the table lives in exactly one place.
pub(crate) fn modern_fingerprint(family: Family, tier: CarrierTier) -> u64 {
    crate::model::fingerprint_for(family, tier.to_model())
        .expect("every (family, tier) pair is present in model::FINGERPRINTS")
}

fn validate_fingerprint_partition(nr: &NrDocument) -> anyhow::Result<BTreeMap<String, u64>> {
    ensure!(
        !nr.bitmask_carriers.is_empty(),
        "bitmask_carriers must be nonempty"
    );
    let mut whitelist = BTreeSet::new();
    for carrier in &nr.bitmask_carriers {
        validate_carrier_name(carrier)?;
        ensure!(
            whitelist.insert(carrier.as_str()),
            "duplicate carrier `{carrier}` in bitmask_carriers"
        );
    }

    let mut fingerprints = BTreeSet::new();
    let mut assigned = BTreeSet::new();
    let mut by_carrier = BTreeMap::new();
    for group in &nr.bitmask_fingerprints {
        ensure!(
            fingerprints.insert(group.fingerprint),
            "duplicate fingerprint {} in bitmask_fingerprint",
            group.fingerprint
        );
        ensure!(
            !group.carriers.is_empty(),
            "bitmask fingerprint {} must have a nonempty carrier list",
            group.fingerprint
        );
        for carrier in &group.carriers {
            validate_carrier_name(carrier)?;
            ensure!(
                whitelist.contains(carrier.as_str()),
                "fingerprint carrier `{carrier}` is not a bitmask carrier"
            );
            ensure!(
                assigned.insert(carrier.as_str()),
                "carrier `{carrier}` belongs to more than one fingerprint group"
            );
            by_carrier.insert(carrier.clone(), group.fingerprint);
        }
    }
    ensure!(
        assigned == whitelist,
        "bitmask_fingerprint groups must partition bitmask_carriers exactly"
    );
    Ok(by_carrier)
}

/// A carrier name must be a nonempty, already-trimmed canonical identifier. Shared by the
/// compiler's NR ingestion (`nr.rs`) and schema validation.
pub(crate) fn validate_carrier_name(carrier: &str) -> anyhow::Result<()> {
    ensure!(!carrier.is_empty(), "carrier names must be nonempty");
    ensure!(
        carrier.trim() == carrier,
        "carrier name `{carrier}` is not canonical"
    );
    Ok(())
}

fn parse_decimal_key(value: &str, description: &str) -> anyhow::Result<u64> {
    super::parse_shortest_u64(value)
        .with_context(|| format!("{description} must be a shortest-decimal u64"))
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use crate::compiler::{features::DlFeatureSource, lte_from_kdl, nr_from_kdl, selection::Sku};

    use super::{parse_sources, to_kdl};

    const MINIMAL_NR: &str = r#"
version 1
bitmask-carriers "LEGACY"

bitmask-fingerprint 715188856 {
    carriers "LEGACY"
}
"#;

    const MINIMAL_LTE: &str = r#"
version 1

file "400907661" fingerprint=862505271 bitmask=4082165014
"#;

    fn profiled_nr(profile_key: &str) -> String {
        format!(
            r#"
version 1
bitmask-carriers "LEGACY"

bitmask-fingerprint 715188856 {{
    carriers "LEGACY"
}}

carrier "PROFILED" profiled-id=7 signature=1 tier="main" {{
    profile "{profile_key}" multiplier=66813533 unknown=0
}}
"#
        )
    }

    fn lte_with_file_key(file_key: &str) -> String {
        format!(
            r#"
version 1

file "{file_key}" fingerprint=862505271 bitmask=4082165014
"#
        )
    }

    fn nr_with_carrier_sections(sections: &str) -> String {
        format!(
            r#"
version 1
bitmask-carriers "LEGACY"

bitmask-fingerprint 715188856 {{
    carriers "LEGACY"
}}

{sections}
"#
        )
    }

    fn nr_with_complete_domain() -> String {
        nr_with_carrier_sections(
            r#"
carrier "PROFILED" profiled-id=7 signature=1 tier="main" {
    profile "66813533" multiplier=66813533 unknown=0
    profile "8969" multiplier=8969 unknown=0
}

carrier "MAPPING" mapping-id=8 {
    plmns
}
"#,
        )
    }

    fn lte_with_complete_domain() -> String {
        r#"
version 1

file "400907661" fingerprint=862505271 bitmask=1
file "564260317" fingerprint=874888686 bitmask=2
"#
        .into()
    }

    #[test]
    fn parses_the_minimal_version_one_documents() {
        parse_sources(MINIMAL_NR, MINIMAL_LTE).unwrap();
    }

    #[test]
    fn carrier_ids_are_independent_and_mapping_ids_alone_are_unique() {
        let nr = format!(
            r#"{MINIMAL_NR}
carrier "A" profiled-id=0 mapping-id=7 signature=1 tier="main" {{
    plmns
    profile "66813533" multiplier=66813533 unknown=0
}}

carrier "B" profiled-id=0 mapping-id=8 signature=1 tier="main" {{
    plmns
    profile "66813533" multiplier=66813533 unknown=0
}}
"#
        );
        let parsed = parse_sources(&nr, MINIMAL_LTE).unwrap();
        assert_eq!(parsed.nr.carriers["A"].profiled_id, Some(0));
        assert_eq!(parsed.nr.carriers["B"].profiled_id, Some(0));
        assert_eq!(parsed.nr.carriers["A"].mapping_id, Some(7));
        assert_eq!(parsed.nr.carriers["B"].mapping_id, Some(8));
    }

    #[test]
    fn mapping_id_requires_plmns_and_must_be_unique() {
        let missing = format!("{MINIMAL_NR}\ncarrier \"MAP\" mapping-id=7\n");
        let error = parse_sources(&missing, MINIMAL_LTE)
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("must provide mapping_id and plmns together"),
            "{error}"
        );

        let missing_id = format!("{MINIMAL_NR}\ncarrier \"MAP\" {{\n    plmns\n}}\n");
        let error = parse_sources(&missing_id, MINIMAL_LTE)
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("must provide mapping_id and plmns together"),
            "{error}"
        );

        let duplicate = format!(
            "{MINIMAL_NR}\ncarrier \"A\" mapping-id=7 {{\n    plmns\n}}\ncarrier \"B\" mapping-id=7 {{\n    plmns\n}}\n"
        );
        let error = parse_sources(&duplicate, MINIMAL_LTE)
            .unwrap_err()
            .to_string();
        assert!(error.contains("mapping_id 7 is used by both"), "{error}");
    }

    #[test]
    fn canonical_feature_catalogs_prune_deduplicate_sort_and_renumber() {
        let nr = format!(
            "{MINIMAL_NR}\n\
             dl-feature max-scs=3\n\
             dl-feature max-scs=1\n\
             dl-feature max-scs=3\n\
             ul-feature max-scs=9\n\
             combo {{\n    nr 77 dl-bw-class=1 dl-feature=1\n}}\n\
             combo {{\n    nr 78 dl-bw-class=1 dl-feature=3\n}}\n"
        );
        let parsed = parse_sources(&nr, MINIMAL_LTE).unwrap();
        let (canonical, _) = to_kdl(&parsed.nr.source, &parsed.lte.source).unwrap();
        assert_eq!(parsed.nr.features.dl.len(), 1);
        assert!(parsed.nr.features.ul.is_empty());
        assert_eq!(parsed.nr.features.dl[0].max_scs, Some(3));
        assert!(canonical.contains("dl-feature max-scs=3"), "{canonical}");
        assert!(!canonical.contains("max-scs=1"), "{canonical}");
        assert!(!canonical.contains("ul-feature"), "{canonical}");
        assert!(canonical.contains("dl-feature=1"), "{canonical}");
    }

    #[test]
    fn feature_catalog_preserves_referenced_absent_and_explicit_zero_records() {
        let nr = format!(
            "{MINIMAL_NR}\n\
             dl-feature\n\
             dl-feature max-scs=0\n\
             combo {{\n    nr 77 dl-bw-class=1 dl-feature=2\n}}\n\
             combo {{\n    nr 78 dl-bw-class=1 dl-feature=1\n}}\n"
        );
        let parsed = parse_sources(&nr, MINIMAL_LTE).unwrap();
        assert_eq!(parsed.nr.features.dl.len(), 2);
        assert_eq!(parsed.nr.features.dl[0], DlFeatureSource::default());
        assert_eq!(parsed.nr.features.dl[1].max_scs, Some(0));
    }

    #[test]
    fn feature_references_reject_zero_out_of_range_and_old_inline_fields() {
        for (cc_line, expected) in [
            ("nr 78 dl-feature=0", "dl_feature index must be 1-based"),
            ("nr 78 dl-feature=2", "exceeds the dl catalog length 1"),
            ("nr 78 ul-feature=0", "ul_feature index must be 1-based"),
            ("nr 78 ul-feature=2", "exceeds the ul catalog length 1"),
        ] {
            let nr = format!(
                "{MINIMAL_NR}\ndl-feature max-scs=3\nul-feature max-scs=4\ncombo {{\n    {cc_line}\n}}\n"
            );
            let error = parse_sources(&nr, MINIMAL_LTE).unwrap_err().to_string();
            assert!(error.contains(expected), "{error}");
        }

        let old = format!("{MINIMAL_NR}\ncombo {{\n    nr 78 dl-max-scs=3\n}}\n");
        assert!(nr_from_kdl(&old).is_err());
    }

    #[test]
    fn global_source_catalog_can_exceed_one_byte() {
        let mut nr = MINIMAL_NR.to_string();
        for value in 1..=300 {
            nr.push_str(&format!("\ndl-feature max-bw={value}\n"));
            nr.push_str(&format!(
                "\ncombo {{\n    nr {value} dl-bw-class=1 dl-feature={value}\n}}\n"
            ));
        }
        let parsed = parse_sources(&nr, MINIMAL_LTE).unwrap();
        assert_eq!(parsed.nr.features.dl.len(), 300);
    }

    #[test]
    fn versions_are_required_and_only_version_one_is_supported() {
        let missing = MINIMAL_NR.replacen("version 1\n", "", 1);
        assert!(parse_sources(&missing, MINIMAL_LTE).is_err());

        let unsupported_nr = MINIMAL_NR.replacen("version 1", "version 2", 1);
        assert!(
            parse_sources(&unsupported_nr, MINIMAL_LTE)
                .unwrap_err()
                .to_string()
                .contains("unsupported nr.kdl version")
        );

        let missing = MINIMAL_LTE.replacen("version 1\n", "", 1);
        assert!(parse_sources(MINIMAL_NR, &missing).is_err());

        let unsupported_lte = MINIMAL_LTE.replacen("version 1", "version 2", 1);
        assert!(
            parse_sources(MINIMAL_NR, &unsupported_lte)
                .unwrap_err()
                .to_string()
                .contains("unsupported lte.kdl version")
        );
    }

    #[test]
    fn numeric_map_keys_use_shortest_decimal_u64_syntax() {
        parse_sources(&profiled_nr("66813533"), MINIMAL_LTE).unwrap();
        parse_sources(MINIMAL_NR, &lte_with_file_key("400907661")).unwrap();

        for key in ["066813533", "+66813533", " 66813533", "66813533 ", "anchor"] {
            let error = parse_sources(&profiled_nr(key), MINIMAL_LTE)
                .unwrap_err()
                .to_string();
            assert!(error.contains("profile key"), "{key:?}: {error}");
        }

        for key in [
            "0400907661",
            "+400907661",
            " 400907661",
            "400907661 ",
            "file",
        ] {
            let error = parse_sources(MINIMAL_NR, &lte_with_file_key(key))
                .unwrap_err()
                .to_string();
            assert!(error.contains("LTE file key"), "{key:?}: {error}");
        }
    }

    fn assert_nr_error(nr: &str, needle: &str) {
        let error = parse_sources(nr, MINIMAL_LTE).unwrap_err().to_string();
        assert!(error.contains(needle), "expected {needle:?} in {error:?}");
    }

    #[test]
    fn fingerprint_groups_are_nonempty_disjoint_and_exhaustive() {
        assert_nr_error(
            r#"
version 1
bitmask-carriers "A"
bitmask-fingerprint 1 {
    carriers
}
"#,
            "nonempty",
        );

        assert_nr_error(
            r#"
version 1
bitmask-carriers "A" "B"
bitmask-fingerprint 1 {
    carriers "A"
}
bitmask-fingerprint 2 {
    carriers "A" "B"
}
"#,
            "more than one fingerprint",
        );

        assert_nr_error(
            r#"
version 1
bitmask-carriers "A" "B"
bitmask-fingerprint 1 {
    carriers "A"
}
"#,
            "partition",
        );

        assert_nr_error(
            r#"
version 1
bitmask-carriers "A"
bitmask-fingerprint 1 {
    carriers "A" "B"
}
"#,
            "not a bitmask carrier",
        );

        assert_nr_error(
            r#"
version 1
bitmask-carriers "A" "A"
bitmask-fingerprint 1 {
    carriers "A"
}
"#,
            "duplicate carrier",
        );

        assert_nr_error(
            r#"
version 1
bitmask-carriers "A"
bitmask-fingerprint 1 {
    carriers "A"
}
bitmask-fingerprint 1 {
    carriers "A"
}
"#,
            "duplicate fingerprint",
        );
    }

    #[test]
    fn carrier_shapes_and_id_ranges_are_validated() {
        assert_nr_error(
            &nr_with_carrier_sections(
                r#"
carrier "PROFILED" tier="main" {
    profile "66813533" multiplier=66813533 unknown=0
}
"#,
            ),
            "signature",
        );
        assert_nr_error(
            &nr_with_carrier_sections(
                r#"
carrier "PROFILED" signature=1 {
    profile "66813533" multiplier=66813533 unknown=0
}
"#,
            ),
            "tier",
        );
        assert_nr_error(
            &nr_with_carrier_sections(
                r#"
carrier "ORPHAN" signature=1 tier="main"
"#,
            ),
            "without profiles",
        );
        assert_nr_error(
            &nr_with_carrier_sections(
                r#"
carrier "MAPPING" {
    plmns
}
"#,
            ),
            "must provide mapping_id and plmns together",
        );
        assert_nr_error(
            &nr_with_carrier_sections(
                r#"
carrier "MAPPING" mapping-id=7
"#,
            ),
            "must provide mapping_id and plmns together",
        );
        assert_nr_error(
            &nr_with_carrier_sections(
                r#"
carrier "MAPPING" profiled-id=7
"#,
            ),
            "has profiled_id but no profiled NR files",
        );
        assert_nr_error(
            &nr_with_carrier_sections(
                r#"
carrier "NOT_LEGACY" bitmask-id=1
"#,
            ),
            "bitmask_carriers",
        );
        assert_nr_error(
            &nr_with_carrier_sections(
                r#"
carrier "LEGACY" bitmask-id=2147483648
"#,
            ),
            "int32",
        );
        assert_nr_error(
            &nr_with_carrier_sections(
                r#"
carrier "PROFILED" profiled-id=2147483648 signature=1 tier="main" {
    profile "66813533" multiplier=66813533 unknown=0
}
"#,
            ),
            "int32",
        );

        parse_sources(
            &nr_with_carrier_sections(
                r#"
carrier "MAPPING" mapping-id=18446744073709551615 {
    plmns
}
"#,
            ),
            MINIMAL_LTE,
        )
        .unwrap();
    }

    #[test]
    fn profile_products_resolve_exactly_the_keyed_registered_anchor() {
        parse_sources(&profiled_nr("66813533"), MINIMAL_LTE).unwrap();

        assert_nr_error(&profiled_nr("123"), "unknown profile anchor");
        assert_nr_error(
            &profiled_nr("66813533").replace("multiplier=66813533", "multiplier=1176929627"),
            "wrong profile anchor",
        );
        assert_nr_error(
            &nr_with_carrier_sections(
                r#"
carrier "PROFILED" signature=1 tier="main" {
    profile "167" multiplier=308449 unknown=0
}
"#,
            ),
            "ambiguous",
        );
        assert_nr_error(
            &nr_with_carrier_sections(
                r#"
carrier "PROFILED" signature=18446744073709551615 tier="main" {
    profile "167" multiplier=2 unknown=0
}
"#,
            ),
            "overflow",
        );
    }

    #[test]
    fn plmns_are_parsed_but_order_and_duplicates_are_legal() {
        parse_sources(
            &nr_with_carrier_sections(
                r#"
carrier "MAPPING" mapping-id=7 {
    plmn mcc=302 mnc=220
    plmn mcc=250 mnc=1
    plmn mcc=302 mnc=220
}
"#,
            ),
            MINIMAL_LTE,
        )
        .unwrap();

        // A syntactically well-formed `plmn` node whose mcc/mnc reconstruct into a string
        // `Plmn::from_str` rejects (mnc has more than 3 digits) now fails inside
        // `nr_from_kdl` itself (`read_plmn` re-validates), not in the later semantic
        // carrier validation, so unwrap the full chain rather than using
        // `assert_nr_error`'s plain (unwrapped) `Display`.
        let error = format!(
            "{:#}",
            parse_sources(
                &nr_with_carrier_sections(
                    r#"
carrier "MAPPING" mapping-id=7 {
    plmn mcc=302 mnc=99999
}
"#,
                ),
                MINIMAL_LTE,
            )
            .unwrap_err()
        );
        assert!(error.contains("PLMN"), "{error}");
    }

    #[test]
    fn modern_nr_bitmasks_cannot_be_stored_in_source() {
        let nr = format!("{MINIMAL_NR}\ncombo bitmask=1 {{\n    nr 78\n}}\n");
        assert!(parse_sources(&nr, MINIMAL_LTE).is_err());
    }

    #[test]
    fn domains_expand_only_registered_models_or_one_synthetic_token() {
        let sources =
            parse_sources(&nr_with_complete_domain(), &lte_with_complete_domain()).unwrap();
        let nr = sources.nr.domain.denorm_members();
        assert_eq!(
            nr,
            BTreeSet::from([
                ("LEGACY".into(), Sku::Legacy),
                ("PROFILED".into(), Sku::Model("G2YBB".into())),
                ("PROFILED".into(), Sku::Prime(8969)),
            ])
        );
        assert!(nr.iter().all(|(carrier, _)| carrier != "MAPPING"));

        let lte: BTreeSet<_> = sources.lte.domain.iter().cloned().collect();
        assert_eq!(
            lte,
            BTreeSet::from([
                Sku::Model("G2YBB".into()),
                Sku::Model("GGX8B".into()),
                Sku::Model("GR83Y".into()),
                Sku::Lte(564_260_317),
            ])
        );
    }

    #[test]
    fn selections_are_resolved_and_cached_during_validation() {
        let nr = format!(
            "{}\ncombo {{\n    selection {{\n        carriers \"PROFILED\"\n        skus \"G2YBB\"\n    }}\n    nr 78\n}}\n",
            nr_with_complete_domain()
        );
        let lte = format!(
            "{}\ncombo {{\n    selection {{\n        skus \"G2YBB\" \"lte:564260317\"\n    }}\n    subblock 1 dl-bw-class-mimo=32768\n}}\n",
            lte_with_complete_domain()
        );
        let sources = parse_sources(&nr, &lte).unwrap();
        assert_eq!(
            sources
                .nr
                .domain
                .denorm_relation(&sources.nr.combo[0].relation),
            BTreeSet::from([("PROFILED".into(), Sku::Model("G2YBB".into()))])
        );
        assert_eq!(
            sources.lte.combo[0]
                .relation
                .iter()
                .cloned()
                .collect::<BTreeSet<_>>(),
            BTreeSet::from([Sku::Model("G2YBB".into()), Sku::Lte(564_260_317)])
        );
    }

    #[test]
    fn nr_payloads_require_valid_components_and_canonicalize_them() {
        for (catalog, combo_body) in [("", ""), ("", "nr 0\n"), ("", "lte 1 srs-tx-switch=5\n")] {
            let nr = format!("{MINIMAL_NR}\n{catalog}combo {{\n{combo_body}}}\n");
            assert!(
                parse_sources(&nr, MINIMAL_LTE).is_err(),
                "accepted {combo_body:?}"
            );
        }

        let nr = format!(
            "{MINIMAL_NR}\ndl-feature max-scs=3\ncombo {{\n    nr 78 dl-bw-class=1 dl-feature=1\n    lte 1\n}}\n"
        );
        let sources = parse_sources(&nr, MINIMAL_LTE).unwrap();
        let cc = &sources.nr.combo[0].payload.sub_blocks;
        assert_eq!(
            cc.iter().map(|cc| cc.band_label()).collect::<Vec<_>>(),
            ["B1", "n78"]
        );
        assert_eq!(cc[1].dl_selector(), None);
    }

    #[test]
    fn duplicate_canonical_nr_payload_records_are_rejected() {
        let nr = format!(
            "{MINIMAL_NR}\ndl-feature max-scs=3\ndl-feature max-scs=3\ncombo {{\n    nr 78 dl-bw-class=1 dl-feature=1\n    lte 1\n}}\ncombo {{\n    lte 1\n    nr 78 dl-bw-class=1 dl-feature=2\n}}\n"
        );
        assert_nr_error(&nr, "duplicate canonical NR payload");
    }

    #[test]
    fn lte_payloads_require_components_preserve_order_and_reject_exact_duplicates() {
        let empty = format!("{MINIMAL_LTE}\ncombo {{\n}}\n");
        assert!(parse_sources(MINIMAL_NR, &empty).is_err());

        let duplicate = format!(
            "{MINIMAL_LTE}\ncombo {{\n    subblock 1 dl-bw-class-mimo=32768\n}}\ncombo {{\n    subblock 1 dl-bw-class-mimo=32768\n}}\n"
        );
        assert!(
            parse_sources(MINIMAL_NR, &duplicate)
                .unwrap_err()
                .to_string()
                .contains("duplicate canonical LTE payload")
        );

        let ordered = format!(
            "{MINIMAL_LTE}\ncombo {{\n    subblock 1 dl-bw-class-mimo=1\n    subblock 3 dl-bw-class-mimo=3\n}}\ncombo {{\n    subblock 3 dl-bw-class-mimo=3\n    subblock 1 dl-bw-class-mimo=1\n}}\n"
        );
        let sources = parse_sources(MINIMAL_NR, &ordered).unwrap();
        assert_eq!(sources.lte.combo.len(), 2);
        assert_eq!(sources.lte.combo[0].source.components[0].band, 1);
        assert_eq!(sources.lte.combo[1].source.components[0].band, 3);

        let optional_presence = format!(
            "{MINIMAL_LTE}\ncombo {{\n    subblock 1 dl-bw-class-mimo=1\n}}\ncombo bcs=0 {{\n    subblock 1 dl-bw-class-mimo=1 ul-bw-class-mimo=0\n}}\n"
        );
        let sources = parse_sources(MINIMAL_NR, &optional_presence).unwrap();
        assert_eq!(sources.lte.combo.len(), 2);
        assert_eq!(sources.lte.combo[0].source.bcs, None);
        assert_eq!(sources.lte.combo[1].source.bcs, Some(0));
        assert_eq!(
            sources.lte.combo[1].source.components[0].ul_bw_class_mimo,
            Some(0)
        );
    }

    #[test]
    fn validated_metadata_caches_derived_fingerprints_and_parsed_plmns() {
        let nr = nr_with_carrier_sections(
            r#"
carrier "PROFILED" profiled-id=7 mapping-id=7 signature=1 tier="alt" {
    plmn mcc=302 mnc=220
    plmn mcc=250 mnc=1
    plmn mcc=302 mnc=220
    profile "66813533" multiplier=66813533 unknown=9
}
"#,
        );
        let sources = parse_sources(&nr, MINIMAL_LTE).unwrap();
        assert_eq!(sources.nr.bitmask_fingerprints["LEGACY"], 715_188_856);
        let carrier = &sources.nr.carriers["PROFILED"];
        assert_eq!(carrier.plmns, Some(vec![197_154, 5_435_408, 197_154]));
        let profile = &carrier.profiles[&66_813_533];
        assert_eq!(profile.number, 66_813_533);
        assert_eq!(profile.fingerprint, 627_223_094);
        assert_eq!(profile.unknown, 9);
    }

    #[test]
    fn to_kdl_canonicalizes_metadata_payloads_and_selections() {
        let nr_text = format!(
            "{}\ndl-feature max-scs=3\ncombo {{\n    selection {{\n        carriers \"PROFILED\" \"PROFILED\"\n        skus \"G2YBB\" \"G2YBB\"\n    }}\n    nr 78 dl-bw-class=1 dl-feature=1\n    lte 1\n}}\ncombo {{\n    selection {{\n        carriers \"LEGACY\"\n    }}\n    lte 3\n}}\n",
            nr_with_complete_domain()
        );
        let lte_text = format!(
            "{}\ncombo {{\n    selection {{\n        skus \"lte:564260317\" \"G2YBB\" \"G2YBB\"\n    }}\n    subblock 3 dl-bw-class-mimo=3\n    subblock 1 dl-bw-class-mimo=1\n}}\ncombo {{\n    selection {{\n        skus \"GR83Y\"\n    }}\n    subblock 7 dl-bw-class-mimo=7\n}}\n",
            lte_with_complete_domain()
        );
        let nr = nr_from_kdl(&nr_text).unwrap();
        let lte = lte_from_kdl(&lte_text).unwrap();
        let (nr_text, lte_text) = to_kdl(&nr, &lte).unwrap();

        assert!(nr_text.ends_with('\n') && !nr_text.ends_with("\n\n"));
        assert!(lte_text.ends_with('\n') && !lte_text.ends_with("\n\n"));

        let canonical = parse_sources(&nr_text, &lte_text).unwrap();
        assert_eq!(
            canonical.nr.combo[0]
                .payload
                .sub_blocks
                .iter()
                .map(|cc| cc.band_label())
                .collect::<Vec<_>>(),
            ["B1", "n78"]
        );
        assert_eq!(
            canonical.nr.combo[0].payload.sub_blocks[1].dl_selector(),
            None
        );
        assert_eq!(
            canonical.nr.combo[1].payload.sub_blocks[0].band_label(),
            "B3"
        );
        assert_eq!(canonical.lte.combo[0].source.components[0].band, 3);
        assert_eq!(canonical.lte.combo[1].source.components[0].band, 7);

        let canonical_nr = nr_from_kdl(&nr_text).unwrap();
        let canonical_lte = lte_from_kdl(&lte_text).unwrap();
        assert_eq!(
            to_kdl(&canonical_nr, &canonical_lte).unwrap(),
            (nr_text, lte_text)
        );
    }

    #[test]
    fn to_kdl_preserves_plmn_presence_order_duplicates_and_large_mapping_ids() {
        let nr_text = nr_with_carrier_sections(
            r#"
carrier "ABSENT" bitmask-id=1

carrier "MAP_ONLY" mapping-id=18446744073709551615 {
    plmns
}

carrier "ORDERED" mapping-id=7 {
    plmn mcc=302 mnc=220
    plmn mcc=228
    plmn mcc=302 mnc=220
}
"#,
        )
        .replace(
            "bitmask-carriers \"LEGACY\"",
            "bitmask-carriers \"ABSENT\" \"LEGACY\"",
        )
        .replace("carriers \"LEGACY\"", "carriers \"ABSENT\" \"LEGACY\"");
        let nr = nr_from_kdl(&nr_text).unwrap();
        let lte = lte_from_kdl(MINIMAL_LTE).unwrap();
        let (nr_text, _) = to_kdl(&nr, &lte).unwrap();
        let nr = nr_from_kdl(&nr_text).unwrap();
        assert_eq!(nr.carriers["ABSENT"].plmns, None);
        assert_eq!(nr.carriers["MAP_ONLY"].plmns, Some(Vec::new()));
        assert_eq!(
            nr.carriers["ORDERED"].plmns,
            Some(vec!["302-220".into(), "228-ff".into(), "302-220".into()])
        );
        assert_eq!(nr.carriers["MAP_ONLY"].profiled_id, None);
        assert_eq!(nr.carriers["MAP_ONLY"].mapping_id, Some(u64::MAX));
        assert!(nr_text.contains("mapping-id=18446744073709551615"));
    }
}
