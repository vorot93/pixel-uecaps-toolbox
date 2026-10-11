//! Human-readable views of typed capability values.
use super::{
    detail::Detail,
    identity::{CombinationKey, LocatedCombination, Selector, ShapeKey, shape},
};
use crate::{
    formatting::Optional,
    proto::{ShannonFeatureSetDlPerCcNr, ShannonFeatureSetUlPerCcNr},
    raw_nr::{SubBlockKind, bw_class_letter},
};
use std::fmt::Write;

pub(crate) fn bw_letter(c: Option<i32>) -> String {
    match c {
        Some(0) | None => String::new(),
        Some(n) => u8::try_from(n)
            .ok()
            .and_then(bw_class_letter)
            .map_or_else(|| format!("({n})"), |letter| letter.to_string()),
    }
}
fn cc_class(dl: Option<i32>, ul: Option<i32>) -> String {
    let (d, u) = (bw_letter(dl), bw_letter(ul));
    match (d.is_empty(), u.is_empty()) {
        (true, true) => String::new(),
        (false, true) => format!("{d}↓"),
        (true, false) => format!("{u}↑"),
        (false, false) if d == u => d,
        _ => format!("{d}/{u}"),
    }
}
pub(crate) fn render_component(band: i32, dl: Option<i32>, ul: Option<i32>) -> String {
    let (kind, plain) = SubBlockKind::split_raw_band(band);
    render_known_component(kind, plain, dl, ul)
}
pub(crate) fn render_known_component(
    kind: SubBlockKind,
    band: i32,
    dl: Option<i32>,
    ul: Option<i32>,
) -> String {
    format!("{}{}", kind.band_label(band), cc_class(dl, ul))
}

pub(super) fn render_shape(key: &ShapeKey) -> String {
    key.iter()
        .map(|component| render_component(component.band, component.dl, component.ul))
        .collect::<Vec<_>>()
        .join(" + ")
}
fn dl_record(r: &ShannonFeatureSetDlPerCcNr) -> String {
    format!(
        "max_scs={} max_mimo={} max_bw={} max_mod_order={} bw_90mhz_supported={}",
        Optional(r.max_scs),
        Optional(r.max_mimo),
        Optional(r.max_bw),
        Optional(r.max_mod_order),
        Optional(r.bw_90mhz_supported)
    )
}
fn ul_record(r: &ShannonFeatureSetUlPerCcNr) -> String {
    format!(
        "max_scs={} max_mimo_cb={} max_bw={} max_mod_order={} bw_90mhz_supported={} max_mimo_non_cb={}",
        Optional(r.max_scs),
        Optional(r.max_mimo_cb),
        Optional(r.max_bw),
        Optional(r.max_mod_order),
        Optional(r.bw_90mhz_supported),
        Optional(r.max_mimo_non_cb)
    )
}
fn render_selector<T>(
    out: &mut String,
    direction: &str,
    selector: &Selector<T>,
    record: impl Fn(&T) -> String,
) {
    match selector {
        Selector::Resolved(records) => {
            for (i, r) in records.iter().enumerate() {
                let _ = writeln!(out, "        {direction}[{}] {}", i + 1, record(r));
            }
        }
        Selector::Absent => {
            let _ = writeln!(out, "        {direction}: absent");
        }
        Selector::Empty => {
            let _ = writeln!(out, "        {direction}: present empty selector");
        }
        Selector::Placeholder(n) => {
            let _ = writeln!(out, "        {direction}: zero placeholder ({n} bytes)");
        }
        Selector::Eutra(raw) => {
            let _ = writeln!(out, "        {direction}: E-UTRA raw selector {raw:?}");
        }
    }
}
pub(super) fn render_key(key: &CombinationKey) -> String {
    let mut out = String::new();
    match key.header {
        None => out.push_str("      header=absent\n"),
        Some(h) => {
            let _ = writeln!(
                out,
                "      header=present power_class={} bcs_nr={} bcs_intra_endc={} bcs_eutra={} intra_band_en_dc_support={}",
                Optional(h.power_class),
                Optional(h.bcs_nr),
                Optional(h.bcs_intra_endc),
                Optional(h.bcs_eutra),
                Optional(h.intra_band_en_dc_support)
            );
        }
    }
    let _ = writeln!(out, "      bitmask={}", Optional(key.bitmask));
    for c in &key.components {
        let _ = writeln!(
            out,
            "      {} band={} dl_bw_class={} ul_bw_class={} dl_feature_index={} ul_feature_index={} srstxswitch={}",
            render_component(c.band, c.dl_bw_class, c.ul_bw_class),
            c.band,
            Optional(c.dl_bw_class),
            Optional(c.ul_bw_class),
            Optional(c.dl_feature_index),
            Optional(c.ul_feature_index),
            Optional(c.srstxswitch)
        );
        render_selector(&mut out, "DL", &c.dl, dl_record);
        render_selector(&mut out, "UL", &c.ul, ul_record);
    }
    out
}
pub(super) fn render_combos(combos: &[LocatedCombination], detail: Detail) -> String {
    let mut out = format!("Band combinations ({})\n", combos.len());
    for c in combos {
        let _ = writeln!(
            out,
            "  g{}.{} {}",
            c.group,
            c.index,
            render_shape(&shape(&c.key))
        );
        if detail.is_full() {
            out.push_str(&render_key(&c.key));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn bandwidth_classes_and_kind_known_bands() {
        for (class, label) in [
            (1, "A"),
            (3, "C"),
            (6, "F"),
            (7, "G"),
            (9, "I"),
            (13, "M"),
            (27, "(27)"),
        ] {
            assert_eq!(bw_letter(Some(class)), label);
        }
        assert_eq!(cc_class(Some(1), Some(2)), "A/B");
        assert_eq!(cc_class(Some(1), None), "A↓");
        assert_eq!(cc_class(None, Some(1)), "A↑");
        assert_eq!(render_component(10078, Some(1), Some(1)), "n78A");
        assert_eq!(
            render_known_component(SubBlockKind::Lte, 10078, None, None),
            "B10078"
        );
    }
}
