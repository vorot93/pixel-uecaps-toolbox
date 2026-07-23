//! Band-combination model and rendering shared by `inspect`.

use crate::proto::{ShannonFeatureSetDlPerCcNr, ShannonFeatureSetUlPerCcNr, UeCaps};
use std::collections::BTreeMap;

/// Marker rendered for an absent / not-applicable capability value.
const NONE_MARK: &str = "—";

/// CA bandwidth-class index -> letter (1=A, 2=B, ...); empty for 0/absent.
pub(crate) fn bw_letter(c: Option<i32>) -> String {
    match c {
        Some(n) if (1..=6).contains(&n) => ((b'A' + (n as u8 - 1)) as char).to_string(),
        Some(0) | None => String::new(),
        Some(n) => format!("({n})"),
    }
}

/// Render a carrier component's DL/UL CA bandwidth class compactly:
/// symmetric -> "A"; asymmetric -> "A/B"; DL-only -> "A↓"; UL-only -> "A↑".
fn cc_class(dl: Option<i32>, ul: Option<i32>) -> String {
    let (d, u) = (bw_letter(dl), bw_letter(ul));
    match (d.is_empty(), u.is_empty()) {
        (true, true) => String::new(),
        (false, true) => format!("{d}↓"),
        (true, false) => format!("{u}↑"),
        (false, false) if d == u => d,
        (false, false) => format!("{d}/{u}"),
    }
}

/// NR bands are stored offset by this base; `band >= NR_BAND_OFFSET` marks an NR band.
pub(crate) const NR_BAND_OFFSET: i32 = 10_000;

/// The band label for a component of a **known** radio kind: `n<num>` (NR) or `B<num>`
/// (E-UTRA). The single source of the band-prefix convention for every caller that can see
/// *both* kinds — `band_label` (which *infers* the kind from `NR_BAND_OFFSET`),
/// `RawSubBlock::band_label`, and all of `raw_nr`'s validation/guard messages (C-band).
/// Display code that is statically single-kind — `report::lte` — formats `B` inline
/// instead; that is correct there because no NR component can reach it.
pub(crate) fn band_label_for(is_nr: bool, band: i32) -> String {
    if is_nr {
        format!("n{band}")
    } else {
        format!("B{band}")
    }
}

/// Canonical band label for a combo component, inferring the kind from the raw protobuf band:
/// `n<num>` (NR, `band >= NR_BAND_OFFSET`) or `B<num>` (E-UTRA).
pub(crate) fn band_label(band: i32) -> String {
    if band >= NR_BAND_OFFSET {
        band_label_for(true, band - NR_BAND_OFFSET)
    } else {
        band_label_for(false, band)
    }
}

/// Convert a canonical report band label (`n78` / `B66`) back to its protobuf band value.
pub(crate) fn raw_band(label: &str) -> Option<i32> {
    if let Some(n) = label.strip_prefix('n') {
        n.parse::<i32>()
            .ok()
            .filter(|&n| n > 0)
            .map(|n| NR_BAND_OFFSET + n)
    } else if let Some(b) = label.strip_prefix('B') {
        b.parse::<i32>().ok().filter(|&b| b > 0)
    } else {
        None
    }
}

fn is_nr_band(label: &str) -> bool {
    label.starts_with('n')
}

/// Render one component as `n<band><class>` (NR) / `B<band><class>` (E-UTRA).
pub(crate) fn render_component(band: i32, dl: Option<i32>, ul: Option<i32>) -> String {
    format!("{}{}", band_label(band), cc_class(dl, ul))
}

/// Band+class label for a component, e.g. `n78A` / `B1` — the same per-component
/// rendering the combo `bands` string uses. The combo identity key is built from these.
pub(crate) fn cc_component_label(cc: &SubBlock) -> String {
    format!("{}{}", cc.band, cc_class(cc.dl_bw_class, cc.ul_bw_class))
}

/// Order-normalized identity key: sorted band+class labels joined with " + ".
pub(crate) fn combo_key(combo: &Combo) -> String {
    let mut parts: Vec<String> = combo.sub_blocks.iter().map(cc_component_label).collect();
    parts.sort_unstable();
    parts.join(" + ")
}

/// NR subcarrier-spacing code -> kHz. Unknown -> None.
/// Decode tables cross-checked against the pixel-pb decoder: https://nxij.github.io/pixel-pb
pub(crate) const fn scs_khz(v: i32) -> Option<u32> {
    match v {
        1 => Some(15),
        2 => Some(30),
        3 => Some(60),
        4 => Some(120),
        5 => Some(240),
        _ => None,
    }
}

/// DL MIMO code -> label. 0 = not supported; unknown -> "(N)".
pub(crate) fn dl_mimo_label(v: i32) -> String {
    match v {
        0 => NONE_MARK.to_string(),
        1 => "2x2".to_string(),
        2 => "4x4".to_string(),
        3 => "8x8".to_string(),
        n => format!("({n})"),
    }
}

/// UL codebook-MIMO support code -> label. 0 = not supported; unknown -> "(N)".
pub(crate) fn ul_mimo_cb_label(v: i32) -> String {
    match v {
        0 => NONE_MARK.to_string(),
        1 => "No".to_string(),
        2 => "Yes".to_string(),
        n => format!("({n})"),
    }
}

/// Modulation-order code -> label. 0 = not supported; unknown -> "(N)".
pub(crate) fn mod_order_label(v: i32) -> String {
    match v {
        0 => NONE_MARK.to_string(),
        1 => "QAM64".to_string(),
        2 => "QAM256".to_string(),
        n => format!("({n})"),
    }
}

/// First per-CC id byte -> 0-based index into a feature-set list of length `len`.
/// Byte 0 / absent / out-of-range = no NR feature set; k>=1 is 1-based. Still used
/// where a single leading-byte check suffices (e.g. the compiler's compact-list
/// coverage check) — superseded for full per-CC resolution by [`resolve_all`].
pub(crate) fn feature_index(ids: Option<&[u8]>, len: usize) -> Option<usize> {
    let k = *ids?.first()? as usize;
    (1..=len).contains(&k).then(|| k - 1)
}

/// Resolve a per-CC selector array against `list` (1-based). Returns one resolved value per
/// CC iff EVERY byte is in `1..=list.len()`; otherwise `None` (keep the raw bytes verbatim).
/// Replaces the old first-byte `feature_index` for full per-CC resolution: a non-uniform
/// array like `[22, 23]` now resolves to TWO distinct features instead of silently dropping
/// every CC after the first (the data-loss bug this model fixes). `[2, 99]` (99 out of
/// range) stays raw rather than resolving on the in-range prefix.
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
        .collect() // Option<Vec<T>>: None if any element is None
}

/// One carrier component (`cc`) with its full fields. Optional fields are omitted
/// from the text-report rendering when absent.
#[derive(Clone, Default, Debug)]
pub(crate) struct SubBlock {
    pub(crate) band: String,
    pub(crate) dl_bw_class: Option<i32>,
    pub(crate) ul_bw_class: Option<i32>,
    pub(crate) dl_feature_index: Option<i32>,
    pub(crate) ul_feature_index: Option<i32>,
    pub(crate) dl_feature_per_cc_ids: Option<Vec<u8>>,
    pub(crate) ul_feature_per_cc_ids: Option<Vec<u8>>,
    /// One entry per resolved CC (empty when unresolved / absent). The scalar display
    /// fields below (`dl_scs_khz`, `dl_mimo`, …) always project CC0 — text reports
    /// render one line per component, so `--full` shows only the first CC's decoded
    /// values. The full per-CC vec is here for callers that need every CC; since Task 7,
    /// `RawSubBlock::from_sub_block` carries every entry too.
    pub(crate) dl_features: Vec<ShannonFeatureSetDlPerCcNr>,
    pub(crate) ul_features: Vec<ShannonFeatureSetUlPerCcNr>,
    pub(crate) srs_tx_switch: Option<i32>,
    pub(crate) dl_scs_khz: Option<u32>,
    pub(crate) dl_mimo: Option<String>,
    pub(crate) dl_max_bw_mhz: Option<i32>,
    pub(crate) dl_mod_order: Option<String>,
    pub(crate) dl_bw90mhz: Option<bool>,
    pub(crate) ul_scs_khz: Option<u32>,
    pub(crate) ul_mimo_cb: Option<String>,
    pub(crate) ul_mimo_non_cb: Option<i32>,
    pub(crate) ul_max_bw_mhz: Option<i32>,
    pub(crate) ul_mod_order: Option<String>,
    pub(crate) ul_bw90mhz: Option<bool>,
}

/// One carrier-aggregation combo: its rendered band string, group/combo context,
/// and components.
#[derive(Clone, Default, Debug)]
pub(crate) struct Combo {
    pub(crate) group: usize,
    pub(crate) index: usize,
    pub(crate) bands: String,
    pub(crate) power_class: Option<i32>,
    pub(crate) bcs_nr: Option<u32>,
    pub(crate) bcs_intra_endc: Option<u32>,
    pub(crate) bcs_eutra: Option<u32>,
    pub(crate) intra_band_en_dc_support: Option<i32>,
    pub(crate) bit_mask: u32,
    pub(crate) sub_blocks: Vec<SubBlock>,
}

/// Build combo views together with their exact optional wire bitmask presence.
/// Most report callers intentionally use [`build_combos`], whose
/// historical scalar view maps absence to zero. Folder ingestion needs the
/// optional form to distinguish the modern input contract at its boundary.
impl SubBlock {
    /// Build a display `SubBlock` from a component's raw protobuf fields plus its resolved DL/UL
    /// feature sets. The one place the 11 derived display fields (SCS / MIMO / max-BW /
    /// mod-order / 90 MHz, per direction) are projected from the feature sets — used by the
    /// folder compiler's `build_combos_with_bitmasks` (C-proj).
    ///
    /// `is_nr` is the explicit kind assertion: the caller already knows whether this is an
    /// NR or E-UTRA component, so it routes through `band_label_for` directly instead of the
    /// inferring `band_label`. `plain_band` is the actual band number (e.g. `78` for n78,
    /// `66` for B66), NOT the raw protobuf `NR_BAND_OFFSET + n` encoding.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn from_raw_fields(
        is_nr: bool,
        plain_band: i32,
        dl_bw_class: Option<i32>,
        ul_bw_class: Option<i32>,
        dl_feature_index: Option<i32>,
        ul_feature_index: Option<i32>,
        dl_feature_per_cc_ids: Option<Vec<u8>>,
        ul_feature_per_cc_ids: Option<Vec<u8>>,
        srs_tx_switch: Option<i32>,
        dl: Vec<ShannonFeatureSetDlPerCcNr>,
        ul: Vec<ShannonFeatureSetUlPerCcNr>,
    ) -> Self {
        // Display fields always project CC0 — see the `dl_features` doc comment.
        let dl0 = dl.first();
        let ul0 = ul.first();
        Self {
            band: band_label_for(is_nr, plain_band),
            dl_bw_class,
            ul_bw_class,
            dl_feature_index,
            ul_feature_index,
            dl_feature_per_cc_ids,
            ul_feature_per_cc_ids,
            srs_tx_switch,
            dl_scs_khz: dl0.and_then(|f| f.max_scs).and_then(scs_khz),
            dl_mimo: dl0.and_then(|f| f.max_mimo).map(dl_mimo_label),
            dl_max_bw_mhz: dl0.and_then(|f| f.max_bw),
            dl_mod_order: dl0.and_then(|f| f.max_mod_order).map(mod_order_label),
            dl_bw90mhz: dl0.and_then(|f| f.bw_90mhz_supported),
            ul_scs_khz: ul0.and_then(|f| f.max_scs).and_then(scs_khz),
            ul_mimo_cb: ul0.and_then(|f| f.max_mimo_cb).map(ul_mimo_cb_label),
            ul_mimo_non_cb: ul0.and_then(|f| f.max_mimo_non_cb),
            ul_max_bw_mhz: ul0.and_then(|f| f.max_bw),
            ul_mod_order: ul0.and_then(|f| f.max_mod_order).map(mod_order_label),
            ul_bw90mhz: ul0.and_then(|f| f.bw_90mhz_supported),
            // `dl`/`ul` are moved in last, after every projection above.
            dl_features: dl,
            ul_features: ul,
        }
    }
}

pub(crate) fn build_combos_with_bitmasks(caps: &UeCaps) -> Vec<(Combo, Option<u32>)> {
    let mut combo = Vec::new();
    for (gi, cg) in caps.combo_groups.iter().enumerate() {
        let h = cg.combo_header.as_ref();
        for (ci, c) in cg.combo.iter().enumerate() {
            let sub_blocks: Vec<SubBlock> = c
                .sub_blocks
                .iter()
                .map(|x| {
                    // All-or-nothing resolution per spec: every per-CC id must be in range
                    // for the whole array to resolve. E-UTRA components carry id 0 in the
                    // data, so they resolve to nothing without an explicit `nr` gate.
                    let dl_fs = resolve_all(
                        x.dl_feature_per_cc_ids.as_deref(),
                        &caps.dl_feature_per_cc_list,
                    )
                    .unwrap_or_default();
                    let ul_fs = resolve_all(
                        x.ul_feature_per_cc_ids.as_deref(),
                        &caps.ul_feature_per_cc_list,
                    )
                    .unwrap_or_default();
                    // Protobuf bands encode the kind via the offset; split it once at the
                    // call site so `from_raw_fields` gets an explicit kind assertion.
                    let is_nr = x.band >= NR_BAND_OFFSET;
                    let plain_band = if is_nr {
                        x.band - NR_BAND_OFFSET
                    } else {
                        x.band
                    };
                    SubBlock::from_raw_fields(
                        is_nr,
                        plain_band,
                        x.dl_bw_class,
                        x.ul_bw_class,
                        x.dl_feature_index,
                        x.ul_feature_index,
                        x.dl_feature_per_cc_ids.clone(),
                        x.ul_feature_per_cc_ids.clone(),
                        x.srstxswitch,
                        dl_fs,
                        ul_fs,
                    )
                })
                .collect();
            let bands = c
                .sub_blocks
                .iter()
                .map(|x| render_component(x.band, x.dl_bw_class, x.ul_bw_class))
                .collect::<Vec<_>>()
                .join(" + ");
            combo.push((
                Combo {
                    group: gi + 1,
                    index: ci + 1,
                    bands,
                    power_class: h.and_then(|x| x.power_class),
                    bcs_nr: h.and_then(|x| x.bcs_nr),
                    bcs_intra_endc: h.and_then(|x| x.bcs_intra_endc),
                    bcs_eutra: h.and_then(|x| x.bcs_eutra),
                    intra_band_en_dc_support: h.and_then(|x| x.intra_band_en_dc_support),
                    bit_mask: c.bitmask.unwrap_or(0),
                    sub_blocks,
                },
                c.bitmask,
            ));
        }
    }
    combo
}

/// Build the historical scalar combo view used by reports.
pub(crate) fn build_combos(caps: &UeCaps) -> Vec<Combo> {
    build_combos_with_bitmasks(caps)
        .into_iter()
        .map(|(combo, _)| combo)
        .collect()
}

/// Render a component's decoded NR feature set for `--full`. No feature set
/// (E-UTRA component, or NR with id 0) yields a short marker. `srs:` is
/// appended only when present, preserving today's datum.
fn has_dl_feature_value(cc: &SubBlock) -> bool {
    cc.dl_features.first().is_some_and(|f| {
        f.max_scs.is_some()
            || f.max_mimo.is_some()
            || f.max_bw.is_some()
            || f.max_mod_order.is_some()
            || f.bw_90mhz_supported.is_some()
    }) || cc.dl_scs_khz.is_some()
        || cc.dl_mimo.is_some()
        || cc.dl_max_bw_mhz.is_some()
        || cc.dl_mod_order.is_some()
        || cc.dl_bw90mhz.is_some()
}

fn has_ul_feature_value(cc: &SubBlock) -> bool {
    cc.ul_features.first().is_some_and(|f| {
        f.max_scs.is_some()
            || f.max_mimo_cb.is_some()
            || f.max_bw.is_some()
            || f.max_mod_order.is_some()
            || f.bw_90mhz_supported.is_some()
            || f.max_mimo_non_cb.is_some()
    }) || cc.ul_scs_khz.is_some()
        || cc.ul_mimo_cb.is_some()
        || cc.ul_mimo_non_cb.is_some()
        || cc.ul_max_bw_mhz.is_some()
        || cc.ul_mod_order.is_some()
        || cc.ul_bw90mhz.is_some()
}

fn bw_text(mhz: Option<i32>) -> String {
    mhz.map_or_else(|| NONE_MARK.to_string(), |bw| format!("{bw}MHz"))
}

/// Per-direction SCS suffix: decoded kHz, else the raw code, else empty. Rendered inside
/// the DL/UL part so the two directions never fold together (R5).
fn dir_scs(khz: Option<u32>, raw_code: Option<i32>) -> String {
    if let Some(k) = khz {
        format!(" SCS {k}kHz")
    } else if let Some(n) = raw_code {
        format!(" SCS ({n})")
    } else {
        String::new()
    }
}

/// Per-direction 90 MHz suffix.
const fn dir_bw90(supported: Option<bool>) -> &'static str {
    match supported {
        Some(true) => " +90MHz",
        _ => "",
    }
}

pub(crate) fn fmt_cc_features(cc: &SubBlock) -> String {
    let has_dl = has_dl_feature_value(cc);
    let has_ul = has_ul_feature_value(cc);
    let base = if !has_dl && !has_ul {
        if is_nr_band(&cc.band) {
            "(no NR feature set)".to_string()
        } else {
            "E-UTRA — no NR feature set".to_string()
        }
    } else {
        let mut parts: Vec<String> = Vec::new();
        // SCS and 90 MHz are rendered per-direction (not folded via `.or()`), so a
        // UL-only SCS/90 MHz difference is visible to both `inspect --full` and the
        // `compare` signature (R5).
        if has_dl {
            let scs = dir_scs(
                cc.dl_scs_khz,
                cc.dl_features.first().and_then(|f| f.max_scs),
            );
            parts.push(format!(
                "DL {} {} {}{scs}{}",
                bw_text(cc.dl_max_bw_mhz),
                cc.dl_mimo.as_deref().unwrap_or(NONE_MARK),
                cc.dl_mod_order.as_deref().unwrap_or(NONE_MARK),
                dir_bw90(cc.dl_bw90mhz),
            ));
        }
        if has_ul {
            let noncb = cc
                .ul_mimo_non_cb
                .map_or_else(|| NONE_MARK.to_string(), |n| n.to_string());
            let scs = dir_scs(
                cc.ul_scs_khz,
                cc.ul_features.first().and_then(|f| f.max_scs),
            );
            parts.push(format!(
                "UL {} cb:{} nonCb:{} {}{scs}{}",
                bw_text(cc.ul_max_bw_mhz),
                cc.ul_mimo_cb.as_deref().unwrap_or(NONE_MARK),
                noncb,
                cc.ul_mod_order.as_deref().unwrap_or(NONE_MARK),
                dir_bw90(cc.ul_bw90mhz),
            ));
        }
        parts.join(" · ")
    };
    match cc.srs_tx_switch {
        Some(v) => format!("{base} · srs:{v}"),
        None => base,
    }
}

/// Print the band-combinations section: one compact `g<grp> <bands>` line per
/// combo, plus indented per-component detail when `full`.
pub(crate) fn print_combos(combos: &[Combo], full: bool) {
    if combos.is_empty() {
        println!("Band combinations: none (reference stub)");
        return;
    }
    let mut per_group: BTreeMap<usize, usize> = BTreeMap::new();
    for c in combos {
        *per_group.entry(c.group).or_default() += 1;
    }
    println!("Band combinations ({})", combos.len());
    for c in combos {
        let label = if per_group[&c.group] > 1 {
            format!("g{}.{}", c.group, c.index)
        } else {
            format!("g{}", c.group)
        };
        println!("  {:<6} {}", label, c.bands);
        if full {
            for x in &c.sub_blocks {
                println!("       {:<5} {}", x.band, fmt_cc_features(x));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cc_base(nr: bool) -> SubBlock {
        SubBlock {
            band: if nr { "n78".into() } else { "B1".into() },
            dl_bw_class: None,
            ul_bw_class: None,
            dl_feature_index: None,
            ul_feature_index: None,
            dl_feature_per_cc_ids: None,
            ul_feature_per_cc_ids: None,
            dl_features: Vec::new(),
            ul_features: Vec::new(),
            srs_tx_switch: None,
            dl_scs_khz: None,
            dl_mimo: None,
            dl_max_bw_mhz: None,
            dl_mod_order: None,
            dl_bw90mhz: None,
            ul_scs_khz: None,
            ul_mimo_cb: None,
            ul_mimo_non_cb: None,
            ul_max_bw_mhz: None,
            ul_mod_order: None,
            ul_bw90mhz: None,
        }
    }

    #[test]
    fn component_label_band_and_class() {
        let mut cc = cc_base(true); // n78, no class
        assert_eq!(cc_component_label(&cc), "n78");
        cc.dl_bw_class = Some(1);
        cc.ul_bw_class = Some(1);
        assert_eq!(cc_component_label(&cc), "n78A");
        assert_eq!(cc_component_label(&cc_base(false)), "B1");
    }

    #[test]
    fn format_features_nr() {
        let mut cc = cc_base(true);
        cc.dl_max_bw_mhz = Some(100);
        cc.dl_mimo = Some("4x4".into());
        cc.dl_mod_order = Some("QAM256".into());
        cc.dl_scs_khz = Some(30);
        cc.dl_bw90mhz = Some(true);
        cc.ul_max_bw_mhz = Some(100);
        cc.ul_mimo_cb = Some("Yes".into());
        cc.ul_mimo_non_cb = Some(1);
        cc.ul_mod_order = Some("QAM256".into());
        // SCS/90 MHz render inside the DL part (they are DL values here), not a shared tail.
        assert_eq!(
            fmt_cc_features(&cc),
            "DL 100MHz 4x4 QAM256 SCS 30kHz +90MHz · UL 100MHz cb:Yes nonCb:1 QAM256"
        );
    }

    #[test]
    fn format_features_partial_nr_without_bandwidth() {
        let mut cc = cc_base(true);
        cc.dl_mimo = Some("(7)".into());

        assert_eq!(fmt_cc_features(&cc), "DL — (7) —");
    }

    #[test]
    fn format_features_unknown_raw_scs_without_bandwidth() {
        let mut cc = cc_base(true);
        cc.dl_features = vec![crate::proto::ShannonFeatureSetDlPerCcNr {
            max_scs: Some(9),
            ..Default::default()
        }];

        assert_eq!(fmt_cc_features(&cc), "DL — — — SCS (9)");
    }

    #[test]
    fn format_features_markers() {
        assert_eq!(
            fmt_cc_features(&cc_base(false)),
            "E-UTRA — no NR feature set"
        );
        assert_eq!(fmt_cc_features(&cc_base(true)), "(no NR feature set)");
        let mut cc = cc_base(false);
        cc.srs_tx_switch = Some(1);
        assert_eq!(fmt_cc_features(&cc), "E-UTRA — no NR feature set · srs:1");
    }

    #[test]
    fn bandwidth_class_letters() {
        assert_eq!(bw_letter(Some(1)), "A");
        assert_eq!(bw_letter(Some(3)), "C");
        assert_eq!(bw_letter(Some(6)), "F");
        assert_eq!(bw_letter(None), "");
        assert_eq!(bw_letter(Some(0)), "");
        assert_eq!(bw_letter(Some(9)), "(9)");
    }

    #[test]
    fn carrier_component_class() {
        assert_eq!(cc_class(Some(1), Some(1)), "A");
        assert_eq!(cc_class(Some(1), Some(2)), "A/B");
        assert_eq!(cc_class(Some(1), None), "A↓");
        assert_eq!(cc_class(None, Some(1)), "A↑");
        assert_eq!(cc_class(None, None), "");
    }

    #[test]
    fn decode_scs() {
        assert_eq!(scs_khz(1), Some(15));
        assert_eq!(scs_khz(2), Some(30));
        assert_eq!(scs_khz(3), Some(60));
        assert_eq!(scs_khz(4), Some(120));
        assert_eq!(scs_khz(5), Some(240));
        assert_eq!(scs_khz(0), None);
        assert_eq!(scs_khz(9), None);
    }

    #[test]
    fn decode_mimo_and_mod() {
        assert_eq!(dl_mimo_label(0), "—");
        assert_eq!(dl_mimo_label(1), "2x2");
        assert_eq!(dl_mimo_label(3), "8x8");
        assert_eq!(dl_mimo_label(7), "(7)");
        assert_eq!(ul_mimo_cb_label(1), "No");
        assert_eq!(ul_mimo_cb_label(2), "Yes");
        assert_eq!(ul_mimo_cb_label(5), "(5)");
        assert_eq!(mod_order_label(1), "QAM64");
        assert_eq!(mod_order_label(2), "QAM256");
        assert_eq!(mod_order_label(9), "(9)");
    }

    #[test]
    fn resolve_all_keeps_every_cc_or_none() {
        let list = vec![10i32, 20, 30]; // 1-based selectors 1..=3
        assert_eq!(resolve_all(Some(&[1, 3]), &list), Some(vec![10, 30])); // non-uniform, both kept
        assert_eq!(resolve_all(Some(&[2]), &list), Some(vec![20]));
        assert_eq!(resolve_all(Some(&[2, 99]), &list), None); // any out-of-range -> keep raw
        assert_eq!(resolve_all(Some(&[0]), &list), None); // zero -> not resolved (placeholder)
        assert_eq!(resolve_all(None, &list), None);
    }

    #[test]
    fn linkage_first_byte_one_based() {
        assert_eq!(feature_index(Some(&[1]), 18), Some(0));
        assert_eq!(feature_index(Some(&[17]), 18), Some(16));
        assert_eq!(feature_index(Some(&[0]), 18), None); // 0 = no NR feature
        assert_eq!(feature_index(Some(&[19]), 18), None); // out of range
        assert_eq!(feature_index(None, 18), None);
        assert_eq!(feature_index(Some(&[]), 18), None);
    }

    #[test]
    fn build_combos_resolves_feature_sets() {
        use crate::proto::{
            ComboGroup, ShannonFeatureSetDlPerCcNr, ShannonFeatureSetUlPerCcNr, UeCaps, combo_group,
        };
        let caps = UeCaps {
            dl_feature_per_cc_list: vec![ShannonFeatureSetDlPerCcNr {
                max_scs: Some(2),
                max_mimo: Some(2),
                max_bw: Some(100),
                max_mod_order: Some(2),
                bw_90mhz_supported: Some(true),
            }],
            ul_feature_per_cc_list: vec![ShannonFeatureSetUlPerCcNr {
                max_scs: Some(2),
                max_mimo_cb: Some(2),
                max_bw: Some(100),
                max_mod_order: Some(2),
                bw_90mhz_supported: None,
                max_mimo_non_cb: Some(1),
            }],
            combo_groups: vec![ComboGroup {
                combo_header: None,
                combo: vec![combo_group::Combo {
                    bitmask: Some(0),
                    sub_blocks: vec![
                        combo_group::combo::SubBlock {
                            band: 10078,
                            dl_feature_per_cc_ids: Some(vec![1]),
                            ul_feature_per_cc_ids: Some(vec![1]),
                            ..Default::default()
                        },
                        combo_group::combo::SubBlock {
                            band: 1,
                            dl_feature_per_cc_ids: Some(vec![0]),
                            ul_feature_per_cc_ids: Some(vec![0]),
                            ..Default::default()
                        },
                    ],
                }],
            }],
            ..Default::default()
        };
        let combos = build_combos(&caps);
        let cc = &combos[0].sub_blocks;
        assert_eq!(cc[0].dl_feature_per_cc_ids, Some(vec![1]));
        assert_eq!(cc[0].ul_feature_per_cc_ids, Some(vec![1]));
        assert_eq!(cc[1].dl_feature_per_cc_ids, Some(vec![0]));
        assert_eq!(cc[1].ul_feature_per_cc_ids, Some(vec![0]));
        // n78 (NR) resolves to DL/UL feature-set entry 1
        assert_eq!(cc[0].band, "n78");
        assert_eq!(cc[0].dl_max_bw_mhz, Some(100));
        assert_eq!(cc[0].dl_mimo.as_deref(), Some("4x4"));
        assert_eq!(cc[0].dl_scs_khz, Some(30));
        assert_eq!(cc[0].dl_mod_order.as_deref(), Some("QAM256"));
        assert_eq!(cc[0].dl_bw90mhz, Some(true));
        assert_eq!(cc[0].ul_mimo_cb.as_deref(), Some("Yes"));
        assert_eq!(cc[0].ul_mimo_non_cb, Some(1));
        let dl_raw = cc[0]
            .dl_features
            .first()
            .expect("raw DL feature set is retained");
        assert_eq!(dl_raw.max_scs, Some(2));
        assert_eq!(dl_raw.max_mimo, Some(2));
        assert_eq!(dl_raw.max_bw, Some(100));
        assert_eq!(dl_raw.max_mod_order, Some(2));
        assert_eq!(dl_raw.bw_90mhz_supported, Some(true));

        let ul_raw = cc[0]
            .ul_features
            .first()
            .expect("raw UL feature set is retained");
        assert_eq!(ul_raw.max_scs, Some(2));
        assert_eq!(ul_raw.max_mimo_cb, Some(2));
        assert_eq!(ul_raw.max_bw, Some(100));
        assert_eq!(ul_raw.max_mod_order, Some(2));
        assert_eq!(ul_raw.bw_90mhz_supported, None);
        assert_eq!(ul_raw.max_mimo_non_cb, Some(1));
        assert!(cc[1].dl_features.is_empty());
        assert!(cc[1].ul_features.is_empty());
        // B1 (E-UTRA, id 0) resolves to nothing
        assert_eq!(cc[1].band, "B1");
        assert_eq!(cc[1].dl_max_bw_mhz, None);
        assert_eq!(cc[1].dl_mimo, None);
        assert_eq!(cc[1].ul_mimo_cb, None);
    }

    #[test]
    fn ul_only_scs_change_is_visible() {
        // R5: DL and UL SCS must render independently; a UL-only SCS change (DL equal)
        // must change the caps line so `compare` sees it.
        let mut a = cc_base(true);
        a.dl_scs_khz = Some(30);
        a.ul_scs_khz = Some(15);
        let mut b = cc_base(true);
        b.dl_scs_khz = Some(30);
        b.ul_scs_khz = Some(30);
        assert_ne!(
            fmt_cc_features(&a),
            fmt_cc_features(&b),
            "a UL-only SCS change must be visible"
        );
    }

    #[test]
    fn ul_90mhz_is_not_masked_by_dl() {
        // R5: dl_bw90=false must not fold away ul_bw90=true (inspect --full dropped it).
        let mut cc = cc_base(true);
        cc.dl_bw90mhz = Some(false);
        cc.ul_bw90mhz = Some(true);
        let text = fmt_cc_features(&cc);
        assert!(text.contains("+90MHz"), "UL 90MHz must show: {text}");
    }
}
