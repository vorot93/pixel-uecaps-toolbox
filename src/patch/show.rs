//! `patch show` — render a combo patch (TOML) in human-readable form.

use super::format::{self, LtePatch, NrPatch, Patch, lte_set_entry_key, set_entry_key};
use crate::{
    proto::LteComponent,
    report::{combos::fmt_cc_features, lte::cc_detail},
};
use std::{fmt::Write as _, path::Path};

/// `patch show [FILE]`: read a patch (FILE or stdin) and print it. `full` adds per-component caps.
pub fn show(input: Option<&Path>, full: bool) -> anyhow::Result<i32> {
    let text = super::read_patch_source(input)?;
    let patch = format::from_toml(&text)?;
    print!("{}", render(&patch, full));
    Ok(0)
}

/// Render a patch to a human-readable string (pure; unit-tested directly).
fn render(patch: &Patch, full: bool) -> String {
    let mut out = String::new();
    match patch {
        Patch::Nr(p) => render_nr(p, full, &mut out),
        Patch::Lte(p) => render_lte(p, full, &mut out),
    }
    out
}

/// `+` add, `~` change, `·` when the entry has no kind.
fn marker(kind: Option<&str>) -> char {
    match kind {
        Some("add") => '+',
        Some("change") => '~',
        _ => '·',
    }
}

/// Count `set` entries by kind for the header breakdown.
fn count_kinds<'a>(kinds: impl Iterator<Item = Option<&'a str>>) -> (usize, usize) {
    let (mut add, mut change) = (0, 0);
    for k in kinds {
        match k {
            Some("add") => add += 1,
            Some("change") => change += 1,
            _ => {}
        }
    }
    (add, change)
}

/// Header line + `delete` count + (when non-empty) the `deletes` section; then the `sets` heading
/// when `set_len > 0`. Shared by both patch kinds.
fn header_and_deletes(
    out: &mut String,
    kind: &str,
    version: u32,
    delete: &[String],
    set_len: usize,
    add: usize,
    change: usize,
) {
    let _ = writeln!(out, "Combo patch ({kind}) · format v{version}");
    let _ = writeln!(
        out,
        "  delete {} · set {set_len} ({add} add, {change} change)",
        delete.len()
    );
    if !delete.is_empty() {
        let _ = writeln!(out, "\ndeletes");
        for k in delete {
            let _ = writeln!(out, "  {k}");
        }
    }
    if set_len > 0 {
        let _ = writeln!(out, "\nsets");
    }
}

/// One set entry's summary line: `  <marker> <key>  (<kind>)`.
fn set_summary(out: &mut String, kind: Option<&str>, key: &str) {
    let _ = writeln!(
        out,
        "  {} {key}  ({})",
        marker(kind),
        kind.unwrap_or("none")
    );
}

fn render_nr(p: &NrPatch, full: bool, out: &mut String) {
    let (add, change) = count_kinds(p.set.iter().map(|e| e.kind.as_deref()));
    header_and_deletes(out, "nr", p.version, &p.delete, p.set.len(), add, change);
    for e in &p.set {
        let key = set_entry_key(e).expect("validated nr set entry");
        set_summary(out, e.kind.as_deref(), &key);
        if full {
            for combo in &e.combo {
                for cc in &combo.cc {
                    let raw = cc.to_cc().expect("validated nr component");
                    let label = cc.band_label();
                    let _ = writeln!(out, "       {label:<5} {}", fmt_cc_features(&raw));
                }
            }
        }
    }
}

fn render_lte(p: &LtePatch, full: bool, out: &mut String) {
    let (add, change) = count_kinds(p.set.iter().map(|e| e.kind.as_deref()));
    header_and_deletes(out, "lte", p.version, &p.delete, p.set.len(), add, change);
    for e in &p.set {
        let key = lte_set_entry_key(e).expect("validated lte set entry");
        set_summary(out, e.kind.as_deref(), &key);
        if full {
            for combo in &e.combo {
                for comp in &combo.components {
                    let c = LteComponent {
                        band: comp.band,
                        bw_class_mimo_dl: comp.bw_class_mimo_dl,
                        bw_class_mimo_ul: Some(comp.bw_class_mimo_ul),
                    };
                    let _ = writeln!(out, "       B{:<4} {}", comp.band, cc_detail(&c));
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::patch::format::{
        CcKind, Kind, LtePatchCombo, LtePatchComponent, LteSetEntry, PatchCc, PatchCombo, SetEntry,
    };

    fn nr_patch() -> Patch {
        Patch::Nr(NrPatch {
            kind: Kind::Nr,
            version: 1,
            delete: vec!["n41A".into()],
            set: vec![
                SetEntry {
                    kind: Some("add".into()),
                    combo: vec![PatchCombo {
                        cc: vec![PatchCc {
                            kind: CcKind::Nr,
                            band: 2,
                            bw_class_dl: Some(1),
                            bw_class_ul: Some(1),
                            dl_max_bw: Some(40),
                            dl_max_mimo: Some(2),
                            dl_max_mod_order: Some(2),
                            ..Default::default()
                        }],
                        ..Default::default()
                    }],
                },
                SetEntry {
                    kind: Some("change".into()),
                    combo: vec![PatchCombo {
                        cc: vec![PatchCc {
                            kind: CcKind::Nr,
                            band: 78,
                            bw_class_dl: Some(1),
                            bw_class_ul: Some(1),
                            dl_max_bw: Some(100),
                            dl_max_mimo: Some(3),
                            dl_max_mod_order: Some(2),
                            ..Default::default()
                        }],
                        ..Default::default()
                    }],
                },
            ],
        })
    }

    fn lte_patch() -> Patch {
        Patch::Lte(LtePatch {
            kind: Kind::Lte,
            version: 1,
            delete: vec!["B5A↓".into()],
            set: vec![LteSetEntry {
                kind: Some("add".into()),
                combo: vec![LtePatchCombo {
                    components: vec![LtePatchComponent {
                        band: 7,
                        bw_class_mimo_dl: 32768,
                        bw_class_mimo_ul: 0,
                    }],
                    bcs: 0,
                    unknown1: 0,
                    unknown2: 0,
                }],
            }],
        })
    }

    #[test]
    fn nr_summary_lists_deletes_and_sets() {
        let s = render(&nr_patch(), false);
        assert!(s.contains("Combo patch (nr) · format v1"), "{s}");
        assert!(s.contains("delete 1 · set 2 (1 add, 1 change)"), "{s}");
        assert!(s.contains("n41A"), "{s}");
        assert!(s.contains("+ n2A"), "{s}");
        assert!(s.contains("~ n78A"), "{s}");
        // summary has no per-component caps
        assert!(!s.contains("DL 40MHz"), "{s}");
    }

    #[test]
    fn nr_full_adds_per_component_caps() {
        let s = render(&nr_patch(), true);
        assert!(s.contains("DL 40MHz 4x4 QAM256"), "{s}");
        assert!(s.contains("DL 100MHz 8x8 QAM256"), "{s}");
        let n78_line = s
            .lines()
            .find(|l| l.contains("DL 100MHz 8x8"))
            .expect("n78 caps line");
        assert!(
            n78_line.contains("n78"),
            "n78 label on caps line: {n78_line}"
        );
        let n2_line = s
            .lines()
            .find(|l| l.contains("DL 40MHz 4x4"))
            .expect("n2 caps line");
        assert!(n2_line.contains("n2"), "n2 label on caps line: {n2_line}");
    }

    #[test]
    fn nr_full_shows_partial_raw_caps_without_bandwidth() {
        let patch = Patch::Nr(NrPatch {
            kind: Kind::Nr,
            version: 1,
            delete: vec![],
            set: vec![SetEntry {
                kind: Some("add".into()),
                combo: vec![PatchCombo {
                    cc: vec![PatchCc {
                        kind: CcKind::Nr,
                        band: 78,
                        bw_class_dl: Some(1),
                        bw_class_ul: Some(1),
                        dl_max_mimo: Some(7),
                        ..Default::default()
                    }],
                    ..Default::default()
                }],
            }],
        });

        let s = render(&patch, true);

        assert!(s.contains("DL — (7) —"), "{s}");
        assert!(!s.contains("(no NR feature set)"), "{s}");
    }

    #[test]
    fn nr_full_shows_unknown_raw_scs_without_bandwidth() {
        let patch = Patch::Nr(NrPatch {
            kind: Kind::Nr,
            version: 1,
            delete: vec![],
            set: vec![SetEntry {
                kind: Some("add".into()),
                combo: vec![PatchCombo {
                    cc: vec![PatchCc {
                        kind: CcKind::Nr,
                        band: 78,
                        bw_class_dl: Some(1),
                        bw_class_ul: Some(1),
                        dl_max_scs: Some(9),
                        ..Default::default()
                    }],
                    ..Default::default()
                }],
            }],
        });

        let s = render(&patch, true);

        assert!(s.contains("DL — — — · SCS (9)"), "{s}");
        assert!(!s.contains("(no NR feature set)"), "{s}");
    }

    #[test]
    fn lte_summary_and_full() {
        let summary = render(&lte_patch(), false);
        assert!(
            summary.contains("Combo patch (lte) · format v1"),
            "{summary}"
        );
        assert!(summary.contains("B5A↓"), "{summary}");
        assert!(summary.contains("+ B7A↓"), "{summary}");
        let full = render(&lte_patch(), true);
        // cc_detail always emits "DL … UL …"
        let comp_line = full
            .lines()
            .find(|l| l.contains("DL ") && l.contains("UL "))
            .expect("lte component line");
        assert!(
            comp_line.contains("B7"),
            "B7 label on component line: {comp_line}"
        );
    }

    #[test]
    fn kindless_entry_uses_dot_marker() {
        let patch = Patch::Nr(NrPatch {
            kind: Kind::Nr,
            version: 1,
            delete: vec![],
            set: vec![SetEntry {
                kind: None,
                combo: vec![PatchCombo {
                    cc: vec![PatchCc {
                        kind: CcKind::Nr,
                        band: 1,
                        bw_class_dl: Some(1),
                        bw_class_ul: Some(1),
                        ..Default::default()
                    }],
                    ..Default::default()
                }],
            }],
        });
        let s = render(&patch, false);
        assert!(s.contains("· n1A"), "{s}");
        assert!(s.contains("(none)"), "{s}");
    }

    #[test]
    fn show_reads_a_file() {
        let dir = std::env::temp_dir().join(format!("uecaps-patchshow-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("p.toml");
        std::fs::write(&path, format::to_toml(&nr_patch()).unwrap()).unwrap();
        let code = show(Some(&path), true).unwrap();
        std::fs::remove_dir_all(&dir).ok();
        assert_eq!(code, 0);
    }

    #[test]
    fn show_errors_on_bad_input() {
        let dir = std::env::temp_dir().join(format!("uecaps-patchshow-bad-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("bad.toml");
        std::fs::write(&path, "not a patch").unwrap();
        let res = show(Some(&path), false);
        std::fs::remove_dir_all(&dir).ok();
        assert!(res.is_err());
    }
}
