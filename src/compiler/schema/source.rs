use crate::{
    compiler::{features::SourceSubBlock, selection::SelectionRect},
    mapping::Plmn,
    model::Tier,
    proto::{ComboHeader, LteComponent, ShannonFeatureSetDlPerCcNr, ShannonFeatureSetUlPerCcNr},
};
use std::collections::BTreeMap;
/// The source document's format version.
///
/// This number identifies *the* format, not a count of revisions — it is reset rather than
/// advanced when a format change lands in an unpublished series, because this repo's history is
/// squashable and a reader at HEAD must see one coherent state. The check below is an
/// inequality, not an ordering, so a reset still rejects a tree from any build that emitted a
/// different number. That is also why `kdl_keys::doc::VERSION` is the one key left
/// unabbreviated — the marker announcing the version cannot be renamed by the change it
/// describes, or the check below would be unreachable for exactly the documents it exists to
/// diagnose.
pub(crate) const SOURCE_FORMAT_VERSION: u32 = 1;

#[derive(Clone, Debug)]
pub(crate) struct BitmaskFingerprint {
    pub(crate) fingerprint: u64,
    pub(crate) carriers: Vec<String>,
}

#[derive(Clone, Debug)]
pub(crate) struct ProfileSource {
    pub(crate) multiplier: u64,
    pub(crate) unknown: u64,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct CarrierSource {
    pub(crate) bitmask_id: Option<i64>,
    pub(crate) profiled_id: Option<i64>,
    pub(crate) mapping_id: Option<u64>,
    pub(crate) plmns: Option<Vec<Plmn>>,
    pub(crate) signature: Option<u64>,
    pub(crate) tier: Option<Tier>,
    pub(crate) profiles: BTreeMap<u64, ProfileSource>,
}

#[derive(Clone, Debug)]
pub(crate) struct NrSourceCombo {
    pub(crate) selection: Option<Vec<SelectionRect>>,
    pub(crate) header: ComboHeader,
    pub(crate) sub_blocks: Vec<SourceSubBlock>,
}

/// One source document: the NR and LTE halves it carries.
///
/// The format version is deliberately *not* a field. One file has one version — a state the
/// two-document format had to reject at runtime — and by the time anything downstream sees a
/// `SourceDocument` that version is always [`SOURCE_FORMAT_VERSION`]: the reader
/// (`kdl_source::checked_version`) refuses any other value before returning, and the writer
/// (`kdl_source::source_to_kdl`) stamps the constant unconditionally. Storing it would only carry
/// a number both ends already know, so it was carried write-only until it was dropped.
#[derive(Clone, Debug)]
pub(crate) struct SourceDocument {
    pub(crate) nr: NrDocument,
    pub(crate) lte: LteDocument,
}

/// The NR half. `Default` is the empty half, which the tests that exercise only the LTE half
/// pass to [`source_to_kdl`](crate::compiler::source_to_kdl) — the writer always emits `bc`, so an empty
/// half still produces a parseable document.
#[derive(Clone, Debug, Default)]
pub(crate) struct NrDocument {
    pub(crate) bitmask_carriers: Vec<String>,
    pub(crate) bitmask_fingerprints: Vec<BitmaskFingerprint>,
    pub(crate) carriers: BTreeMap<String, CarrierSource>,
    pub(crate) dl_features: Vec<ShannonFeatureSetDlPerCcNr>,
    pub(crate) ul_features: Vec<ShannonFeatureSetUlPerCcNr>,
    pub(crate) combos: Vec<NrSourceCombo>,
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

/// The LTE half. `Default` is the empty half — see [`NrDocument`].
#[derive(Clone, Debug, Default)]
pub(crate) struct LteDocument {
    pub(crate) files: BTreeMap<u64, LteFileSource>,
    pub(crate) combos: Vec<LteSourceCombo>,
}
