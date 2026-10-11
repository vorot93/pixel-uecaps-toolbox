//! LTE-only fallback (`lte_*.binarypb`) decoding + rendering for `inspect`.

use super::{
    combos::{bw_letter, render_known_component},
    detail::Detail,
};
use crate::{
    formatting::Optional,
    proto::{LteCaps, LteCombo, LteComponent},
    raw_nr::{BandLabel, SubBlockKind},
};
use std::fmt::{self, Display, Formatter};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LteClass {
    Disabled,
    Known { class: i32, mimo: i32 },
    Unknown { raw: i32, mimo: i32 },
}

/// Preserve disabled directions and unknown bitfields separately from known class/MIMO pairs.
const fn lte_class(value: i32) -> LteClass {
    if value == 0 {
        return LteClass::Disabled;
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
    match idx {
        Some(class) => LteClass::Known { class, mimo },
        None => LteClass::Unknown { raw: value, mimo },
    }
}

/// This module is statically single-kind, so every band it prints is an E-UTRA band — including
/// one at or above `NR_BAND_OFFSET`, which is an out-of-range E-UTRA band rather than an NR one.
/// Going through the kind-known method keeps that true everywhere here; the raw-band
/// `render_component` helper would render such a band as NR.
fn eutra(band: i32) -> BandLabel {
    SubBlockKind::Lte.band_label(band)
}

impl LteClass {
    fn class(self) -> Option<i32> {
        match self {
            Self::Known { class, .. } => Some(class),
            _ => None,
        }
    }
}

/// One component as the house-style band+class label, e.g. `B1A↓`, `B5A`, `B1A/C`.
/// An out-of-table DL value renders defensively as `B{band}[{value}]`.
fn component_label(c: &LteComponent) -> String {
    let dl = lte_class(c.dl_bw_class_mimo);
    let ul = lte_class(c.ul_bw_class_mimo.unwrap_or(0));
    if let LteClass::Unknown { raw, .. } = dl {
        return format!("{}[{raw}]", eutra(c.band));
    }
    if let LteClass::Unknown { raw, .. } = ul {
        return format!("{}{}/[{raw}]", eutra(c.band), bw_letter(dl.class()));
    }
    // The kind is statically E-UTRA here, so the label comes from `SubBlockKind::Lte`, not from
    // the raw-band `render_component`, which *infers* the kind from the raw band. That
    // inference reads any band >= NR_BAND_OFFSET as NR, so a crafted `lte_*.binarypb` carrying
    // band 10078 rendered as `n78A↓` on the combo line while the `--full` per-CC line below
    // printed `B10078` — two contradictory labels for one component in one report. This module
    // exists to render foreign files defensively, so that is exactly its case.
    render_known_component(SubBlockKind::Lte, c.band, dl.class(), ul.class()).to_string()
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

fn direction_detail(value: i32) -> String {
    match lte_class(value) {
        LteClass::Disabled => "—".into(),
        LteClass::Unknown { raw, .. } => format!("[{raw}]"),
        LteClass::Known { class, mimo } => format!("{} {mimo}x{mimo}", bw_letter(Some(class))),
    }
}

/// `--full` per-CC detail; a missing UL field is distinct from explicit disabled UL.
fn cc_detail(c: &LteComponent) -> String {
    let dl = direction_detail(c.dl_bw_class_mimo);
    let ul = c
        .ul_bw_class_mimo
        .map_or_else(|| "absent".into(), direction_detail);
    format!("DL {dl}  UL {ul}")
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
pub(super) fn render_lte_combos(caps: &LteCaps, detail: Detail) -> LteCombosView<'_> {
    LteCombosView { caps, detail }
}

pub(super) struct LteCombosView<'a> {
    caps: &'a LteCaps,
    detail: Detail,
}
impl Display for LteCombosView<'_> {
    fn fmt(&self, out: &mut Formatter<'_>) -> fmt::Result {
        writeln!(out, "LTE band combinations ({})", self.caps.combos.len())?;
        for (i, combo) in self.caps.combos.iter().enumerate() {
            writeln!(out, "  {:<6} {}", format!("g{}", i + 1), combo_bands(combo))?;
            if self.detail.is_full() {
                for c in &combo.components {
                    writeln!(
                        out,
                        "       {:<5} {}  dl_bw_class_mimo={} ul_bw_class_mimo={}",
                        eutra(c.band),
                        cc_detail(c),
                        c.dl_bw_class_mimo,
                        Optional(c.ul_bw_class_mimo)
                    )?;
                }
                writeln!(
                    out,
                    "       bcs={} unknown1={} unknown2={}",
                    Optional(combo.bcs),
                    Optional(combo.unknown1),
                    Optional(combo.unknown2)
                )?;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proto::LteCombo;

    fn comp(band: i32, dl: i32, ul: i32) -> LteComponent {
        LteComponent {
            band,
            dl_bw_class_mimo: dl,
            ul_bw_class_mimo: Some(ul),
        }
    }

    #[test]
    fn config_block_identified_unidentified_and_unknown() {
        assert_eq!(
            config_block(2_160_127_815),
            vec![
                "LTE config : sub6 — Pixel 9 / 9 Pro / 9 Pro XL, sub-6 (RoW)".to_string(),
                "             modem-selected by hardware category 0x112/0x122/0x142 (Shannon g5400), not SIM/MCC".to_string(),
            ]
        );
        assert_eq!(
            config_block(844_857_560),
            vec![
                "LTE config : sta5_na — Pixel 10a (US)".to_string(),
                "             modem-selected by hardware category 0x812 (Shannon g5400), not SIM/MCC".to_string(),
            ]
        );
        assert_eq!(
            config_block(1_534_561_764),
            vec![
                "LTE config : sta5_jp — Pixel 10a (Japan, inferred)".to_string(),
                "             modem-selected by hardware category 0x814 (Shannon g5400), not SIM/MCC".to_string(),
            ]
        );
        assert_eq!(
            config_block(564_260_317),
            vec![
                "LTE config : tki3".to_string(),
                "             modem-selected by hardware category 0x211 (Shannon g5400), not SIM/MCC".to_string(),
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
        assert_eq!(lte_class(32768), LteClass::Known { class: 1, mimo: 2 });
        assert_eq!(lte_class(32769), LteClass::Known { class: 1, mimo: 4 });
        assert_eq!(lte_class(16384), LteClass::Known { class: 2, mimo: 2 });
        assert_eq!(lte_class(1024), LteClass::Known { class: 6, mimo: 2 });
        assert_eq!(lte_class(0), LteClass::Disabled);
        assert_eq!(lte_class(999), LteClass::Unknown { raw: 999, mimo: 4 });
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

    /// An `lte_*.binarypb` is statically single-kind, so a band at or above `NR_BAND_OFFSET` is
    /// an out-of-range E-UTRA band — not an NR band. Routing it through the kind-*inferring*
    /// `band_label` rendered it `n78A↓` on the combo line while the `--full` per-CC line below
    /// printed `B10078` for the same component: two contradictory labels in one report. This
    /// module renders crafted/foreign files defensively, so the case is reachable.
    #[test]
    fn an_out_of_range_eutra_band_still_renders_as_eutra() {
        let caps = LteCaps {
            combos: vec![LteCombo {
                components: vec![comp(10_001, 32768, 0)],
                ..Default::default()
            }],
            ..Default::default()
        };
        let text = render_lte_combos(&caps, Detail::Full).to_string();
        assert!(text.contains("B10001A↓"), "{text}");
        assert!(text.contains("B10001 DL A 2x2  UL —"), "{text}");
        assert!(
            !text.split_whitespace().any(|token| token.starts_with("n1")),
            "{text}"
        );
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
    fn cc_detail_dl_disabled_renders_dash() {
        // A DL-disabled component (dl_bw_class_mimo == 0) must render "DL —",
        // mirroring the UL twin — not "DL  0x0" from an unguarded `0x0` MIMO format.
        assert_eq!(cc_detail(&comp(1, 0, 32768)), "DL —  UL A 2x2"); // DL off, UL A
        assert_eq!(cc_detail(&comp(1, 32768, 0)), "DL A 2x2  UL —"); // UL off, DL A 2x2
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
                    dl_bw_class_mimo: 32768,
                    ul_bw_class_mimo: Some(0),
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
