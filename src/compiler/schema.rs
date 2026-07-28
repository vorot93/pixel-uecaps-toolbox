use std::collections::{BTreeMap, BTreeSet, HashMap};

use anyhow::{Context, bail, ensure};
use compact_str::CompactString;

use super::{
    features::{FeatureCatalogs, NrSourceSubBlock},
    selection::{
        CarrierId, LteDomain, LteRelation, NrDomain, NrRelation, SelectionRect, Sku, SkuId,
    },
};
use crate::{
    compiler::lte::RawLteCombo,
    mapping::{MappingEntry, MappingRoot, Plmn, map_to_root, root_to_map},
    model::{Family, PROFILES, Tier, lte_model_codes, matching_anchors, profile_model_codes},
    proto::{LteComponent, ShannonFeatureSetDlPerCcNr, ShannonFeatureSetUlPerCcNr},
    raw_nr::{RawNrPayload, RawNrPayloadKey, RawSubBlockKey},
};

/// The source-document format version both `nr.kdl` and `lte.kdl` carry.
///
/// This number identifies *the* format, not a count of revisions — it is reset rather than
/// advanced when a format change lands in an unpublished series, because this repo's history is
/// squashable and a reader at HEAD must see one coherent state. The check below is an
/// inequality, not an ordering, so a reset still rejects a tree from any build that emitted a
/// different number. That is also why `kdl_keys::nr_doc::VERSION` is the one key left
/// unabbreviated — the marker announcing the version cannot be renamed by the change it
/// describes, or the check below would be unreachable for exactly the documents it exists to
/// diagnose.
pub(crate) const SOURCE_FORMAT_VERSION: u32 = 1;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct DecimalU64(pub(crate) u64);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CarrierTier {
    Main,
    Alt,
}

/// `CarrierTier` (the source-format spelling) and `model::Tier` are the same two-valued
/// distinction, so the mapping is a pair of `From` impls rather than a `to_model` method here
/// and a free `source_tier` function in another module.
impl From<CarrierTier> for Tier {
    fn from(tier: CarrierTier) -> Self {
        match tier {
            CarrierTier::Main => Self::Main,
            CarrierTier::Alt => Self::Alt,
        }
    }
}

impl From<Tier> for CarrierTier {
    fn from(tier: Tier) -> Self {
        match tier {
            Tier::Main => Self::Main,
            Tier::Alt => Self::Alt,
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
    pub(crate) dl_features: Vec<ShannonFeatureSetDlPerCcNr>,
    pub(crate) ul_features: Vec<ShannonFeatureSetUlPerCcNr>,
    pub(crate) combo: Vec<NrSourceCombo>,
}

#[derive(Clone, Debug)]
pub(crate) struct LteFileSource {
    pub(crate) fingerprint: u64,
    pub(crate) bitmask: u64,
}

#[derive(Clone, Debug)]
pub(crate) struct LteSourceCombo {
    pub(crate) selection: Option<Vec<SelectionRect>>,
    pub(crate) bcs: Option<u64>,
    pub(crate) unknown1: Option<u64>,
    pub(crate) unknown2: Option<u64>,
    pub(crate) components: Vec<LteComponent>,
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
            let combo_index = u32::try_from(combo_index).expect("c count fits in u32");
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
    /// Test-only combo surgery: replace `combo` and rebuild every field derived from it, so
    /// they stay consistent. Production sets them once in [`validate_documents`]; because
    /// `selection_index` points at `combo` positions, any post-construction combo replacement
    /// must go through here or those indices dangle.
    ///
    /// "Every field derived from it" is five, not two. Besides `features` and
    /// `selection_index`, `canonicalize_sources` also derives `source.combo`,
    /// `source.dl_features` and `source.ul_features` — and `to_kdl` serializes `source`, while
    /// generation reads `features`. Rebuilding only the first two left a test that performed
    /// combo surgery and then serialized emitting the *pre-surgery* document, with the emitted
    /// catalog disagreeing with the catalog generation actually used, on the byte-identity
    /// critical path.
    pub(crate) fn set_combos(&mut self, combo: Vec<ValidatedNrCombo>) {
        self.features = FeatureCatalogs::from_payloads(combo.iter().map(|combo| &combo.payload));
        self.selection_index = NrSelectionIndex::build(&combo);
        self.combo = combo;
        self.source.dl_features = self.features.dl.clone();
        self.source.ul_features = self.features.ul.clone();
        self.source.combo = nr_source_combos(&self.combo, &self.domain, &self.features)
            .expect("c surgery must produce serializable source combos");
    }
}

/// A carrier after validation, shaped the way `validate_carrier_role` proved it: the two
/// legend fields imply each other, and so do signature/tier/profiles. Keeping them as
/// independent `Option`s meant five downstream sites had to recover the proof with
/// `expect`/`unwrap`; as paired sub-structs the proof travels with the data.
#[derive(Clone, Debug)]
pub(crate) struct ValidatedCarrier {
    pub(crate) bitmask_id: Option<i32>,
    pub(crate) profiled_id: Option<i32>,
    /// The carrier's PLMN-legend entry, present iff it has one.
    legend: Option<LegendEntry>,
    /// The carrier's profiled role, present iff it ships profile files.
    pub(crate) profiled: Option<ProfiledRole>,
}

/// A carrier's entry in the PLMN legend. `mapping_id` and `plmns` are validated to imply each
/// other, so they are never separately absent.
#[derive(Clone, Debug)]
struct LegendEntry {
    mapping_id: u64,
    plmns: Vec<Plmn>,
}

/// A carrier's profiled role. Signature, tier, and a non-empty profile table are validated to
/// imply one another.
#[derive(Clone, Debug)]
pub(crate) struct ProfiledRole {
    pub(crate) signature: u64,
    pub(crate) tier: CarrierTier,
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
            super::lte_to_kdl(&self.lte.source)?,
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
    ensure!(
        nr.version == SOURCE_FORMAT_VERSION,
        "nr.kdl is source-format version {} but this build reads version {SOURCE_FORMAT_VERSION}; \
         re-run `decompose` to regenerate it",
        nr.version
    );
    ensure!(
        lte.version == SOURCE_FORMAT_VERSION,
        "lte.kdl is source-format version {} but this build reads version {SOURCE_FORMAT_VERSION}; \
         re-run `decompose` to regenerate it",
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

/// A carrier's PLMNs in their canonical `mcc-mnc` spelling — what `nr.kdl` stores. `Plmn` has
/// exactly one rendering, so this is `Display`, not a re-derivation that could fail.
fn canonical_plmn_strings(plmns: &[Plmn]) -> Vec<String> {
    plmns.iter().map(Plmn::to_string).collect()
}

/// One `nr.kdl` combo projected from a payload and its relation: the selection in canonical
/// rectangle form, the five combo-header fields verbatim, and per-component catalog references
/// re-derived via `source_sub_block`. The write-side inverse of `validate_nr_combos`/`resolve`.
///
/// The single source for this projection — both the ingest side (`compiler::nr`'s
/// `finish_nr_document`, building the document for the first time) and the canonicalize side
/// ([`nr_source_combos`], rebuilding it from validated data) go through it, so a sixth
/// combo-header field cannot be added to one and forgotten in the other.
pub(crate) fn nr_source_combo(
    payload: &RawNrPayload,
    relation: &NrRelation,
    domain: &NrDomain,
    features: &FeatureCatalogs,
) -> anyhow::Result<NrSourceCombo> {
    Ok(NrSourceCombo {
        selection: relation.canonical_selection(domain)?,
        power_class: payload.power_class,
        bcs_nr: payload.bcs_nr,
        bcs_intra_endc: payload.bcs_intra_endc,
        bcs_eutra: payload.bcs_eutra,
        intra_band_en_dc_support: payload.intra_band_en_dc_support,
        sub_blocks: payload
            .sub_blocks
            .iter()
            .map(|component| features.source_sub_block(component))
            .collect(),
    })
}

fn nr_source_combos(
    combo: &[ValidatedNrCombo],
    domain: &NrDomain,
    features: &FeatureCatalogs,
) -> anyhow::Result<Vec<NrSourceCombo>> {
    combo
        .iter()
        .map(|combo| nr_source_combo(&combo.payload, &combo.relation, domain, features))
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
        if let Some(legend) = &parsed.legend {
            source.plmns = Some(canonical_plmn_strings(&legend.plmns));
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

fn build_nr_domain(nr: &NrDocument, carriers: &BTreeMap<String, ValidatedCarrier>) -> NrDomain {
    let mut members: BTreeSet<(CompactString, Sku)> = nr
        .bitmask_carriers
        .iter()
        .map(|carrier| (carrier.as_str().into(), Sku::Legacy))
        .collect();
    for (carrier, source) in carriers {
        let anchors = source
            .profiled
            .iter()
            .flat_map(|profiled| profiled.profiles.keys().copied());
        for anchor in anchors {
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

impl From<&LteSourceCombo> for RawLteCombo {
    /// A source combo minus its `selection` — the payload identity `validate_lte_combos`
    /// dedups on. Shares the type (and therefore the `Ord`) with `compiler::lte`'s ingest.
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
            // Neither state is representable in `lte.kdl`, and neither occurs in the corpus.
            // Rejecting here — ahead of `to_kdl` — makes the omit-when-0 rule value-faithful by
            // construction rather than by assumption, and gives a message naming the component
            // instead of the codec's "value 0 has no known bandwidth-class letter".
            ensure!(
                component.dl_bw_class_mimo != 0,
                "LTE combo {} band {} has dl_bw_class_mimo 0; the source format cannot represent \
                 a disabled downlink",
                index + 1,
                component.band
            );
            ensure!(
                component.ul_bw_class_mimo.is_some(),
                "LTE combo {} band {} omits ul_bw_class_mimo; the source format cannot represent \
                 an absent uplink class (an omitted property means the explicit zero)",
                index + 1,
                component.band
            );
        }
        ensure!(
            seen.insert(RawLteCombo::from(source)),
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
) -> anyhow::Result<Option<(u64, CarrierTier)>> {
    let has_profiles = !source.profiles.is_empty();
    let profiled = if has_profiles {
        let signature = source
            .signature
            .with_context(|| format!("profiled carrier `{carrier}` requires signature"))?;
        let tier = source
            .tier
            .with_context(|| format!("profiled carrier `{carrier}` requires tier"))?;
        Some((signature.0, tier))
    } else {
        ensure!(
            source.signature.is_none() && source.tier.is_none(),
            "carrier `{carrier}` has signature or tier without profiles"
        );
        None
    };
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
    Ok(profiled)
}

/// Encodes a carrier's PLMN list to its packed form, or `None` when the carrier has no PLMN
/// list at all. Each entry's parse error names the offending PLMN and carrier.
fn parse_carrier_plmns(
    plmns: Option<&[String]>,
    carrier: &str,
) -> anyhow::Result<Option<Vec<Plmn>>> {
    plmns
        .map(|plmns| {
            plmns
                .iter()
                .map(|plmn| {
                    plmn.parse::<Plmn>()
                        .with_context(|| format!("invalid PLMN `{plmn}` for carrier `{carrier}`"))
                })
                .collect::<anyhow::Result<Vec<_>>>()
        })
        .transpose()
}

/// Builds one carrier's `anchor -> ValidatedProfile` table: each profile key's filename
/// product (`signature * multiplier`) must round-trip through `matching_anchors` to the SAME
/// anchor it was declared under, or the profile is rejected as ambiguous/mismatched.
/// `signature`/`tier` come from `validate_carrier_role`, which is what proved they exist.
fn validated_profiles(
    source_profiles: &BTreeMap<String, ProfileSource>,
    carrier: &str,
    signature: u64,
    tier: CarrierTier,
) -> anyhow::Result<BTreeMap<u64, ValidatedProfile>> {
    let mut profiles = BTreeMap::new();
    for (key, profile_source) in source_profiles {
        let anchor =
            parse_decimal_key(key, &format!("profile key `{key}` for carrier `{carrier}`"))?;
        let Some(profile) = PROFILES.iter().find(|profile| profile.anchor == anchor) else {
            bail!("u profile anchor {anchor} for carrier `{carrier}`");
        };
        let number = signature
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
                fingerprint: modern_fingerprint(profile.family, tier),
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

        let profiled_role = validate_carrier_role(carrier, source, &bitmask_carriers)?;

        let profiled_id = checked_i32_id(source.profiled_id, "profiled_id", carrier)?;

        if let Some(id) = source.mapping_id
            && let Some(previous) = mapping_ids.insert(id, carrier)
        {
            bail!("mapping_id {id} is used by both carrier `{previous}` and `{carrier}`");
        }

        // `validate_carrier_role` proved mapping_id and plmns imply each other.
        let legend = match (
            source.mapping_id,
            parse_carrier_plmns(source.plmns.as_deref(), carrier)?,
        ) {
            (Some(mapping_id), Some(plmns)) => Some(LegendEntry { mapping_id, plmns }),
            _ => None,
        };
        let profiled = profiled_role
            .map(|(signature, tier)| {
                validated_profiles(&source.profiles, carrier, signature, tier).map(|profiles| {
                    ProfiledRole {
                        signature,
                        tier,
                        profiles,
                    }
                })
            })
            .transpose()?;

        validated.insert(
            carrier.clone(),
            ValidatedCarrier {
                bitmask_id,
                profiled_id,
                legend,
                profiled,
            },
        );
    }
    Ok(validated)
}

/// The PLMN legend every validated carrier with a legend entry contributes, in `mapping_id`
/// order — the order the generated `ap_plmn_mapping.binarypb` carries.
///
/// The single source for this projection: `provision`'s `generate_mapping_file` ships these
/// bytes, `decompose`'s `rebuild_mapping` self-checks against them, and
/// [`validate_mapping_projection`] proves they re-encode. Building it three times is how the
/// three could drift.
pub(crate) fn legend_root(carriers: &BTreeMap<String, ValidatedCarrier>) -> MappingRoot {
    let mut mappings: Vec<_> = carriers
        .iter()
        .filter_map(|(name, carrier)| {
            let legend = carrier.legend.as_ref()?;
            Some(MappingEntry {
                id: legend.mapping_id,
                name: name.clone(),
                plmns: legend.plmns.clone(),
            })
        })
        .collect();
    mappings.sort_by_key(|entry| entry.id);
    MappingRoot { mappings }
}

fn validate_mapping_projection(
    carriers: &BTreeMap<String, ValidatedCarrier>,
) -> anyhow::Result<()> {
    let root = legend_root(carriers);
    let map = root_to_map(&root).context("validating source mapping projection")?;
    map_to_root(&map).context("round-tripping source mapping projection")?;
    Ok(())
}

/// The modern (profiled) fingerprint for a `(family, tier)` pair. Thin wrapper over the one
/// source `model::fingerprint_for` (from `model::FINGERPRINTS`), shared by the NR generation
/// and schema-validation paths so the table lives in exactly one place.
pub(crate) fn modern_fingerprint(family: Family, tier: CarrierTier) -> u64 {
    crate::model::fingerprint_for(family, tier.into())
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
                "fp carrier `{carrier}` is not a bitmask carrier"
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

    use crate::{
        compiler::{lte_from_kdl, nr_from_kdl, selection::Sku},
        proto::{LteComponent, ShannonFeatureSetDlPerCcNr},
    };

    use super::{LteSourceCombo, parse_sources, to_kdl};

    const MINIMAL_NR: &str = r#"
version 1
bc "LEGACY"

bf 715188856 {
    c "LEGACY"
}
"#;

    const MINIMAL_LTE: &str = r#"
version 1

f "400907661" fp=862505271 bm=4082165014
"#;

    fn profiled_nr(profile_key: &str) -> String {
        format!(
            r#"
version 1
bc "LEGACY"

bf 715188856 {{
    c "LEGACY"
}}

cr "PROFILED" pi=7 sg=1 t="main" {{
    pf "{profile_key}" x=66813533 u=0
}}
"#
        )
    }

    fn lte_with_file_key(file_key: &str) -> String {
        format!(
            r#"
version 1

f "{file_key}" fp=862505271 bm=4082165014
"#
        )
    }

    fn nr_with_carrier_sections(sections: &str) -> String {
        format!(
            r#"
version 1
bc "LEGACY"

bf 715188856 {{
    c "LEGACY"
}}

{sections}
"#
        )
    }

    fn nr_with_complete_domain() -> String {
        nr_with_carrier_sections(
            r#"
cr "PROFILED" pi=7 sg=1 t="main" {
    pf "66813533" x=66813533 u=0
    pf "8969" x=8969 u=0
}

cr "MAPPING" mi=8 {
    ps
}
"#,
        )
    }

    fn lte_with_complete_domain() -> String {
        r#"
version 1

f "400907661" fp=862505271 bm=1
f "564260317" fp=874888686 bm=2
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
cr "A" pi=0 mi=7 sg=1 t="main" {{
    ps
    pf "66813533" x=66813533 u=0
}}

cr "B" pi=0 mi=8 sg=1 t="main" {{
    ps
    pf "66813533" x=66813533 u=0
}}
"#
        );
        let parsed = parse_sources(&nr, MINIMAL_LTE).unwrap();
        assert_eq!(parsed.nr.carriers["A"].profiled_id, Some(0));
        assert_eq!(parsed.nr.carriers["B"].profiled_id, Some(0));
        assert_eq!(
            parsed.nr.carriers["A"]
                .legend
                .as_ref()
                .map(|l| l.mapping_id),
            Some(7)
        );
        assert_eq!(
            parsed.nr.carriers["B"]
                .legend
                .as_ref()
                .map(|l| l.mapping_id),
            Some(8)
        );
    }

    #[test]
    fn mapping_id_requires_plmns_and_must_be_unique() {
        let missing = format!("{MINIMAL_NR}\ncr \"MAP\" mi=7\n");
        let error = parse_sources(&missing, MINIMAL_LTE)
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("must provide mapping_id and plmns together"),
            "{error}"
        );

        let missing_id = format!("{MINIMAL_NR}\ncr \"MAP\" {{\n    ps\n}}\n");
        let error = parse_sources(&missing_id, MINIMAL_LTE)
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("must provide mapping_id and plmns together"),
            "{error}"
        );

        let duplicate =
            format!("{MINIMAL_NR}\ncr \"A\" mi=7 {{\n    ps\n}}\ncr \"B\" mi=7 {{\n    ps\n}}\n");
        let error = parse_sources(&duplicate, MINIMAL_LTE)
            .unwrap_err()
            .to_string();
        assert!(error.contains("mapping_id 7 is used by both"), "{error}");
    }

    #[test]
    fn canonical_feature_catalogs_prune_deduplicate_sort_and_renumber() {
        let nr = format!(
            "{MINIMAL_NR}\n\
             df s=3\n\
             df s=1\n\
             df s=3\n\
             uf s=9\n\
             c {{\n    n77 d=A1\n}}\n\
             c {{\n    n78 d=A3\n}}\n"
        );
        let parsed = parse_sources(&nr, MINIMAL_LTE).unwrap();
        let (canonical, _) = to_kdl(&parsed.nr.source, &parsed.lte.source).unwrap();
        assert_eq!(parsed.nr.features.dl.len(), 1);
        assert!(parsed.nr.features.ul.is_empty());
        assert_eq!(parsed.nr.features.dl[0].max_scs, Some(3));
        assert!(canonical.contains("df s=3"), "{canonical}");
        assert!(!canonical.contains("s=1"), "{canonical}");
        assert!(!canonical.contains("ul-feature"), "{canonical}");
        assert!(canonical.contains("d=A1"), "{canonical}");
    }

    #[test]
    fn feature_catalog_preserves_referenced_absent_and_explicit_zero_records() {
        let nr = format!(
            "{MINIMAL_NR}\n\
             df\n\
             df s=0\n\
             c {{\n    n77 d=A2\n}}\n\
             c {{\n    n78 d=A1\n}}\n"
        );
        let parsed = parse_sources(&nr, MINIMAL_LTE).unwrap();
        assert_eq!(parsed.nr.features.dl.len(), 2);
        assert_eq!(
            parsed.nr.features.dl[0],
            ShannonFeatureSetDlPerCcNr::default()
        );
        assert_eq!(parsed.nr.features.dl[1].max_scs, Some(0));
    }

    #[test]
    fn feature_references_reject_zero_out_of_range_and_old_inline_fields() {
        // A 0 reference is now caught at PARSE time by the composite value's own rule, while
        // an out-of-range one is still caught at validation against the catalog length — the
        // reader knows references are 1-based, but not how long the catalog is.
        for (cc_line, expected) in [
            ("n78 d=A0", "1-based"),
            ("n78 d=A2", "exceeds the dl catalog length 1"),
            ("n78 u=A0", "1-based"),
            ("n78 u=A2", "exceeds the ul catalog length 1"),
        ] {
            let nr = format!("{MINIMAL_NR}\ndf s=3\nuf s=4\nc {{\n    {cc_line}\n}}\n");
            // `{:#}` for the whole chain: a parse-time rejection is wrapped in
            // "parsing nr.kdl", so `to_string()` alone would show only that.
            let error = format!("{:#}", parse_sources(&nr, MINIMAL_LTE).unwrap_err());
            assert!(error.contains(expected), "{error}");
        }

        let old = format!("{MINIMAL_NR}\nc {{\n    n78 dl-max-scs=3\n}}\n");
        assert!(nr_from_kdl(&old).is_err());
    }

    #[test]
    fn global_source_catalog_can_exceed_one_byte() {
        let mut nr = MINIMAL_NR.to_string();
        for value in 1..=300 {
            nr.push_str(&format!("\ndf b={value}\n"));
            nr.push_str(&format!("\nc {{\n    n{value} d=A{value}\n}}\n"));
        }
        let parsed = parse_sources(&nr, MINIMAL_LTE).unwrap();
        assert_eq!(parsed.nr.features.dl.len(), 300);
    }

    #[test]
    fn versions_are_required_and_only_the_current_one_is_supported() {
        let missing = MINIMAL_NR.replacen("version 1\n", "", 1);
        assert!(parse_sources(&missing, MINIMAL_LTE).is_err());

        let unsupported_nr = MINIMAL_NR.replacen("\nversion 1", "\nversion 2", 1);
        assert!(
            parse_sources(&unsupported_nr, MINIMAL_LTE)
                .unwrap_err()
                .to_string()
                .contains("source-format version 2")
        );

        let missing = MINIMAL_LTE.replacen("version 1\n", "", 1);
        assert!(parse_sources(MINIMAL_NR, &missing).is_err());

        let unsupported_lte = MINIMAL_LTE.replacen("\nversion 1", "\nversion 2", 1);
        assert!(
            parse_sources(MINIMAL_NR, &unsupported_lte)
                .unwrap_err()
                .to_string()
                .contains("source-format version 2")
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
        // `{:#}` for the whole chain: a parse-time rejection is wrapped in "parsing nr.kdl",
        // so `to_string()` alone would show only that.
        let error = format!("{:#}", parse_sources(nr, MINIMAL_LTE).unwrap_err());
        assert!(error.contains(needle), "expected {needle:?} in {error:?}");
    }

    #[test]
    fn fingerprint_groups_are_nonempty_disjoint_and_exhaustive() {
        assert_nr_error(
            r#"
version 1
bc "A"
bf 1 {
    c
}
"#,
            "nonempty",
        );

        assert_nr_error(
            r#"
version 1
bc "A" "B"
bf 1 {
    c "A"
}
bf 2 {
    c "A" "B"
}
"#,
            "more than one fingerprint",
        );

        assert_nr_error(
            r#"
version 1
bc "A" "B"
bf 1 {
    c "A"
}
"#,
            "partition",
        );

        assert_nr_error(
            r#"
version 1
bc "A"
bf 1 {
    c "A" "B"
}
"#,
            "not a bitmask carrier",
        );

        assert_nr_error(
            r#"
version 1
bc "A" "A"
bf 1 {
    c "A"
}
"#,
            "duplicate carrier",
        );

        assert_nr_error(
            r#"
version 1
bc "A"
bf 1 {
    c "A"
}
bf 1 {
    c "A"
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
cr "PROFILED" t="main" {
    pf "66813533" x=66813533 u=0
}
"#,
            ),
            "signature",
        );
        assert_nr_error(
            &nr_with_carrier_sections(
                r#"
cr "PROFILED" sg=1 {
    pf "66813533" x=66813533 u=0
}
"#,
            ),
            "tier",
        );
        assert_nr_error(
            &nr_with_carrier_sections(
                r#"
cr "ORPHAN" sg=1 t="main"
"#,
            ),
            "without profiles",
        );
        assert_nr_error(
            &nr_with_carrier_sections(
                r#"
cr "MAPPING" {
    ps
}
"#,
            ),
            "must provide mapping_id and plmns together",
        );
        assert_nr_error(
            &nr_with_carrier_sections(
                r#"
cr "MAPPING" mi=7
"#,
            ),
            "must provide mapping_id and plmns together",
        );
        assert_nr_error(
            &nr_with_carrier_sections(
                r#"
cr "MAPPING" pi=7
"#,
            ),
            "has profiled_id but no profiled NR files",
        );
        assert_nr_error(
            &nr_with_carrier_sections(
                r#"
cr "NOT_LEGACY" bi=1
"#,
            ),
            "bitmask_carriers",
        );
        assert_nr_error(
            &nr_with_carrier_sections(
                r#"
cr "LEGACY" bi=2147483648
"#,
            ),
            "int32",
        );
        assert_nr_error(
            &nr_with_carrier_sections(
                r#"
cr "PROFILED" pi=2147483648 sg=1 t="main" {
    pf "66813533" x=66813533 u=0
}
"#,
            ),
            "int32",
        );

        parse_sources(
            &nr_with_carrier_sections(
                r#"
cr "MAPPING" mi=18446744073709551615 {
    ps
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

        assert_nr_error(&profiled_nr("123"), "u profile anchor");
        assert_nr_error(
            &profiled_nr("66813533").replace("x=66813533", "x=1176929627"),
            "wrong profile anchor",
        );
        assert_nr_error(
            &nr_with_carrier_sections(
                r#"
cr "PROFILED" sg=1 t="main" {
    pf "167" x=308449 u=0
}
"#,
            ),
            "ambiguous",
        );
        assert_nr_error(
            &nr_with_carrier_sections(
                r#"
cr "PROFILED" sg=18446744073709551615 t="main" {
    pf "167" x=2 u=0
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
cr "MAPPING" mi=7 {
    p mcc=302 mnc=220
    p mcc=250 mnc=1
    p mcc=302 mnc=220
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
cr "MAPPING" mi=7 {
    p mcc=302 mnc=99999
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
        let nr = format!("{MINIMAL_NR}\nc bm=1 {{\n    n78\n}}\n");
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
            "{}\nc {{\n    s {{\n        c \"PROFILED\"\n        m \"G2YBB\"\n    }}\n    n78\n}}\n",
            nr_with_complete_domain()
        );
        let lte = format!(
            "{}\nc {{\n    s {{\n        m \"G2YBB\" \"lte:564260317\"\n    }}\n    B1 d=A4\n}}\n",
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
        for (catalog, combo_body) in [("", ""), ("", "n0\n"), ("", "B1 st=5\n")] {
            let nr = format!("{MINIMAL_NR}\n{catalog}combo {{\n{combo_body}}}\n");
            assert!(
                parse_sources(&nr, MINIMAL_LTE).is_err(),
                "accepted {combo_body:?}"
            );
        }

        let nr = format!("{MINIMAL_NR}\ndf s=3\nc {{\n    n78 d=A1\n    B1\n}}\n");
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
            "{MINIMAL_NR}\ndf s=3\ndf s=3\nc {{\n    n78 d=A1\n    B1\n}}\nc {{\n    B1\n    n78 d=A2\n}}\n"
        );
        assert_nr_error(&nr, "duplicate canonical NR payload");
    }

    #[test]
    fn lte_payloads_require_components_preserve_order_and_reject_exact_duplicates() {
        let empty = format!("{MINIMAL_LTE}\nc {{\n}}\n");
        assert!(parse_sources(MINIMAL_NR, &empty).is_err());

        let duplicate = format!("{MINIMAL_LTE}\nc {{\n    B1 d=A4\n}}\nc {{\n    B1 d=A4\n}}\n");
        assert!(
            parse_sources(MINIMAL_NR, &duplicate)
                .unwrap_err()
                .to_string()
                .contains("duplicate canonical LTE payload")
        );

        let ordered = format!(
            "{MINIMAL_LTE}\nc {{\n    B1 d=A4\n    B3 d=A4\n}}\nc {{\n    B3 d=A4\n    B1 d=A4\n}}\n"
        );
        let sources = parse_sources(MINIMAL_NR, &ordered).unwrap();
        assert_eq!(sources.lte.combo.len(), 2);
        assert_eq!(sources.lte.combo[0].source.components[0].band, 1);
        assert_eq!(sources.lte.combo[1].source.components[0].band, 3);

        // `bcs` is the surviving optional-presence pair: absent `b` is `None`, `b=0` is `Some(0)`.
        // It is also what keeps these two combos distinct under `RawLteCombo`, now that their
        // components are identical.
        let optional_presence =
            format!("{MINIMAL_LTE}\nc {{\n    B1 d=A4\n}}\nc b=0 {{\n    B1 d=A4\n}}\n");
        let sources = parse_sources(MINIMAL_NR, &optional_presence).unwrap();
        assert_eq!(sources.lte.combo.len(), 2);
        assert_eq!(sources.lte.combo[0].source.bcs, None);
        assert_eq!(sources.lte.combo[1].source.bcs, Some(0));
        // An omitted `u` is the explicit zero, not an absent field — in both combos.
        for combo in &sources.lte.combo {
            assert_eq!(combo.source.components[0].ul_bw_class_mimo, Some(0));
        }
    }

    /// The source format spells UL-disabled by omitting the property, so `None` has no spelling
    /// left; and with `off` gone, neither does a disabled DL. Both are corpus-absent (0 of 12 159
    /// sub-blocks), so this rejects rather than silently normalising — a foreign file gets a loud
    /// error instead of a quiet re-encode.
    #[test]
    fn lte_components_reject_a_disabled_dl_and_an_absent_ul_class() {
        let nr = nr_from_kdl(MINIMAL_NR).unwrap();
        let base = lte_from_kdl(MINIMAL_LTE).unwrap();

        let bad_combo = |component: LteComponent| LteSourceCombo {
            selection: None,
            bcs: None,
            unknown1: None,
            unknown2: None,
            components: vec![component],
        };

        let mut disabled_dl = base.clone();
        disabled_dl.combo.push(bad_combo(LteComponent {
            band: 7,
            dl_bw_class_mimo: 0,
            ul_bw_class_mimo: Some(0),
        }));
        let error = to_kdl(&nr, &disabled_dl).unwrap_err().to_string();
        assert!(
            error.contains("band 7") && error.contains("dl_bw_class_mimo 0"),
            "{error}"
        );

        let mut absent_ul = base;
        absent_ul.combo.push(bad_combo(LteComponent {
            band: 7,
            dl_bw_class_mimo: 32_769,
            ul_bw_class_mimo: None,
        }));
        let error = to_kdl(&nr, &absent_ul).unwrap_err().to_string();
        assert!(
            error.contains("band 7") && error.contains("omits ul_bw_class_mimo"),
            "{error}"
        );
    }

    #[test]
    fn validated_metadata_caches_derived_fingerprints_and_parsed_plmns() {
        let nr = nr_with_carrier_sections(
            r#"
cr "PROFILED" pi=7 mi=7 sg=1 t="alt" {
    p mcc=302 mnc=220
    p mcc=250 mnc=1
    p mcc=302 mnc=220
    pf "66813533" x=66813533 u=9
}
"#,
        );
        let sources = parse_sources(&nr, MINIMAL_LTE).unwrap();
        assert_eq!(sources.nr.bitmask_fingerprints["LEGACY"], 715_188_856);
        let carrier = &sources.nr.carriers["PROFILED"];
        let legend_plmns: Vec<u64> = carrier
            .legend
            .as_ref()
            .expect("cr has a legend entry")
            .plmns
            .iter()
            .map(|plmn| plmn.to_encoded())
            .collect();
        assert_eq!(legend_plmns, [197_154, 5_435_408, 197_154]);
        let profiled = carrier.profiled.as_ref().expect("cr is profiled");
        let profile = &profiled.profiles[&66_813_533];
        assert_eq!(profile.number, 66_813_533);
        assert_eq!(profile.fingerprint, 627_223_094);
        assert_eq!(profile.unknown, 9);
    }

    #[test]
    fn to_kdl_canonicalizes_metadata_payloads_and_selections() {
        let nr_text = format!(
            "{}\ndf s=3\nc {{\n    s {{\n        c \"PROFILED\" \"PROFILED\"\n        m \"G2YBB\" \"G2YBB\"\n    }}\n    n78 d=A1\n    B1\n}}\nc {{\n    s {{\n        c \"LEGACY\"\n    }}\n    B3\n}}\n",
            nr_with_complete_domain()
        );
        let lte_text = format!(
            "{}\nc {{\n    s {{\n        m \"lte:564260317\" \"G2YBB\" \"G2YBB\"\n    }}\n    B3 d=A4\n    B1 d=A4\n}}\nc {{\n    s {{\n        m \"GR83Y\"\n    }}\n    B7 d=A4\n}}\n",
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
cr "ABSENT" bi=1

cr "MAP_ONLY" mi=18446744073709551615 {
    ps
}

cr "ORDERED" mi=7 {
    p mcc=302 mnc=220
    p mcc=228
    p mcc=302 mnc=220
}
"#,
        )
        .replace("bc \"LEGACY\"", "bc \"ABSENT\" \"LEGACY\"")
        .replace("c \"LEGACY\"", "c \"ABSENT\" \"LEGACY\"");
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
        assert!(nr_text.contains("mi=18446744073709551615"));
    }
}
