//! `compare`: diff the band combinations of two capability files.

use super::combos::{Combo, cc_component_label, combo_key, fmt_cc_features};
use crate::model::{Family, fp_info, identify_profile, tier_short};
use std::{
    collections::{BTreeMap, BTreeSet},
    fmt::Write as _,
    path::Path,
};

/// One combo's capability signature: sorted `(component_label, caps_line)` pairs.
/// Two combos with the same key have equal caps iff their signatures are equal.
type Signature = Vec<(String, String)>;

pub(crate) struct ComboDiff {
    pub(crate) only_in_a: Vec<String>,
    pub(crate) only_in_b: Vec<String>,
    pub(crate) changed: Vec<ChangedCombo>,
    pub(crate) common: Vec<String>,
}

pub(crate) struct ChangedCombo {
    pub(crate) key: String,
    pub(crate) change: CapsChange,
}

pub(crate) enum CapsChange {
    /// One signature per side, unique component labels: per differing component,
    /// `(component_label, a_caps_line, b_caps_line)`.
    PerComponent(Vec<(String, String, String)>),
    /// Otherwise: each side's distinct signatures, listed verbatim.
    Variants {
        a: Vec<Signature>,
        b: Vec<Signature>,
    },
}

impl ComboDiff {
    pub(crate) const fn has_differences(&self) -> bool {
        !self.only_in_a.is_empty() || !self.only_in_b.is_empty() || !self.changed.is_empty()
    }
}

fn combo_signature(combo: &Combo) -> Signature {
    let mut sig: Signature = combo
        .cc
        .iter()
        .map(|cc| (cc_component_label(cc), fmt_cc_features(cc)))
        .collect();
    sig.sort();
    sig
}

/// key -> the set of distinct signatures seen for that key in one file.
fn index(combos: &[Combo]) -> BTreeMap<String, BTreeSet<Signature>> {
    let mut m: BTreeMap<String, BTreeSet<Signature>> = BTreeMap::new();
    for c in combos {
        m.entry(combo_key(c))
            .or_default()
            .insert(combo_signature(c));
    }
    m
}

fn has_unique_labels(sig: &Signature) -> bool {
    let mut labels: Vec<&str> = sig.iter().map(|(l, _)| l.as_str()).collect();
    labels.sort_unstable();
    labels.windows(2).all(|w| w[0] != w[1])
}

fn classify_change(sa: &BTreeSet<Signature>, sb: &BTreeSet<Signature>) -> CapsChange {
    if sa.len() == 1 && sb.len() == 1 {
        let a = sa.iter().next().unwrap();
        let b = sb.iter().next().unwrap();
        if has_unique_labels(a) && has_unique_labels(b) {
            let bmap: BTreeMap<&str, &str> =
                b.iter().map(|(l, ln)| (l.as_str(), ln.as_str())).collect();
            let mut diffs = Vec::new();
            for (label, a_line) in a {
                if let Some(&b_line) = bmap.get(label.as_str())
                    && a_line != b_line
                {
                    diffs.push((label.clone(), a_line.clone(), b_line.to_string()));
                }
            }
            return CapsChange::PerComponent(diffs);
        }
    }
    CapsChange::Variants {
        a: sa.iter().cloned().collect(),
        b: sb.iter().cloned().collect(),
    }
}

/// Diff two files' combos. Keys come out sorted (BTreeMap iteration order).
pub(crate) fn diff_combos(a: &[Combo], b: &[Combo]) -> ComboDiff {
    let ia = index(a);
    let ib = index(b);
    let only_in_a: Vec<String> = ia
        .keys()
        .filter(|k| !ib.contains_key(*k))
        .cloned()
        .collect();
    let only_in_b: Vec<String> = ib
        .keys()
        .filter(|k| !ia.contains_key(*k))
        .cloned()
        .collect();
    let mut changed = Vec::new();
    let mut common = Vec::new();
    for (k, sa) in &ia {
        if let Some(sb) = ib.get(k) {
            common.push(k.clone());
            if sa != sb {
                changed.push(ChangedCombo {
                    key: k.clone(),
                    change: classify_change(sa, sb),
                });
            }
        }
    }
    ComboDiff {
        only_in_a,
        only_in_b,
        changed,
        common,
    }
}

const fn family_short(f: Family) -> &'static str {
    match f {
        Family::A => "A",
        Family::B => "B",
    }
}

/// `A: <file>   <profile>   fp <version> (<tier>/<family>)`.
fn render_header_line(label: char, filename: &str, number: Option<u64>, version: u64) -> String {
    let profile = number
        .and_then(identify_profile)
        .map_or_else(|| "?".to_string(), |p| p.anchor.to_string());
    let fp = match fp_info(version) {
        Some((fam, tier)) => format!("{}/{}", tier_short(tier), family_short(fam)),
        None => "unknown".to_string(),
    };
    format!("{label}: {filename}   {profile}   fp {version} ({fp})")
}

/// Render the `common (N):` section: every key present in both files, marked
/// `=` (identical caps) or `~` (caps differ). Returns "" when there are no
/// common combos, so callers can invoke it unconditionally.
fn render_common_section(diff: &ComboDiff) -> String {
    if diff.common.is_empty() {
        return String::new();
    }
    let changed: BTreeSet<&str> = diff.changed.iter().map(|c| c.key.as_str()).collect();
    let mut out = format!("\ncommon ({}):\n", diff.common.len());
    for k in &diff.common {
        let mark = if changed.contains(k.as_str()) {
            '~'
        } else {
            '='
        };
        let _ = writeln!(out, "  {mark} {k}");
    }
    out
}

/// Render the summary + set diff (+ caps detail when `full`). Header is separate.
fn render_diff_body(diff: &ComboDiff, full: bool, show_common: bool) -> String {
    let mut out = String::new();
    if !diff.has_differences() {
        let _ = writeln!(out, "  {} common · no differences", diff.common.len());
        if show_common {
            out.push_str(&render_common_section(diff));
        }
        return out;
    }
    let _ = writeln!(
        out,
        "  {} common ({} caps-changed) · {} only in A · {} only in B",
        diff.common.len(),
        diff.changed.len(),
        diff.only_in_a.len(),
        diff.only_in_b.len(),
    );
    if !diff.only_in_a.is_empty() {
        let _ = writeln!(out, "\nonly in A ({}):", diff.only_in_a.len());
        for k in &diff.only_in_a {
            let _ = writeln!(out, "  - {k}");
        }
    }
    if !diff.only_in_b.is_empty() {
        let _ = writeln!(out, "\nonly in B ({}):", diff.only_in_b.len());
        for k in &diff.only_in_b {
            let _ = writeln!(out, "  + {k}");
        }
    }
    if show_common {
        out.push_str(&render_common_section(diff));
    }
    if full && !diff.changed.is_empty() {
        let _ = writeln!(out, "\ncaps changed ({}):", diff.changed.len());
        for ch in &diff.changed {
            match &ch.change {
                CapsChange::PerComponent(diffs) => {
                    let _ = writeln!(out, "  ~ {}", ch.key);
                    for (label, a_line, b_line) in diffs {
                        let _ = writeln!(out, "      {label:<6} A: {a_line}");
                        let _ = writeln!(out, "      {:<6} B: {b_line}", "");
                    }
                }
                CapsChange::Variants { a, b } => {
                    let _ = writeln!(out, "  ~ {}  (multiple variants)", ch.key);
                    for (i, sig) in a.iter().enumerate() {
                        let _ = writeln!(out, "      A[{}]:", i + 1);
                        for (label, line) in sig {
                            let _ = writeln!(out, "        {label:<6} {line}");
                        }
                    }
                    for (i, sig) in b.iter().enumerate() {
                        let _ = writeln!(out, "      B[{}]:", i + 1);
                        for (label, line) in sig {
                            let _ = writeln!(out, "        {label:<6} {line}");
                        }
                    }
                }
            }
        }
    }
    out
}

/// Load and validate one file; return its combos and its header line.
/// Errors (exit 2 at the boundary) on non-capability files and reference stubs.
fn load(path: &Path, label: char) -> anyhow::Result<(Vec<Combo>, String)> {
    let cc = crate::report::load_carrier_combos(path)?;
    let header = render_header_line(label, &cc.filename, cc.number, cc.version);
    Ok((cc.combos, header))
}

/// `compare`: diff the band combinations of two capability files (stdin not used).
pub fn compare(a: &Path, b: &Path, full: bool, show_common: bool) -> anyhow::Result<i32> {
    let (combos_a, header_a) = load(a, 'A')?;
    let (combos_b, header_b) = load(b, 'B')?;
    let diff = diff_combos(&combos_a, &combos_b);
    println!("{header_a}");
    println!("{header_b}");
    print!("{}", render_diff_body(&diff, full, show_common));
    Ok(i32::from(diff.has_differences()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::report::combos::{Cc, NR_BAND_OFFSET};

    fn nr_cc(band_n: i32, class: i32, mimo: &str) -> Cc {
        Cc {
            band: NR_BAND_OFFSET + band_n,
            bw_class_dl: Some(class),
            bw_class_ul: Some(class),
            dl_max_bw_mhz: Some(100),
            dl_mimo: Some(mimo.to_string()),
            ..Default::default()
        }
    }
    fn combo(ccs: Vec<Cc>) -> Combo {
        Combo {
            cc: ccs,
            ..Default::default()
        }
    }

    #[test]
    fn key_is_order_independent() {
        let a = combo(vec![nr_cc(78, 1, "4x4"), nr_cc(3, 1, "4x4")]);
        let b = combo(vec![nr_cc(3, 1, "4x4"), nr_cc(78, 1, "4x4")]);
        assert_eq!(combo_key(&a), "n3A + n78A");
        assert_eq!(combo_key(&a), combo_key(&b));
    }

    #[test]
    fn set_diff_added_removed_shared() {
        let a = vec![
            combo(vec![nr_cc(78, 1, "4x4")]),
            combo(vec![nr_cc(41, 1, "4x4")]),
        ];
        let b = vec![
            combo(vec![nr_cc(78, 1, "4x4")]),
            combo(vec![nr_cc(1, 1, "4x4")]),
        ];
        let d = diff_combos(&a, &b);
        assert_eq!(d.only_in_a, vec!["n41A"]);
        assert_eq!(d.only_in_b, vec!["n1A"]);
        assert_eq!(d.common, vec!["n78A"]);
        assert!(d.changed.is_empty());
        assert!(d.has_differences());
    }

    #[test]
    fn detects_caps_change_per_component() {
        let a = vec![combo(vec![nr_cc(78, 1, "4x4")])];
        let b = vec![combo(vec![nr_cc(78, 1, "8x8")])];
        let d = diff_combos(&a, &b);
        assert_eq!(d.only_in_a.len(), 0);
        assert_eq!(d.only_in_b.len(), 0);
        assert_eq!(d.changed.len(), 1);
        assert_eq!(d.changed[0].key, "n78A");
        match &d.changed[0].change {
            CapsChange::PerComponent(diffs) => {
                assert_eq!(diffs.len(), 1);
                assert_eq!(diffs[0].0, "n78A");
                assert!(diffs[0].1.contains("4x4"));
                assert!(diffs[0].2.contains("8x8"));
            }
            _ => panic!("expected PerComponent"),
        }
    }

    #[test]
    fn identical_inputs_have_no_differences() {
        let a = vec![combo(vec![nr_cc(78, 1, "4x4")])];
        let d = diff_combos(&a, &a);
        assert!(!d.has_differences());
        assert_eq!(d.common, vec!["n78A"]);
    }

    #[test]
    fn multi_variant_takes_block_path() {
        // same key n78A, two distinct caps variants on side A, one on side B
        let a = vec![
            combo(vec![nr_cc(78, 1, "4x4")]),
            combo(vec![nr_cc(78, 1, "8x8")]),
        ];
        let b = vec![combo(vec![nr_cc(78, 1, "4x4")])];
        let d = diff_combos(&a, &b);
        assert_eq!(d.changed.len(), 1);
        match &d.changed[0].change {
            CapsChange::Variants { a, b } => {
                assert_eq!(a.len(), 2);
                assert_eq!(b.len(), 1);
            }
            _ => panic!("expected Variants"),
        }
    }

    #[test]
    fn header_line_format() {
        // 193698151252893 is a VZW anchor-167 number; 874888686 is a main/A fingerprint.
        let h = render_header_line(
            'A',
            "VZW_193698151252893.binarypb",
            Some(193_698_151_252_893),
            874_888_686,
        );
        assert_eq!(
            h,
            "A: VZW_193698151252893.binarypb   167   fp 874888686 (main/A)"
        );
    }

    #[test]
    fn body_no_differences() {
        let a = vec![combo(vec![nr_cc(78, 1, "4x4")])];
        let d = diff_combos(&a, &a);
        assert_eq!(
            render_diff_body(&d, false, false),
            "  1 common · no differences\n"
        );
    }

    #[test]
    fn body_set_diff_summary_and_lists() {
        let a = vec![
            combo(vec![nr_cc(78, 1, "4x4")]),
            combo(vec![nr_cc(41, 1, "4x4")]),
        ];
        let b = vec![
            combo(vec![nr_cc(78, 1, "4x4")]),
            combo(vec![nr_cc(1, 1, "4x4")]),
        ];
        let out = render_diff_body(&diff_combos(&a, &b), false, false);
        assert!(
            out.starts_with("  1 common (0 caps-changed) · 1 only in A · 1 only in B\n"),
            "{out}"
        );
        assert!(out.contains("only in A (1):\n  - n41A\n"), "{out}");
        assert!(out.contains("only in B (1):\n  + n1A\n"), "{out}");
    }

    #[test]
    fn body_full_shows_caps_change() {
        let a = vec![combo(vec![nr_cc(78, 1, "4x4")])];
        let b = vec![combo(vec![nr_cc(78, 1, "8x8")])];
        let out = render_diff_body(&diff_combos(&a, &b), true, false);
        assert!(out.contains("caps changed (1):\n"), "{out}");
        assert!(out.contains("~ n78A\n"), "{out}");
        assert!(out.contains("A: DL 100MHz 4x4"), "{out}");
        assert!(out.contains("B: DL 100MHz 8x8"), "{out}");
    }

    #[test]
    fn body_omits_caps_detail_without_full() {
        let a = vec![combo(vec![nr_cc(78, 1, "4x4")])];
        let b = vec![combo(vec![nr_cc(78, 1, "8x8")])];
        let out = render_diff_body(&diff_combos(&a, &b), false, false);
        assert!(out.contains("1 common (1 caps-changed)"), "{out}");
        assert!(!out.contains("caps changed (1):"), "{out}");
    }

    #[test]
    fn common_lists_all_with_markers() {
        // n41A identical in both (=), n78A caps differ (~), one unique key each side.
        let a = vec![
            combo(vec![nr_cc(41, 1, "4x4")]),
            combo(vec![nr_cc(78, 1, "4x4")]),
            combo(vec![nr_cc(5, 1, "4x4")]),
        ];
        let b = vec![
            combo(vec![nr_cc(41, 1, "4x4")]),
            combo(vec![nr_cc(78, 1, "8x8")]),
            combo(vec![nr_cc(1, 1, "4x4")]),
        ];
        let out = render_diff_body(&diff_combos(&a, &b), false, true);
        assert!(out.contains("\ncommon (2):\n"), "{out}");
        assert!(out.contains("  = n41A\n"), "{out}");
        assert!(out.contains("  ~ n78A\n"), "{out}");
        // sorted: n41A before n78A
        assert!(
            out.find("= n41A").unwrap() < out.find("~ n78A").unwrap(),
            "{out}"
        );
    }

    #[test]
    fn common_off_by_default_has_no_section() {
        let a = vec![
            combo(vec![nr_cc(78, 1, "4x4")]),
            combo(vec![nr_cc(41, 1, "4x4")]),
        ];
        let b = vec![
            combo(vec![nr_cc(78, 1, "4x4")]),
            combo(vec![nr_cc(1, 1, "4x4")]),
        ];
        let out = render_diff_body(&diff_combos(&a, &b), false, false);
        assert!(!out.contains("\ncommon ("), "{out}");
        assert!(out.starts_with("  1 common (0 caps-changed)"), "{out}");
    }

    #[test]
    fn common_with_no_differences_lists_all() {
        let a = vec![
            combo(vec![nr_cc(78, 1, "4x4")]),
            combo(vec![nr_cc(41, 1, "4x4")]),
        ];
        let out = render_diff_body(&diff_combos(&a, &a), false, true);
        assert!(out.starts_with("  2 common · no differences\n"), "{out}");
        assert!(out.contains("\ncommon (2):\n"), "{out}");
        assert!(out.contains("  = n41A\n"), "{out}");
        assert!(out.contains("  = n78A\n"), "{out}");
        assert!(!out.contains('~'), "{out}");
    }

    #[test]
    fn common_empty_omits_section() {
        let a = vec![combo(vec![nr_cc(78, 1, "4x4")])];
        let b = vec![combo(vec![nr_cc(1, 1, "4x4")])];
        let out = render_diff_body(&diff_combos(&a, &b), false, true);
        assert!(!out.contains("\ncommon ("), "{out}");
        assert!(
            out.starts_with("  0 common (0 caps-changed) · 1 only in A · 1 only in B\n"),
            "{out}"
        );
    }

    #[test]
    fn common_marks_multi_variant_with_tilde() {
        // n78A is present in both, but side A carries two caps variants — this
        // lands in `changed` via the Variants path (not PerComponent). It must
        // still be marked `~` in the common section, since the marker keys off
        // `changed` membership, not the CapsChange variant.
        let a = vec![
            combo(vec![nr_cc(78, 1, "4x4")]),
            combo(vec![nr_cc(78, 1, "8x8")]),
        ];
        let b = vec![combo(vec![nr_cc(78, 1, "4x4")])];
        let d = diff_combos(&a, &b);
        assert!(matches!(d.changed[0].change, CapsChange::Variants { .. }));
        let out = render_diff_body(&d, false, true);
        assert!(out.contains("\ncommon (1):\n"), "{out}");
        assert!(out.contains("  ~ n78A\n"), "{out}");
        assert!(!out.contains("  = n78A\n"), "{out}");
    }

    #[test]
    fn load_rejects_non_capability_files() {
        use std::path::Path;
        // These bail at parse_name (filename only) before any filesystem read.
        let e = load(Path::new("ap_plmn_mapping.binarypb"), 'A').unwrap_err();
        assert!(e.to_string().contains("PLMN legend"), "{e}");
        let e = load(Path::new("lte_844857560.binarypb"), 'A').unwrap_err();
        assert!(e.to_string().contains("LTE fallback"), "{e}");
    }

    #[test]
    fn load_carrier_combos_rejects_legend_and_lte() {
        use std::path::Path;
        let e =
            crate::report::load_carrier_combos(Path::new("ap_plmn_mapping.binarypb")).unwrap_err();
        assert!(e.to_string().contains("PLMN legend"), "{e}");
        let e =
            crate::report::load_carrier_combos(Path::new("lte_844857560.binarypb")).unwrap_err();
        assert!(e.to_string().contains("LTE fallback"), "{e}");
    }
}
