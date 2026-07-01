//! `patch filter include|exclude` — keep/drop a patch's combos by band.

use super::format::{self, Patch};
use crate::report::combos::band_label;
use anyhow::Context;
use std::{collections::HashSet, io::Write, path::Path};

/// How `filter` matches an entry's band set against the requested bands.
#[derive(Clone, Copy)]
pub enum FilterMode {
    /// Keep entries whose bands intersect the requested set (default `include`, ANY-match).
    Include,
    /// Keep entries whose (non-empty) band set is a subset of the requested set (`include --only`).
    IncludeOnly,
    /// Keep entries whose bands are disjoint from the requested set (`exclude`).
    Exclude,
}

/// `patch filter include|exclude`: read a patch (FILE or stdin), filter by band, write (OUT or stdout).
/// Mode picks the predicate: include (any band), include-only (every band), exclude (no band).
pub fn filter(
    mode: FilterMode,
    bands: &[String],
    input: Option<&Path>,
    out: Option<&Path>,
) -> anyhow::Result<i32> {
    let text = super::read_patch_source(input)?;
    let mut patch = format::from_toml(&text)?;

    let wanted: HashSet<String> = bands
        .iter()
        .map(|b| parse_band_arg(b))
        .collect::<anyhow::Result<_>>()?;

    filter_patch(&mut patch, mode, &wanted);

    if patch_is_empty(&patch) {
        eprintln!("warning: filtered patch is empty (no combos matched)");
    }

    let text = format::to_toml(&patch)?;
    match out {
        Some(path) => std::fs::write(path, text.as_bytes())
            .with_context(|| format!("writing patch {}", path.display()))?,
        None => {
            let mut handle = std::io::stdout().lock();
            handle
                .write_all(text.as_bytes())
                .context("writing patch to stdout")?;
            handle.flush().context("flushing stdout")?;
        }
    }
    Ok(0)
}

/// Parse & canonicalize a user band argument (`n77`, `N77`, `B66`, `b66`) -> `n77` / `B66`.
fn parse_band_arg(s: &str) -> anyhow::Result<String> {
    let kind = match s.chars().next() {
        Some('n' | 'N') => 'n',
        Some('b' | 'B') => 'B',
        _ => anyhow::bail!("invalid band {s:?}; use an NR/LTE band label like n77 or B66"),
    };
    // The matched first char is ASCII, so `s[1..]` is a valid byte boundary.
    let num = &s[1..];
    if num.is_empty() || !num.bytes().all(|b| b.is_ascii_digit()) {
        anyhow::bail!("invalid band {s:?}; use an NR/LTE band label like n77 or B66");
    }
    Ok(format!("{kind}{num}"))
}

/// Band labels referenced by a delete-key string, e.g. "B66A + n77A" -> {"B66","n77"}.
fn key_bands(key: &str) -> HashSet<String> {
    let mut out = HashSet::new();
    for part in key.split('+') {
        let p = part.trim();
        let kind = match p.chars().next() {
            Some('n') => 'n',
            Some('B') => 'B',
            _ => continue,
        };
        // First char is ASCII n/B, so `p[1..]` is a valid byte boundary.
        let num: String = p[1..].chars().take_while(char::is_ascii_digit).collect();
        if !num.is_empty() {
            out.insert(format!("{kind}{num}"));
        }
    }
    out
}

/// Whether an entry with these band labels is kept under `mode`.
fn keep(bands: &HashSet<String>, mode: FilterMode, wanted: &HashSet<String>) -> bool {
    match mode {
        FilterMode::Include => !bands.is_disjoint(wanted),
        FilterMode::IncludeOnly => !bands.is_empty() && bands.is_subset(wanted),
        FilterMode::Exclude => bands.is_disjoint(wanted),
    }
}

/// Filter a patch in place (both `delete` and `set`). `kind`/`version` are untouched.
fn filter_patch(patch: &mut Patch, mode: FilterMode, wanted: &HashSet<String>) {
    match patch {
        Patch::Nr(p) => {
            p.delete.retain(|k| keep(&key_bands(k), mode, wanted));
            p.set.retain(|e| {
                let bands: HashSet<String> = e
                    .combo
                    .iter()
                    .flat_map(|c| c.cc.iter())
                    .map(|cc| band_label(cc.band))
                    .collect();
                keep(&bands, mode, wanted)
            });
        }
        Patch::Lte(p) => {
            p.delete.retain(|k| keep(&key_bands(k), mode, wanted));
            p.set.retain(|e| {
                let bands: HashSet<String> = e
                    .combo
                    .iter()
                    .flat_map(|c| c.components.iter())
                    .map(|comp| format!("B{}", comp.band))
                    .collect();
                keep(&bands, mode, wanted)
            });
        }
    }
}

/// `true` if the patch has no `delete` and no `set` entries left.
const fn patch_is_empty(patch: &Patch) -> bool {
    match patch {
        Patch::Nr(p) => p.delete.is_empty() && p.set.is_empty(),
        Patch::Lte(p) => p.delete.is_empty() && p.set.is_empty(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        patch::format::{
            Kind, LtePatch, LtePatchCombo, LtePatchComponent, LteSetEntry, NrPatch, SetEntry,
        },
        report::combos::{Cc, Combo},
    };

    fn nr_set(key: &str, bands: &[i32]) -> SetEntry {
        SetEntry {
            key: key.into(),
            kind: Some("add".into()),
            combo: vec![Combo {
                cc: bands
                    .iter()
                    .map(|&band| Cc {
                        band,
                        ..Default::default()
                    })
                    .collect(),
                ..Default::default()
            }],
        }
    }

    fn nr_patch() -> Patch {
        Patch::Nr(NrPatch {
            kind: Kind::Nr,
            version: 1,
            delete: vec!["n2A".into(), "n77A".into()],
            set: vec![
                nr_set("n2A", &[10002]),
                nr_set("n77A", &[10077]),
                nr_set("B66A + n77A", &[66, 10077]),
            ],
        })
    }

    fn lte_set(key: &str, bands: &[i32]) -> LteSetEntry {
        LteSetEntry {
            key: key.into(),
            kind: Some("add".into()),
            combo: vec![LtePatchCombo {
                bands: None,
                components: bands
                    .iter()
                    .map(|&band| LtePatchComponent {
                        band,
                        bw_class_mimo_dl: 0,
                        bw_class_mimo_ul: 0,
                    })
                    .collect(),
                bcs: 0,
                unknown1: 0,
                unknown2: 0,
            }],
        }
    }

    fn lte_patch() -> Patch {
        Patch::Lte(LtePatch {
            kind: Kind::Lte,
            version: 1,
            delete: vec!["B5A↓".into(), "B7A".into()],
            set: vec![lte_set("B5A↓", &[5]), lte_set("B7A", &[7])],
        })
    }

    fn set_keys(p: &Patch) -> Vec<String> {
        match p {
            Patch::Nr(p) => p.set.iter().map(|e| e.key.clone()).collect(),
            Patch::Lte(p) => p.set.iter().map(|e| e.key.clone()).collect(),
        }
    }
    fn deletes(p: &Patch) -> Vec<String> {
        match p {
            Patch::Nr(p) => p.delete.clone(),
            Patch::Lte(p) => p.delete.clone(),
        }
    }

    #[test]
    fn parse_band_arg_canonicalizes_and_rejects() {
        assert_eq!(parse_band_arg("n77").unwrap(), "n77");
        assert_eq!(parse_band_arg("N77").unwrap(), "n77");
        assert_eq!(parse_band_arg("B66").unwrap(), "B66");
        assert_eq!(parse_band_arg("b66").unwrap(), "B66");
        assert!(parse_band_arg("77").is_err());
        assert!(parse_band_arg("x5").is_err());
        assert!(parse_band_arg("n").is_err());
    }

    #[test]
    fn key_bands_parses_components() {
        assert_eq!(key_bands("n78A"), HashSet::from(["n78".to_string()]));
        assert_eq!(
            key_bands("B66A + n77A"),
            HashSet::from(["B66".to_string(), "n77".to_string()])
        );
        assert_eq!(key_bands("B5A↓"), HashSet::from(["B5".to_string()]));
    }

    #[test]
    fn nr_include_keeps_only_matching() {
        let mut p = nr_patch();
        filter_patch(
            &mut p,
            FilterMode::Include,
            &HashSet::from(["n77".to_string()]),
        );
        assert_eq!(deletes(&p), vec!["n77A".to_string()]);
        assert_eq!(
            set_keys(&p),
            vec!["n77A".to_string(), "B66A + n77A".to_string()]
        );
    }

    #[test]
    fn nr_exclude_drops_matching() {
        let mut p = nr_patch();
        filter_patch(
            &mut p,
            FilterMode::Exclude,
            &HashSet::from(["n77".to_string()]),
        );
        assert_eq!(deletes(&p), vec!["n2A".to_string()]);
        assert_eq!(set_keys(&p), vec!["n2A".to_string()]);
    }

    #[test]
    fn nr_include_matches_lte_component_and_any_of_many() {
        // B66 only appears in the EN-DC combo; deletes have no B66.
        let mut p = nr_patch();
        filter_patch(
            &mut p,
            FilterMode::Include,
            &HashSet::from(["B66".to_string()]),
        );
        assert!(deletes(&p).is_empty());
        assert_eq!(set_keys(&p), vec!["B66A + n77A".to_string()]);
        // ANY-match: {n2, B66} keeps the n2 combo and the B66 combo, drops n77-only.
        let mut p = nr_patch();
        filter_patch(
            &mut p,
            FilterMode::Include,
            &HashSet::from(["n2".to_string(), "B66".to_string()]),
        );
        assert_eq!(
            set_keys(&p),
            vec!["n2A".to_string(), "B66A + n77A".to_string()]
        );
    }

    #[test]
    fn lte_include_filters_by_lte_band() {
        let mut p = lte_patch();
        filter_patch(
            &mut p,
            FilterMode::Include,
            &HashSet::from(["B7".to_string()]),
        );
        assert_eq!(deletes(&p), vec!["B7A".to_string()]);
        assert_eq!(set_keys(&p), vec!["B7A".to_string()]);
    }

    #[test]
    fn filter_roundtrips_through_files() {
        let dir = std::env::temp_dir().join(format!("uecaps-pf-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let inp = dir.join("in.toml");
        std::fs::write(&inp, format::to_toml(&nr_patch()).unwrap()).unwrap();
        let outp = dir.join("out.toml");
        let code = filter(
            FilterMode::Include,
            &["n77".to_string()],
            Some(&inp),
            Some(&outp),
        )
        .unwrap();
        let parsed = format::from_toml(&std::fs::read_to_string(&outp).unwrap()).unwrap();
        std::fs::remove_dir_all(&dir).ok();
        assert_eq!(code, 0);
        assert_eq!(
            set_keys(&parsed),
            vec!["n77A".to_string(), "B66A + n77A".to_string()]
        );
    }

    #[test]
    fn filter_empty_result_is_ok_and_parseable() {
        let dir = std::env::temp_dir().join(format!("uecaps-pf-empty-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let inp = dir.join("in.toml");
        std::fs::write(&inp, format::to_toml(&nr_patch()).unwrap()).unwrap();
        let outp = dir.join("out.toml");
        let code = filter(
            FilterMode::Include,
            &["n99".to_string()],
            Some(&inp),
            Some(&outp),
        )
        .unwrap();
        let parsed = format::from_toml(&std::fs::read_to_string(&outp).unwrap()).unwrap();
        std::fs::remove_dir_all(&dir).ok();
        assert_eq!(code, 0);
        assert!(patch_is_empty(&parsed));
    }

    #[test]
    fn filter_bad_band_errors() {
        let dir = std::env::temp_dir().join(format!("uecaps-pf-bad-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let inp = dir.join("in.toml");
        std::fs::write(&inp, format::to_toml(&nr_patch()).unwrap()).unwrap();
        let res = filter(FilterMode::Include, &["77".to_string()], Some(&inp), None);
        std::fs::remove_dir_all(&dir).ok();
        assert!(res.is_err());
    }

    #[test]
    fn include_only_keep_predicate() {
        let wanted = HashSet::from(["n77".to_string(), "B66".to_string()]);
        // non-empty subset → keep
        assert!(keep(
            &HashSet::from(["n77".to_string()]),
            FilterMode::IncludeOnly,
            &wanted
        ));
        assert!(keep(
            &HashSet::from(["n77".to_string(), "B66".to_string()]),
            FilterMode::IncludeOnly,
            &wanted
        ));
        // an outside band → drop the whole entry
        assert!(!keep(
            &HashSet::from(["n77".to_string(), "n2".to_string()]),
            FilterMode::IncludeOnly,
            &wanted
        ));
        // empty band set → dropped, not vacuously kept
        assert!(!keep(&HashSet::new(), FilterMode::IncludeOnly, &wanted));
    }

    #[test]
    fn nr_include_only_keeps_subsets_drops_outside() {
        let mut p = nr_patch();
        filter_patch(
            &mut p,
            FilterMode::IncludeOnly,
            &HashSet::from(["n77".to_string()]),
        );
        // B66A + n77A drops (B66 outside the set); n2A drops; only the pure-n77 entry stays.
        assert_eq!(set_keys(&p), vec!["n77A".to_string()]);
        assert_eq!(deletes(&p), vec!["n77A".to_string()]);
    }

    #[test]
    fn nr_include_only_multiband_keeps_endc() {
        let mut p = nr_patch();
        filter_patch(
            &mut p,
            FilterMode::IncludeOnly,
            &HashSet::from(["n77".to_string(), "B66".to_string()]),
        );
        // {n77,B66} now covers the EN-DC combo, so it stays alongside n77A; n2A still drops.
        assert_eq!(
            set_keys(&p),
            vec!["n77A".to_string(), "B66A + n77A".to_string()]
        );
        assert_eq!(deletes(&p), vec!["n77A".to_string()]);
    }

    #[test]
    fn lte_include_only_filters_by_band() {
        let mut p = lte_patch();
        filter_patch(
            &mut p,
            FilterMode::IncludeOnly,
            &HashSet::from(["B7".to_string()]),
        );
        assert_eq!(set_keys(&p), vec!["B7A".to_string()]);
        assert_eq!(deletes(&p), vec!["B7A".to_string()]);
    }
}
