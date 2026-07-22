//! Protobuf-shaped NR combo payloads shared by patching and folder compilation.

use crate::{
    proto::{
        ShannonFeatureSetDlPerCcNr, ShannonFeatureSetUlPerCcNr,
        combo_group::{Combo as ProtoCombo, ComboHeader, combo::SubBlock as ProtoSubBlock},
    },
    report::combos::{
        Combo, NR_BAND_OFFSET, SubBlock, band_label_for, raw_band, render_component, resolve_all,
    },
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

/// One protobuf-shaped component in an NR combo. `band` is the plain human band
/// number (`78`, not the protobuf's internal `10078`); `kind` supplies `B`/`n`.
/// (De)serialized by hand as KDL — see `patch::format::{cc_to_node, read_cc}`; the
/// KDL reader's `NodeReader::finish()` is the strictness equivalent of the former
/// `#[serde(deny_unknown_fields)]`, and the reader simply skips a field to leave it
/// absent instead of a `skip_serializing_if` attribute.
#[derive(Clone, Default, Debug, PartialEq, Eq)]
pub(crate) struct RawSubBlock {
    pub(crate) kind: SubBlockKind,
    pub(crate) band: i32,
    pub(crate) dl_bw_class: Option<i32>,
    pub(crate) ul_bw_class: Option<i32>,
    pub(crate) dl_feature_index: Option<i32>,
    pub(crate) ul_feature_index: Option<i32>,
    /// Raw per-CC selector bytes when NOT resolved (`dl_features` empty); `None` once
    /// resolved. See [`with_resolved_feature_sets`](Self::with_resolved_feature_sets).
    pub(crate) dl_cc_ids: Option<Vec<u8>>,
    pub(crate) ul_cc_ids: Option<Vec<u8>>,
    pub(crate) srs_tx_switch: Option<i32>,
    /// One resolved feature set per CC when resolved; empty when unresolved (raw
    /// `dl_cc_ids` selector bytes apply instead) or absent entirely.
    pub(crate) dl_features: Vec<ShannonFeatureSetDlPerCcNr>,
    pub(crate) ul_features: Vec<ShannonFeatureSetUlPerCcNr>,
}

impl RawSubBlock {
    pub(crate) fn raw_band(&self) -> i32 {
        self.kind.raw_band(self.band)
    }

    /// Whether a DL feature set is present, without constructing it. Hot: `RawSubBlockKey::from` needs
    /// only this bool.
    pub(crate) fn dl_feature_set_is_present(&self) -> bool {
        !self.dl_features.is_empty()
    }

    /// The number of resolved DL CCs (0 when unresolved/absent).
    pub(crate) fn dl_cc_count(&self) -> usize {
        self.dl_features.len()
    }

    /// CC0's resolved DL feature set, or `None` when unresolved/absent. Transitional
    /// single-CC accessor kept for callers not yet updated to the full per-CC vec
    /// (`dl_features`) — see DESIGN.md / the per-CC feature model design doc.
    pub(crate) fn dl_feature_set(&self) -> Option<ShannonFeatureSetDlPerCcNr> {
        self.dl_features.first().copied()
    }

    /// Whether a UL feature set is present, without constructing it. See
    /// [`dl_feature_set_is_present`](Self::dl_feature_set_is_present).
    pub(crate) fn ul_feature_set_is_present(&self) -> bool {
        !self.ul_features.is_empty()
    }

    /// The number of resolved UL CCs (0 when unresolved/absent).
    pub(crate) fn ul_cc_count(&self) -> usize {
        self.ul_features.len()
    }

    /// CC0's resolved UL feature set. See [`dl_feature_set`](Self::dl_feature_set).
    pub(crate) fn ul_feature_set(&self) -> Option<ShannonFeatureSetUlPerCcNr> {
        self.ul_features.first().copied()
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

/// The feature-index to persist in KDL source: `None` (omit — it will be re-derived on build)
/// for an NR component whose stored value equals the derived value, else the stored value
/// (LTE, or an NR value that violates the formula and must be kept as an explicit override).
pub(crate) fn source_feature_index(
    kind: SubBlockKind,
    stored: Option<i32>,
    derived: i32,
) -> Option<i32> {
    match kind {
        SubBlockKind::Nr if stored == Some(derived) => None,
        _ => stored,
    }
}

/// Whether `bytes` is a non-placeholder selector: at least one non-zero byte. The all-zero
/// placeholder always resolves to no feature set and is valid; any other selector that
/// resolves to no feature set cannot be carried and must be rejected by the caller. Shared by
/// the decode boundary ([`resolve_or_placeholder`]) and its write-path mirror
/// ([`RawSubBlock::ensure_selector_resolved`]).
fn is_non_placeholder(bytes: &[u8]) -> bool {
    bytes.iter().any(|&b| b != 0)
}

/// Post-resolution split: resolved features clear the raw bytes; an unresolved selector may
/// survive ONLY as the all-zero placeholder (re-derivable from bw_class on the source round
/// trip). A non-zero unresolvable selector can no longer be carried — fail loudly.
fn resolve_or_placeholder<T>(
    resolved: Option<Vec<T>>,
    raw: Option<&[u8]>,
    kind: SubBlockKind,
    direction: &str,
    band: i32,
) -> anyhow::Result<(Vec<T>, Option<Vec<u8>>)> {
    match resolved {
        Some(features) => Ok((features, None)),
        None => {
            if let Some(bytes) = raw {
                anyhow::ensure!(
                    !is_non_placeholder(bytes),
                    "component {} {direction} selector {bytes:?} resolves to no feature and is not the all-zero placeholder",
                    band_label_for(matches!(kind, SubBlockKind::Nr), band),
                );
            }
            Ok((Vec::new(), raw.map(<[u8]>::to_vec)))
        }
    }
}

impl RawSubBlock {
    /// For NR the source format derives the feature index rather than storing it, so a decoded
    /// index that disagrees with the derivation cannot round-trip — reject it here.
    fn ensure_nr_feature_index_derivable(&self) -> anyhow::Result<()> {
        if self.kind != SubBlockKind::Nr {
            return Ok(());
        }
        if let Some(stored) = self.dl_feature_index {
            let derived = self.derived_dl_feature_index();
            anyhow::ensure!(
                stored == derived,
                "NR component {} stored DL feature index {stored} != derived {derived}",
                self.band_label()
            );
        }
        if let Some(stored) = self.ul_feature_index {
            let derived = self.derived_ul_feature_index();
            anyhow::ensure!(
                stored == derived,
                "NR component {} stored UL feature index {stored} != derived {derived}",
                self.band_label()
            );
        }
        Ok(())
    }

    /// Write-path mirror of the decode boundary's [`resolve_or_placeholder`]: an NR direction
    /// whose raw selector carries a non-placeholder byte (per [`is_non_placeholder`]) but
    /// resolved to no feature set. `from_proto_sub_block` (decode) already rejects this state
    /// via `resolve_or_placeholder`; a `RawSubBlock` built by the lenient report-DTO path
    /// (`from_sub_block`, e.g. `patch create`'s `build_combos` ingest) does not construct it
    /// through that guard, so [`try_from_sub_block`](Self::try_from_sub_block) — `patch
    /// create`'s pre-diff gate — calls this explicitly. Without it, `create` used to silently
    /// drop the component instead of erroring: `patch::format::sub_block_to_node` emits no
    /// `dl-cc`/`ul-cc` children for an unresolved direction, and nothing else in the `create`
    /// write path notices. Deliberately NOT folded into [`validate`](Self::validate): that
    /// method is also reached by `patch apply`'s reconstruction (`reconstruct_sub_block`),
    /// which legitimately preserves a selector-only `PatchSubBlock` byte-for-byte when one is
    /// handed to it directly (see `reconstruct_sub_block_without_feature_set_preserves_raw_selector_ids`
    /// in `src/patch/build.rs`) — that capability is out of scope for this guard.
    fn ensure_selector_resolved(&self) -> anyhow::Result<()> {
        if self.kind != SubBlockKind::Nr {
            return Ok(());
        }
        let unresolved = |present: bool, ids: &Option<Vec<u8>>| -> bool {
            !present && ids.as_deref().is_some_and(is_non_placeholder)
        };
        anyhow::ensure!(
            !unresolved(self.dl_feature_set_is_present(), &self.dl_cc_ids),
            "NR component {} DL selector {:?} resolves to no feature and is not the all-zero placeholder",
            self.band_label(),
            self.dl_cc_ids
        );
        anyhow::ensure!(
            !unresolved(self.ul_feature_set_is_present(), &self.ul_cc_ids),
            "NR component {} UL selector {:?} resolves to no feature and is not the all-zero placeholder",
            self.band_label(),
            self.ul_cc_ids
        );
        Ok(())
    }

    /// Derived DL feature index for this component's resolved DL feature set (0 if none).
    pub(crate) fn derived_dl_feature_index(&self) -> i32 {
        derive_nr_dl_index(self.dl_feature_set().map(|fs| fs.max_scs.unwrap_or(0)))
    }

    /// Derived UL feature index for this component's resolved UL feature set (0 if none).
    pub(crate) fn derived_ul_feature_index(&self) -> i32 {
        derive_nr_ul_index(self.ul_feature_set().map(|fs| fs.max_mimo_cb.unwrap_or(0)))
    }

    /// The `dl_feature_index` to write into the binary: derived for an NR component that
    /// omitted it in source (`None`), else the stored value (LTE, or an explicit NR override).
    pub(crate) fn materialized_dl_feature_index(&self) -> Option<i32> {
        match (self.kind, self.dl_feature_index) {
            (SubBlockKind::Nr, None) => Some(self.derived_dl_feature_index()),
            (_, stored) => stored,
        }
    }

    /// The `ul_feature_index` to write into the binary; see [`materialized_dl_feature_index`].
    pub(crate) fn materialized_ul_feature_index(&self) -> Option<i32> {
        match (self.kind, self.ul_feature_index) {
            (SubBlockKind::Nr, None) => Some(self.derived_ul_feature_index()),
            (_, stored) => stored,
        }
    }

    /// The `dl_feature_index` to persist in source (see [`source_feature_index`]).
    pub(crate) fn source_dl_feature_index(&self) -> Option<i32> {
        source_feature_index(
            self.kind,
            self.dl_feature_index,
            self.derived_dl_feature_index(),
        )
    }

    /// The `ul_feature_index` to persist in source.
    pub(crate) fn source_ul_feature_index(&self) -> Option<i32> {
        source_feature_index(
            self.kind,
            self.ul_feature_index,
            self.derived_ul_feature_index(),
        )
    }

    /// Store the resolved per-CC feature vecs, clearing the corresponding raw selector
    /// bytes when a vec is non-empty (resolved wins over the selector).
    pub(crate) fn with_resolved_feature_sets(
        mut self,
        dl: Vec<ShannonFeatureSetDlPerCcNr>,
        ul: Vec<ShannonFeatureSetUlPerCcNr>,
    ) -> Self {
        if !dl.is_empty() {
            self.dl_cc_ids = None;
        }
        self.dl_features = dl;
        if !ul.is_empty() {
            self.ul_cc_ids = None;
        }
        self.ul_features = ul;
        self
    }

    /// Build a raw component from the report `SubBlock` DTO. Carries every per-CC
    /// feature-set entry the DTO holds — an empty `dl_features`/`ul_features` vec IS the
    /// "no feature set" identity, a non-empty vec (even one whose sole entry has every
    /// field `None`, a legitimate all-absent catalog record) IS present. This is the
    /// same presence rule [`dl_feature_set_is_present`](Self::dl_feature_set_is_present)
    /// and [`with_resolved_feature_sets`](Self::with_resolved_feature_sets) already use;
    /// prior to Task 7 this conversion additionally truncated to CC0 and gated presence on
    /// "does CC0 have any field set", both now removed — the patch format is per-CC end to
    /// end, so there is no longer a lossy report/patch axis here.
    pub(crate) fn from_sub_block(cc: &SubBlock) -> Self {
        let raw = raw_band(&cc.band).expect("report component band is canonical");
        let is_nr = raw >= NR_BAND_OFFSET;
        let kind = if is_nr {
            SubBlockKind::Nr
        } else {
            SubBlockKind::Lte
        };
        let band = if is_nr { raw - NR_BAND_OFFSET } else { raw };
        Self {
            kind,
            band,
            dl_bw_class: cc.dl_bw_class,
            ul_bw_class: cc.ul_bw_class,
            dl_feature_index: cc.dl_feature_index,
            ul_feature_index: cc.ul_feature_index,
            dl_cc_ids: cc.dl_feature_per_cc_ids.clone(),
            ul_cc_ids: cc.ul_feature_per_cc_ids.clone(),
            srs_tx_switch: cc.srs_tx_switch,
            dl_features: cc.dl_features.clone(),
            ul_features: cc.ul_features.clone(),
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
        let dl = resolve_all(component.dl_feature_per_cc_ids.as_deref(), dl_list);
        let ul = resolve_all(component.ul_feature_per_cc_ids.as_deref(), ul_list);
        let (dl_features, dl_cc_ids) = resolve_or_placeholder(
            dl,
            component.dl_feature_per_cc_ids.as_deref(),
            kind,
            "DL",
            band,
        )?;
        let (ul_features, ul_cc_ids) = resolve_or_placeholder(
            ul,
            component.ul_feature_per_cc_ids.as_deref(),
            kind,
            "UL",
            band,
        )?;
        let raw = Self {
            kind,
            band,
            dl_bw_class: component.dl_bw_class,
            ul_bw_class: component.ul_bw_class,
            dl_feature_index: component.dl_feature_index,
            ul_feature_index: component.ul_feature_index,
            dl_cc_ids,
            ul_cc_ids,
            srs_tx_switch: component.srstxswitch,
            dl_features,
            ul_features,
        };
        raw.ensure_nr_feature_index_derivable()?;
        Ok(raw)
    }

    /// Fallible sibling of [`from_sub_block`](Self::from_sub_block) for the `patch create` load path:
    /// rejects a band label `raw_band` cannot invert (`from_sub_block` would panic on it) and a
    /// band outside the plain `1..NR_BAND_OFFSET` range (which would yield a patch the
    /// parser rejects), returning the validated component. See DESIGN.md "Invariants".
    pub(crate) fn try_from_sub_block(cc: &SubBlock) -> anyhow::Result<Self> {
        anyhow::ensure!(
            raw_band(&cc.band).is_some(),
            "component band {:?} is not a valid band label (expected n<1..9999> or B<1..9999>)",
            cc.band
        );
        let raw = Self::from_sub_block(cc);
        raw.validate()?;
        // Fail closed on a corpus-impossible selector-only-unresolved NR component: `create`'s
        // report-DTO ingest (`from_sub_block`, unlike the proto decode boundary's
        // `from_proto_sub_block`) builds this leniently, with no other check catching it before
        // the patch emitter silently drops the component. See `ensure_selector_resolved`.
        raw.ensure_selector_resolved()?;
        Ok(raw)
    }

    pub(crate) fn validate(&self) -> anyhow::Result<()> {
        anyhow::ensure!(self.band > 0, "component band must be positive");
        anyhow::ensure!(
            self.band < NR_BAND_OFFSET,
            "component band must be the plain band number, not raw protobuf encoding"
        );
        if self.kind == SubBlockKind::Lte && self.has_nr_only_fields() {
            anyhow::bail!("LTE component {} carries NR-only fields", self.band_label());
        }
        self.validate_dl_cc_count()?;
        self.validate_ul_cc_count()?;
        self.validate_cross_cc_feature_index_agreement()?;
        Ok(())
    }

    /// The stored DL per-CC list length (resolved `dl_features`, else the raw fallback
    /// `dl_cc_ids`) must equal `cc_count(kind, dl_bw_class)`. DL has no "disabled" class
    /// (unlike UL's `ul_bw_class == 0`), so any per-CC DL data requires a class to check
    /// against.
    fn validate_dl_cc_count(&self) -> anyhow::Result<()> {
        let has_data = !self.dl_features.is_empty() || self.dl_cc_ids.is_some();
        if !has_data {
            return Ok(());
        }
        let dl_bw_class = self.dl_bw_class.ok_or_else(|| {
            anyhow::anyhow!(
                "component {} carries per-CC DL data without a dl_bw_class",
                self.band_label()
            )
        })?;
        let expected = cc_count(self.kind, dl_bw_class)?;
        let len = if !self.dl_features.is_empty() {
            self.dl_cc_count()
        } else {
            self.dl_cc_ids.as_ref().map_or(0, Vec::len)
        };
        anyhow::ensure!(
            len == expected,
            "component {} DL per-CC list length {len} does not match cc_count {expected} for dl_bw_class {dl_bw_class}",
            self.band_label()
        );
        Ok(())
    }

    /// UL counterpart of [`validate_dl_cc_count`](Self::validate_dl_cc_count), gated on
    /// `ul_bw_class >= 1` — `ul_bw_class == 0` means UL is disabled (no UL data expected),
    /// and `cc_count` must never be called with `0`.
    fn validate_ul_cc_count(&self) -> anyhow::Result<()> {
        let has_data = !self.ul_features.is_empty() || self.ul_cc_ids.is_some();
        if !has_data {
            return Ok(());
        }
        let ul_bw_class = self.ul_bw_class.ok_or_else(|| {
            anyhow::anyhow!(
                "component {} carries per-CC UL data without a ul_bw_class",
                self.band_label()
            )
        })?;
        if ul_bw_class == 0 {
            return Ok(());
        }
        let expected = cc_count(self.kind, ul_bw_class)?;
        let len = if !self.ul_features.is_empty() {
            self.ul_cc_count()
        } else {
            self.ul_cc_ids.as_ref().map_or(0, Vec::len)
        };
        anyhow::ensure!(
            len == expected,
            "component {} UL per-CC list length {len} does not match cc_count {expected} for ul_bw_class {ul_bw_class}",
            self.band_label()
        );
        Ok(())
    }

    /// All CCs in an NR sub-block must derive the same single `dl_feature_index`/
    /// `ul_feature_index` — physically you cannot aggregate FR1+FR2 (or mixed MIMO
    /// presence) into one band's combo entry. LTE never derives a feature index, so this
    /// is NR-only.
    fn validate_cross_cc_feature_index_agreement(&self) -> anyhow::Result<()> {
        if self.kind != SubBlockKind::Nr {
            return Ok(());
        }
        if let Some(first) = self.dl_features.first() {
            let want = derive_nr_dl_index(Some(first.max_scs.unwrap_or(0)));
            for feature in &self.dl_features[1..] {
                let got = derive_nr_dl_index(Some(feature.max_scs.unwrap_or(0)));
                anyhow::ensure!(
                    got == want,
                    "component {} CCs disagree on derived DL feature index ({want} vs {got}); cannot aggregate FR1+FR2 in one band",
                    self.band_label()
                );
            }
        }
        if let Some(first) = self.ul_features.first() {
            let want = derive_nr_ul_index(Some(first.max_mimo_cb.unwrap_or(0)));
            for feature in &self.ul_features[1..] {
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

    fn has_nr_only_fields(&self) -> bool {
        // Feature-set indexes (and their raw selector bytes) are references used by both
        // LTE and NR components. A resolved per-CC feature set is NR-specific patch data.
        self.srs_tx_switch.is_some() || !self.dl_features.is_empty() || !self.ul_features.is_empty()
    }

    pub(crate) fn to_sub_block(&self) -> anyhow::Result<SubBlock> {
        self.validate()?;
        // Shared 11-field display projection with the folder compiler's combo builder (C-proj).
        // The kind is asserted, not inferred: `self.kind` already classifies the component, so
        // we pass it through explicitly and `band` stays the plain band number it always is.
        // The index is materialized, not raw, so this projection matches the value a decoded
        // binary actually carries (an NR component that omitted it derives one) — patch
        // self-verify builds its "want" side through this path and compares it against exactly
        // that decoded, materialized "got" side.
        Ok(SubBlock::from_raw_fields(
            matches!(self.kind, SubBlockKind::Nr),
            self.band,
            self.dl_bw_class,
            self.ul_bw_class,
            self.materialized_dl_feature_index(),
            self.materialized_ul_feature_index(),
            self.dl_cc_ids.clone(),
            self.ul_cc_ids.clone(),
            self.srs_tx_switch,
            self.dl_features.clone(),
            self.ul_features.clone(),
        ))
    }

    pub(crate) fn band_label(&self) -> String {
        band_label_for(matches!(self.kind, SubBlockKind::Nr), self.band)
    }

    pub(crate) fn component_label(&self) -> String {
        render_component(
            self.kind.raw_band(self.band),
            self.dl_bw_class,
            self.ul_bw_class,
        )
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
            .map(|component| {
                let mut component = RawSubBlock::from_sub_block(component);
                if component.dl_feature_set_is_present() {
                    component.dl_cc_ids = None;
                }
                if component.ul_feature_set_is_present() {
                    component.ul_cc_ids = None;
                }
                component
            })
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
    /// The combo header (`ComboHeader`) for this payload. The single source shared by
    /// compiler NR generation and patch reconstruction — the compiler↔patch axis (C-hdr).
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
        // Presence is computed once per direction. A per-cc-id list is part of the
        // identity only when its feature set is absent (empty vec).
        let dl_present = cc.dl_feature_set_is_present();
        let ul_present = cc.ul_feature_set_is_present();
        Self {
            kind: cc.kind,
            band: cc.band,
            dl_bw_class: cc.dl_bw_class,
            ul_bw_class: cc.ul_bw_class,
            // Materialized, not raw: identity must reflect the value that will actually land
            // in the binary (an NR component that omits the index derives one), or a key built
            // from source disagrees with the same combo's key rebuilt after decoding the
            // materialized output. Equivalent to the raw field whenever it is `Some(_)`.
            dl_feature_index: cc.materialized_dl_feature_index(),
            ul_feature_index: cc.materialized_ul_feature_index(),
            dl_cc_ids: (!dl_present).then(|| cc.dl_cc_ids.clone()).flatten(),
            ul_cc_ids: (!ul_present).then(|| cc.ul_cc_ids.clone()).flatten(),
            srs_tx_switch: cc.srs_tx_switch,
            dl_features: cc.dl_features.iter().map(dl_feature_key).collect(),
            ul_features: cc.ul_features.iter().map(ul_feature_key).collect(),
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

/// Shared top-level feature-set lists used while rebuilding raw components.
#[derive(Default)]
pub(crate) struct FeatureLists {
    pub(crate) dl: Vec<ShannonFeatureSetDlPerCcNr>,
    pub(crate) ul: Vec<ShannonFeatureSetUlPerCcNr>,
}

/// Find an equal entry or append `item`; return its 0-based index.
fn find_or_append<T: PartialEq>(list: &mut Vec<T>, item: T) -> usize {
    if let Some(i) = list.iter().position(|x| *x == item) {
        i
    } else {
        list.push(item);
        list.len() - 1
    }
}

/// One selector byte per `features` entry (find-or-append dedup into `list`, 1-based), or
/// the raw fallback bytes verbatim when `features` is empty. Patch-build counterpart of the
/// compiler's `compiler::features::LocalFeaturePlan::reconstruct_sub_block`: that one
/// resolves against a pre-scanned local catalog (`binary_search`), this one grows `lists`
/// on the fly (`find_or_append`) since the patch-apply path has no such pre-pass.
fn per_cc_ids<T: PartialEq + Copy>(
    list: &mut Vec<T>,
    features: &[T],
    raw: &Option<Vec<u8>>,
) -> anyhow::Result<Option<Vec<u8>>> {
    if features.is_empty() {
        return Ok(raw.clone());
    }
    let mut ids = Vec::with_capacity(features.len());
    for &feature in features {
        let idx = find_or_append(list, feature) + 1;
        if idx > u8::MAX as usize {
            anyhow::bail!("feature-set list exceeds 255 entries");
        }
        ids.push(idx as u8);
    }
    Ok(Some(ids))
}

/// Rebuild one protobuf component. Every resolved per-CC feature set is deduplicated into
/// `lists` and wins over raw selector bytes. Any failure restores both lists.
pub(crate) fn reconstruct_sub_block(
    cc: &RawSubBlock,
    lists: &mut FeatureLists,
) -> anyhow::Result<ProtoSubBlock> {
    let dl_mark = lists.dl.len();
    let ul_mark = lists.ul.len();
    let result = (|| {
        cc.validate()?;
        let dl_ids = per_cc_ids(&mut lists.dl, &cc.dl_features, &cc.dl_cc_ids)?;
        let ul_ids = per_cc_ids(&mut lists.ul, &cc.ul_features, &cc.ul_cc_ids)?;
        Ok(ProtoSubBlock {
            band: cc.raw_band(),
            dl_bw_class: cc.dl_bw_class,
            ul_bw_class: cc.ul_bw_class,
            dl_feature_index: cc.materialized_dl_feature_index(),
            ul_feature_index: cc.materialized_ul_feature_index(),
            dl_feature_per_cc_ids: dl_ids,
            ul_feature_per_cc_ids: ul_ids,
            srstxswitch: cc.srs_tx_switch,
        })
    })();
    if result.is_err() {
        lists.dl.truncate(dl_mark);
        lists.ul.truncate(ul_mark);
    }
    result
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

    fn nr_cc(band: i32) -> RawSubBlock {
        RawSubBlock {
            kind: SubBlockKind::Nr,
            band,
            dl_bw_class: Some(1),
            ul_bw_class: Some(1),
            ..Default::default()
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

        assert_eq!(cc.dl_features.len(), 2, "both CC feature sets must survive");
        assert_eq!(cc.dl_features[0].max_scs, Some(1));
        assert_eq!(cc.dl_features[1].max_scs, Some(2));
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
        let none = nr_cc(78);
        let empty = RawSubBlock {
            dl_cc_ids: Some(vec![]),
            ..nr_cc(78)
        };
        let multibyte = RawSubBlock {
            dl_cc_ids: Some(vec![0, 2]),
            ..nr_cc(78)
        };

        assert!(RawSubBlockKey::from(&none) < RawSubBlockKey::from(&empty));
        assert!(RawSubBlockKey::from(&empty) < RawSubBlockKey::from(&multibyte));
    }

    #[test]
    fn explicit_zero_raw_value_is_resolved_and_remains_in_the_key() {
        let zero_empty = RawSubBlock {
            dl_cc_ids: Some(vec![]),
            dl_features: vec![ShannonFeatureSetDlPerCcNr {
                max_scs: Some(0),
                ..Default::default()
            }],
            ..nr_cc(78)
        };
        let zero_multibyte = RawSubBlock {
            dl_cc_ids: Some(vec![0, 2]),
            dl_features: vec![ShannonFeatureSetDlPerCcNr {
                max_scs: Some(0),
                ..Default::default()
            }],
            ..nr_cc(78)
        };
        let absent = RawSubBlock {
            dl_cc_ids: Some(vec![]),
            ..nr_cc(78)
        };

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
    fn lte_component_rejects_nr_only_fields() {
        let cc = RawSubBlock {
            kind: SubBlockKind::Lte,
            band: 66,
            dl_features: vec![ShannonFeatureSetDlPerCcNr {
                max_bw: Some(0),
                ..Default::default()
            }],
            ..Default::default()
        };

        let error = cc.validate().unwrap_err().to_string();
        assert!(error.contains("LTE component B66 carries NR-only fields"));
    }

    #[test]
    fn nr_component_validation_errors_use_the_n_band_label() {
        // `validate()` used to hardcode `B{}`, misprinting every NR band (n78 as B78).
        // Route through `band_label()` so the label always matches the component's kind.
        // DL branch: cc_count(Nr, 1) == 1, so two per-CC features is a length mismatch.
        let dl_mismatch = RawSubBlock {
            kind: SubBlockKind::Nr,
            band: 78,
            dl_bw_class: Some(1),
            ul_bw_class: Some(0),
            dl_features: vec![
                ShannonFeatureSetDlPerCcNr {
                    max_scs: Some(1),
                    ..Default::default()
                },
                ShannonFeatureSetDlPerCcNr {
                    max_scs: Some(1),
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        let error = dl_mismatch.validate().unwrap_err().to_string();
        assert!(error.contains("component n78"), "{error}");
        assert!(
            !error.contains("B78"),
            "must not misprint an NR band as B78: {error}"
        );

        // Cross-CC agreement branch (NR-only function — the label there was always wrong):
        // scs 1 derives DL index 1, scs 4 derives 2, so two CCs disagree.
        let disagreeing = RawSubBlock {
            kind: SubBlockKind::Nr,
            band: 41,
            dl_bw_class: Some(2), // cc_count(Nr, 2) == 2, so the length check passes first
            ul_bw_class: Some(0),
            dl_features: vec![
                ShannonFeatureSetDlPerCcNr {
                    max_scs: Some(1),
                    ..Default::default()
                },
                ShannonFeatureSetDlPerCcNr {
                    max_scs: Some(4),
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        let error = disagreeing.validate().unwrap_err().to_string();
        assert!(error.contains("component n41"), "{error}");
        assert!(
            !error.contains("B41"),
            "must not misprint an NR band as B41: {error}"
        );
    }

    #[test]
    fn payload_key_is_independent_of_component_order() {
        let first = payload(vec![nr_cc(78), nr_cc(41)]);
        let second = payload(vec![nr_cc(41), nr_cc(78)]);

        assert_eq!(
            RawNrPayloadKey::from(&first),
            RawNrPayloadKey::from(&second)
        );
    }

    #[test]
    fn reconstruct_sub_block_uses_resolved_values_and_preserves_selector_only_bytes() {
        let cc = RawSubBlock {
            dl_cc_ids: Some(vec![9]),
            ul_cc_ids: Some(vec![0, 2]),
            // 2 raw UL selector bytes need a UL class with cc_count 2.
            ul_bw_class: Some(2),
            dl_features: vec![ShannonFeatureSetDlPerCcNr {
                max_scs: Some(0),
                ..Default::default()
            }],
            ..nr_cc(78)
        };
        let mut lists = FeatureLists::default();

        let reconstructed = reconstruct_sub_block(&cc, &mut lists).unwrap();

        assert_eq!(reconstructed.dl_feature_per_cc_ids, Some(vec![1]));
        assert_eq!(reconstructed.ul_feature_per_cc_ids, Some(vec![0, 2]));
        assert_eq!(lists.dl.len(), 1);
        assert_eq!(lists.dl[0].max_scs, Some(0));
        assert!(lists.ul.is_empty());
    }

    #[test]
    fn reconstruct_sub_block_rolls_back_both_feature_lists_on_error() {
        let mut lists = FeatureLists {
            dl: Vec::new(),
            ul: (1..=255)
                .map(|max_scs| ShannonFeatureSetUlPerCcNr {
                    max_scs: Some(max_scs),
                    ..Default::default()
                })
                .collect(),
        };
        let before_dl = lists.dl.clone();
        let before_ul = lists.ul.clone();
        let cc = RawSubBlock {
            dl_features: vec![ShannonFeatureSetDlPerCcNr {
                max_scs: Some(1),
                ..Default::default()
            }],
            ul_features: vec![ShannonFeatureSetUlPerCcNr {
                max_scs: Some(999),
                ..Default::default()
            }],
            ..nr_cc(78)
        };

        let error = reconstruct_sub_block(&cc, &mut lists)
            .unwrap_err()
            .to_string();

        assert!(error.contains("feature-set list exceeds 255 entries"));
        assert_eq!(lists.dl, before_dl);
        assert_eq!(lists.ul, before_ul);
    }

    #[test]
    fn reconstruct_sub_block_uses_every_single_byte_one_based_selector_through_255() {
        let mut lists = FeatureLists::default();
        for value in 1..=255 {
            let component = RawSubBlock {
                dl_features: vec![ShannonFeatureSetDlPerCcNr {
                    max_bw: Some(value),
                    ..Default::default()
                }],
                ..nr_cc(78)
            };
            let reconstructed = reconstruct_sub_block(&component, &mut lists).unwrap();
            assert_eq!(reconstructed.dl_feature_per_cc_ids, Some(vec![value as u8]));
        }
        assert_eq!(lists.dl.len(), 255);

        let duplicate = RawSubBlock {
            dl_features: vec![ShannonFeatureSetDlPerCcNr {
                max_bw: Some(255),
                ..Default::default()
            }],
            ..nr_cc(78)
        };
        let reconstructed = reconstruct_sub_block(&duplicate, &mut lists).unwrap();
        assert_eq!(reconstructed.dl_feature_per_cc_ids, Some(vec![255]));
        assert_eq!(lists.dl.len(), 255, "an equal entry must deduplicate");
    }

    #[test]
    fn reconstruct_sub_block_rejects_and_rolls_back_the_256th_feature_entry() {
        let mut lists = FeatureLists {
            dl: (1..=255)
                .map(|max_bw| ShannonFeatureSetDlPerCcNr {
                    max_bw: Some(max_bw),
                    ..Default::default()
                })
                .collect(),
            ul: vec![ShannonFeatureSetUlPerCcNr {
                max_bw: Some(1),
                ..Default::default()
            }],
        };
        let before_dl = lists.dl.clone();
        let before_ul = lists.ul.clone();
        let component = RawSubBlock {
            dl_features: vec![ShannonFeatureSetDlPerCcNr {
                max_bw: Some(256),
                ..Default::default()
            }],
            ul_features: vec![ShannonFeatureSetUlPerCcNr {
                max_bw: Some(2),
                ..Default::default()
            }],
            ..nr_cc(78)
        };

        let error = reconstruct_sub_block(&component, &mut lists)
            .unwrap_err()
            .to_string();

        assert!(error.contains("feature-set list exceeds 255 entries"));
        assert_eq!(lists.dl, before_dl);
        assert_eq!(lists.ul, before_ul);
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
    fn raw_cc_derived_and_materialized_indexes() {
        let nr = RawSubBlock {
            kind: SubBlockKind::Nr,
            band: 78,
            dl_features: vec![ShannonFeatureSetDlPerCcNr {
                max_scs: Some(4),
                ..Default::default()
            }],
            ul_features: vec![ShannonFeatureSetUlPerCcNr {
                max_mimo_cb: Some(2),
                ..Default::default()
            }],
            ..Default::default()
        };
        assert_eq!(nr.derived_dl_feature_index(), 2);
        assert_eq!(nr.derived_ul_feature_index(), 2);
        // No explicit override -> materialize derives.
        assert_eq!(nr.materialized_dl_feature_index(), Some(2));
        assert_eq!(nr.materialized_ul_feature_index(), Some(2));

        // NR with no feature set derives 0.
        let bare = RawSubBlock {
            kind: SubBlockKind::Nr,
            band: 78,
            ..Default::default()
        };
        assert_eq!(bare.materialized_dl_feature_index(), Some(0));

        // Explicit NR override is preserved.
        let override_nr = RawSubBlock {
            kind: SubBlockKind::Nr,
            band: 78,
            dl_feature_index: Some(1),
            dl_features: vec![ShannonFeatureSetDlPerCcNr {
                max_scs: Some(4),
                ..Default::default()
            }],
            ..Default::default()
        };
        assert_eq!(override_nr.materialized_dl_feature_index(), Some(1));

        // LTE is never derived: None stays None, Some stays Some.
        let lte = RawSubBlock {
            kind: SubBlockKind::Lte,
            band: 66,
            dl_feature_index: Some(3),
            ..Default::default()
        };
        assert_eq!(lte.materialized_dl_feature_index(), Some(3));
        let lte_none = RawSubBlock {
            kind: SubBlockKind::Lte,
            band: 66,
            ..Default::default()
        };
        assert_eq!(lte_none.materialized_dl_feature_index(), None);
    }

    #[test]
    fn source_feature_index_omits_only_matching_nr() {
        // NR, stored == derived -> omit.
        assert_eq!(source_feature_index(SubBlockKind::Nr, Some(2), 2), None);
        // NR, stored != derived (formula violation) -> keep as explicit override.
        assert_eq!(source_feature_index(SubBlockKind::Nr, Some(1), 2), Some(1));
        // NR already absent -> stays None.
        assert_eq!(source_feature_index(SubBlockKind::Nr, None, 2), None);
        // LTE is never omitted.
        assert_eq!(source_feature_index(SubBlockKind::Lte, Some(2), 2), Some(2));
    }

    #[test]
    fn raw_cc_source_indexes_round_trip_with_materialized() {
        let cc = RawSubBlock {
            kind: SubBlockKind::Nr,
            band: 78,
            dl_feature_index: Some(2), // matches derived (FR2)
            dl_features: vec![ShannonFeatureSetDlPerCcNr {
                max_scs: Some(4),
                ..Default::default()
            }],
            ul_feature_index: Some(0), // matches derived (no UL set)
            ..Default::default()
        };
        // Emit omits, materialize brings it back identically.
        assert_eq!(cc.source_dl_feature_index(), None);
        assert_eq!(cc.source_ul_feature_index(), None);
        assert_eq!(cc.materialized_dl_feature_index(), Some(2));
        assert_eq!(cc.materialized_ul_feature_index(), Some(0));
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
        let sb = RawSubBlock {
            kind: SubBlockKind::Nr,
            band: 48,
            dl_bw_class: Some(2), // expects 2 CCs
            dl_features: vec![ShannonFeatureSetDlPerCcNr::default()], // only 1
            ..Default::default()
        };
        assert!(sb.validate().unwrap_err().to_string().contains("cc_count"));
    }

    #[test]
    fn validate_rejects_disagreeing_cross_cc_feature_index() {
        // CC0 FR1 (scs<4), CC1 FR2 (scs>=4) -> ambiguous single dl_feature_index.
        let sb = RawSubBlock {
            kind: SubBlockKind::Nr,
            band: 78,
            dl_bw_class: Some(2),
            dl_features: vec![
                ShannonFeatureSetDlPerCcNr {
                    max_scs: Some(1),
                    ..Default::default()
                },
                ShannonFeatureSetDlPerCcNr {
                    max_scs: Some(4),
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
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

    // Final review, Fix 1: `try_from_sub_block` is `patch create`'s pre-diff gate
    // (`patch::validate_nr_combo_bands`) — it must reject a corpus-impossible NR component
    // whose per-CC selector is present, non-placeholder, and unresolved, mirroring the decode
    // boundary's `resolve_or_placeholder`. These are the direct, fast-running counterpart of
    // `patch::tests::create_nr_rejects_selector_only_unresolved_component`, which drives the
    // same guard through the actual `patch create` entry point end to end.

    #[test]
    fn try_from_sub_block_rejects_selector_only_unresolved_dl() {
        let cc = report_cc(Some(vec![5]), None); // non-zero, no resolved DL feature set
        let err = RawSubBlock::try_from_sub_block(&cc)
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("DL") && err.contains("selector") && err.contains("placeholder"),
            "{err}"
        );
    }

    #[test]
    fn try_from_sub_block_rejects_selector_only_unresolved_ul() {
        let mut cc = report_cc(None, None);
        cc.ul_feature_per_cc_ids = Some(vec![3]); // non-zero, no resolved UL feature set
        let err = RawSubBlock::try_from_sub_block(&cc)
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("UL") && err.contains("selector") && err.contains("placeholder"),
            "{err}"
        );
    }

    #[test]
    fn try_from_sub_block_accepts_all_zero_placeholder_selector() {
        // The all-zero placeholder is re-derivable from bw_class/cc_count and is valid —
        // must NOT be rejected by the new guard (do not over-guard).
        let cc = report_cc(Some(vec![0]), None);
        assert!(RawSubBlock::try_from_sub_block(&cc).is_ok());
    }

    #[test]
    fn try_from_sub_block_accepts_normally_resolved_component() {
        // A component with an actual resolved feature set must NOT be rejected (do not
        // over-guard) — this is the ordinary, corpus-common shape.
        let cc = report_cc(
            Some(vec![1]),
            Some(ShannonFeatureSetDlPerCcNr {
                max_scs: Some(1),
                ..Default::default()
            }),
        );
        assert!(RawSubBlock::try_from_sub_block(&cc).is_ok());
    }

    #[test]
    fn try_from_sub_block_does_not_guard_lte_selector_only_bytes() {
        // The new guard is NR-only (kind-gated): an LTE component's raw selector bytes are
        // not this guard's concern and must not trip it (do not over-guard).
        let cc = SubBlock {
            band: "B66".to_string(),
            dl_bw_class: Some(1),
            ul_bw_class: Some(1),
            dl_feature_per_cc_ids: Some(vec![5]),
            ..Default::default()
        };
        assert!(RawSubBlock::try_from_sub_block(&cc).is_ok());
    }
}
