//! Band-combination model and rendering shared by `inspect`.

use crate::{
    proto::{
        ShannonFeatureSetDlPerCcNr, ShannonFeatureSetUlPerCcNr, SubBlock as ProtoSubBlock, UeCaps,
    },
    raw_nr::SubBlockKind,
    report::detail::Detail,
};
use compact_str::CompactString;
use std::collections::BTreeMap;

/// Marker rendered for an absent / not-applicable capability value.
const NONE_MARK: &str = "—";

/// [`NONE_MARK`] as an owned string, for the `map_or_else` default at the render sites.
fn none_mark() -> String {
    NONE_MARK.to_string()
}

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

/// Canonical band label for a combo component, inferring the kind from the raw protobuf band:
/// `n<num>` (NR, `band >= NR_BAND_OFFSET`) or `B<num>` (E-UTRA). The kind-known counterpart —
/// the single source of the `n`/`B` prefix convention — is [`SubBlockKind::band_label`].
fn band_label(band: i32) -> CompactString {
    let (kind, plain) = SubBlockKind::split_raw_band(band);
    kind.band_label(plain)
}

fn is_nr_band(label: &str) -> bool {
    label.starts_with('n')
}

/// Render one component as `n<band><class>` (NR) / `B<band><class>` (E-UTRA), **inferring** the
/// kind from the raw protobuf band. Only for callers that genuinely do not know the kind; a
/// caller that does must use [`render_known_component`], since inference reads any band at or
/// above [`NR_BAND_OFFSET`] as NR and would mislabel an out-of-range value.
pub(crate) fn render_component(band: i32, dl: Option<i32>, ul: Option<i32>) -> String {
    format!("{}{}", band_label(band), cc_class(dl, ul))
}

/// [`render_component`] for a caller that already knows the component's kind — the plain band
/// number is labelled as that kind, whatever its magnitude.
pub(crate) fn render_known_component(
    kind: SubBlockKind,
    band: i32,
    dl: Option<i32>,
    ul: Option<i32>,
) -> String {
    format!("{}{}", kind.band_label(band), cc_class(dl, ul))
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
const fn scs_khz(v: i32) -> Option<u32> {
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
fn dl_mimo_label(v: i32) -> String {
    match v {
        0 => NONE_MARK.to_string(),
        1 => "2x2".to_string(),
        2 => "4x4".to_string(),
        3 => "8x8".to_string(),
        n => format!("({n})"),
    }
}

/// UL codebook-MIMO support code -> label. 0 = not supported; unknown -> "(N)".
fn ul_mimo_cb_label(v: i32) -> String {
    match v {
        0 => NONE_MARK.to_string(),
        1 => "No".to_string(),
        2 => "Yes".to_string(),
        n => format!("({n})"),
    }
}

/// Modulation-order code -> label. 0 = not supported; unknown -> "(N)".
fn mod_order_label(v: i32) -> String {
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

/// One carrier component (`cc`) as the text reports need it: the rendered band label, the
/// two CA bandwidth classes, and the resolved per-CC feature sets.
///
/// The decoded display values (SCS in kHz, the MIMO/modulation labels, max bandwidth, 90 MHz)
/// are **not** stored — they are pure functions of `dl_features`/`ul_features` and are
/// projected at the one place that renders them, [`fmt_cc_features`]. Storing them alongside
/// the records they come from would be two representations of one fact with nothing keeping
/// them in agreement.
#[derive(Clone, Default, Debug)]
pub(crate) struct SubBlock {
    pub(crate) band: CompactString,
    pub(crate) dl_bw_class: Option<i32>,
    pub(crate) ul_bw_class: Option<i32>,
    /// One entry per resolved CC (empty when unresolved / absent). Text reports render one
    /// line per component, so the display projection shows CC0's decoded values; the full
    /// per-CC vec is kept for callers that need every CC.
    pub(crate) dl_features: Vec<ShannonFeatureSetDlPerCcNr>,
    pub(crate) ul_features: Vec<ShannonFeatureSetUlPerCcNr>,
    pub(crate) srs_tx_switch: Option<i32>,
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
    /// The per-combo bitmask exactly as the file carries it, **including its presence**.
    /// `proto::Combo::bitmask` is `optional` precisely because real files carry an explicit
    /// zero that a bare proto3 scalar would drop on re-encode; flattening `None` and `Some(0)`
    /// to `0` here made `compare` — whose entire job is diffing capability files — blind to a
    /// byte-level difference the schema goes out of its way to model.
    pub(crate) bit_mask: Option<u32>,
    pub(crate) sub_blocks: Vec<SubBlock>,
}

/// Build one display `SubBlock` from a raw protobuf component: resolve its DL/UL
/// per-CC feature ids against `caps`'s catalogs (all-or-nothing per spec — every per-CC id
/// must be in range for the whole array to resolve; E-UTRA components carry id 0 in the
/// data, so they resolve to nothing without an explicit `nr` gate), and label the band with
/// the kind [`SubBlockKind::split_raw_band`] asserts, rather than the inferring [`band_label`].
fn build_sub_block(raw: &ProtoSubBlock, caps: &UeCaps) -> SubBlock {
    let (kind, plain_band) = SubBlockKind::split_raw_band(raw.band);
    SubBlock {
        band: kind.band_label(plain_band),
        dl_bw_class: raw.dl_bw_class,
        ul_bw_class: raw.ul_bw_class,
        dl_features: resolve_all(
            raw.dl_feature_per_cc_ids.as_deref(),
            &caps.dl_feature_per_cc_list,
        )
        .unwrap_or_default(),
        ul_features: resolve_all(
            raw.ul_feature_per_cc_ids.as_deref(),
            &caps.ul_feature_per_cc_list,
        )
        .unwrap_or_default(),
        srs_tx_switch: raw.srstxswitch,
    }
}

/// Build combo views together with their exact optional wire bitmask presence. Most
/// report callers intentionally use [`build_combos`], whose historical scalar view maps
/// absence to zero — folder ingestion needs the optional form to distinguish the modern
/// input contract at its boundary. Per-component resolution is delegated to
/// [`build_sub_block`].
pub(crate) fn build_combos_with_bitmasks(caps: &UeCaps) -> Vec<(Combo, Option<u32>)> {
    let mut combo = Vec::new();
    for (gi, cg) in caps.combo_groups.iter().enumerate() {
        let h = cg.combo_header.as_ref();
        for (ci, c) in cg.combo.iter().enumerate() {
            let sub_blocks: Vec<SubBlock> = c
                .sub_blocks
                .iter()
                .map(|x| build_sub_block(x, caps))
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
                    bit_mask: c.bitmask,
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

/// CC0's DL feature record, if it has anything to show. Text reports render one line per
/// component, so only the first CC's values reach the display. An all-`None` record is a
/// legitimate catalog entry with no displayable value, and reads the same as no record.
fn dl_display_record(cc: &SubBlock) -> Option<&ShannonFeatureSetDlPerCcNr> {
    cc.dl_features.first().filter(|f| {
        f.max_scs.is_some()
            || f.max_mimo.is_some()
            || f.max_bw.is_some()
            || f.max_mod_order.is_some()
            || f.bw_90mhz_supported.is_some()
    })
}

/// See [`dl_display_record`].
fn ul_display_record(cc: &SubBlock) -> Option<&ShannonFeatureSetUlPerCcNr> {
    cc.ul_features.first().filter(|f| {
        f.max_scs.is_some()
            || f.max_mimo_cb.is_some()
            || f.max_bw.is_some()
            || f.max_mod_order.is_some()
            || f.bw_90mhz_supported.is_some()
            || f.max_mimo_non_cb.is_some()
    })
}

fn bw_text(mhz: Option<i32>) -> String {
    mhz.map_or_else(|| NONE_MARK.to_string(), |bw| format!("{bw}MHz"))
}

/// Per-direction SCS suffix: decoded kHz, else the raw code, else empty. Rendered inside
/// the DL/UL part so the two directions never fold together.
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
    let dl = dl_display_record(cc);
    let ul = ul_display_record(cc);
    let base = if dl.is_none() && ul.is_none() {
        if is_nr_band(&cc.band) {
            "(no NR feature set)".to_string()
        } else {
            "E-UTRA — no NR feature set".to_string()
        }
    } else {
        // SCS and 90 MHz are rendered per-direction (not folded via `.or()`), so a
        // UL-only SCS/90 MHz difference is visible to both `inspect --full` and the
        // `compare` signature.
        let mut parts: Vec<String> = Vec::new();
        if let Some(f) = dl {
            parts.push(format!(
                "DL {} {} {}{}{}",
                bw_text(f.max_bw),
                f.max_mimo.map_or_else(none_mark, dl_mimo_label),
                f.max_mod_order.map_or_else(none_mark, mod_order_label),
                dir_scs(f.max_scs.and_then(scs_khz), f.max_scs),
                dir_bw90(f.bw_90mhz_supported),
            ));
        }
        if let Some(f) = ul {
            parts.push(format!(
                "UL {} cb:{} nonCb:{} {}{}{}",
                bw_text(f.max_bw),
                f.max_mimo_cb.map_or_else(none_mark, ul_mimo_cb_label),
                f.max_mimo_non_cb.map_or_else(none_mark, |n| n.to_string()),
                f.max_mod_order.map_or_else(none_mark, mod_order_label),
                dir_scs(f.max_scs.and_then(scs_khz), f.max_scs),
                dir_bw90(f.bw_90mhz_supported),
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
/// combo, plus indented per-component detail under [`Detail::Full`].
pub(crate) fn print_combos(combos: &[Combo], detail: Detail) {
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
        if detail.is_full() {
            for x in &c.sub_blocks {
                println!("       {:<5} {}", x.band, fmt_cc_features(x));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cc_base(kind: SubBlockKind) -> SubBlock {
        SubBlock {
            band: match kind {
                SubBlockKind::Nr => "n78".into(),
                SubBlockKind::Lte => "B1".into(),
            },
            ..Default::default()
        }
    }

    /// `cc_base` plus CC0 DL/UL feature records — the only way display values reach a report
    /// now that nothing is pre-projected onto the DTO.
    fn cc_with_features(
        dl: Option<ShannonFeatureSetDlPerCcNr>,
        ul: Option<ShannonFeatureSetUlPerCcNr>,
    ) -> SubBlock {
        SubBlock {
            dl_features: dl.into_iter().collect(),
            ul_features: ul.into_iter().collect(),
            ..cc_base(SubBlockKind::Nr)
        }
    }

    #[test]
    fn sub_block_kind_band_label_uses_n_and_b_prefixes() {
        use crate::raw_nr::SubBlockKind;
        assert_eq!(SubBlockKind::Nr.band_label(78), "n78");
        assert_eq!(SubBlockKind::Lte.band_label(66), "B66");
    }

    #[test]
    fn component_label_band_and_class() {
        let mut cc = cc_base(SubBlockKind::Nr); // n78, no class
        assert_eq!(cc_component_label(&cc), "n78");
        cc.dl_bw_class = Some(1);
        cc.ul_bw_class = Some(1);
        assert_eq!(cc_component_label(&cc), "n78A");
        assert_eq!(cc_component_label(&cc_base(SubBlockKind::Lte)), "B1");
    }

    #[test]
    fn format_features_nr() {
        let cc = cc_with_features(
            Some(ShannonFeatureSetDlPerCcNr {
                max_scs: Some(2),  // 30 kHz
                max_mimo: Some(2), // 4x4
                max_bw: Some(100),
                max_mod_order: Some(2), // QAM256
                bw_90mhz_supported: Some(true),
            }),
            Some(ShannonFeatureSetUlPerCcNr {
                max_scs: None,
                max_mimo_cb: Some(2), // Yes
                max_bw: Some(100),
                max_mod_order: Some(2), // QAM256
                bw_90mhz_supported: None,
                max_mimo_non_cb: Some(1),
            }),
        );
        // SCS/90 MHz render inside the DL part (they are DL values here), not a shared tail.
        assert_eq!(
            fmt_cc_features(&cc),
            "DL 100MHz 4x4 QAM256 SCS 30kHz +90MHz · UL 100MHz cb:Yes nonCb:1 QAM256"
        );
    }

    #[test]
    fn format_features_partial_nr_without_bandwidth() {
        let cc = cc_with_features(
            Some(ShannonFeatureSetDlPerCcNr {
                max_mimo: Some(7), // out of table -> "(7)"
                ..Default::default()
            }),
            None,
        );

        assert_eq!(fmt_cc_features(&cc), "DL — (7) —");
    }

    #[test]
    fn format_features_unknown_raw_scs_without_bandwidth() {
        let cc = cc_with_features(
            Some(ShannonFeatureSetDlPerCcNr {
                max_scs: Some(9),
                ..Default::default()
            }),
            None,
        );

        assert_eq!(fmt_cc_features(&cc), "DL — — — SCS (9)");
    }

    #[test]
    fn an_all_absent_record_reads_as_no_feature_set() {
        // A catalog record whose every field is `None` has nothing to display, so it renders
        // exactly like an absent one rather than a row of markers.
        let cc = cc_with_features(Some(ShannonFeatureSetDlPerCcNr::default()), None);
        assert_eq!(fmt_cc_features(&cc), "(no NR feature set)");
    }

    #[test]
    fn format_features_markers() {
        assert_eq!(
            fmt_cc_features(&cc_base(SubBlockKind::Lte)),
            "E-UTRA — no NR feature set"
        );
        assert_eq!(
            fmt_cc_features(&cc_base(SubBlockKind::Nr)),
            "(no NR feature set)"
        );
        let mut cc = cc_base(SubBlockKind::Lte);
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
            Combo as ProtoCombo, ComboGroup, ShannonFeatureSetDlPerCcNr,
            ShannonFeatureSetUlPerCcNr, SubBlock as ProtoSubBlock, UeCaps,
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
                combo: vec![ProtoCombo {
                    bitmask: Some(0),
                    sub_blocks: vec![
                        ProtoSubBlock {
                            band: 10078,
                            dl_feature_per_cc_ids: Some(vec![1]),
                            ul_feature_per_cc_ids: Some(vec![1]),
                            ..Default::default()
                        },
                        ProtoSubBlock {
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

        // n78 (NR) resolves to DL/UL feature-set entry 1, retained verbatim.
        assert_eq!(cc[0].band, "n78");
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

        // …and those records are what the display projection reads.
        assert_eq!(
            fmt_cc_features(&cc[0]),
            "DL 100MHz 4x4 QAM256 SCS 30kHz +90MHz · UL 100MHz cb:Yes nonCb:1 QAM256 SCS 30kHz"
        );

        // B1 (E-UTRA, selector id 0) resolves to nothing.
        assert_eq!(cc[1].band, "B1");
        assert!(cc[1].dl_features.is_empty());
        assert!(cc[1].ul_features.is_empty());
        assert_eq!(fmt_cc_features(&cc[1]), "E-UTRA — no NR feature set");
    }

    #[test]
    fn ul_only_scs_change_is_visible() {
        // DL and UL SCS must render independently; a UL-only SCS change (DL equal)
        // must change the caps line so `compare` sees it.
        let with_ul_scs = |ul_scs| {
            cc_with_features(
                Some(ShannonFeatureSetDlPerCcNr {
                    max_scs: Some(2), // 30 kHz, equal on both sides
                    ..Default::default()
                }),
                Some(ShannonFeatureSetUlPerCcNr {
                    max_scs: Some(ul_scs),
                    ..Default::default()
                }),
            )
        };
        assert_ne!(
            fmt_cc_features(&with_ul_scs(1)), // 15 kHz
            fmt_cc_features(&with_ul_scs(2)), // 30 kHz
            "a UL-only SCS change must be visible"
        );
    }

    #[test]
    fn ul_90mhz_is_not_masked_by_dl() {
        // dl_bw90=false must not fold away ul_bw90=true (inspect --full dropped it).
        let cc = cc_with_features(
            Some(ShannonFeatureSetDlPerCcNr {
                bw_90mhz_supported: Some(false),
                ..Default::default()
            }),
            Some(ShannonFeatureSetUlPerCcNr {
                bw_90mhz_supported: Some(true),
                ..Default::default()
            }),
        );
        let text = fmt_cc_features(&cc);
        assert!(text.contains("+90MHz"), "UL 90MHz must show: {text}");
    }
}
