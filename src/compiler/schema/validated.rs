#[cfg(test)]
use super::canonicalize::nr_source_combos;
use super::source::{LteDocument, LteFileSource, LteSourceCombo, NrDocument};
use crate::{
    compiler::{
        feature_palette::FeaturePalette,
        selection::{CarrierId, LteDomain, LteRelation, NrDomain, NrRelation, SkuId},
    },
    mapping::Plmn,
    raw_nr::RawNrPayload,
};
use std::collections::{BTreeMap, HashMap};
#[derive(Clone, Debug)]
pub(crate) struct ValidatedNr {
    pub(crate) source: NrDocument,
    pub(crate) bitmask_fingerprints: BTreeMap<String, u64>,
    pub(crate) carriers: BTreeMap<String, ValidatedCarrier>,
    pub(crate) domain: NrDomain,
    pub(crate) palette: FeaturePalette,
    pub(crate) combos: Vec<ValidatedNrCombo>,
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
/// byte-identical. Combo validation fixes the order; canonicalization never reorders the
/// validated combos, so the indices stay valid through generation.
#[derive(Clone, Debug, Default)]
pub(crate) struct NrSelectionIndex(HashMap<(CarrierId, SkuId), Vec<u32>>);

impl NrSelectionIndex {
    pub(super) fn build(combos: &[ValidatedNrCombo]) -> Self {
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
    /// Test-only combo surgery: replace `combo`, union every newly referenced value into the
    /// existing complete palette, canonicalize that union, and rebuild the selection/source
    /// projections. The union is load-bearing: tests may start with manual unreferenced rows,
    /// and changing their combos must not silently discard those rows. Production sets these
    /// fields once in `validate_documents`; because `selection_index` points at `combo`
    /// positions, any post-construction combo replacement must go through here or those indices
    /// dangle.
    pub(crate) fn set_combos(&mut self, combo: Vec<ValidatedNrCombo>) {
        let mut dl = self.palette.dl.clone();
        let mut ul = self.palette.ul.clone();
        for component in combo
            .iter()
            .flat_map(|combo| combo.payload.sub_blocks.iter())
        {
            dl.extend_from_slice(component.dl_features());
            ul.extend_from_slice(component.ul_features());
        }
        self.palette = FeaturePalette::from_rows(dl, ul).canonicalized();
        self.selection_index = NrSelectionIndex::build(&combo);
        self.combos = combo;
        self.source.dl_features = self.palette.dl.clone();
        self.source.ul_features = self.palette.ul.clone();
        self.source.combos = nr_source_combos(&self.combos, &self.domain, &self.palette)
            .expect("combo surgery must produce serializable source combos");
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
    pub(super) legend: Option<LegendEntry>,
    /// The carrier's profiled role, present iff it ships profile files.
    pub(crate) profiled: Option<ProfiledRole>,
}

/// A carrier's entry in the PLMN legend. `mapping_id` and `plmns` are validated to imply each
/// other, so they are never separately absent.
#[derive(Clone, Debug)]
pub(super) struct LegendEntry {
    pub(super) mapping_id: u64,
    pub(super) plmns: Vec<Plmn>,
}

/// A carrier's profiled role. Signature, tier, and a non-empty profile table are validated to
/// imply one another. Only the derived per-profile identities are needed after validation;
/// the original signature and tier remain in the source document for serialization.
#[derive(Clone, Debug)]
pub(crate) struct ProfiledRole {
    pub(crate) profiles: BTreeMap<u64, ValidatedProfile>,
}

#[derive(Clone, Debug)]
pub(crate) struct ValidatedProfile {
    pub(crate) number: u64,
    pub(crate) unknown: u64,
    pub(crate) fingerprint: u64,
}

#[derive(Clone, Debug)]
pub(crate) struct ValidatedLte {
    pub(crate) source: LteDocument,
    pub(crate) files: BTreeMap<u64, LteFileSource>,
    pub(crate) domain: LteDomain,
    pub(crate) combos: Vec<ValidatedLteCombo>,
}

#[derive(Clone, Debug)]
pub(crate) struct ValidatedLteCombo {
    pub(crate) source: LteSourceCombo,
    pub(crate) relation: LteRelation,
}

/// A fully parsed, cross-referenced, canonicalized source set. Produced once by
/// [`load_sources`](crate::compiler::load_sources) and consumed by
/// [`provision_from_sources`](crate::compiler::provision_from_sources) for every model, so a
/// batch `provision` run parses the multi-megabyte source a single time. Fields are crate-internal; externally it
/// is an opaque handle.
#[derive(Clone, Debug)]
pub struct ValidatedSources {
    pub(crate) nr: ValidatedNr,
    pub(crate) lte: ValidatedLte,
}

impl ValidatedSources {
    /// Serialize these already-validated, canonical sources **without** re-validating.
    /// `validate_documents` leaves `nr.source`/`lte.source` canonical, so serializing them
    /// directly reproduces the canonical writer's output — letting `decompose` drop a redundant
    /// third `validate_documents` pass while its byte-idempotence assertion still proves the
    /// emitted document is a fixed point.
    pub(crate) fn to_kdl(&self) -> anyhow::Result<String> {
        crate::compiler::source_to_kdl(&self.nr.source, &self.lte.source)
    }
}
