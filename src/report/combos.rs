//! Band-combination model and rendering shared by `inspect`.

use crate::proto::UeCaps;
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

/// Canonical band label for a combo component: `n<num>` (NR, `band >= NR_BAND_OFFSET`)
/// or `B<num>` (E-UTRA). The crate-wide single source of the band-prefix convention.
pub(crate) fn band_label(band: i32) -> String {
    if band >= NR_BAND_OFFSET {
        format!("n{}", band - NR_BAND_OFFSET)
    } else {
        format!("B{band}")
    }
}

/// Render one component as `n<band><class>` (NR) / `B<band><class>` (E-UTRA).
pub(crate) fn render_component(band: i32, dl: Option<i32>, ul: Option<i32>) -> String {
    format!("{}{}", band_label(band), cc_class(dl, ul))
}

/// Band+class label for a component, e.g. `n78A` / `B1` — the same per-component
/// rendering the combo `bands` string uses. The combo identity key is built from these.
pub(crate) fn cc_component_label(cc: &Cc) -> String {
    render_component(cc.band, cc.bw_class_dl, cc.bw_class_ul)
}

/// Order-normalized identity key: sorted band+class labels joined with " + ".
pub(crate) fn combo_key(combo: &Combo) -> String {
    let mut parts: Vec<String> = combo.cc.iter().map(cc_component_label).collect();
    parts.sort();
    parts.join(" + ")
}

/// Bytes -> lowercase hex (e.g. `[0x0b] -> "0b"`); None passes through.
fn hex_bytes(b: Option<&[u8]>) -> Option<String> {
    b.map(|v| v.iter().map(|x| format!("{x:02x}")).collect())
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

/// kHz -> NR subcarrier-spacing code (inverse of `scs_khz`). Unknown -> None.
pub(crate) const fn scs_code(khz: u32) -> Option<i32> {
    match khz {
        15 => Some(1),
        30 => Some(2),
        60 => Some(3),
        120 => Some(4),
        240 => Some(5),
        _ => None,
    }
}

/// DL MIMO label -> code (inverse of `dl_mimo_label`). Unknown -> None.
pub(crate) fn dl_mimo_code(label: &str) -> Option<i32> {
    match label {
        NONE_MARK => Some(0),
        "2x2" => Some(1),
        "4x4" => Some(2),
        "8x8" => Some(3),
        _ => None,
    }
}

/// UL codebook-MIMO label -> code (inverse of `ul_mimo_cb_label`). Unknown -> None.
pub(crate) fn ul_mimo_cb_code(label: &str) -> Option<i32> {
    match label {
        NONE_MARK => Some(0),
        "No" => Some(1),
        "Yes" => Some(2),
        _ => None,
    }
}

/// Modulation-order label -> code (inverse of `mod_order_label`). Unknown -> None.
pub(crate) fn mod_order_code(label: &str) -> Option<i32> {
    match label {
        NONE_MARK => Some(0),
        "QAM64" => Some(1),
        "QAM256" => Some(2),
        _ => None,
    }
}

/// First per-CC id byte -> 0-based index into a feature-set list of length `len`.
/// Byte 0 / absent / out-of-range = no NR feature set; k>=1 is 1-based.
fn feature_index(ids: Option<&[u8]>, len: usize) -> Option<usize> {
    let k = *ids?.first()? as usize;
    (1..=len).contains(&k).then(|| k - 1)
}

/// One carrier component (`cc`) with its full fields. Optional proto fields are
/// omitted from TOML when absent.
#[derive(serde::Serialize, serde::Deserialize, Clone, Default, Debug)]
#[serde(default)]
pub(crate) struct Cc {
    pub(crate) band: i32,
    pub(crate) band_label: String,
    pub(crate) nr: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) bw_class_dl: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) bw_class_ul: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) dl_feature_index: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) ul_feature_index: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) dl_feature_per_cc_ids: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) ul_feature_per_cc_ids: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) srs_tx_switch: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) dl_scs_khz: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) dl_mimo: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) dl_max_bw_mhz: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) dl_mod_order: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) dl_bw90mhz: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) ul_scs_khz: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) ul_mimo_cb: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) ul_mimo_non_cb: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) ul_max_bw_mhz: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) ul_mod_order: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) ul_bw90mhz: Option<bool>,
}

/// One carrier-aggregation combo: its rendered band string, group/combo context,
/// and components. `cc` is declared last so the TOML is valid.
#[derive(serde::Serialize, serde::Deserialize, Clone, Default, Debug)]
#[serde(default)]
pub(crate) struct Combo {
    pub(crate) group: usize,
    pub(crate) index: usize,
    pub(crate) bands: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) power_class: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) bcs_nr: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) bcs_intra_endc: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) bcs_eutra: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) intra_band_en_dc_support: Option<i32>,
    pub(crate) bit_mask: u32,
    pub(crate) cc: Vec<Cc>,
}

/// Build the combo-grouped band-combination data from a decoded capability file.
pub(crate) fn build_combos(caps: &UeCaps) -> Vec<Combo> {
    let mut combo = Vec::new();
    for (gi, cg) in caps.combo_groups.iter().enumerate() {
        let h = cg.combo_header.as_ref();
        for (ci, c) in cg.combo.iter().enumerate() {
            let cc: Vec<Cc> =
                c.cc.iter()
                    .map(|x| {
                        let nr = x.band >= NR_BAND_OFFSET;
                        // Resolution is byte-based per spec: a per-CC id of 0 (or absent /
                        // out-of-range) means no NR feature set. E-UTRA components carry id 0
                        // in the data, so they resolve to None without an explicit `nr` gate.
                        let dl_fs = feature_index(
                            x.dl_feature_per_cc_ids.as_deref(),
                            caps.dl_feature_per_cc_list.len(),
                        )
                        .map(|i| &caps.dl_feature_per_cc_list[i]);
                        let ul_fs = feature_index(
                            x.ul_feature_per_cc_ids.as_deref(),
                            caps.ul_feature_per_cc_list.len(),
                        )
                        .map(|i| &caps.ul_feature_per_cc_list[i]);
                        Cc {
                            band: x.band,
                            band_label: band_label(x.band),
                            nr,
                            bw_class_dl: x.bw_class_dl,
                            bw_class_ul: x.bw_class_ul,
                            dl_feature_index: x.dl_feature_index,
                            ul_feature_index: x.ul_feature_index,
                            dl_feature_per_cc_ids: hex_bytes(x.dl_feature_per_cc_ids.as_deref()),
                            ul_feature_per_cc_ids: hex_bytes(x.ul_feature_per_cc_ids.as_deref()),
                            srs_tx_switch: x.srstxswitch,
                            dl_scs_khz: dl_fs.and_then(|f| f.max_scs).and_then(scs_khz),
                            dl_mimo: dl_fs.and_then(|f| f.max_mimo).map(dl_mimo_label),
                            dl_max_bw_mhz: dl_fs.and_then(|f| f.max_bw),
                            dl_mod_order: dl_fs.and_then(|f| f.max_mod_order).map(mod_order_label),
                            dl_bw90mhz: dl_fs.and_then(|f| f.bw_90mhz_supported),
                            ul_scs_khz: ul_fs.and_then(|f| f.max_scs).and_then(scs_khz),
                            ul_mimo_cb: ul_fs.and_then(|f| f.max_mimo_cb).map(ul_mimo_cb_label),
                            ul_mimo_non_cb: ul_fs.and_then(|f| f.max_mimo_non_cb),
                            ul_max_bw_mhz: ul_fs.and_then(|f| f.max_bw),
                            ul_mod_order: ul_fs.and_then(|f| f.max_mod_order).map(mod_order_label),
                            ul_bw90mhz: ul_fs.and_then(|f| f.bw_90mhz_supported),
                        }
                    })
                    .collect();
            let bands =
                c.cc.iter()
                    .map(|x| render_component(x.band, x.bw_class_dl, x.bw_class_ul))
                    .collect::<Vec<_>>()
                    .join(" + ");
            combo.push(Combo {
                group: gi + 1,
                index: ci + 1,
                bands,
                power_class: h.and_then(|x| x.power_class),
                bcs_nr: h.and_then(|x| x.bcs_nr),
                bcs_intra_endc: h.and_then(|x| x.bcs_intra_endc),
                bcs_eutra: h.and_then(|x| x.bcs_eutra),
                intra_band_en_dc_support: h.and_then(|x| x.intra_band_en_dc_support),
                bit_mask: c.bitmask.unwrap_or(0),
                cc,
            });
        }
    }
    combo
}

/// Render a component's decoded NR feature set for `--full`. No feature set
/// (E-UTRA component, or NR with id 0) yields a short marker. `srs:` is
/// appended only when present, preserving today's datum.
pub(crate) fn fmt_cc_features(cc: &Cc) -> String {
    // `*_max_bw_mhz` is the presence proxy for a resolved feature set: it is always
    // populated for a real NR feature set, so its absence means "no feature set here".
    let base = if cc.dl_max_bw_mhz.is_none() && cc.ul_max_bw_mhz.is_none() {
        if cc.nr {
            "(no NR feature set)".to_string()
        } else {
            "E-UTRA — no NR feature set".to_string()
        }
    } else {
        let mut parts: Vec<String> = Vec::new();
        if let Some(bw) = cc.dl_max_bw_mhz {
            parts.push(format!(
                "DL {}MHz {} {}",
                bw,
                cc.dl_mimo.as_deref().unwrap_or(NONE_MARK),
                cc.dl_mod_order.as_deref().unwrap_or(NONE_MARK),
            ));
        }
        if let Some(bw) = cc.ul_max_bw_mhz {
            let noncb = cc
                .ul_mimo_non_cb
                .map_or_else(|| NONE_MARK.to_string(), |n| n.to_string());
            parts.push(format!(
                "UL {}MHz cb:{} nonCb:{} {}",
                bw,
                cc.ul_mimo_cb.as_deref().unwrap_or(NONE_MARK),
                noncb,
                cc.ul_mod_order.as_deref().unwrap_or(NONE_MARK),
            ));
        }
        let mut tail = String::new();
        if let Some(scs) = cc.dl_scs_khz.or(cc.ul_scs_khz) {
            tail.push_str(&format!("SCS {scs}kHz"));
        }
        if cc.dl_bw90mhz.or(cc.ul_bw90mhz).unwrap_or(false) {
            if !tail.is_empty() {
                tail.push(' ');
            }
            tail.push_str("+90MHz");
        }
        if !tail.is_empty() {
            parts.push(tail);
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
            for x in &c.cc {
                println!("       {:<5} {}", x.band_label, fmt_cc_features(x));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cc_base(nr: bool) -> Cc {
        Cc {
            band: if nr { 10078 } else { 1 },
            band_label: if nr { "n78".into() } else { "B1".into() },
            nr,
            bw_class_dl: None,
            bw_class_ul: None,
            dl_feature_index: None,
            ul_feature_index: None,
            dl_feature_per_cc_ids: None,
            ul_feature_per_cc_ids: None,
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
        cc.bw_class_dl = Some(1);
        cc.bw_class_ul = Some(1);
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
        assert_eq!(
            fmt_cc_features(&cc),
            "DL 100MHz 4x4 QAM256 · UL 100MHz cb:Yes nonCb:1 QAM256 · SCS 30kHz +90MHz"
        );
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
    fn linkage_first_byte_one_based() {
        assert_eq!(feature_index(Some(&[1]), 18), Some(0));
        assert_eq!(feature_index(Some(&[17]), 18), Some(16));
        assert_eq!(feature_index(Some(&[0]), 18), None); // 0 = no NR feature
        assert_eq!(feature_index(Some(&[19]), 18), None); // out of range
        assert_eq!(feature_index(None, 18), None);
        assert_eq!(feature_index(Some(&[]), 18), None);
    }

    #[test]
    fn build_combos_resolves_features() {
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
                bw_90mhz_supported: Some(true),
                max_mimo_non_cb: Some(1),
            }],
            combo_groups: vec![ComboGroup {
                combo_header: None,
                combo: vec![combo_group::Nested2 {
                    bitmask: Some(0),
                    cc: vec![
                        combo_group::nested2::ComboFeatures {
                            band: 10078,
                            dl_feature_per_cc_ids: Some(vec![1]),
                            ul_feature_per_cc_ids: Some(vec![1]),
                            ..Default::default()
                        },
                        combo_group::nested2::ComboFeatures {
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
        let cc = &combos[0].cc;
        // n78 (NR) resolves to DL/UL feature-set entry 1
        assert_eq!(cc[0].dl_max_bw_mhz, Some(100));
        assert_eq!(cc[0].dl_mimo.as_deref(), Some("4x4"));
        assert_eq!(cc[0].dl_scs_khz, Some(30));
        assert_eq!(cc[0].dl_mod_order.as_deref(), Some("QAM256"));
        assert_eq!(cc[0].dl_bw90mhz, Some(true));
        assert_eq!(cc[0].ul_mimo_cb.as_deref(), Some("Yes"));
        assert_eq!(cc[0].ul_mimo_non_cb, Some(1));
        // B1 (E-UTRA, id 0) resolves to nothing
        assert_eq!(cc[1].dl_max_bw_mhz, None);
        assert_eq!(cc[1].dl_mimo, None);
        assert_eq!(cc[1].ul_mimo_cb, None);
    }

    #[test]
    fn inverse_maps_round_trip() {
        for code in 1..=5 {
            assert_eq!(scs_code(scs_khz(code).unwrap()), Some(code));
        }
        for code in 0..=3 {
            assert_eq!(dl_mimo_code(&dl_mimo_label(code)), Some(code));
        }
        for code in 0..=2 {
            assert_eq!(ul_mimo_cb_code(&ul_mimo_cb_label(code)), Some(code));
            assert_eq!(mod_order_code(&mod_order_label(code)), Some(code));
        }
    }

    #[test]
    fn inverse_maps_reject_unknown() {
        assert_eq!(scs_code(17), None);
        assert_eq!(dl_mimo_code("(7)"), None);
        assert_eq!(ul_mimo_cb_code("maybe"), None);
        assert_eq!(mod_order_code("QAM1024"), None);
    }

    #[test]
    fn cc_deserializes_partial_and_clones() {
        // Only `band` present; everything else falls back to Default via serde(default).
        let cc: Cc = toml::from_str("band = 10078\n").unwrap();
        assert_eq!(cc.band, 10078);
        assert_eq!(cc.dl_mimo, None);
        let _ = cc; // Clone derive must exist
    }
}
