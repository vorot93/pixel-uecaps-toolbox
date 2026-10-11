//! Human-readable views of typed capability values.
use super::{
    detail::Detail,
    identity::{CombinationKey, LocatedCombination, Selector, ShapeKey, shape},
};
use crate::{
    formatting::Optional,
    proto::{ShannonFeatureSetDlPerCcNr, ShannonFeatureSetUlPerCcNr},
    raw_nr::{Direction, SubBlockKind, bw_class_letter},
};
use std::fmt::{self, Display, Formatter};

pub(super) struct BwLetter(Option<i32>);
impl Display for BwLetter {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        match self.0 {
            Some(0) | None => Ok(()),
            Some(n) => match u8::try_from(n).ok().and_then(bw_class_letter) {
                Some(letter) => write!(f, "{letter}"),
                None => write!(f, "({n})"),
            },
        }
    }
}
pub(super) fn bw_letter(c: Option<i32>) -> BwLetter {
    BwLetter(c)
}

struct ClassView {
    dl: Option<i32>,
    ul: Option<i32>,
}
impl Display for ClassView {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        let (dl, ul) = (self.dl.filter(|n| *n != 0), self.ul.filter(|n| *n != 0));
        let (d, u) = (BwLetter(dl), BwLetter(ul));
        match (dl, ul) {
            (None, None) => Ok(()),
            (Some(_), None) => write!(f, "{d}↓"),
            (None, Some(_)) => write!(f, "{u}↑"),
            _ if dl == ul => write!(f, "{d}"),
            _ => write!(f, "{d}/{u}"),
        }
    }
}
pub(super) struct ComponentView {
    kind: SubBlockKind,
    band: i32,
    class: ClassView,
}
impl Display for ComponentView {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(f, "{}{}", self.kind.band_label(self.band), self.class)
    }
}
pub(super) fn render_component(band: i32, dl: Option<i32>, ul: Option<i32>) -> ComponentView {
    let (kind, plain) = SubBlockKind::split_raw_band(band);
    render_known_component(kind, plain, dl, ul)
}
pub(super) fn render_known_component(
    kind: SubBlockKind,
    band: i32,
    dl: Option<i32>,
    ul: Option<i32>,
) -> ComponentView {
    ComponentView {
        kind,
        band,
        class: ClassView { dl, ul },
    }
}

pub(super) struct ShapeView<'a>(pub &'a ShapeKey);
impl Display for ShapeView<'_> {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        for (i, c) in self.0.iter().enumerate() {
            if i != 0 {
                f.write_str(" + ")?;
            }
            write!(f, "{}", render_component(c.band, c.dl, c.ul))?;
        }
        Ok(())
    }
}
struct DlRecord<'a>(&'a ShannonFeatureSetDlPerCcNr);
impl Display for DlRecord<'_> {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        let r = self.0;
        write!(
            f,
            "max_scs={} max_mimo={} max_bw={} max_mod_order={} bw_90mhz_supported={}",
            Optional(r.max_scs),
            Optional(r.max_mimo),
            Optional(r.max_bw),
            Optional(r.max_mod_order),
            Optional(r.bw_90mhz_supported)
        )
    }
}
struct UlRecord<'a>(&'a ShannonFeatureSetUlPerCcNr);
impl Display for UlRecord<'_> {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        let r = self.0;
        write!(
            f,
            "max_scs={} max_mimo_cb={} max_bw={} max_mod_order={} bw_90mhz_supported={} max_mimo_non_cb={}",
            Optional(r.max_scs),
            Optional(r.max_mimo_cb),
            Optional(r.max_bw),
            Optional(r.max_mod_order),
            Optional(r.bw_90mhz_supported),
            Optional(r.max_mimo_non_cb)
        )
    }
}
fn write_selector<'a, T, V: Display>(
    out: &mut Formatter<'_>,
    direction: Direction,
    selector: &'a Selector<T>,
    record: impl Fn(&'a T) -> V,
) -> fmt::Result {
    match selector {
        Selector::Resolved(records) => {
            for (i, r) in records.iter().enumerate() {
                writeln!(out, "        {direction}[{}] {}", i + 1, record(r))?;
            }
        }
        Selector::Absent => {
            writeln!(out, "        {direction}: absent")?;
        }
        Selector::Empty => {
            writeln!(out, "        {direction}: present empty selector")?;
        }
        Selector::Placeholder(n) => {
            writeln!(out, "        {direction}: zero placeholder ({n} bytes)")?;
        }
        Selector::Eutra(raw) => {
            writeln!(out, "        {direction}: E-UTRA raw selector {raw:?}")?;
        }
    }
    Ok(())
}
pub(super) struct KeyView<'a>(pub &'a CombinationKey);
impl Display for KeyView<'_> {
    fn fmt(&self, out: &mut Formatter<'_>) -> fmt::Result {
        let key = self.0;
        match key.header {
            None => out.write_str("      header=absent\n")?,
            Some(h) => {
                writeln!(
                    out,
                    "      header=present power_class={} bcs_nr={} bcs_intra_endc={} bcs_eutra={} intra_band_en_dc_support={}",
                    Optional(h.power_class),
                    Optional(h.bcs_nr),
                    Optional(h.bcs_intra_endc),
                    Optional(h.bcs_eutra),
                    Optional(h.intra_band_en_dc_support)
                )?;
            }
        }
        writeln!(out, "      bitmask={}", Optional(key.bitmask))?;
        for c in &key.components {
            writeln!(
                out,
                "      {} band={} dl_bw_class={} ul_bw_class={} dl_feature_index={} ul_feature_index={} srstxswitch={}",
                render_component(c.band, c.dl_bw_class, c.ul_bw_class),
                c.band,
                Optional(c.dl_bw_class),
                Optional(c.ul_bw_class),
                Optional(c.dl_feature_index),
                Optional(c.ul_feature_index),
                Optional(c.srstxswitch)
            )?;
            write_selector(out, Direction::Dl, &c.dl, DlRecord)?;
            write_selector(out, Direction::Ul, &c.ul, UlRecord)?;
        }
        Ok(())
    }
}
pub(super) struct CombosView<'a> {
    pub combos: &'a [LocatedCombination],
    pub detail: Detail,
}
impl Display for CombosView<'_> {
    fn fmt(&self, out: &mut Formatter<'_>) -> fmt::Result {
        writeln!(out, "Band combinations ({})", self.combos.len())?;
        for c in self.combos {
            writeln!(
                out,
                "  g{}.{} {}",
                c.group,
                c.index,
                ShapeView(&shape(&c.key))
            )?;
            if self.detail.is_full() {
                write!(out, "{}", KeyView(&c.key))?;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn display_views_propagate_sink_errors() {
        struct Closed;
        impl fmt::Write for Closed {
            fn write_str(&mut self, _: &str) -> fmt::Result {
                Err(fmt::Error)
            }
        }
        use fmt::Write;
        let key = CombinationKey {
            header: None,
            bitmask: None,
            components: vec![],
        };
        let shape = vec![super::super::identity::ShapeComponent {
            band: 10078,
            dl: Some(1),
            ul: None,
        }];
        assert!(write!(Closed, "{}", ShapeView(&shape)).is_err());
        assert!(write!(Closed, "{}", KeyView(&key)).is_err());
        assert!(
            write!(
                Closed,
                "{}",
                CombosView {
                    combos: &[],
                    detail: Detail::Full
                }
            )
            .is_err()
        );
        assert!(
            write!(
                Closed,
                "{}",
                DlRecord(&ShannonFeatureSetDlPerCcNr::default())
            )
            .is_err()
        );
        assert!(
            write!(
                Closed,
                "{}",
                UlRecord(&ShannonFeatureSetUlPerCcNr::default())
            )
            .is_err()
        );
    }
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
            assert_eq!(bw_letter(Some(class)).to_string(), label);
        }
        assert_eq!(
            ClassView {
                dl: Some(1),
                ul: Some(2)
            }
            .to_string(),
            "A/B"
        );
        assert_eq!(
            ClassView {
                dl: Some(1),
                ul: None
            }
            .to_string(),
            "A↓"
        );
        assert_eq!(
            ClassView {
                dl: None,
                ul: Some(1)
            }
            .to_string(),
            "A↑"
        );
        assert_eq!(
            render_component(10078, Some(1), Some(1)).to_string(),
            "n78A"
        );
        assert_eq!(
            render_known_component(SubBlockKind::Lte, 10078, None, None).to_string(),
            "B10078"
        );
    }
}
