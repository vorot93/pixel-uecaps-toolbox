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
pub(in crate::compiler) const SOURCE_FORMAT_VERSION: u32 = 1;

#[derive(Clone, Debug)]
pub(in crate::compiler) struct BitmaskFingerprint {
    pub(in crate::compiler) fingerprint: u64,
    pub(in crate::compiler) carriers: Vec<String>,
}

#[derive(Clone, Debug)]
pub(in crate::compiler) struct ProfileSource {
    pub(in crate::compiler) multiplier: u64,
    pub(in crate::compiler) unknown: u64,
}

#[derive(Clone, Debug, Default)]
pub(in crate::compiler) struct CarrierSource {
    pub(in crate::compiler) bitmask_id: Option<i64>,
    pub(in crate::compiler) profiled_id: Option<i64>,
    pub(in crate::compiler) mapping_id: Option<u64>,
    pub(in crate::compiler) plmns: Option<Vec<Plmn>>,
    pub(in crate::compiler) signature: Option<u64>,
    pub(in crate::compiler) tier: Option<Tier>,
    pub(in crate::compiler) profiles: BTreeMap<u64, ProfileSource>,
}

#[derive(Clone, Debug)]
pub(in crate::compiler) struct NrSourceCombo {
    pub(in crate::compiler) selection: Option<Vec<SelectionRect>>,
    pub(in crate::compiler) header: ComboHeader,
    pub(in crate::compiler) sub_blocks: Vec<SourceSubBlock>,
}

/// One source document: the NR and LTE halves it carries.
///
/// The format version is deliberately *not* a field. One file has one version — a state the
/// two-document format had to reject at runtime — and by the time anything downstream sees a
/// `SourceDocument` that version is always [`SOURCE_FORMAT_VERSION`]: the reader
/// (`kdl_source::checked_version`) refuses any other value before returning, and the writer
/// (`kdl_source::write_sources`) stamps the constant unconditionally. Storing it would only carry
/// a number both ends already know, so it was carried write-only until it was dropped.
#[derive(Clone, Debug)]
pub(in crate::compiler) struct SourceDocument {
    pub(in crate::compiler) nr: NrDocument,
    pub(in crate::compiler) lte: LteDocument,
}

/// The NR half. `Default` is the empty half, which the tests that exercise only the LTE half
/// pass to [`write_sources`](crate::compiler::write_sources) — the writer always emits `bc`, so an empty
/// half still produces a parseable document.
#[derive(Clone, Debug, Default)]
pub(in crate::compiler) struct NrDocument {
    pub(in crate::compiler) bitmask_carriers: Vec<String>,
    pub(in crate::compiler) bitmask_fingerprints: Vec<BitmaskFingerprint>,
    pub(in crate::compiler) carriers: BTreeMap<String, CarrierSource>,
    pub(in crate::compiler) dl_features: Vec<ShannonFeatureSetDlPerCcNr>,
    pub(in crate::compiler) ul_features: Vec<ShannonFeatureSetUlPerCcNr>,
    pub(in crate::compiler) combos: Vec<NrSourceCombo>,
}

#[derive(Clone, Debug)]
pub(in crate::compiler) struct LteFileSource {
    pub(in crate::compiler) fingerprint: u64,
    pub(in crate::compiler) bitmask: u64,
}

#[derive(Clone, Debug)]
pub(in crate::compiler) struct LteSourceCombo {
    pub(in crate::compiler) selection: Option<Vec<SelectionRect>>,
    pub(in crate::compiler) bcs: Option<u64>,
    pub(in crate::compiler) unknown1: Option<u64>,
    pub(in crate::compiler) unknown2: Option<u64>,
    pub(in crate::compiler) components: Vec<LteComponent>,
}

/// The LTE half. `Default` is the empty half — see [`NrDocument`].
#[derive(Clone, Debug, Default)]
pub(in crate::compiler) struct LteDocument {
    pub(in crate::compiler) files: BTreeMap<u64, LteFileSource>,
    pub(in crate::compiler) combos: Vec<LteSourceCombo>,
}
