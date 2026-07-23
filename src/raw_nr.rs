//! Protobuf-shaped NR combo payloads shared by the compiler's ingest and generation paths.

use crate::{
    proto::{
        ShannonFeatureSetDlPerCcNr, ShannonFeatureSetUlPerCcNr,
        combo_group::{Combo as ProtoCombo, ComboHeader, combo::SubBlock as ProtoSubBlock},
    },
    report::combos::{Combo, NR_BAND_OFFSET, SubBlock, band_label_for, raw_band, resolve_all},
};

/// Per-component radio kind for a raw NR combo payload.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum SubBlockKind {
    #[default]
    Lte,
    Nr,
}

impl SubBlockKind {
    const fn raw_band(self, band: i32) -> i32 {
        match self {
            Self::Lte => band,
            Self::Nr => NR_BAND_OFFSET + band,
        }
    }
}

/// Which of a sub-block's two directions a shared check or message is about.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Direction {
    Dl,
    Ul,
}

impl Direction {
    const fn lowercase(self) -> &'static str {
        match self {
            Self::Dl => "dl",
            Self::Ul => "ul",
        }
    }
}

impl std::fmt::Display for Direction {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Dl => "DL",
            Self::Ul => "UL",
        })
    }
}

/// Proto field 6/7 for one direction, as the flat report DTO carries it: resolved values win
/// over selector bytes when a hand-built DTO supplies both.
fn per_cc_from_dto<T: Copy>(features: &[T], selector: Option<Vec<u8>>) -> Option<PerCc<T>> {
    if features.is_empty() {
        selector.map(PerCc::Selector)
    } else {
        Some(PerCc::Resolved(features.to_vec()))
    }
}

/// How proto field 6/7 (`dl/ul_feature_per_cc_ids`) is spelled when it IS present — the two
/// encodings are alternatives, never a mix. Absence is the enclosing `Option`, matching the
/// wire, where the field is simply missing (e.g. UL disabled, `ul_bw_class == 0`).
///
/// * `Selector(bytes)` — present but resolving to no catalog record. Only the all-zero
///   placeholder survives the decode boundary ([`resolve_or_placeholder`]); a nonzero
///   unresolvable selector is a hard error there, so the bytes reaching generation are
///   always `[0; cc_count]`.
/// * `Resolved(features)` — one feature set per CC. Never empty: an empty resolution means
///   "did not resolve", which is what [`NrDirection::resolve`] leaves alone.
///
/// NR-only. An E-UTRA component references no per-CC feature catalog, so [`LteDirection`]
/// carries plain selector bytes instead.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum PerCc<T> {
    Selector(Vec<u8>),
    Resolved(Vec<T>),
}

/// One direction of an NR component: its CA bandwidth class plus proto field 6/7.
///
/// The per-CC accessors live here rather than on [`PerCc`] because every length question is
/// really a question about `bw_class` too — `cc_count(kind, bw_class)` is what a per-CC list's
/// length must equal.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct NrDirection<T> {
    pub(crate) bw_class: Option<i32>,
    pub(crate) features: Option<PerCc<T>>,
}

impl<T: Copy> NrDirection<T> {
    /// One resolved feature set per CC, or `&[]` when this direction resolved to nothing.
    pub(crate) fn resolved(&self) -> &[T] {
        match &self.features {
            Some(PerCc::Resolved(features)) => features,
            Some(PerCc::Selector(_)) | None => &[],
        }
    }

    /// The raw selector bytes, or `None` once resolved (or when absent).
    pub(crate) fn selector(&self) -> Option<&[u8]> {
        match &self.features {
            Some(PerCc::Selector(bytes)) => Some(bytes),
            Some(PerCc::Resolved(_)) | None => None,
        }
    }

    /// CC0's resolved feature set, or `None` when unresolved/absent.
    pub(crate) fn first(&self) -> Option<T> {
        self.resolved().first().copied()
    }

    /// How many CCs this direction describes — one per resolved feature set or per selector
    /// byte — or `None` when it carries no per-CC data at all. This is the length
    /// [`RawSubBlock::validate_cc_count`] checks against `cc_count(kind, bw_class)`.
    pub(crate) fn per_cc_len(&self) -> Option<usize> {
        self.features.as_ref().map(|per_cc| match per_cc {
            PerCc::Selector(bytes) => bytes.len(),
            PerCc::Resolved(features) => features.len(),
        })
    }

    /// Adopt resolved feature sets, dropping any selector bytes they supersede. An empty
    /// `features` is "did not resolve" and leaves the current state untouched — the same
    /// presence rule the decode boundary uses (a one-element vec of all-`None` fields is a
    /// legitimate resolved record and IS present). Only the test-only
    /// [`RawSubBlock::with_resolved_feature_sets`] still needs it: production ingest builds
    /// each direction already resolved.
    #[cfg(test)]
    pub(crate) fn resolve(&mut self, features: Vec<T>) {
        if !features.is_empty() {
            self.features = Some(PerCc::Resolved(features));
        }
    }
}

/// Constructors used by tests to spell a direction in one line. Production code builds
/// `NrDirection` field-wise from data it already has (a decoded selector, a resolved catalog
/// reference), so these exist purely to keep assertions readable.
#[cfg(test)]
impl<T: Copy> NrDirection<T> {
    /// A direction with one resolved feature set per CC.
    pub(crate) fn with_features(bw_class: i32, features: Vec<T>) -> Self {
        Self {
            bw_class: Some(bw_class),
            features: (!features.is_empty()).then_some(PerCc::Resolved(features)),
        }
    }

    /// A direction carrying raw selector bytes that resolved to nothing.
    pub(crate) fn with_selector(bw_class: i32, bytes: Vec<u8>) -> Self {
        Self {
            bw_class: Some(bw_class),
            features: Some(PerCc::Selector(bytes)),
        }
    }

    /// A direction with a bandwidth class but no per-CC data at all.
    pub(crate) const fn bare(bw_class: Option<i32>) -> Self {
        Self {
            bw_class,
            features: None,
        }
    }
}

/// One direction of an E-UTRA component: its CA bandwidth class, the stored
/// `parseLteFeatureIndex` value (proto 4/5 — a MIMO × CC-count code, not a catalog
/// reference), and proto field 6/7. The selector is always the all-zero placeholder in the
/// corpus (LTE references no per-CC feature catalog) and so is fully derivable from
/// `bw_class`; it is kept only so a decoded component re-encodes with the field presence it
/// arrived with.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct LteDirection {
    pub(crate) bw_class: Option<i32>,
    pub(crate) feature_index: Option<i32>,
    pub(crate) selector: Option<Vec<u8>>,
}

/// An E-UTRA component inside an EN-DC combo. It carries no resolved per-CC feature sets and
/// no `srs_tx_switch` — both NR-only, and previously excluded by a runtime check
/// (`has_nr_only_fields`) rather than by the type.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct RawLteSubBlock {
    pub(crate) band: i32,
    pub(crate) dl: LteDirection,
    pub(crate) ul: LteDirection,
}

/// An NR component. Its `dl/ul_feature_index` (proto 4/5) is deliberately **not** a field: it
/// is a pure function of the resolved per-CC feature set (`derive_nr_dl_index` /
/// `derive_nr_ul_index`), re-derived wherever the binary needs it, and a decoded value that
/// disagrees is rejected at the boundary ([`ensure_feature_index_derivable`](RawNrSubBlock::ensure_feature_index_derivable)).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct RawNrSubBlock {
    pub(crate) band: i32,
    pub(crate) dl: NrDirection<ShannonFeatureSetDlPerCcNr>,
    pub(crate) ul: NrDirection<ShannonFeatureSetUlPerCcNr>,
    pub(crate) srs_tx_switch: Option<i32>,
}

/// One protobuf-shaped component in an NR combo, as a closed sum over the two radio kinds:
/// an E-UTRA component and an NR component carry genuinely different data, so neither can
/// hold the other's fields. `band` is the plain human band number (`78`, not the protobuf's
/// internal `10078`); the variant supplies `B`/`n`.
///
/// (De)serialized by hand as KDL — see `compiler::kdl_source`; the KDL reader's
/// `NodeReader::finish()` is the strictness equivalent of the former
/// `#[serde(deny_unknown_fields)]`, and the reader simply skips a field to leave it absent
/// instead of a `skip_serializing_if` attribute.
///
/// The accessors below (`dl_bw_class`, `dl_features`, `dl_feature_index`, …) are the shared
/// read surface for code that treats both kinds alike; anything that has to *build* a
/// component matches on the variant and fills only the fields that kind actually has.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum RawSubBlock {
    Lte(RawLteSubBlock),
    Nr(RawNrSubBlock),
}

impl From<RawLteSubBlock> for RawSubBlock {
    fn from(component: RawLteSubBlock) -> Self {
        Self::Lte(component)
    }
}

impl From<RawNrSubBlock> for RawSubBlock {
    fn from(component: RawNrSubBlock) -> Self {
        Self::Nr(component)
    }
}

impl RawSubBlock {
    pub(crate) const fn kind(&self) -> SubBlockKind {
        match self {
            Self::Lte(_) => SubBlockKind::Lte,
            Self::Nr(_) => SubBlockKind::Nr,
        }
    }

    pub(crate) const fn band(&self) -> i32 {
        match self {
            Self::Lte(component) => component.band,
            Self::Nr(component) => component.band,
        }
    }

    pub(crate) const fn raw_band(&self) -> i32 {
        self.kind().raw_band(self.band())
    }

    pub(crate) const fn dl_bw_class(&self) -> Option<i32> {
        match self {
            Self::Lte(component) => component.dl.bw_class,
            Self::Nr(component) => component.dl.bw_class,
        }
    }

    pub(crate) const fn ul_bw_class(&self) -> Option<i32> {
        match self {
            Self::Lte(component) => component.ul.bw_class,
            Self::Nr(component) => component.ul.bw_class,
        }
    }

    pub(crate) const fn srs_tx_switch(&self) -> Option<i32> {
        match self {
            Self::Lte(_) => None,
            Self::Nr(component) => component.srs_tx_switch,
        }
    }

    /// One resolved DL feature set per CC; always empty for LTE, which has none.
    pub(crate) fn dl_features(&self) -> &[ShannonFeatureSetDlPerCcNr] {
        match self {
            Self::Lte(_) => &[],
            Self::Nr(component) => component.dl.resolved(),
        }
    }

    /// See [`dl_features`](Self::dl_features).
    pub(crate) fn ul_features(&self) -> &[ShannonFeatureSetUlPerCcNr] {
        match self {
            Self::Lte(_) => &[],
            Self::Nr(component) => component.ul.resolved(),
        }
    }

    /// Raw DL selector bytes, or `None` once resolved (or when absent).
    pub(crate) fn dl_selector(&self) -> Option<&[u8]> {
        match self {
            Self::Lte(component) => component.dl.selector.as_deref(),
            Self::Nr(component) => component.dl.selector(),
        }
    }

    /// See [`dl_selector`](Self::dl_selector).
    pub(crate) fn ul_selector(&self) -> Option<&[u8]> {
        match self {
            Self::Lte(component) => component.ul.selector.as_deref(),
            Self::Nr(component) => component.ul.selector(),
        }
    }
}

/// Derive an NR component's `dl_feature_index` from its resolved DL per-CC feature set:
/// 0 = no feature set, 1 = FR1 (`max_scs < 4`), 2 = FR2 (`max_scs >= 4`). `scs` is `None`
/// iff there is no DL feature set; a present set with an absent `max_scs` maps to FR1.
/// Corpus-verified over 1.72M NR components — see
/// DESIGN.md.
/// LTE feature indexes are a different encoding (parseLteFeatureIndex) and are never derived.
pub(crate) fn derive_nr_dl_index(scs: Option<i32>) -> i32 {
    match scs {
        None => 0,
        Some(scs) => {
            if scs >= 4 {
                2
            } else {
                1
            }
        }
    }
}

/// Derive an NR component's `ul_feature_index` from its resolved UL per-CC feature set:
/// 0 = no feature set, 1 = no MIMO (`max_mimo_cb != 2`), 2 = MIMO (`max_mimo_cb == 2`).
pub(crate) fn derive_nr_ul_index(max_mimo_cb: Option<i32>) -> i32 {
    match max_mimo_cb {
        None => 0,
        Some(cb) => {
            if cb == 2 {
                2
            } else {
                1
            }
        }
    }
}

/// Observed Samsung Shannon `bw_class` → aggregated CC count for NR sub-blocks.
/// Exception-free across 3.46M corpus sub-blocks (DL and UL share this table).
pub(crate) const NR_CC_COUNTS: &[(i32, usize)] = &[
    (1, 1),
    (2, 2),
    (3, 2),
    (7, 2),
    (8, 3),
    (9, 4),
    (10, 5),
    (11, 6),
    (12, 7),
    (13, 8),
];

/// Observed `bw_class` → CC count for E-UTRA (LTE) sub-blocks. Distinct from NR.
pub(crate) const LTE_CC_COUNTS: &[(i32, usize)] = &[(1, 1), (2, 2), (3, 2), (4, 3), (5, 4)];

/// Number of component carriers a sub-block of this kind and bandwidth class carries.
/// Fail-closed: an unobserved class errors rather than mis-deriving a per-CC list length.
/// `bw_class == 0` (UL disabled) is not a valid input here — callers gate on it first.
pub(crate) fn cc_count(kind: SubBlockKind, bw_class: i32) -> anyhow::Result<usize> {
    let table = match kind {
        SubBlockKind::Nr => NR_CC_COUNTS,
        SubBlockKind::Lte => LTE_CC_COUNTS,
    };
    table
        .iter()
        .find(|(c, _)| *c == bw_class)
        .map(|(_, n)| *n)
        .ok_or_else(|| {
            anyhow::anyhow!("unknown {kind:?} bw_class {bw_class}: cannot determine CC count")
        })
}

/// Whether `bytes` is a non-placeholder selector: at least one non-zero byte. The all-zero
/// placeholder always resolves to no feature set and is valid; any other selector that
/// resolves to no feature set cannot be carried and must be rejected by the caller. Used at
/// the decode boundary ([`resolve_or_placeholder`]).
fn is_non_placeholder(bytes: &[u8]) -> bool {
    bytes.iter().any(|&b| b != 0)
}

/// Post-resolution split: resolved features clear the raw bytes; an unresolved selector may
/// survive ONLY as the all-zero placeholder (re-derivable from bw_class on the source round
/// trip). A non-zero unresolvable selector can no longer be carried — fail loudly.
fn resolve_or_placeholder<T: Copy>(
    resolved: Option<Vec<T>>,
    raw: Option<&[u8]>,
    kind: SubBlockKind,
    direction: &str,
    band: i32,
) -> anyhow::Result<Option<PerCc<T>>> {
    match resolved {
        // `resolve_all` never yields an empty vec (it returns `None` for an empty selector),
        // so a `Some` here is always a real resolution.
        Some(features) => Ok(Some(PerCc::Resolved(features))),
        None => {
            let Some(bytes) = raw else {
                return Ok(None);
            };
            anyhow::ensure!(
                !is_non_placeholder(bytes),
                "component {} {direction} selector {bytes:?} resolves to no feature and is not the all-zero placeholder",
                band_label_for(kind, band),
            );
            Ok(Some(PerCc::Selector(bytes.to_vec())))
        }
    }
}

impl RawNrSubBlock {
    /// For NR the feature index is derived, never stored, so a decoded index that disagrees
    /// with the derivation cannot round-trip — reject it at the decode boundary rather than
    /// silently dropping it.
    fn ensure_feature_index_derivable(&self, stored: FeatureIndexes) -> anyhow::Result<()> {
        if let Some(stored) = stored.dl {
            let derived = self.derived_dl_feature_index();
            anyhow::ensure!(
                stored == derived,
                "NR component {} stored DL feature index {stored} != derived {derived}",
                self.band_label()
            );
        }
        if let Some(stored) = stored.ul {
            let derived = self.derived_ul_feature_index();
            anyhow::ensure!(
                stored == derived,
                "NR component {} stored UL feature index {stored} != derived {derived}",
                self.band_label()
            );
        }
        Ok(())
    }

    /// Derived DL feature index for this component's resolved DL feature set (0 if none).
    pub(crate) fn derived_dl_feature_index(&self) -> i32 {
        derive_nr_dl_index(self.dl.first().map(|fs| fs.max_scs.unwrap_or(0)))
    }

    /// Derived UL feature index for this component's resolved UL feature set (0 if none).
    pub(crate) fn derived_ul_feature_index(&self) -> i32 {
        derive_nr_ul_index(self.ul.first().map(|fs| fs.max_mimo_cb.unwrap_or(0)))
    }

    fn band_label(&self) -> String {
        band_label_for(SubBlockKind::Nr, self.band)
    }
}

/// The proto-4/5 pair as read off the wire, before it is validated away. NR does not keep it
/// (see [`RawNrSubBlock`]); this exists only so the decode boundary can compare what a file
/// stored against what the feature sets derive.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct FeatureIndexes {
    pub(crate) dl: Option<i32>,
    pub(crate) ul: Option<i32>,
}

impl RawSubBlock {
    /// The `dl_feature_index` to write into the binary: the stored `parseLteFeatureIndex`
    /// value for LTE, the value derived from the per-CC feature set for NR.
    pub(crate) fn dl_feature_index(&self) -> Option<i32> {
        match self {
            Self::Lte(component) => component.dl.feature_index,
            Self::Nr(component) => Some(component.derived_dl_feature_index()),
        }
    }

    /// The `ul_feature_index` to write into the binary; see [`dl_feature_index`](Self::dl_feature_index).
    pub(crate) fn ul_feature_index(&self) -> Option<i32> {
        match self {
            Self::Lte(component) => component.ul.feature_index,
            Self::Nr(component) => Some(component.derived_ul_feature_index()),
        }
    }

    /// The `dl_feature_index` to persist in KDL source: the stored value for LTE, nothing for
    /// NR — the source format omits it and every consumer re-derives it.
    pub(crate) const fn source_dl_feature_index(&self) -> Option<i32> {
        match self {
            Self::Lte(component) => component.dl.feature_index,
            Self::Nr(_) => None,
        }
    }

    /// See [`source_dl_feature_index`](Self::source_dl_feature_index).
    pub(crate) const fn source_ul_feature_index(&self) -> Option<i32> {
        match self {
            Self::Lte(component) => component.ul.feature_index,
            Self::Nr(_) => None,
        }
    }

    /// Adopt resolved per-CC feature sets, dropping the selector bytes they supersede. A
    /// no-op for LTE (which has no feature sets) and for an empty vec (see
    /// [`PerCc::resolve`]). Only the test-only `from_compiler_combo` still needs it:
    /// production ingest builds each direction already resolved.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn with_resolved_feature_sets(
        mut self,
        dl: Vec<ShannonFeatureSetDlPerCcNr>,
        ul: Vec<ShannonFeatureSetUlPerCcNr>,
    ) -> Self {
        if let Self::Nr(component) = &mut self {
            component.dl.resolve(dl);
            component.ul.resolve(ul);
        }
        self
    }

    /// Build a raw component from the report `SubBlock` DTO. Carries every per-CC
    /// feature-set entry the DTO holds — an empty `dl_features`/`ul_features` vec IS the
    /// "no feature set" identity, a non-empty vec (even one whose sole entry has every
    /// field `None`, a legitimate all-absent catalog record) IS present. This is the
    /// same presence rule [`dl_feature_set_is_present`](Self::dl_feature_set_is_present)
    /// and [`with_resolved_feature_sets`](Self::with_resolved_feature_sets) already use;
    /// prior to Task 7 this conversion additionally truncated to CC0 and gated presence on
    /// "does CC0 have any field set", both now removed — the per-CC model is uniform across
    /// the crate, so there is no longer a lossy report axis here.
    ///
    /// The DTO is flat — it can hold a stored NR feature index and both a selector and
    /// resolved values for one direction — so this is where those redundant encodings
    /// collapse: resolved values win over the selector bytes for a direction that has both,
    /// and an NR index is dropped rather than stored (it is re-derived by
    /// [`dl_feature_index`](Self::dl_feature_index)). Neither can lose information on a DTO
    /// that came from a decoded file, where the decode boundary has already cleared the
    /// selector and pinned the index to its derivation.
    pub(crate) fn from_sub_block(cc: &SubBlock) -> Self {
        let raw = raw_band(&cc.band).expect("report component band is canonical");
        let is_nr = raw >= NR_BAND_OFFSET;
        let band = if is_nr { raw - NR_BAND_OFFSET } else { raw };
        if is_nr {
            RawNrSubBlock {
                band,
                dl: NrDirection {
                    bw_class: cc.dl_bw_class,
                    features: per_cc_from_dto(&cc.dl_features, cc.dl_feature_per_cc_ids.clone()),
                },
                ul: NrDirection {
                    bw_class: cc.ul_bw_class,
                    features: per_cc_from_dto(&cc.ul_features, cc.ul_feature_per_cc_ids.clone()),
                },
                srs_tx_switch: cc.srs_tx_switch,
            }
            .into()
        } else {
            RawLteSubBlock {
                band,
                dl: LteDirection {
                    bw_class: cc.dl_bw_class,
                    feature_index: cc.dl_feature_index,
                    selector: cc.dl_feature_per_cc_ids.clone(),
                },
                ul: LteDirection {
                    bw_class: cc.ul_bw_class,
                    feature_index: cc.ul_feature_index,
                    selector: cc.ul_feature_per_cc_ids.clone(),
                },
            }
            .into()
        }
    }

    /// Build a raw component directly from its protobuf `SubBlock` and the file's
    /// feature-set lists — the folder-ingest counterpart of [`from_sub_block`](Self::from_sub_block). It
    /// skips constructing the report `SubBlock` DTO, so it allocates no band-label string, does not
    /// re-parse the band back out (the `raw_band` R3 panic surface), and computes none of the
    /// discarded display projections. Byte-equivalent to resolving the DTO and calling
    /// `from_sub_block(..).with_resolved_feature_sets(dl, ul)` (E6).
    ///
    /// This is the strict ingest boundary for `ul_bw_class`: corpus-verified always `Some`
    /// on a real decoded sub-block (never `None`), so its absence here — which the compiler
    /// KDL source now normalizes away by omitting `Some(0)` (Task 8) — fails closed instead
    /// of silently normalizing to `0` on data that has never actually shown that shape.
    pub(crate) fn from_proto_sub_block(
        component: &ProtoSubBlock,
        dl_list: &[ShannonFeatureSetDlPerCcNr],
        ul_list: &[ShannonFeatureSetUlPerCcNr],
    ) -> anyhow::Result<Self> {
        anyhow::ensure!(
            component.ul_bw_class.is_some(),
            "sub-block omits ul_bw_class (never observed; refusing to normalize to 0)"
        );
        let is_nr = component.band >= NR_BAND_OFFSET;
        let (kind, band) = if is_nr {
            (SubBlockKind::Nr, component.band - NR_BAND_OFFSET)
        } else {
            (SubBlockKind::Lte, component.band)
        };
        if !is_nr {
            return Ok(RawLteSubBlock {
                band,
                dl: LteDirection {
                    bw_class: component.dl_bw_class,
                    feature_index: component.dl_feature_index,
                    selector: component.dl_feature_per_cc_ids.clone(),
                },
                ul: LteDirection {
                    bw_class: component.ul_bw_class,
                    feature_index: component.ul_feature_index,
                    selector: component.ul_feature_per_cc_ids.clone(),
                },
            }
            .into());
        }
        let dl = resolve_all(component.dl_feature_per_cc_ids.as_deref(), dl_list);
        let ul = resolve_all(component.ul_feature_per_cc_ids.as_deref(), ul_list);
        let raw = RawNrSubBlock {
            band,
            dl: NrDirection {
                bw_class: component.dl_bw_class,
                features: resolve_or_placeholder(
                    dl,
                    component.dl_feature_per_cc_ids.as_deref(),
                    kind,
                    "DL",
                    band,
                )?,
            },
            ul: NrDirection {
                bw_class: component.ul_bw_class,
                features: resolve_or_placeholder(
                    ul,
                    component.ul_feature_per_cc_ids.as_deref(),
                    kind,
                    "UL",
                    band,
                )?,
            },
            srs_tx_switch: component.srstxswitch,
        };
        // NR does not keep proto 4/5 — validate what the file stored against the derivation,
        // then let it go.
        raw.ensure_feature_index_derivable(FeatureIndexes {
            dl: component.dl_feature_index,
            ul: component.ul_feature_index,
        })?;
        Ok(raw.into())
    }

    /// Invariants that the type cannot express. "LTE carries NR-only fields" is no longer
    /// among them — [`RawLteSubBlock`] simply has no feature-set or `srs_tx_switch` field.
    pub(crate) fn validate(&self) -> anyhow::Result<()> {
        anyhow::ensure!(self.band() > 0, "component band must be positive");
        anyhow::ensure!(
            self.band() < NR_BAND_OFFSET,
            "component band must be the plain band number, not raw protobuf encoding"
        );
        self.validate_cc_count(Direction::Dl)?;
        self.validate_cc_count(Direction::Ul)?;
        if let Self::Nr(component) = self {
            component.validate_cross_cc_feature_index_agreement()?;
        }
        Ok(())
    }

    /// A direction's per-CC list length (resolved feature sets, else the raw selector bytes)
    /// must equal `cc_count(kind, bw_class)`. `ul_bw_class == 0` means UL is disabled (no UL
    /// data expected, and `cc_count` must never be called with `0`); DL has no such
    /// "disabled" class, so any per-CC DL data requires a class to check against.
    fn validate_cc_count(&self, direction: Direction) -> anyhow::Result<()> {
        let (len, bw_class) = match direction {
            Direction::Dl => (self.dl_per_cc_len(), self.dl_bw_class()),
            Direction::Ul => (self.ul_per_cc_len(), self.ul_bw_class()),
        };
        let Some(len) = len else {
            return Ok(());
        };
        let bw_class = bw_class.ok_or_else(|| {
            anyhow::anyhow!(
                "component {} carries per-CC {direction} data without a {}_bw_class",
                self.band_label(),
                direction.lowercase()
            )
        })?;
        if direction == Direction::Ul && bw_class == 0 {
            return Ok(());
        }
        let expected = cc_count(self.kind(), bw_class)?;
        anyhow::ensure!(
            len == expected,
            "component {} {direction} per-CC list length {len} does not match cc_count {expected} for {}_bw_class {bw_class}",
            self.band_label(),
            direction.lowercase()
        );
        Ok(())
    }

    /// The DL per-CC list length, or `None` when the direction carries no per-CC data at all.
    fn dl_per_cc_len(&self) -> Option<usize> {
        match self {
            Self::Lte(component) => component.dl.selector.as_ref().map(Vec::len),
            Self::Nr(component) => component.dl.per_cc_len(),
        }
    }

    /// See [`dl_per_cc_len`](Self::dl_per_cc_len).
    fn ul_per_cc_len(&self) -> Option<usize> {
        match self {
            Self::Lte(component) => component.ul.selector.as_ref().map(Vec::len),
            Self::Nr(component) => component.ul.per_cc_len(),
        }
    }

    pub(crate) fn band_label(&self) -> String {
        band_label_for(self.kind(), self.band())
    }
}

impl RawNrSubBlock {
    /// All CCs in an NR sub-block must derive the same single `dl_feature_index`/
    /// `ul_feature_index` — physically you cannot aggregate FR1+FR2 (or mixed MIMO
    /// presence) into one band's combo entry. LTE never derives a feature index, so this
    /// is NR-only, which the type now states directly.
    fn validate_cross_cc_feature_index_agreement(&self) -> anyhow::Result<()> {
        let dl = self.dl.resolved();
        if let Some(first) = dl.first() {
            let want = derive_nr_dl_index(Some(first.max_scs.unwrap_or(0)));
            for feature in &dl[1..] {
                let got = derive_nr_dl_index(Some(feature.max_scs.unwrap_or(0)));
                anyhow::ensure!(
                    got == want,
                    "component {} CCs disagree on derived DL feature index ({want} vs {got}); cannot aggregate FR1+FR2 in one band",
                    self.band_label()
                );
            }
        }
        let ul = self.ul.resolved();
        if let Some(first) = ul.first() {
            let want = derive_nr_ul_index(Some(first.max_mimo_cb.unwrap_or(0)));
            for feature in &ul[1..] {
                let got = derive_nr_ul_index(Some(feature.max_mimo_cb.unwrap_or(0)));
                anyhow::ensure!(
                    got == want,
                    "component {} CCs disagree on derived UL feature index ({want} vs {got})",
                    self.band_label()
                );
            }
        }
        Ok(())
    }
}

/// Header and component fields that make up one NR combo payload. Source
/// provenance, group packing, and wire bitmask live outside this neutral value.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RawNrPayload {
    pub(crate) power_class: Option<i32>,
    pub(crate) bcs_nr: Option<u32>,
    pub(crate) bcs_intra_endc: Option<u32>,
    pub(crate) bcs_eutra: Option<u32>,
    pub(crate) intra_band_en_dc_support: Option<i32>,
    pub(crate) sub_blocks: Vec<RawSubBlock>,
}

impl From<&Combo> for RawNrPayload {
    fn from(combo: &Combo) -> Self {
        let mut sub_blocks: Vec<_> = combo
            .sub_blocks
            .iter()
            // `from_sub_block` already drops the selector bytes a resolved direction
            // supersedes — `PerCc` cannot hold both — so the explicit clearing this
            // conversion used to do is now structural.
            .map(RawSubBlock::from_sub_block)
            .collect();
        sub_blocks.sort_by_cached_key(|component| RawSubBlockKey::from(component));
        Self {
            power_class: combo.power_class,
            bcs_nr: combo.bcs_nr,
            bcs_intra_endc: combo.bcs_intra_endc,
            bcs_eutra: combo.bcs_eutra,
            intra_band_en_dc_support: combo.intra_band_en_dc_support,
            sub_blocks,
        }
    }
}

impl RawNrPayload {
    /// The combo header (`ComboHeader`) for this payload, materialized for compiler NR
    /// generation (C-hdr).
    ///
    /// Four of the five header fields (all but `bcs_intra_endc`) are corpus-verified
    /// always `Some` once a payload has passed through the strict decode boundary
    /// (`from_proto_combo` below) or the KDL source reader (`compiler::kdl_source::read_combo`,
    /// Task 8's omit-when-0 defaulting) — so the old "all five fields `None` ⇒ no header"
    /// case is unrepresentable from real source and has never been observed in the corpus.
    /// This always returns `Some`; the `Option` return type is kept only because
    /// `ComboGroup::combo_header` is itself `Option<ComboHeader>`.
    pub(crate) const fn header(&self) -> Option<ComboHeader> {
        Some(ComboHeader {
            bcs_nr: self.bcs_nr,
            bcs_intra_endc: self.bcs_intra_endc,
            bcs_eutra: self.bcs_eutra,
            power_class: self.power_class,
            intra_band_en_dc_support: self.intra_band_en_dc_support,
        })
    }

    /// Build a raw payload directly from a protobuf combo `Combo` and its group header,
    /// using the file's feature-set lists — the folder-ingest path that avoids the report
    /// `Combo`/`SubBlock` DTO round-trip (E6). Byte-equivalent to `from_compiler_combo` applied to
    /// the same combo's DTO.
    ///
    /// This is the strict ingest boundary for the four always-present header fields
    /// (`power_class`, `bcs_nr`, `bcs_eutra`, `intra_band_en_dc_support` — corpus-verified,
    /// unlike `bcs_intra_endc` which has genuine `None`): a real decoded combo always
    /// carries a header with these four fields set, so a missing header or a missing field
    /// fails closed instead of silently normalizing to `0` on data that has never actually
    /// shown that shape.
    pub(crate) fn from_proto_combo(
        header: Option<&ComboHeader>,
        combo: &ProtoCombo,
        dl_list: &[ShannonFeatureSetDlPerCcNr],
        ul_list: &[ShannonFeatureSetUlPerCcNr],
    ) -> anyhow::Result<Self> {
        anyhow::ensure!(
            header.is_some(),
            "combo omits its header (never observed; refusing to normalize to 0)"
        );
        anyhow::ensure!(
            header.is_some_and(|header| header.power_class.is_some()),
            "combo header omits power_class (never observed; refusing to normalize to 0)"
        );
        anyhow::ensure!(
            header.is_some_and(|header| header.bcs_nr.is_some()),
            "combo header omits bcs_nr (never observed; refusing to normalize to 0)"
        );
        anyhow::ensure!(
            header.is_some_and(|header| header.bcs_eutra.is_some()),
            "combo header omits bcs_eutra (never observed; refusing to normalize to 0)"
        );
        anyhow::ensure!(
            header.is_some_and(|header| header.intra_band_en_dc_support.is_some()),
            "combo header omits intra_band_en_dc_support (never observed; refusing to normalize to 0)"
        );
        let mut sub_blocks = combo
            .sub_blocks
            .iter()
            .map(|component| RawSubBlock::from_proto_sub_block(component, dl_list, ul_list))
            .collect::<anyhow::Result<Vec<_>>>()?;
        sub_blocks.sort_by_cached_key(|component| RawSubBlockKey::from(component));
        Ok(Self {
            power_class: header.and_then(|header| header.power_class),
            bcs_nr: header.and_then(|header| header.bcs_nr),
            bcs_intra_endc: header.and_then(|header| header.bcs_intra_endc),
            bcs_eutra: header.and_then(|header| header.bcs_eutra),
            intra_band_en_dc_support: header.and_then(|header| header.intra_band_en_dc_support),
            sub_blocks,
        })
    }

    #[cfg(test)]
    pub(crate) fn from_compiler_combo(combo: &Combo) -> Self {
        let mut sub_blocks = combo
            .sub_blocks
            .iter()
            .map(|component| {
                RawSubBlock::from_sub_block(component).with_resolved_feature_sets(
                    component.dl_features.clone(),
                    component.ul_features.clone(),
                )
            })
            .collect::<Vec<_>>();
        sub_blocks.sort_by_cached_key(|component| RawSubBlockKey::from(component));
        Self {
            power_class: combo.power_class,
            bcs_nr: combo.bcs_nr,
            bcs_intra_endc: combo.bcs_intra_endc,
            bcs_eutra: combo.bcs_eutra,
            intra_band_en_dc_support: combo.intra_band_en_dc_support,
            sub_blocks,
        }
    }
}

/// A DL per-CC feature set reduced to an `Ord`-able tuple (`ShannonFeatureSetDlPerCcNr`
/// derives `Eq`/`Hash` but not `Ord`, and pulling in the compiler's `DlFeatureSource`
/// would create a reverse module dependency — raw_nr must not depend on `compiler`).
/// Field order matches the proto: scs, mimo, bw, mod_order, 90mhz.
type DlFeatureKey = (
    Option<i32>,
    Option<i32>,
    Option<i32>,
    Option<i32>,
    Option<bool>,
);

/// A UL per-CC feature set reduced to an `Ord`-able tuple. Field order matches the
/// proto: scs, mimo_cb, bw, mod_order, 90mhz, mimo_non_cb.
type UlFeatureKey = (
    Option<i32>,
    Option<i32>,
    Option<i32>,
    Option<i32>,
    Option<bool>,
    Option<i32>,
);

const fn dl_feature_key(f: &ShannonFeatureSetDlPerCcNr) -> DlFeatureKey {
    (
        f.max_scs,
        f.max_mimo,
        f.max_bw,
        f.max_mod_order,
        f.bw_90mhz_supported,
    )
}

const fn ul_feature_key(f: &ShannonFeatureSetUlPerCcNr) -> UlFeatureKey {
    (
        f.max_scs,
        f.max_mimo_cb,
        f.max_bw,
        f.max_mod_order,
        f.bw_90mhz_supported,
        f.max_mimo_non_cb,
    )
}

/// Full ordered form of one raw component. Resolved feature values win over
/// selector bytes; selector-only values retain exact `Option<Vec<u8>>` presence.
/// An empty `dl_features`/`ul_features` vec IS the "no feature set" identity (mirrors
/// the old `dl_feature_set_present` bool: a resolved-but-all-absent wrapper is a
/// one-element vec of all-`None` fields, distinct from a zero-element vec).
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct RawSubBlockKey {
    kind: SubBlockKind,
    band: i32,
    dl_bw_class: Option<i32>,
    ul_bw_class: Option<i32>,
    dl_feature_index: Option<i32>,
    ul_feature_index: Option<i32>,
    dl_cc_ids: Option<Vec<u8>>,
    ul_cc_ids: Option<Vec<u8>>,
    srs_tx_switch: Option<i32>,
    dl_features: Vec<DlFeatureKey>,
    ul_features: Vec<UlFeatureKey>,
}

impl From<&RawSubBlock> for RawSubBlockKey {
    fn from(cc: &RawSubBlock) -> Self {
        Self {
            kind: cc.kind(),
            band: cc.band(),
            dl_bw_class: cc.dl_bw_class(),
            ul_bw_class: cc.ul_bw_class(),
            // Materialized, not raw: identity must reflect the value that will actually land
            // in the binary (an NR component derives its index), or a key built from source
            // disagrees with the same combo's key rebuilt after decoding the materialized
            // output.
            dl_feature_index: cc.dl_feature_index(),
            ul_feature_index: cc.ul_feature_index(),
            // `PerCc` yields selector bytes only for a direction that did not resolve, so
            // the old explicit "mask the selector once the feature set is present" step is
            // now structural.
            dl_cc_ids: cc.dl_selector().map(<[u8]>::to_vec),
            ul_cc_ids: cc.ul_selector().map(<[u8]>::to_vec),
            srs_tx_switch: cc.srs_tx_switch(),
            dl_features: cc.dl_features().iter().map(dl_feature_key).collect(),
            ul_features: cc.ul_features().iter().map(ul_feature_key).collect(),
        }
    }
}

/// Ordered payload identity with component order normalized away.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct RawNrPayloadKey {
    power_class: Option<i32>,
    bcs_nr: Option<u32>,
    bcs_intra_endc: Option<u32>,
    bcs_eutra: Option<u32>,
    intra_band_en_dc_support: Option<i32>,
    sub_blocks: Vec<RawSubBlockKey>,
}

impl From<&RawNrPayload> for RawNrPayloadKey {
    fn from(payload: &RawNrPayload) -> Self {
        let mut sub_blocks: Vec<RawSubBlockKey> = payload
            .sub_blocks
            .iter()
            .map(RawSubBlockKey::from)
            .collect();
        sub_blocks.sort_unstable();
        Self {
            power_class: payload.power_class,
            bcs_nr: payload.bcs_nr,
            bcs_intra_endc: payload.bcs_intra_endc,
            bcs_eutra: payload.bcs_eutra,
            intra_band_en_dc_support: payload.intra_band_en_dc_support,
            sub_blocks,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        proto::{ComboGroup, ShannonFeatureSetDlPerCcNr, ShannonFeatureSetUlPerCcNr, UeCaps},
        report::combos::build_combos_with_bitmasks,
    };

    fn report_cc(
        dl_feature_per_cc_ids: Option<Vec<u8>>,
        dl_feature_per_cc: Option<ShannonFeatureSetDlPerCcNr>,
    ) -> SubBlock {
        SubBlock {
            band: "n78".to_string(),
            dl_bw_class: Some(1),
            ul_bw_class: Some(1),
            dl_feature_per_cc_ids,
            dl_features: dl_feature_per_cc.into_iter().collect(),
            ..Default::default()
        }
    }

    /// Returns the NR *variant struct*, not the enum, so tests can keep using functional
    /// update (`RawNrSubBlock { dl: …, ..nr_cc(78) }`) and `.into()` at the use site.
    fn nr_cc(band: i32) -> RawNrSubBlock {
        RawNrSubBlock {
            band,
            dl: NrDirection::bare(Some(1)),
            ul: NrDirection::bare(Some(1)),
            srs_tx_switch: None,
        }
    }

    fn payload(sub_blocks: Vec<RawSubBlock>) -> RawNrPayload {
        RawNrPayload {
            power_class: Some(3),
            bcs_nr: Some(1),
            bcs_intra_endc: None,
            bcs_eutra: None,
            intra_band_en_dc_support: None,
            sub_blocks,
        }
    }

    #[test]
    fn from_proto_combo_matches_the_report_dto_ingest_path() {
        // Direct protobuf ingest (E6) must be byte-equivalent to the old path that first
        // built the report `Combo`/`SubBlock` DTO and then reparsed it via `from_compiler_combo`.
        // Exercise all three component shapes: a resolved DL+UL feature set (selector cleared),
        // an NR component whose selector is a raw byte 0 (no feature set, id kept), and a plain
        // E-UTRA component.
        let caps = UeCaps {
            dl_feature_per_cc_list: vec![ShannonFeatureSetDlPerCcNr {
                max_scs: Some(3),
                max_mimo: Some(4),
                max_bw: Some(100),
                max_mod_order: Some(8),
                bw_90mhz_supported: Some(true),
            }],
            ul_feature_per_cc_list: vec![ShannonFeatureSetUlPerCcNr {
                max_scs: Some(1),
                max_mimo_cb: Some(2),
                max_bw: Some(50),
                max_mod_order: Some(6),
                bw_90mhz_supported: None,
                max_mimo_non_cb: Some(1),
            }],
            combo_groups: vec![ComboGroup {
                // The four corpus-verified always-`Some` header fields (all but
                // `bcs_intra_endc`) must be `Some` here — the strict `from_proto_combo`
                // ingest boundary now fails closed on a missing one (Task 8).
                combo_header: Some(ComboHeader {
                    power_class: Some(3),
                    bcs_nr: Some(1),
                    bcs_eutra: Some(0),
                    intra_band_en_dc_support: Some(0),
                    ..Default::default()
                }),
                combo: vec![ProtoCombo {
                    sub_blocks: vec![
                        ProtoSubBlock {
                            band: NR_BAND_OFFSET + 78,
                            dl_bw_class: Some(1),
                            ul_bw_class: Some(1),
                            dl_feature_per_cc_ids: Some(vec![1]),
                            ul_feature_per_cc_ids: Some(vec![1]),
                            ..Default::default()
                        },
                        ProtoSubBlock {
                            band: NR_BAND_OFFSET + 41,
                            dl_bw_class: Some(2),
                            // DL-only: UL disabled but still corpus-verified `Some(0)`,
                            // never absent.
                            ul_bw_class: Some(0),
                            // All-zero placeholder selector: resolves to no feature set, id
                            // kept. (The stored DL index derives to 0, so no explicit
                            // `dl_feature_index` is carried — the source override is gone.)
                            dl_feature_per_cc_ids: Some(vec![0]),
                            ..Default::default()
                        },
                        ProtoSubBlock {
                            band: 3,
                            dl_bw_class: Some(4),
                            ul_bw_class: Some(2),
                            ..Default::default()
                        },
                    ],
                    bitmask: Some(0),
                }],
            }],
            ..Default::default()
        };

        let dto: Vec<RawNrPayload> = build_combos_with_bitmasks(&caps)
            .into_iter()
            .map(|(combo, _)| RawNrPayload::from_compiler_combo(&combo))
            .collect();
        let dl_list = &caps.dl_feature_per_cc_list;
        let ul_list = &caps.ul_feature_per_cc_list;
        let direct: Vec<RawNrPayload> = caps
            .combo_groups
            .iter()
            .flat_map(|group| {
                let header = group.combo_header.as_ref();
                group.combo.iter().map(move |combo| {
                    RawNrPayload::from_proto_combo(header, combo, dl_list, ul_list).unwrap()
                })
            })
            .collect();
        assert_eq!(direct, dto);
    }

    #[test]
    fn from_proto_sub_block_resolves_distinct_non_uniform_per_cc_features() {
        // Model-layer regression for the data-loss bug: a 2-byte DL selector `[1, 2]`
        // against a 2-entry catalog must resolve to BOTH distinct feature sets, not just
        // CC0's. This is what the old first-byte-only resolution lost.
        let dl_list = vec![
            ShannonFeatureSetDlPerCcNr {
                max_scs: Some(1),
                ..Default::default()
            },
            ShannonFeatureSetDlPerCcNr {
                max_scs: Some(2),
                ..Default::default()
            },
        ];
        let component = ProtoSubBlock {
            band: NR_BAND_OFFSET + 78,
            dl_bw_class: Some(1),
            ul_bw_class: Some(0),
            dl_feature_per_cc_ids: Some(vec![1, 2]),
            ..Default::default()
        };

        let cc = RawSubBlock::from_proto_sub_block(&component, &dl_list, &[]).unwrap();

        assert_eq!(
            cc.dl_features().len(),
            2,
            "both CC feature sets must survive"
        );
        assert_eq!(cc.dl_features()[0].max_scs, Some(1));
        assert_eq!(cc.dl_features()[1].max_scs, Some(2));
    }

    #[test]
    fn from_proto_sub_block_keys_differ_when_only_the_second_cc_feature_differs() {
        let dl_list = vec![
            ShannonFeatureSetDlPerCcNr {
                max_scs: Some(1),
                ..Default::default()
            },
            ShannonFeatureSetDlPerCcNr {
                max_scs: Some(2),
                ..Default::default()
            },
        ];
        let base = ProtoSubBlock {
            band: NR_BAND_OFFSET + 78,
            dl_bw_class: Some(1),
            ul_bw_class: Some(0),
            dl_feature_per_cc_ids: Some(vec![1, 2]),
            ..Default::default()
        };
        let differs_in_second_cc = ProtoSubBlock {
            dl_feature_per_cc_ids: Some(vec![1, 1]),
            ..base.clone()
        };

        let a = RawSubBlock::from_proto_sub_block(&base, &dl_list, &[]).unwrap();
        let b = RawSubBlock::from_proto_sub_block(&differs_in_second_cc, &dl_list, &[]).unwrap();

        assert_ne!(
            RawSubBlockKey::from(&a),
            RawSubBlockKey::from(&b),
            "two sub-blocks differing only in the second CC's feature must have distinct keys"
        );
    }

    #[test]
    fn from_proto_sub_block_rejects_missing_ul_bw_class() {
        // Task 8: `ul_bw_class` is corpus-verified always `Some` on a real sub-block, so a
        // synthetic decode with it truly absent (`None`, not the omitted-`Some(0)` KDL
        // shape) must fail closed rather than silently normalize to `0`.
        let component = ProtoSubBlock {
            band: NR_BAND_OFFSET + 78,
            dl_bw_class: Some(1),
            ul_bw_class: None,
            ..Default::default()
        };
        let err = RawSubBlock::from_proto_sub_block(&component, &[], &[]).unwrap_err();
        assert!(err.to_string().contains("ul_bw_class"), "{err}");
    }

    #[test]
    fn from_proto_rejects_nr_feature_index_mismatch() {
        // NR sub-block whose stored index disagrees with the value derived from its feature set:
        // the source format no longer carries an override, so decode must fail loudly.
        let list = vec![ShannonFeatureSetDlPerCcNr {
            max_scs: Some(1),
            ..Default::default()
        }];
        let proto = ProtoSubBlock {
            band: NR_BAND_OFFSET + 78,
            dl_bw_class: Some(1),
            ul_bw_class: Some(0),
            dl_feature_index: Some(99), // deliberately != derive_nr_dl_index(Some(1))
            dl_feature_per_cc_ids: Some(vec![1]),
            ..Default::default()
        };
        let err = RawSubBlock::from_proto_sub_block(&proto, &list, &[])
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("feature index") && err.contains("derived"),
            "{err}"
        );
    }

    #[test]
    fn from_proto_rejects_non_placeholder_unresolvable_selector() {
        // A non-zero selector byte that indexes past the feature list can no longer be carried
        // as a raw `dl-cc-id` fallback — decode must fail loudly. (All-zero placeholders are fine.)
        let proto = ProtoSubBlock {
            band: NR_BAND_OFFSET + 78,
            dl_bw_class: Some(1),
            ul_bw_class: Some(0),
            dl_feature_per_cc_ids: Some(vec![7]), // 7 > empty list length -> unresolvable, non-zero
            ..Default::default()
        };
        let err = RawSubBlock::from_proto_sub_block(&proto, &[], &[])
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("selector") && err.contains("no feature"),
            "{err}"
        );
    }

    #[test]
    fn from_proto_accepts_the_all_zero_placeholder_selector() {
        // Counterpart to `from_proto_rejects_non_placeholder_unresolvable_selector`: the
        // all-zero placeholder resolves to no feature set and its bytes are kept verbatim,
        // so `from_proto_sub_block` must pass it through rather than reject it (do not
        // over-guard).
        let proto = ProtoSubBlock {
            band: NR_BAND_OFFSET + 78,
            dl_bw_class: Some(1),
            ul_bw_class: Some(0),
            dl_feature_per_cc_ids: Some(vec![0]),
            ..Default::default()
        };

        let raw = RawSubBlock::from_proto_sub_block(&proto, &[], &[]).unwrap();

        assert!(
            raw.dl_features().is_empty(),
            "placeholder resolves to no feature set"
        );
        assert_eq!(
            raw.dl_selector(),
            Some([0].as_slice()),
            "placeholder bytes are kept verbatim"
        );
    }

    #[test]
    fn from_proto_accepts_a_matching_nr_feature_index() {
        // Counterpart to `from_proto_rejects_nr_feature_index_mismatch`: a stored index that
        // AGREES with the derivation round-trips fine and must not be rejected.
        // derive_nr_dl_index(Some(1)) == 1.
        let list = vec![ShannonFeatureSetDlPerCcNr {
            max_scs: Some(1),
            ..Default::default()
        }];
        let proto = ProtoSubBlock {
            band: NR_BAND_OFFSET + 78,
            dl_bw_class: Some(1),
            ul_bw_class: Some(0),
            dl_feature_index: Some(derive_nr_dl_index(Some(1))),
            dl_feature_per_cc_ids: Some(vec![1]),
            ..Default::default()
        };

        let raw = RawSubBlock::from_proto_sub_block(&proto, &list, &[]).unwrap();

        assert_eq!(raw.dl_feature_index(), Some(1));
        assert_eq!(raw.dl_features().len(), 1);
    }

    #[test]
    fn from_proto_does_not_apply_the_index_derivation_guard_to_lte() {
        // The derivation guard is NR-only (kind-gated). An E-UTRA component's
        // dl_feature_index is a genuine stored value with no derivation to check against,
        // so an arbitrary one must NOT trip the guard (do not over-guard).
        let proto = ProtoSubBlock {
            band: 66, // < NR_BAND_OFFSET -> SubBlockKind::Lte
            dl_bw_class: Some(1),
            ul_bw_class: Some(1),
            dl_feature_index: Some(99),
            ..Default::default()
        };

        let raw = RawSubBlock::from_proto_sub_block(&proto, &[], &[]).unwrap();

        assert_eq!(raw.kind(), SubBlockKind::Lte);
        assert_eq!(raw.dl_feature_index(), Some(99));
    }

    #[test]
    fn from_proto_combo_rejects_missing_header_or_header_fields() {
        // Task 8: a real combo always carries a header with the four always-`Some` fields
        // set (all but `bcs_intra_endc`); a missing header, or a header missing one of
        // those fields, must fail closed.
        let combo = ProtoCombo {
            sub_blocks: vec![ProtoSubBlock {
                band: NR_BAND_OFFSET + 78,
                dl_bw_class: Some(1),
                ul_bw_class: Some(1),
                ..Default::default()
            }],
            bitmask: Some(0),
        };

        let no_header = RawNrPayload::from_proto_combo(None, &combo, &[], &[]).unwrap_err();
        assert!(no_header.to_string().contains("header"), "{no_header}");

        let partial_header = ComboHeader {
            power_class: Some(3),
            bcs_nr: Some(1),
            // bcs_eutra and intra_band_en_dc_support omitted (None) on purpose.
            ..Default::default()
        };
        let missing_field =
            RawNrPayload::from_proto_combo(Some(&partial_header), &combo, &[], &[]).unwrap_err();
        assert!(
            missing_field.to_string().contains("bcs_eutra"),
            "{missing_field}"
        );
    }

    #[test]
    fn raw_sub_block_key_prefers_any_resolved_dl_vec_over_raw_selector_bytes() {
        // A non-empty `dl_features` vec IS the presence signal now (Task 7 removed the old
        // CC0-only "does the one entry have any field set" gate) — even a single all-`None`
        // entry (a legitimate all-absent catalog record) counts as resolved and masks the
        // raw selector bytes from identity, exactly like a partial-value entry.
        let partial = ShannonFeatureSetDlPerCcNr {
            max_bw: Some(100),
            ..Default::default()
        };
        let resolved_empty = RawSubBlock::from_sub_block(&report_cc(Some(vec![]), Some(partial)));
        let resolved_multibyte =
            RawSubBlock::from_sub_block(&report_cc(Some(vec![0, 2]), Some(partial)));
        assert_eq!(
            RawSubBlockKey::from(&resolved_empty),
            RawSubBlockKey::from(&resolved_multibyte),
            "a real resolved value must win over selector bytes"
        );

        let wrapper_empty = RawSubBlock::from_sub_block(&report_cc(
            Some(vec![]),
            Some(ShannonFeatureSetDlPerCcNr::default()),
        ));
        let wrapper_multibyte = RawSubBlock::from_sub_block(&report_cc(
            Some(vec![0, 2]),
            Some(ShannonFeatureSetDlPerCcNr::default()),
        ));
        assert_eq!(
            RawSubBlockKey::from(&wrapper_empty),
            RawSubBlockKey::from(&wrapper_multibyte),
            "an all-None resolved wrapper is still a present (non-empty) vec, not selector-only"
        );
    }

    #[test]
    fn raw_sub_block_key_totally_orders_selector_only_none_empty_and_multibyte_values() {
        let none: RawSubBlock = nr_cc(78).into();
        let empty: RawSubBlock = RawNrSubBlock {
            dl: NrDirection::with_selector(1, vec![]),
            ..nr_cc(78)
        }
        .into();
        let multibyte: RawSubBlock = RawNrSubBlock {
            dl: NrDirection::with_selector(1, vec![0, 2]),
            ..nr_cc(78)
        }
        .into();

        assert!(RawSubBlockKey::from(&none) < RawSubBlockKey::from(&empty));
        assert!(RawSubBlockKey::from(&empty) < RawSubBlockKey::from(&multibyte));
    }

    #[test]
    fn explicit_zero_raw_value_is_resolved_and_remains_in_the_key() {
        // A resolved direction cannot also hold selector bytes, so the two "same feature
        // set, different leftover selector" cases collapse to one value by construction.
        let zero_empty: RawSubBlock = RawNrSubBlock {
            dl: NrDirection::with_features(
                1,
                vec![ShannonFeatureSetDlPerCcNr {
                    max_scs: Some(0),
                    ..Default::default()
                }],
            ),
            ..nr_cc(78)
        }
        .into();
        let zero_multibyte = zero_empty.clone();
        let absent: RawSubBlock = RawNrSubBlock {
            dl: NrDirection::with_selector(1, vec![]),
            ..nr_cc(78)
        }
        .into();

        assert_eq!(
            RawSubBlockKey::from(&zero_empty),
            RawSubBlockKey::from(&zero_multibyte)
        );
        assert_ne!(
            RawSubBlockKey::from(&zero_empty),
            RawSubBlockKey::from(&absent)
        );
    }

    #[test]
    fn nr_component_validation_errors_use_the_n_band_label() {
        // `validate()` used to hardcode `B{}`, misprinting every NR band (n78 as B78).
        // Route through `band_label()` so the label always matches the component's kind.
        // DL branch: cc_count(Nr, 1) == 1, so two per-CC features is a length mismatch.
        let dl_mismatch: RawSubBlock = RawNrSubBlock {
            band: 78,
            dl: NrDirection::with_features(
                1,
                vec![
                    ShannonFeatureSetDlPerCcNr {
                        max_scs: Some(1),
                        ..Default::default()
                    },
                    ShannonFeatureSetDlPerCcNr {
                        max_scs: Some(1),
                        ..Default::default()
                    },
                ],
            ),
            ul: NrDirection::bare(Some(0)),
            srs_tx_switch: None,
        }
        .into();
        let error = dl_mismatch.validate().unwrap_err().to_string();
        assert!(error.contains("component n78"), "{error}");
        assert!(
            !error.contains("B78"),
            "must not misprint an NR band as B78: {error}"
        );

        // Cross-CC agreement branch (NR-only function — the label there was always wrong):
        // scs 1 derives DL index 1, scs 4 derives 2, so two CCs disagree.
        let disagreeing: RawSubBlock = RawNrSubBlock {
            band: 41,
            // cc_count(Nr, 2) == 2, so the length check passes first
            dl: NrDirection::with_features(
                2,
                vec![
                    ShannonFeatureSetDlPerCcNr {
                        max_scs: Some(1),
                        ..Default::default()
                    },
                    ShannonFeatureSetDlPerCcNr {
                        max_scs: Some(4),
                        ..Default::default()
                    },
                ],
            ),
            ul: NrDirection::bare(Some(0)),
            srs_tx_switch: None,
        }
        .into();
        let error = disagreeing.validate().unwrap_err().to_string();
        assert!(error.contains("component n41"), "{error}");
        assert!(
            !error.contains("B41"),
            "must not misprint an NR band as B41: {error}"
        );
    }

    #[test]
    fn payload_key_is_independent_of_component_order() {
        let first = payload(vec![nr_cc(78).into(), nr_cc(41).into()]);
        let second = payload(vec![nr_cc(41).into(), nr_cc(78).into()]);

        assert_eq!(
            RawNrPayloadKey::from(&first),
            RawNrPayloadKey::from(&second)
        );
    }

    #[test]
    fn derive_nr_dl_index_maps_scs_to_fr() {
        assert_eq!(derive_nr_dl_index(None), 0); // no DL feature set
        assert_eq!(derive_nr_dl_index(Some(0)), 1); // present, scs absent -> FR1
        assert_eq!(derive_nr_dl_index(Some(1)), 1); // 15 kHz FR1
        assert_eq!(derive_nr_dl_index(Some(2)), 1); // 30 kHz FR1
        assert_eq!(derive_nr_dl_index(Some(4)), 2); // 120 kHz FR2
    }

    #[test]
    fn derive_nr_ul_index_maps_mimo_cb() {
        assert_eq!(derive_nr_ul_index(None), 0); // no UL feature set
        assert_eq!(derive_nr_ul_index(Some(1)), 1); // no MIMO
        assert_eq!(derive_nr_ul_index(Some(2)), 2); // MIMO
    }

    #[test]
    fn nr_derives_its_feature_index_and_lte_stores_one() {
        let nr = RawNrSubBlock {
            band: 78,
            dl: NrDirection::with_features(
                1,
                vec![ShannonFeatureSetDlPerCcNr {
                    max_scs: Some(4),
                    ..Default::default()
                }],
            ),
            ul: NrDirection::with_features(
                1,
                vec![ShannonFeatureSetUlPerCcNr {
                    max_mimo_cb: Some(2),
                    ..Default::default()
                }],
            ),
            srs_tx_switch: None,
        };
        assert_eq!(nr.derived_dl_feature_index(), 2);
        assert_eq!(nr.derived_ul_feature_index(), 2);
        let nr: RawSubBlock = nr.into();
        // There is no stored NR index to prefer, so the binary-bound value IS the derivation
        // and the source-bound value is nothing.
        assert_eq!(nr.dl_feature_index(), Some(2));
        assert_eq!(nr.ul_feature_index(), Some(2));
        assert_eq!(nr.source_dl_feature_index(), None);
        assert_eq!(nr.source_ul_feature_index(), None);

        // NR with no feature set derives 0.
        let bare: RawSubBlock = RawNrSubBlock {
            band: 78,
            ..Default::default()
        }
        .into();
        assert_eq!(bare.dl_feature_index(), Some(0));

        // LTE is never derived: the stored value is used for both the binary and the source,
        // and `None` stays `None`.
        let lte: RawSubBlock = RawLteSubBlock {
            band: 66,
            dl: LteDirection {
                feature_index: Some(3),
                ..Default::default()
            },
            ul: LteDirection::default(),
        }
        .into();
        assert_eq!(lte.dl_feature_index(), Some(3));
        assert_eq!(lte.source_dl_feature_index(), Some(3));
        let lte_none: RawSubBlock = RawLteSubBlock {
            band: 66,
            ..Default::default()
        }
        .into();
        assert_eq!(lte_none.dl_feature_index(), None);
    }

    #[test]
    fn cc_count_matches_observed_tables() {
        use SubBlockKind::{Lte, Nr};
        // NR: DL and UL share one table (corpus-verified, zero exceptions).
        for (bw, n) in [
            (1, 1),
            (2, 2),
            (3, 2),
            (7, 2),
            (8, 3),
            (9, 4),
            (10, 5),
            (11, 6),
            (12, 7),
            (13, 8),
        ] {
            assert_eq!(cc_count(Nr, bw).unwrap(), n, "NR bw_class {bw}");
        }
        // LTE: distinct table.
        for (bw, n) in [(1, 1), (2, 2), (3, 2), (4, 3), (5, 4)] {
            assert_eq!(cc_count(Lte, bw).unwrap(), n, "LTE bw_class {bw}");
        }
    }

    #[test]
    fn validate_rejects_wrong_per_cc_count() {
        let sb: RawSubBlock = RawNrSubBlock {
            band: 48,
            // bw_class 2 expects 2 CCs; only 1 feature set is supplied
            dl: NrDirection::with_features(2, vec![ShannonFeatureSetDlPerCcNr::default()]),
            ..Default::default()
        }
        .into();
        assert!(sb.validate().unwrap_err().to_string().contains("cc_count"));
    }

    #[test]
    fn validate_rejects_disagreeing_cross_cc_feature_index() {
        // CC0 FR1 (scs<4), CC1 FR2 (scs>=4) -> ambiguous single dl_feature_index.
        let sb: RawSubBlock = RawNrSubBlock {
            band: 78,
            dl: NrDirection::with_features(
                2,
                vec![
                    ShannonFeatureSetDlPerCcNr {
                        max_scs: Some(1),
                        ..Default::default()
                    },
                    ShannonFeatureSetDlPerCcNr {
                        max_scs: Some(4),
                        ..Default::default()
                    },
                ],
            ),
            ..Default::default()
        }
        .into();
        assert!(
            sb.validate()
                .unwrap_err()
                .to_string()
                .contains("feature index")
        );
    }

    #[test]
    fn cc_count_fails_closed_on_unknown_class() {
        // NR never observed classes 4/5/6; LTE never observed >=6. Fail, do not guess.
        assert!(cc_count(SubBlockKind::Nr, 4).is_err());
        assert!(cc_count(SubBlockKind::Lte, 6).is_err());
        assert!(cc_count(SubBlockKind::Nr, 0).is_err()); // 0 = disabled, handled by callers, not here
    }
}
