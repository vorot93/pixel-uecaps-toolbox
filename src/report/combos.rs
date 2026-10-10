//! Human-readable views of typed capability values, plus shared wire helpers.
use super::{
    detail::Detail,
    identity::{CombinationKey, LocatedCombination, Selector, ShapeKey, shape},
};
use crate::{
    proto::{ShannonFeatureSetDlPerCcNr, ShannonFeatureSetUlPerCcNr},
    raw_nr::SubBlockKind,
};
use std::fmt::{Display, Write};

pub const NR_BAND_OFFSET: i32 = 10_000;
pub(crate) fn bw_letter(c: Option<i32>) -> String {
    match c {
        Some(n) if (1..=26).contains(&n) => ((b'A' + (n as u8 - 1)) as char).to_string(),
        Some(0) | None => String::new(),
        Some(n) => format!("({n})"),
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
fn optional<T: Display>(value: Option<T>) -> String {
    value.map_or_else(|| "absent".into(), |v| v.to_string())
}
pub(super) fn render_shape(key: &ShapeKey) -> String {
    key.iter()
        .map(|&(band, dl, ul)| render_component(band, dl, ul))
        .collect::<Vec<_>>()
        .join(" + ")
}
fn dl_record(r: &ShannonFeatureSetDlPerCcNr) -> String {
    format!(
        "max_scs={} max_mimo={} max_bw={} max_mod_order={} bw_90mhz_supported={}",
        optional(r.max_scs),
        optional(r.max_mimo),
        optional(r.max_bw),
        optional(r.max_mod_order),
        optional(r.bw_90mhz_supported)
    )
}
fn ul_record(r: &ShannonFeatureSetUlPerCcNr) -> String {
    format!(
        "max_scs={} max_mimo_cb={} max_bw={} max_mod_order={} bw_90mhz_supported={} max_mimo_non_cb={}",
        optional(r.max_scs),
        optional(r.max_mimo_cb),
        optional(r.max_bw),
        optional(r.max_mod_order),
        optional(r.bw_90mhz_supported),
        optional(r.max_mimo_non_cb)
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
                optional(h.power_class),
                optional(h.bcs_nr),
                optional(h.bcs_intra_endc),
                optional(h.bcs_eutra),
                optional(h.intra_band_en_dc_support)
            );
        }
    }
    let _ = writeln!(out, "      bitmask={}", optional(key.bitmask));
    for c in &key.components {
        let _ = writeln!(
            out,
            "      {} band={} dl_bw_class={} ul_bw_class={} dl_feature_index={} ul_feature_index={} srstxswitch={}",
            render_component(c.band, c.dl_bw_class, c.ul_bw_class),
            c.band,
            optional(c.dl_bw_class),
            optional(c.ul_bw_class),
            optional(c.dl_feature_index),
            optional(c.ul_feature_index),
            optional(c.srstxswitch)
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
}
