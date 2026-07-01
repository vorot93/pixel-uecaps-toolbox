//! LTE-only fallback (`lte_*.binarypb`) decoding + rendering for `inspect`.

use super::combos::{bw_letter, render_component};
use crate::proto::{LteCaps, LteCombo, LteComponent};

/// A Shannon class/MIMO value -> (carrier class index `A=1..F=6`, MIMO branches 2/4).
/// `value == 0` -> `(None, 0)` (that direction disabled). An out-of-table nonzero base
/// yields `(None, mimo)`; callers render that defensively as `[value]`.
const fn lte_class(value: i32) -> (Option<i32>, i32) {
    if value == 0 {
        return (None, 0);
    }
    let idx = match value & !1 {
        32768 => Some(1),
        16384 => Some(2),
        8192 => Some(3),
        4096 => Some(4),
        2048 => Some(5),
        1024 => Some(6),
        _ => None,
    };
    let mimo = if value & 1 != 0 { 4 } else { 2 };
    (idx, mimo)
}

/// True when a nonzero class value has no entry in the bandwidth-class table —
/// it fell outside the known set and should render defensively.
const fn out_of_table(value: i32, idx: Option<i32>) -> bool {
    value != 0 && idx.is_none()
}

/// One component as the house-style band+class label, e.g. `B1A↓`, `B5A`, `B1A/C`.
/// An out-of-table DL value renders defensively as `B{band}[{value}]`.
fn component_label(c: &LteComponent) -> String {
    let ul = c.bw_class_mimo_ul.unwrap_or(0);
    let (dl_idx, _) = lte_class(c.bw_class_mimo_dl);
    if out_of_table(c.bw_class_mimo_dl, dl_idx) {
        return format!("B{}[{}]", c.band, c.bw_class_mimo_dl);
    }
    let (ul_idx, _) = lte_class(ul);
    if out_of_table(ul, ul_idx) {
        return format!("B{}{}/[{}]", c.band, bw_letter(dl_idx), ul);
    }
    render_component(c.band, dl_idx, ul_idx)
}

/// Order-normalized LTE combo identity: the sorted `component_label`s joined ` + `.
/// Mirrors the carrier `combos::combo_key`.
pub(crate) fn lte_combo_key(combo: &LteCombo) -> String {
    let mut parts: Vec<String> = combo.components.iter().map(component_label).collect();
    parts.sort();
    parts.join(" + ")
}

/// A combo's components joined ` + `, e.g. `B1A↓ + B5A`.
fn combo_bands(combo: &LteCombo) -> String {
    combo
        .components
        .iter()
        .map(component_label)
        .collect::<Vec<_>>()
        .join(" + ")
}

/// `--full` per-CC detail, e.g. `DL A 2x2  UL —`.
pub(crate) fn cc_detail(c: &LteComponent) -> String {
    let (dl_idx, dl_mimo) = lte_class(c.bw_class_mimo_dl);
    let dl = if out_of_table(c.bw_class_mimo_dl, dl_idx) {
        format!("[{}]", c.bw_class_mimo_dl)
    } else {
        format!("{} {dl_mimo}x{dl_mimo}", bw_letter(dl_idx))
    };
    let ul = c.bw_class_mimo_ul.unwrap_or(0);
    let (ul_idx, _) = lte_class(ul);
    let ul = if ul == 0 {
        "—".to_string()
    } else if ul_idx.is_none() {
        format!("[{ul}]")
    } else {
        bw_letter(ul_idx)
    };
    format!("DL {dl}  UL {ul}")
}

/// One LTE CA component, for `--toml`.
#[derive(serde::Serialize)]
pub(crate) struct LteComponentToml {
    band: i32,
    /// Bandwidth class letter (`A`–`F`); `""` when that direction is disabled.
    dl_class: String,
    dl_mimo: i32,
    ul_class: String,
    ul_mimo: i32,
}

/// One LTE CA combination, for `--toml`.
#[derive(serde::Serialize)]
pub(crate) struct LteComboToml {
    bands: String,
    bcs: u64,
    unknown1: u64,
    unknown2: u64,
    components: Vec<LteComponentToml>,
}

/// Structured combos for `inspect --toml`.
pub(crate) fn lte_combos_toml(caps: &LteCaps) -> Vec<LteComboToml> {
    caps.combos
        .iter()
        .map(|combo| LteComboToml {
            bands: combo_bands(combo),
            bcs: combo.bcs.unwrap_or(0),
            unknown1: combo.unknown1.unwrap_or(0),
            unknown2: combo.unknown2.unwrap_or(0),
            components: combo
                .components
                .iter()
                .map(|c| {
                    let ul = c.bw_class_mimo_ul.unwrap_or(0);
                    let (dl_idx, dl_mimo) = lte_class(c.bw_class_mimo_dl);
                    let (ul_idx, ul_mimo) = lte_class(ul);
                    LteComponentToml {
                        band: c.band,
                        dl_class: if out_of_table(c.bw_class_mimo_dl, dl_idx) {
                            format!("[{}]", c.bw_class_mimo_dl)
                        } else {
                            bw_letter(dl_idx)
                        },
                        dl_mimo,
                        ul_class: if ul == 0 {
                            String::new()
                        } else if ul_idx.is_none() {
                            format!("[{ul}]")
                        } else {
                            bw_letter(ul_idx)
                        },
                        ul_mimo,
                    }
                })
                .collect(),
        })
        .collect()
}

/// The `LTE config : …` block for an `lte_<id>` filename id, as printable lines.
pub(crate) fn config_block(id: u64) -> Vec<String> {
    match crate::model::lte_config(id) {
        None => {
            vec!["LTE config : unrecognised id (not in the known modem selection table)".into()]
        }
        Some(c) => {
            let head = match c.model {
                Some(m) => format!("LTE config : {} — {}", c.family, m),
                None => format!("LTE config : {}", c.family),
            };
            let codes = c
                .category_codes
                .iter()
                .map(|x| format!("0x{x:X}"))
                .collect::<Vec<_>>()
                .join("/");
            vec![
                head,
                format!(
                    "             modem-selected by hardware category {codes} (Shannon g5400), not SIM/MCC"
                ),
            ]
        }
    }
}

/// Print every LTE combo in the carrier house style (no trimming).
pub(crate) fn print_lte_combos(caps: &LteCaps, full: bool) {
    println!("LTE band combinations ({})", caps.combos.len());
    for (i, combo) in caps.combos.iter().enumerate() {
        println!("  {:<6} {}", format!("g{}", i + 1), combo_bands(combo));
        if full {
            for c in &combo.components {
                println!("       {:<5} {}", format!("B{}", c.band), cc_detail(c));
            }
            println!("       bcs {}", combo.bcs.unwrap_or(0));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proto::LteCombo;

    fn comp(band: i32, dl: i32, ul: i32) -> LteComponent {
        LteComponent {
            band,
            bw_class_mimo_dl: dl,
            bw_class_mimo_ul: Some(ul),
        }
    }

    #[test]
    fn lte_combo_key_is_order_normalized() {
        let a = LteCombo {
            components: vec![comp(5, 32768, 32768), comp(1, 32768, 0)],
            bcs: Some(0),
            unknown1: Some(0),
            unknown2: Some(0),
        };
        assert_eq!(lte_combo_key(&a), "B1A↓ + B5A"); // sorted: B1A↓ before B5A
    }

    #[test]
    fn config_block_three_cases() {
        assert_eq!(
            config_block(2_160_127_815),
            vec![
                "LTE config : sub6 — Pixel 9 / 9 Pro / 9 Pro XL, sub-6 (RoW)".to_string(),
                "             modem-selected by hardware category 0x112/0x122/0x142 (Shannon g5400), not SIM/MCC".to_string(),
            ]
        );
        assert_eq!(
            config_block(1_534_561_764),
            vec![
                "LTE config : sta5_jp".to_string(),
                "             modem-selected by hardware category 0x814 (Shannon g5400), not SIM/MCC".to_string(),
            ]
        );
        assert_eq!(
            config_block(123),
            vec![
                "LTE config : unrecognised id (not in the known modem selection table)".to_string()
            ]
        );
    }

    #[test]
    fn lte_class_decodes_class_and_mimo() {
        assert_eq!(lte_class(32768), (Some(1), 2)); // A 2x2
        assert_eq!(lte_class(32769), (Some(1), 4)); // A 4x4
        assert_eq!(lte_class(16384), (Some(2), 2)); // B
        assert_eq!(lte_class(1024), (Some(6), 2)); // F
        assert_eq!(lte_class(0), (None, 0)); // disabled
        assert_eq!(lte_class(999), (None, 4)); // out-of-table base; 999 is odd -> 4x4
    }

    #[test]
    fn combo_bands_house_style() {
        let combo = LteCombo {
            components: vec![comp(1, 32768, 0), comp(5, 32768, 32768)],
            bcs: Some(0),
            unknown1: Some(0),
            unknown2: Some(0),
        };
        assert_eq!(combo_bands(&combo), "B1A↓ + B5A");
    }

    #[test]
    fn combo_bands_asymmetric_and_defensive() {
        let asym = LteCombo {
            components: vec![comp(1, 32768, 8192)], // DL A / UL C
            bcs: Some(0),
            unknown1: Some(0),
            unknown2: Some(0),
        };
        assert_eq!(combo_bands(&asym), "B1A/C");
        let unknown = LteCombo {
            components: vec![comp(7, 999, 0)], // out-of-table DL base
            bcs: Some(0),
            unknown1: Some(0),
            unknown2: Some(0),
        };
        assert_eq!(combo_bands(&unknown), "B7[999]");
        let ul_unknown = LteCombo {
            components: vec![comp(1, 32768, 999)], // DL A / UL out-of-table
            bcs: Some(0),
            unknown1: Some(0),
            unknown2: Some(0),
        };
        assert_eq!(combo_bands(&ul_unknown), "B1A/[999]");
    }

    #[test]
    fn lte_combos_toml_carries_bands_and_components() {
        let caps = LteCaps {
            fingerprint: 0,
            combos: vec![LteCombo {
                components: vec![comp(1, 32768, 0), comp(5, 32768, 32768)],
                bcs: Some(7),
                unknown1: Some(8),
                unknown2: Some(9),
            }],
            bitmask: 0,
        };
        let toml = lte_combos_toml(&caps);
        assert_eq!(toml.len(), 1);
        assert_eq!(toml[0].bands, "B1A↓ + B5A");
        assert_eq!(toml[0].bcs, 7);
        assert_eq!(toml[0].components.len(), 2);
        assert_eq!(toml[0].components[0].band, 1);
        assert_eq!(toml[0].components[0].dl_class, "A");
        assert_eq!(toml[0].components[0].dl_mimo, 2);
        assert_eq!(toml[0].components[0].ul_class, ""); // UL disabled
        assert_eq!(toml[0].components[1].ul_class, "A");
        assert_eq!(toml[0].unknown1, 8);
        assert_eq!(toml[0].unknown2, 9);
        assert_eq!(toml[0].components[0].ul_mimo, 0); // UL disabled -> 0
        assert_eq!(toml[0].components[1].ul_mimo, 2); // UL A -> 2x2
    }

    #[test]
    fn lte_caps_roundtrips_u64_unknown1() {
        use prost::Message;
        let caps = LteCaps {
            fingerprint: 874_888_686,
            combos: vec![LteCombo {
                components: vec![comp(1, 32768, 0)],
                bcs: Some(2_147_483_648),
                unknown1: Some(15_060_300_583_598_546_945), // > u32::MAX
                unknown2: Some(722_712),
            }],
            bitmask: 754_389_470,
        };
        let bytes = caps.encode_to_vec();
        assert_eq!(LteCaps::decode(&bytes[..]).unwrap(), caps);
    }

    #[test]
    fn optional_zero_fields_survive_round_trip() {
        use prost::Message;
        // explicit Some(0) on the now-optional fields must survive decode (plain proto3 would
        // collapse them to absent and a re-encode would drop 4KB of explicit ul=0s).
        let caps = LteCaps {
            fingerprint: 874_888_686,
            combos: vec![LteCombo {
                components: vec![LteComponent {
                    band: 1,
                    bw_class_mimo_dl: 32768,
                    bw_class_mimo_ul: Some(0),
                }],
                bcs: Some(0),
                unknown1: Some(0),
                unknown2: Some(0),
            }],
            bitmask: 754_389_470,
        };
        let bytes = caps.encode_to_vec();
        let back = LteCaps::decode(&bytes[..]).unwrap();
        assert_eq!(back, caps); // Some(0) preserved, not collapsed to None
        assert_eq!(back.encode_to_vec(), bytes); // re-encode is byte-stable
    }
}
