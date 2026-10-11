//! Protobuf-shaped NR combo payloads shared by the compiler's ingest and generation paths.

use crate::{
    formatting::Optional,
    proto::{
        Combo as ProtoCombo, ComboHeader, ShannonFeatureSetDlPerCcNr, ShannonFeatureSetUlPerCcNr,
        SubBlock as ProtoSubBlock,
    },
};
use std::fmt::{self, Display, Formatter};

pub const NR_BAND_OFFSET: i32 = 10_000;

#[cfg(test)]
pub(crate) mod test_support;

/// Compiler-facing first-byte resolution; zero/absent/out-of-range remains unresolved.
pub(crate) fn feature_index(ids: Option<&[u8]>, len: usize) -> Option<usize> {
    let k = *ids?.first()? as usize;
    (1..=len).contains(&k).then(|| k - 1)
}

/// Compiler-facing all-or-nothing resolution. Report validation has a separate classifier.
pub(crate) fn resolve_all<T: Copy>(ids: Option<&[u8]>, list: &[T]) -> Option<Vec<T>> {
    let ids = ids?;
    if ids.is_empty() {
        return None;
    }
    ids.iter()
        .map(|&b| {
            let k = b as usize;
            (1..=list.len()).contains(&k).then(|| list[k - 1])
        })
        .collect()
}

pub(crate) fn bw_class_letter(class: u8) -> Option<char> {
    (1..=26)
        .contains(&class)
        .then(|| char::from(b'A' + class - 1))
}

pub(crate) fn bw_class_index(letter: char) -> Option<u8> {
    letter.is_ascii_uppercase().then(|| letter as u8 - b'A' + 1)
}

pub(crate) struct BandLabel {
    kind: SubBlockKind,
    band: i32,
}

impl Display for BandLabel {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(f, "{}{}", self.kind.prefix(), self.band)
    }
}

/// Per-component radio kind for a raw NR combo payload.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum SubBlockKind {
    Lte,
    Nr,
}

impl Display for SubBlockKind {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Lte => "LTE",
            Self::Nr => "NR",
        })
    }
}

impl SubBlockKind {
    /// The protobuf band value for a plain band number of this kind (NR bands are stored
    /// offset by [`NR_BAND_OFFSET`]).
    const fn raw_band(self, band: u16) -> i32 {
        let band = band as i32;
        match self {
            Self::Lte => band,
            Self::Nr => NR_BAND_OFFSET + band,
        }
    }

    /// The inverse of [`raw_band`](Self::raw_band): classify a protobuf band value and strip
    /// the NR offset, yielding the kind and the plain band number (`10078` -> `(Nr, 78)`).
    /// The single source for this split — every site that needs to *assert* a component's
    /// kind from its raw band goes through here rather than re-deriving the comparison.
    pub(crate) const fn split_raw_band(raw: i32) -> (Self, i32) {
        if raw >= NR_BAND_OFFSET {
            (Self::Nr, raw - NR_BAND_OFFSET)
        } else {
            (Self::Lte, raw)
        }
    }

    /// The human band label for a plain band number of this kind: `n<band>` (NR) or `B<band>`
    /// (E-UTRA). Callers with a known kind must retain it even for out-of-range LTE bands;
    /// only raw protobuf band values should pass through `split_raw_band`.
    pub(crate) fn band_label(self, band: i32) -> BandLabel {
        BandLabel { kind: self, band }
    }

    pub(crate) const fn prefix(self) -> &'static str {
        match self {
            Self::Nr => "n",
            Self::Lte => "B",
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
    /// The lowercase spelling used in *field* names (`dl_feature`, `ul_bw_class`). The
    /// uppercase prose spelling (`DL`/`UL`) is the [`Display`] impl.
    pub(crate) const fn lowercase(self) -> &'static str {
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

/// How proto field 6/7 (`dl/ul_feature_per_cc_ids`) is spelled when it IS present — the two
/// encodings are alternatives, never a mix. Absence is the enclosing `Option`, matching the
/// wire, where the field is simply missing (e.g. UL disabled, `ul_bw_class == 0`).
///
/// * `Selector(bytes)` — present but resolving to no catalog record. Only the all-zero
///   placeholder survives the decode boundary ([`resolve_or_placeholder`]); a nonzero
///   unresolvable selector is a hard error there, so the bytes reaching generation are
///   always `[0; cc_count]`.
/// * `Resolved(features)` — one feature set per CC. Never empty: an empty resolution means
///   "did not resolve" — the direction simply carries no per-CC data.
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
    pub(crate) bw_class: Option<u8>,
    pub(crate) features: Option<PerCc<T>>,
}

impl<T: Copy> NrDirection<T> {
    /// One resolved feature set per CC, or `&[]` when this direction resolved to nothing.
    fn resolved(&self) -> &[T] {
        match &self.features {
            Some(PerCc::Resolved(features)) => features,
            Some(PerCc::Selector(_)) | None => &[],
        }
    }

    /// The raw selector bytes, or `None` once resolved (or when absent).
    fn selector(&self) -> Option<&[u8]> {
        match &self.features {
            Some(PerCc::Selector(bytes)) => Some(bytes),
            Some(PerCc::Resolved(_)) | None => None,
        }
    }

    /// CC0's resolved feature set, or `None` when unresolved/absent.
    fn first(&self) -> Option<T> {
        self.resolved().first().copied()
    }

    /// How many CCs this direction describes — one per resolved feature set or per selector
    /// byte — or `None` when it carries no per-CC data at all. This is the length
    /// [`RawSubBlock::validate_cc_count`] checks against `cc_count(kind, bw_class)`.
    fn per_cc_len(&self) -> Option<usize> {
        self.features.as_ref().map(|per_cc| match per_cc {
            PerCc::Selector(bytes) => bytes.len(),
            PerCc::Resolved(features) => features.len(),
        })
    }
}

/// Constructors used by tests to spell a direction in one line. Production code builds
/// `NrDirection` field-wise from data it already has (a decoded selector, a resolved catalog
/// reference), so these exist purely to keep assertions readable.
#[cfg(test)]
impl<T: Copy> NrDirection<T> {
    /// A direction with one resolved feature set per CC.
    pub(crate) fn with_features(bw_class: u8, features: Vec<T>) -> Self {
        Self {
            bw_class: Some(bw_class),
            features: (!features.is_empty()).then_some(PerCc::Resolved(features)),
        }
    }

    /// A direction carrying raw selector bytes that resolved to nothing.
    pub(crate) fn with_selector(bw_class: u8, bytes: Vec<u8>) -> Self {
        Self {
            bw_class: Some(bw_class),
            features: Some(PerCc::Selector(bytes)),
        }
    }

    /// A direction with a bandwidth class but no per-CC data at all.
    pub(crate) const fn bare(bw_class: Option<u8>) -> Self {
        Self {
            bw_class,
            features: None,
        }
    }
}

/// One direction of an E-UTRA component: its CA bandwidth class, the stored
/// `parseLteFeatureIndex` value (proto 4/5 — a MIMO × CC-count code, not a catalog
/// reference), and proto field 6/7.
///
/// The selector is always the all-zero placeholder in the corpus (LTE references no per-CC
/// feature catalog), so its *contents* are fully derivable from `bw_class` — and since
/// [`RawSubBlock::validate`] now requires presence and `bw_class >= 1` to imply each other, so
/// is its presence. It is kept as a field because the length is what
/// [`validate_cc_count`](RawSubBlock::validate_cc_count) checks against `cc_count`, which is
/// how a wrong-length list is caught. (It previously claimed to exist "only so a decoded
/// component re-encodes with the field presence it arrived with" — that was never true:
/// regeneration re-derives presence from the class and never consulted this field.)
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct LteDirection {
    pub(crate) bw_class: Option<u8>,
    pub(crate) feature_index: Option<u16>,
    pub(crate) selector: Option<Vec<u8>>,
}

/// An E-UTRA component inside an EN-DC combo. It carries no resolved per-CC feature sets and
/// no `srs_tx_switch` — both NR-only, and previously excluded by a runtime check
/// (`has_nr_only_fields`) rather than by the type.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct RawLteSubBlock {
    pub(crate) band: u16,
    pub(crate) dl: LteDirection,
    pub(crate) ul: LteDirection,
}

impl RawLteSubBlock {
    fn direction(&self, direction: Direction) -> &LteDirection {
        match direction {
            Direction::Dl => &self.dl,
            Direction::Ul => &self.ul,
        }
    }
}

/// An NR component. Its `dl/ul_feature_index` (proto 4/5) is deliberately **not** a field: it
/// is a pure function of the resolved per-CC feature set (`derive_nr_dl_index` /
/// `derive_nr_ul_index`), re-derived wherever the binary needs it, and a decoded value that
/// disagrees is rejected at the boundary ([`ensure_feature_index_derivable`](RawNrSubBlock::ensure_feature_index_derivable)).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct RawNrSubBlock {
    pub(crate) band: u16,
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
    const fn kind(&self) -> SubBlockKind {
        match self {
            Self::Lte(_) => SubBlockKind::Lte,
            Self::Nr(_) => SubBlockKind::Nr,
        }
    }

    const fn band(&self) -> u16 {
        match self {
            Self::Lte(component) => component.band,
            Self::Nr(component) => component.band,
        }
    }

    pub(crate) const fn raw_band(&self) -> i32 {
        self.kind().raw_band(self.band())
    }

    pub(crate) fn bw_class(&self, direction: Direction) -> Option<u8> {
        match (self, direction) {
            (Self::Lte(component), direction) => component.direction(direction).bw_class,
            (Self::Nr(component), Direction::Dl) => component.dl.bw_class,
            (Self::Nr(component), Direction::Ul) => component.ul.bw_class,
        }
    }

    /// `None` for E-UTRA is true *by construction*, not by convention:
    /// [`lte_from_proto_sub_block`](Self::lte_from_proto_sub_block) rejects proto field 8 on an
    /// E-UTRA band and the KDL reader rejects `srs-tx-switch` on an `lte` node, so no E-UTRA
    /// component carrying one can be built. That matters because [`RawSubBlockKey`] reads this
    /// method for combo identity — while the field was silently discarded instead, two combos
    /// differing only in field 8 produced identical keys.
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

    /// Raw selector bytes, or `None` once resolved (or when absent).
    pub(crate) fn selector(&self, direction: Direction) -> Option<&[u8]> {
        match (self, direction) {
            (Self::Lte(component), direction) => component.direction(direction).selector.as_deref(),
            (Self::Nr(component), Direction::Dl) => component.dl.selector(),
            (Self::Nr(component), Direction::Ul) => component.ul.selector(),
        }
    }
}

/// Derive an NR component's `dl_feature_index` from its resolved DL per-CC feature set:
/// 0 = no feature set, 1 = FR1 (`max_scs < 4`), 2 = FR2 (`max_scs >= 4`). `scs` is `None`
/// iff there is no DL feature set; a present set with an absent `max_scs` maps to FR1.
/// Corpus-verified over 1.72M NR components — see
/// DESIGN.md.
/// LTE feature indexes are a different encoding (parseLteFeatureIndex) and are never derived.
fn derive_nr_dl_index(scs: Option<i32>) -> i32 {
    scs.map_or(0, |scs| if scs >= 4 { 2 } else { 1 })
}

/// Derive an NR component's `ul_feature_index` from its resolved UL per-CC feature set:
/// 0 = no feature set, 1 = no MIMO (`max_mimo_cb != 2`), 2 = MIMO (`max_mimo_cb == 2`).
fn derive_nr_ul_index(max_mimo_cb: Option<i32>) -> i32 {
    max_mimo_cb.map_or(0, |cb| if cb == 2 { 2 } else { 1 })
}

/// Observed Samsung Shannon `bw_class` → aggregated CC count for NR sub-blocks.
/// Exception-free across 3.46M corpus sub-blocks (DL and UL share this table).
const NR_CC_COUNTS: &[(u8, usize)] = &[
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
const LTE_CC_COUNTS: &[(u8, usize)] = &[(1, 1), (2, 2), (3, 2), (4, 3), (5, 4)];

/// Number of component carriers a sub-block of this kind and bandwidth class carries.
/// Fail-closed: an unobserved class errors rather than mis-deriving a per-CC list length.
/// `bw_class == 0` (UL disabled) is not a valid input here — callers gate on it first.
pub(crate) fn cc_count(kind: SubBlockKind, bw_class: u8) -> anyhow::Result<usize> {
    let table = match kind {
        SubBlockKind::Nr => NR_CC_COUNTS,
        SubBlockKind::Lte => LTE_CC_COUNTS,
    };
    table
        .iter()
        .find_map(|&(class, count)| (class == bw_class).then_some(count))
        .ok_or_else(|| {
            anyhow::anyhow!("unknown {kind} bw_class {bw_class}: cannot determine CC count")
        })
}

/// Whether `bytes` is the all-zero placeholder selector. The placeholder always resolves to
/// no feature set and is valid; any *other* selector that resolves to no feature set cannot be
/// carried and is rejected by the caller. Used at the decode boundary
/// ([`resolve_or_placeholder`]).
fn is_placeholder(bytes: &[u8]) -> bool {
    bytes.iter().all(|&b| b == 0)
}

/// Post-resolution split: resolved features clear the raw bytes; an unresolved selector may
/// survive ONLY as the all-zero placeholder (re-derivable from bw_class on the source round
/// trip). A non-zero unresolvable selector can no longer be carried — fail loudly.
fn resolve_or_placeholder<T: Copy>(
    resolved: Option<Vec<T>>,
    raw: Option<&[u8]>,
    direction: Direction,
    band: u16,
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
                is_placeholder(bytes),
                "component {} {direction} selector {bytes:?} resolves to no feature and is not the all-zero placeholder",
                SubBlockKind::Nr.band_label(i32::from(band)),
            );
            Ok(Some(PerCc::Selector(bytes.to_vec())))
        }
    }
}

impl RawNrSubBlock {
    /// For NR the feature index is derived, never stored, so a decoded index that disagrees
    /// with the derivation cannot round-trip — reject it at the decode boundary rather than
    /// silently dropping it. Absence may normalize only to zero, never to a nonzero index.
    fn ensure_feature_index_derivable(&self, stored: FeatureIndexes) -> anyhow::Result<()> {
        for (direction, stored, derived) in [
            (Direction::Dl, stored.dl, self.derived_dl_feature_index()),
            (Direction::Ul, stored.ul, self.derived_ul_feature_index()),
        ] {
            anyhow::ensure!(
                stored.unwrap_or(0) == derived,
                "NR component {} stored {direction} feature index {} != derived {derived}",
                self.band_label(),
                Optional(stored),
            );
        }
        Ok(())
    }

    /// Derived DL feature index for this component's resolved DL feature set (0 if none).
    fn derived_dl_feature_index(&self) -> i32 {
        derive_nr_dl_index(self.dl.first().map(|fs| fs.max_scs.unwrap_or(0)))
    }

    /// Derived UL feature index for this component's resolved UL feature set (0 if none).
    fn derived_ul_feature_index(&self) -> i32 {
        derive_nr_ul_index(self.ul.first().map(|fs| fs.max_mimo_cb.unwrap_or(0)))
    }

    fn band_label(&self) -> BandLabel {
        SubBlockKind::Nr.band_label(i32::from(self.band))
    }
}

/// The proto-4/5 pair as read off the wire, before it is validated away. NR does not keep it
/// (see [`RawNrSubBlock`]); this exists only so the decode boundary can compare what a file
/// stored against what the feature sets derive.
#[derive(Clone, Copy, Debug, Default)]
struct FeatureIndexes {
    dl: Option<i32>,
    ul: Option<i32>,
}

/// Narrow a protobuf `i32` capability field to the raw model's tighter observed width, failing
/// closed on an out-of-range value rather than silently truncating — the same stance as the
/// `ul_bw_class` presence check in [`from_proto_sub_block`](RawSubBlock::from_proto_sub_block).
/// Proto carries these as `i32`; the raw model uses the true widths (band `u16`, class `u8`).
fn narrow_field<T: TryFrom<i32>>(value: i32, field: &str) -> anyhow::Result<T> {
    T::try_from(value)
        .map_err(|_| anyhow::anyhow!("{field} value {value} is out of range for the raw model"))
}

/// The [`Option`] form of [`narrow_field`]: an absent proto field stays absent.
fn narrow_opt_field<T: TryFrom<i32>>(value: Option<i32>, field: &str) -> anyhow::Result<Option<T>> {
    value.map(|v| narrow_field(v, field)).transpose()
}

impl RawSubBlock {
    /// The feature index to write into the binary: the stored `parseLteFeatureIndex`
    /// value for LTE, the value derived from the per-CC feature set for NR.
    pub(crate) fn feature_index(&self, direction: Direction) -> Option<i32> {
        match (self, direction) {
            (Self::Lte(component), direction) => {
                component.direction(direction).feature_index.map(i32::from)
            }
            (Self::Nr(component), Direction::Dl) => Some(component.derived_dl_feature_index()),
            (Self::Nr(component), Direction::Ul) => Some(component.derived_ul_feature_index()),
        }
    }

    /// The LTE half of [`from_proto_sub_block`](Self::from_proto_sub_block): an E-UTRA
    /// component's fields carry over verbatim — no per-CC feature-set resolution, no
    /// NR-only `srs_tx_switch`.
    ///
    /// Field 8 is *rejected* rather than ignored. [`RawLteSubBlock`] has nowhere to put it, so
    /// reading six of the seven optional fields and dropping the seventh lost data silently —
    /// and because [`RawSubBlockKey`] takes `srs_tx_switch` from
    /// [`RawSubBlock::srs_tx_switch`], which is unconditionally `None` for E-UTRA, two combos
    /// differing only in field 8 collapsed to one key and the legacy branch dropped one of them
    /// outright. `wire`'s scanner cannot catch this: field 8 is modeled on `SubBlock` and is
    /// legitimate for NR, so only the band tells the two apart. The source direction was
    /// already fail-closed here (`read_sub_block`'s `finish()` rejects `srs-tx-switch` on an
    /// `lte` node); this makes proto ingest agree with it.
    fn lte_from_proto_sub_block(component: &ProtoSubBlock, band: u16) -> anyhow::Result<Self> {
        anyhow::ensure!(
            component.srstxswitch.is_none(),
            "component B{band} is an E-UTRA sub-block carrying srstxswitch (proto field 8), \
             which is NR-only and has no place in the E-UTRA model"
        );
        Ok(RawLteSubBlock {
            band,
            dl: LteDirection {
                bw_class: narrow_opt_field(component.dl_bw_class, "dl_bw_class")?,
                feature_index: narrow_opt_field(component.dl_feature_index, "dl_feature_index")?,
                selector: component.dl_feature_per_cc_ids.clone(),
            },
            ul: LteDirection {
                bw_class: narrow_opt_field(component.ul_bw_class, "ul_bw_class")?,
                feature_index: narrow_opt_field(component.ul_feature_index, "ul_feature_index")?,
                selector: component.ul_feature_per_cc_ids.clone(),
            },
        }
        .into())
    }

    /// The NR half of [`from_proto_sub_block`](Self::from_proto_sub_block): resolve both
    /// directions' per-CC selector bytes against the file's catalogs —
    /// [`resolve_or_placeholder`] keeps an unresolved selector only as the all-zero
    /// placeholder, hard-erroring on anything else — then confirm the file's stored
    /// `dl`/`ul_feature_index` agrees with what NR derives from those feature sets, since
    /// NR keeps no source index (see [`feature_index`](Self::feature_index)).
    fn nr_from_proto_sub_block(
        component: &ProtoSubBlock,
        band: u16,
        dl_list: &[ShannonFeatureSetDlPerCcNr],
        ul_list: &[ShannonFeatureSetUlPerCcNr],
    ) -> anyhow::Result<Self> {
        let dl = resolve_all(component.dl_feature_per_cc_ids.as_deref(), dl_list);
        let ul = resolve_all(component.ul_feature_per_cc_ids.as_deref(), ul_list);
        let raw = RawNrSubBlock {
            band,
            dl: NrDirection {
                bw_class: narrow_opt_field(component.dl_bw_class, "dl_bw_class")?,
                features: resolve_or_placeholder(
                    dl,
                    component.dl_feature_per_cc_ids.as_deref(),
                    Direction::Dl,
                    band,
                )?,
            },
            ul: NrDirection {
                bw_class: narrow_opt_field(component.ul_bw_class, "ul_bw_class")?,
                features: resolve_or_placeholder(
                    ul,
                    component.ul_feature_per_cc_ids.as_deref(),
                    Direction::Ul,
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

    /// Build a raw component directly from its protobuf `SubBlock` and the file's feature-set
    /// lists. This is the *only* ingest path: the report `SubBlock` DTO is a rendering type
    /// and never an input, so nothing allocates a band-label string only to parse the band
    /// back out of it.
    ///
    /// This is the strict ingest boundary for `ul_bw_class`: corpus-verified always `Some`
    /// on a real decoded sub-block (never `None`), so its absence here — which the compiler
    /// KDL source now normalizes away by omitting `Some(0)` (Task 8) — fails closed instead
    /// of silently normalizing to `0` on data that has never actually shown that shape. Kind
    /// dispatch is [`lte_from_proto_sub_block`](Self::lte_from_proto_sub_block) /
    /// [`nr_from_proto_sub_block`](Self::nr_from_proto_sub_block).
    fn from_proto_sub_block(
        component: &ProtoSubBlock,
        dl_list: &[ShannonFeatureSetDlPerCcNr],
        ul_list: &[ShannonFeatureSetUlPerCcNr],
    ) -> anyhow::Result<Self> {
        anyhow::ensure!(
            component.ul_bw_class.is_some(),
            "sub-block omits ul_bw_class (never observed; refusing to normalize to 0)"
        );
        let (kind, plain_band) = SubBlockKind::split_raw_band(component.band);
        let band = narrow_field(plain_band, "band")?;
        match kind {
            SubBlockKind::Nr => Self::nr_from_proto_sub_block(component, band, dl_list, ul_list),
            SubBlockKind::Lte => Self::lte_from_proto_sub_block(component, band),
        }
    }

    /// Invariants that the type cannot express. "LTE carries NR-only fields" is no longer
    /// among them — [`RawLteSubBlock`] simply has no feature-set or `srs_tx_switch` field.
    pub(crate) fn validate(&self) -> anyhow::Result<()> {
        anyhow::ensure!(self.band() > 0, "component band must be positive");
        anyhow::ensure!(
            SubBlockKind::split_raw_band(i32::from(self.band())).0 == SubBlockKind::Lte,
            "component band must be the plain band number, not raw protobuf encoding"
        );
        self.validate_cc_count(Direction::Dl)?;
        self.validate_cc_count(Direction::Ul)?;
        if let Self::Nr(component) = self {
            component.validate_cross_cc_feature_index_agreement()?;
        }
        Ok(())
    }

    /// Per-CC data and `bw_class` must imply each other, and when both are present the list
    /// length must equal `cc_count(kind, bw_class)`.
    ///
    /// The biconditional is the load-bearing part. Regeneration does not preserve field 6/7
    /// presence — `resolve` re-derives it from the class via `placeholder_ids` — so a component
    /// that breaks the correspondence cannot survive a round trip in either direction: per-CC
    /// data under a `0`/absent class is silently **dropped**, and a class with no per-CC data
    /// has an all-zero selector **invented** for it. Both used to validate `Ok`.
    ///
    /// This is corpus-verified rather than assumed: across 3.46M real sub-blocks the
    /// correspondence is exact — `class >= 1` always carries a selector (LTE DL 1 741 849,
    /// LTE UL 709 707, NR DL 1 715 899, NR UL 1 028 461) and `class == 0` never does
    /// (LTE UL 1 032 142, NR UL 687 438), with no absent classes and no zero-length selectors.
    /// So enforcing it rejects nothing real while making the data loss unrepresentable.
    fn validate_cc_count(&self, direction: Direction) -> anyhow::Result<()> {
        let (len, bw_class) = (self.per_cc_len(direction), self.bw_class(direction));
        let lower = direction.lowercase();
        match (len, bw_class) {
            // Direction off: the explicit "disabled" class 0 (or none at all), and no data.
            (None, None | Some(0)) => Ok(()),
            (Some(len), Some(bw_class @ 1..)) => {
                let expected = cc_count(self.kind(), bw_class)?;
                anyhow::ensure!(
                    len == expected,
                    "component {} {direction} per-CC list length {len} does not match cc_count \
                     {expected} for {lower}_bw_class {bw_class}",
                    self.band_label(),
                );
                Ok(())
            }
            (Some(len), class) => anyhow::bail!(
                "component {} carries {len} per-CC {direction} value(s) but its {lower}_bw_class \
                 is {}; regeneration derives per-CC presence from the class, so this data would \
                 be dropped",
                self.band_label(),
                Optional(class),
            ),
            (None, Some(bw_class)) => anyhow::bail!(
                "component {} has {lower}_bw_class {bw_class} but carries no per-CC {direction} \
                 data; regeneration would invent an all-zero selector for it",
                self.band_label(),
            ),
        }
    }

    /// Per-CC list length, or `None` when the direction carries no per-CC data at all.
    pub(crate) fn per_cc_len(&self, direction: Direction) -> Option<usize> {
        match (self, direction) {
            (Self::Lte(component), direction) => component
                .direction(direction)
                .selector
                .as_ref()
                .map(Vec::len),
            (Self::Nr(component), Direction::Dl) => component.dl.per_cc_len(),
            (Self::Nr(component), Direction::Ul) => component.ul.per_cc_len(),
        }
    }

    pub(crate) fn band_label(&self) -> BandLabel {
        self.kind().band_label(i32::from(self.band()))
    }
}

impl RawNrSubBlock {
    /// All CCs in an NR sub-block must derive the same single `dl_feature_index`/
    /// `ul_feature_index` — physically you cannot aggregate FR1+FR2 (or mixed MIMO
    /// presence) into one band's combo entry. LTE never derives a feature index, so this
    /// is NR-only, which the type now states directly.
    fn validate_cross_cc_feature_index_agreement(&self) -> anyhow::Result<()> {
        ensure_index_agreement(
            self.dl.resolved(),
            |row| derive_nr_dl_index(Some(row.max_scs.unwrap_or(0))),
            Direction::Dl,
            self.band_label(),
        )?;
        ensure_index_agreement(
            self.ul.resolved(),
            |row| derive_nr_ul_index(Some(row.max_mimo_cb.unwrap_or(0))),
            Direction::Ul,
            self.band_label(),
        )
    }
}

fn ensure_index_agreement<T>(
    features: &[T],
    derive: impl Fn(&T) -> i32,
    direction: Direction,
    band: BandLabel,
) -> anyhow::Result<()> {
    for pair in features.windows(2) {
        let (want, got) = (derive(&pair[0]), derive(&pair[1]));
        let suffix = match direction {
            Direction::Dl => "; cannot aggregate FR1+FR2 in one band",
            Direction::Ul => "",
        };
        anyhow::ensure!(
            got == want,
            "component {band} CCs disagree on derived {direction} feature index ({want} vs {got}){suffix}"
        );
    }
    Ok(())
}

/// Header and component fields that make up one NR combo payload. Source
/// provenance, group packing, and wire bitmask live outside this neutral value.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RawNrPayload {
    pub(crate) header: ComboHeader,
    pub(crate) sub_blocks: Vec<RawSubBlock>,
}

impl RawNrPayload {
    /// Build a raw payload directly from a protobuf combo `Combo` and its group header,
    /// using the file's feature-set lists — the folder-ingest path that avoids the report
    /// `Combo`/`SubBlock` DTO entirely — that DTO is output-only.
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
        let Some(header) = header else {
            anyhow::bail!("combo omits its header (never observed; refusing to normalize to 0)")
        };
        for (label, present) in [
            ("power_class", header.power_class.is_some()),
            ("bcs_nr", header.bcs_nr.is_some()),
            ("bcs_eutra", header.bcs_eutra.is_some()),
            (
                "intra_band_en_dc_support",
                header.intra_band_en_dc_support.is_some(),
            ),
        ] {
            anyhow::ensure!(
                present,
                "combo header omits {label} (never observed; refusing to normalize to 0)"
            );
        }
        let mut sub_blocks = combo
            .sub_blocks
            .iter()
            .map(|component| RawSubBlock::from_proto_sub_block(component, dl_list, ul_list))
            .collect::<anyhow::Result<Vec<_>>>()?;
        sub_blocks.sort_by_cached_key(|component| RawSubBlockKey::from(component));
        Ok(Self {
            header: *header,
            sub_blocks,
        })
    }

    /// Every payload in a decoded capability file, in `combo_groups` order — the same walk
    /// `compiler::nr`'s ingest does. Test-only convenience over
    /// [`from_proto_combo`](Self::from_proto_combo).
    #[cfg(test)]
    pub(crate) fn all_from_caps(caps: &crate::proto::UeCaps) -> anyhow::Result<Vec<Self>> {
        caps.combo_groups
            .iter()
            .flat_map(|group| {
                let header = group.combo_header.as_ref();
                group.combo.iter().map(move |combo| {
                    Self::from_proto_combo(
                        header,
                        combo,
                        &caps.dl_feature_per_cc_list,
                        &caps.ul_feature_per_cc_list,
                    )
                })
            })
            .collect()
    }
}

/// Full ordered form of one raw component. Resolved feature values win over
/// selector bytes; selector-only values retain exact `Option<Vec<u8>>` presence.
/// An empty `dl_features`/`ul_features` vec IS the "no feature set" identity (mirrors
/// the old `dl_feature_set_present` bool: a resolved-but-all-absent wrapper is a
/// one-element vec of all-`None` fields, distinct from a zero-element vec).
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct RawSubBlockKey {
    kind: SubBlockKind,
    band: u16,
    dl_bw_class: Option<u8>,
    ul_bw_class: Option<u8>,
    dl_feature_index: Option<i32>,
    ul_feature_index: Option<i32>,
    dl_cc_ids: Option<Vec<u8>>,
    ul_cc_ids: Option<Vec<u8>>,
    srs_tx_switch: Option<i32>,
    dl_features: Vec<ShannonFeatureSetDlPerCcNr>,
    ul_features: Vec<ShannonFeatureSetUlPerCcNr>,
}

impl From<&RawSubBlock> for RawSubBlockKey {
    fn from(cc: &RawSubBlock) -> Self {
        Self {
            kind: cc.kind(),
            band: cc.band(),
            dl_bw_class: cc.bw_class(Direction::Dl),
            ul_bw_class: cc.bw_class(Direction::Ul),
            // Materialized, not raw: identity must reflect the value that will actually land
            // in the binary (an NR component derives its index), or a key built from source
            // disagrees with the same combo's key rebuilt after decoding the materialized
            // output.
            dl_feature_index: cc.feature_index(Direction::Dl),
            ul_feature_index: cc.feature_index(Direction::Ul),
            // `PerCc` yields selector bytes only for a direction that did not resolve, so
            // the old explicit "mask the selector once the feature set is present" step is
            // now structural.
            dl_cc_ids: cc.selector(Direction::Dl).map(<[u8]>::to_vec),
            ul_cc_ids: cc.selector(Direction::Ul).map(<[u8]>::to_vec),
            srs_tx_switch: cc.srs_tx_switch(),
            dl_features: cc.dl_features().to_vec(),
            ul_features: cc.ul_features().to_vec(),
        }
    }
}

/// Ordered payload identity with component order normalized away.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RawNrPayloadKey {
    header: ComboHeader,
    sub_blocks: Vec<RawSubBlockKey>,
}

impl Ord for RawNrPayloadKey {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        // Canonical source order predates the shared header: power class leads, unlike
        // ComboHeader's wire-declaration order. Every equality field participates.
        let key = |h: ComboHeader| {
            (
                h.power_class,
                h.bcs_nr,
                h.bcs_intra_endc,
                h.bcs_eutra,
                h.intra_band_en_dc_support,
            )
        };
        key(self.header)
            .cmp(&key(other.header))
            .then_with(|| self.sub_blocks.cmp(&other.sub_blocks))
    }
}

impl PartialOrd for RawNrPayloadKey {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
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
            header: payload.header,
            sub_blocks,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_all_keeps_every_cc_or_none() {
        assert_eq!(
            resolve_all(Some(&[1, 3]), &[10, 20, 30]),
            Some(vec![10, 30])
        );
        for ids in [None, Some(&[][..]), Some(&[0][..]), Some(&[2, 99][..])] {
            assert_eq!(resolve_all(ids, &[10, 20, 30]), None);
        }
    }

    #[test]
    fn linkage_first_byte_one_based() {
        assert_eq!(feature_index(Some(&[1]), 18), Some(0));
        assert_eq!(feature_index(Some(&[17]), 18), Some(16));
        for ids in [None, Some(&[][..]), Some(&[0][..]), Some(&[19][..])] {
            assert_eq!(feature_index(ids, 18), None);
        }
    }
    use crate::proto::{
        ComboGroup, ShannonFeatureSetDlPerCcNr, ShannonFeatureSetUlPerCcNr, UeCaps,
    };

    /// An n78 component whose DL direction carries exactly the given per-CC encoding.
    fn nr_dl(features: PerCc<ShannonFeatureSetDlPerCcNr>) -> RawSubBlock {
        RawNrSubBlock {
            band: 78,
            dl: NrDirection {
                bw_class: Some(1),
                features: Some(features),
            },
            ul: NrDirection::bare(Some(1)),
            srs_tx_switch: None,
        }
        .into()
    }

    /// Selector presence and `bw_class` imply each other, so a direction that carries per-CC
    /// data while its class says the direction is off cannot round-trip: `resolve` derives
    /// presence from the class and would silently drop the data. Corpus-verified over 3.46M
    /// sub-blocks — `class >= 1` always has a selector, `class == 0` never does — so this
    /// rejects nothing real.
    #[test]
    fn ul_data_with_a_disabled_ul_class_is_rejected() {
        let component: RawSubBlock = RawLteSubBlock {
            band: 3,
            dl: LteDirection {
                bw_class: Some(1),
                feature_index: None,
                selector: Some(vec![0]),
            },
            ul: LteDirection {
                bw_class: Some(0),
                feature_index: None,
                selector: Some(vec![0, 0]),
            },
        }
        .into();

        let error = format!("{:#}", component.validate().unwrap_err());

        assert!(error.contains("ul_bw_class"), "{error}");
        assert!(error.contains("B3"), "{error}");
    }

    /// The additive half: a class with no selector would have one *invented* on regeneration
    /// (`placeholder_ids` writes an all-zero list unconditionally), so the file that comes back
    /// out differs from the one that went in.
    #[test]
    fn a_bw_class_without_its_selector_is_rejected() {
        let component: RawSubBlock = RawLteSubBlock {
            band: 3,
            dl: LteDirection {
                bw_class: Some(4),
                feature_index: None,
                selector: None,
            },
            ul: LteDirection {
                bw_class: Some(0),
                feature_index: None,
                selector: None,
            },
        }
        .into();

        let error = format!("{:#}", component.validate().unwrap_err());

        assert!(error.contains("dl_bw_class"), "{error}");
        assert!(error.contains("B3"), "{error}");
    }

    /// The shape real files actually have: presence on both sides of the biconditional.
    #[test]
    fn matching_class_and_selector_validate() {
        let component: RawSubBlock = RawLteSubBlock {
            band: 3,
            dl: LteDirection {
                bw_class: Some(4),
                feature_index: None,
                selector: Some(vec![0, 0, 0]), // cc_count(Lte, 4) == 3
            },
            ul: LteDirection {
                bw_class: Some(2),
                feature_index: None,
                selector: Some(vec![0, 0]), // cc_count(Lte, 2) == 2
            },
        }
        .into();

        component.validate().expect("the corpus-verified shape");
    }

    /// Proto field 8 was read for NR and silently dropped for E-UTRA. Worse, the identity key
    /// inherited that `None`, so two combos differing only in field 8 produced byte-identical
    /// keys and the legacy branch dropped one of them outright. The E-UTRA model has no field
    /// to hold it, so ingest must refuse rather than discard. (Field 8 appears zero times in
    /// the corpus — on NR sub-blocks too — so this is hardening, not a live-data fix.)
    #[test]
    fn field_eight_on_an_eutra_sub_block_is_rejected() {
        let component = ProtoSubBlock {
            band: 3,
            dl_bw_class: Some(1),
            ul_bw_class: Some(1),
            dl_feature_per_cc_ids: Some(vec![0]),
            ul_feature_per_cc_ids: Some(vec![0]),
            srstxswitch: Some(7),
            ..Default::default()
        };

        let error = format!(
            "{:#}",
            RawSubBlock::from_proto_sub_block(&component, &[], &[]).unwrap_err()
        );

        assert!(error.contains("srstxswitch"), "{error}");
        assert!(error.contains("B3"), "{error}");
    }

    /// The five `narrow_field`/`narrow_opt_field` call sites are fail-closed paths with no
    /// coverage of their own — the only test exercising the narrowed widths is corpus-gated, so
    /// CI never runs it. Each rejects a value one past the width's maximum, naming the field.
    #[test]
    fn narrowing_rejects_a_value_past_each_fields_width() {
        // band is u16 in the raw model; the proto carries i32.
        let error = format!(
            "{:#}",
            super::narrow_field::<u16>(i32::from(u16::MAX) + 1, "band").unwrap_err()
        );
        assert!(error.contains("band"), "{error}");
        assert!(error.contains("65536"), "{error}");

        // bw_class is u8.
        let error = format!(
            "{:#}",
            super::narrow_field::<u8>(256, "dl_bw_class").unwrap_err()
        );
        assert!(error.contains("dl_bw_class"), "{error}");

        // A negative proto value cannot narrow to an unsigned raw field either.
        assert!(super::narrow_field::<u16>(-1, "band").is_err());

        // The Option form passes absence through and narrows presence.
        assert_eq!(
            super::narrow_opt_field::<u8>(Some(13), "dl_bw_class").unwrap(),
            Some(13)
        );
        assert_eq!(
            super::narrow_opt_field::<u8>(None, "dl_bw_class").unwrap(),
            None
        );
        assert!(super::narrow_opt_field::<u8>(Some(256), "ul_bw_class").is_err());
    }

    /// Returns the NR *variant struct*, not the enum, so tests can keep using functional
    /// update (`RawNrSubBlock { dl: …, ..nr_cc(78) }`) and `.into()` at the use site.
    fn nr_cc(band: u16) -> RawNrSubBlock {
        RawNrSubBlock {
            band,
            dl: NrDirection::bare(Some(1)),
            ul: NrDirection::bare(Some(1)),
            srs_tx_switch: None,
        }
    }

    fn payload(sub_blocks: Vec<RawSubBlock>) -> RawNrPayload {
        RawNrPayload {
            header: ComboHeader {
                power_class: Some(3),
                bcs_nr: Some(1),
                ..Default::default()
            },
            sub_blocks,
        }
    }

    #[test]
    fn from_proto_combo_ingests_all_three_component_shapes() {
        // The three shapes a real combo mixes: a resolved DL+UL feature set (selector
        // superseded), an NR component whose selector is the all-zero placeholder (resolves
        // to no feature set, bytes kept verbatim), and a plain E-UTRA component.
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
                            dl_feature_index: Some(1),
                            ul_feature_index: Some(2),
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

        let payloads = RawNrPayload::all_from_caps(&caps).unwrap();
        let [payload] = &payloads[..] else {
            panic!("one combo group with one combo yields one payload, got {payloads:?}")
        };
        assert_eq!(payload.header.power_class, Some(3));
        assert_eq!(
            payload.header.bcs_intra_endc, None,
            "genuinely absent, not defaulted"
        );

        let by_band = |label: &str| {
            payload
                .sub_blocks
                .iter()
                .find(|component| component.band_label().to_string() == label)
                .unwrap_or_else(|| panic!("component {label} present"))
        };

        // n78: both directions resolve against the catalogs, so the selector is superseded.
        let n78 = by_band("n78");
        assert_eq!(n78.dl_features(), &caps.dl_feature_per_cc_list[..]);
        assert_eq!(n78.ul_features(), &caps.ul_feature_per_cc_list[..]);
        assert_eq!(n78.selector(Direction::Dl), None);
        assert_eq!(n78.selector(Direction::Ul), None);

        // n41: the all-zero placeholder resolves to nothing, so the bytes survive verbatim.
        let n41 = by_band("n41");
        assert!(n41.dl_features().is_empty());
        assert_eq!(n41.selector(Direction::Dl), Some(&[0][..]));

        // B3: an E-UTRA component references no per-CC catalog at all.
        let b3 = by_band("B3");
        assert_eq!(b3.kind(), SubBlockKind::Lte);
        assert!(b3.dl_features().is_empty());
        assert_eq!(b3.bw_class(Direction::Dl), Some(4));
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
            dl_bw_class: Some(3),
            ul_bw_class: Some(0),
            dl_feature_index: Some(1),
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
            dl_bw_class: Some(3),
            ul_bw_class: Some(0),
            dl_feature_index: Some(1),
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

    fn assert_nr_feature_index_presence(direction: Direction) {
        // A resolved record with absent fields still derives index 1, not zero.
        for (scs, mimo, index) in [
            (None, None, 1),
            (Some(1), Some(1), 1),
            (Some(4), Some(2), 2),
        ] {
            let dl = [ShannonFeatureSetDlPerCcNr {
                max_scs: scs,
                ..Default::default()
            }];
            let ul = [ShannonFeatureSetUlPerCcNr {
                max_mimo_cb: mimo,
                ..Default::default()
            }];
            let mut proto = ProtoSubBlock {
                band: NR_BAND_OFFSET + 78,
                dl_bw_class: Some(1),
                ul_bw_class: Some(1),
                dl_feature_index: Some(index),
                ul_feature_index: Some(index),
                dl_feature_per_cc_ids: Some(vec![1]),
                ul_feature_per_cc_ids: Some(vec![1]),
                ..Default::default()
            };
            let raw = RawSubBlock::from_proto_sub_block(&proto, &dl, &ul).unwrap();
            assert_eq!(raw.feature_index(Direction::Dl), Some(index));
            assert_eq!(raw.feature_index(Direction::Ul), Some(index));

            match direction {
                Direction::Dl => proto.dl_feature_index = None,
                Direction::Ul => proto.ul_feature_index = None,
            }
            let error = RawSubBlock::from_proto_sub_block(&proto, &dl, &ul)
                .expect_err("an absent index must not silently become nonzero")
                .to_string();
            for context in [
                direction.to_string(),
                "absent".into(),
                format!("derived {index}"),
            ] {
                assert!(error.contains(&context), "{error}");
            }
        }
    }

    #[test]
    fn from_proto_rejects_missing_nonzero_nr_dl_feature_index() {
        assert_nr_feature_index_presence(Direction::Dl);
    }

    #[test]
    fn from_proto_rejects_missing_nonzero_nr_ul_feature_index() {
        assert_nr_feature_index_presence(Direction::Ul);
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
            raw.selector(Direction::Dl),
            Some([0].as_slice()),
            "placeholder bytes are kept verbatim"
        );
        assert_eq!(raw.feature_index(Direction::Dl), Some(0));
        assert_eq!(raw.feature_index(Direction::Ul), Some(0));
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

        assert_eq!(raw.feature_index(Direction::Dl), Some(1));
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
        assert_eq!(raw.feature_index(Direction::Dl), Some(99));
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
    fn raw_sub_block_key_separates_resolved_features_from_selector_bytes() {
        // `PerCc` makes "resolved AND selector for one direction" unrepresentable, so the old
        // "resolution wins over bytes" tie-break is structural rather than a rule to test.
        // What still needs pinning is presence: a resolved vec is present even when its sole
        // entry has every field `None` (a legitimate all-absent catalog record), so it must
        // key differently from the same direction carrying raw selector bytes.
        let all_none = nr_dl(PerCc::Resolved(vec![ShannonFeatureSetDlPerCcNr::default()]));
        let selector = nr_dl(PerCc::Selector(vec![0]));
        assert_ne!(
            RawSubBlockKey::from(&all_none),
            RawSubBlockKey::from(&selector),
            "an all-None resolved record is present, not selector-only"
        );

        let partial = nr_dl(PerCc::Resolved(vec![ShannonFeatureSetDlPerCcNr {
            max_bw: Some(100),
            ..Default::default()
        }]));
        assert_ne!(
            RawSubBlockKey::from(&partial),
            RawSubBlockKey::from(&all_none),
            "distinct resolved records must key distinctly"
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
    fn payload_order_prioritizes_power_class_over_proto_header_order() {
        let a = RawNrPayload {
            header: ComboHeader {
                power_class: Some(0),
                bcs_nr: Some(9),
                ..Default::default()
            },
            sub_blocks: vec![],
        };
        let b = RawNrPayload {
            header: ComboHeader {
                power_class: Some(1),
                bcs_nr: Some(0),
                ..Default::default()
            },
            sub_blocks: vec![],
        };
        assert!(a.header > b.header);
        assert!(RawNrPayloadKey::from(&a) < RawNrPayloadKey::from(&b));
    }

    #[test]
    fn payload_order_includes_every_header_field_and_component_identity() {
        let base = RawNrPayload {
            header: ComboHeader::default(),
            sub_blocks: vec![],
        };
        for change in [
            |h: &mut ComboHeader| h.power_class = Some(0),
            |h: &mut ComboHeader| h.bcs_nr = Some(0),
            |h: &mut ComboHeader| h.bcs_intra_endc = Some(0),
            |h: &mut ComboHeader| h.bcs_eutra = Some(0),
            |h: &mut ComboHeader| h.intra_band_en_dc_support = Some(0),
        ] {
            let mut changed = base.clone();
            change(&mut changed.header);
            let (left, right) = (
                RawNrPayloadKey::from(&base),
                RawNrPayloadKey::from(&changed),
            );
            assert_ne!(left, right);
            assert!(left < right);
            assert_eq!(left.cmp(&right), left.partial_cmp(&right).unwrap());
        }
        let mut changed = base.clone();
        changed.sub_blocks.push(nr_cc(78).into());
        assert!(RawNrPayloadKey::from(&base) < RawNrPayloadKey::from(&changed));
        let same = RawNrPayloadKey::from(&changed);
        assert_eq!(same.cmp(&same), std::cmp::Ordering::Equal);
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
        // There is no stored NR index to prefer, so the binary-bound value IS the derivation.
        // (There is no source-bound counterpart to check: `SourceNrSubBlock` has no index
        // field at all, so the source document cannot spell one.)
        assert_eq!(nr.feature_index(Direction::Dl), Some(2));
        assert_eq!(nr.feature_index(Direction::Ul), Some(2));

        // NR with no feature set derives 0.
        let bare: RawSubBlock = RawNrSubBlock {
            band: 78,
            ..Default::default()
        }
        .into();
        assert_eq!(bare.feature_index(Direction::Dl), Some(0));

        // LTE is never derived: the stored value is used as-is, and `None` stays `None`.
        let lte: RawSubBlock = RawLteSubBlock {
            band: 66,
            dl: LteDirection {
                feature_index: Some(3),
                ..Default::default()
            },
            ul: LteDirection::default(),
        }
        .into();
        assert_eq!(lte.feature_index(Direction::Dl), Some(3));
        let lte_none: RawSubBlock = RawLteSubBlock {
            band: 66,
            ..Default::default()
        }
        .into();
        assert_eq!(lte_none.feature_index(Direction::Dl), None);
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

#[cfg(test)]
mod conversion_tests {
    use super::*;
    use crate::{
        proto::{Combo, ComboGroup, SubBlock, UeCaps},
        raw_nr::test_support::{cc, full_header, raw_payloads},
    };
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

        let payloads = raw_payloads(&caps);

        assert_eq!(payloads.len(), 2, "group packing must flatten away");
        assert_eq!(payloads[0].header.power_class, Some(0));
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
                        dl_feature_index: Some(1),
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
        assert_eq!(payload.header.bcs_nr, Some(0));
        assert_eq!(payload.header.power_class, Some(0));
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
            component.selector(Direction::Dl),
            None,
            "resolved raw values must suppress the source selector"
        );
        assert_eq!(component.selector(Direction::Ul), Some([0].as_slice()));
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

        assert_eq!(component.bw_class(Direction::Ul), Some(0));
        assert_eq!(component.selector(Direction::Ul), None);
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
                        dl_feature_index: Some(1),
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
        assert_eq!(component.selector(Direction::Dl), None);
        assert_ne!(
            RawSubBlockKey::from(component),
            RawSubBlockKey::from(&RawSubBlock::from(RawNrSubBlock {
                band: 78,
                ..Default::default()
            }))
        );
    }
}
