//! Typed capability set comparison; rendering and differences never determine success.
use super::{
    Report,
    combos::{KeyView, ShapeView},
    detail::{Common, Detail},
    identity::{CombinationKey, ShapeKey, project_combinations, shape},
};
use crate::{
    model::{FileKind, FingerprintLabel, classify_filename, identify_profile},
    outcome::Outcome,
};
use std::{
    collections::{BTreeMap, BTreeSet},
    fmt::Write,
    path::Path,
};

type Index = BTreeMap<ShapeKey, BTreeSet<CombinationKey>>;
struct ComparisonInput {
    index: Index,
    label: String,
}
fn render_variants(
    out: &mut String,
    label: char,
    variants: &BTreeSet<CombinationKey>,
) -> std::fmt::Result {
    for (i, variant) in variants.iter().enumerate() {
        writeln!(out, "    {label}[{}]:\n{}", i + 1, KeyView(variant))?;
    }
    Ok(())
}
fn load(path: &Path) -> anyhow::Result<ComparisonInput> {
    let kind = classify_filename(path);
    match kind {
        FileKind::Mapping => anyhow::bail!(
            "{} is the PLMN legend, not a carrier capability file",
            path.display()
        ),
        FileKind::Lte(_) => anyhow::bail!(
            "{} is an LTE fallback, not a carrier capability file",
            path.display()
        ),
        _ => (),
    }
    let caps = super::input::read_carrier(path)?;
    anyhow::ensure!(
        !caps.combo_groups.is_empty(),
        "{} is a reference stub",
        path.display()
    );
    let combos = project_combinations(&caps).map_err(|errors| {
        anyhow::anyhow!(
            "{}: {}",
            path.display(),
            errors
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join("; ")
        )
    })?;
    let mut index = Index::new();
    for combo in combos {
        index
            .entry(shape(&combo.key))
            .or_default()
            .insert(combo.key);
    }
    let profile = match kind {
        FileKind::Carrier { number, .. } => identify_profile(number).map(|p| p.anchor.to_string()),
        _ => None,
    }
    .unwrap_or_else(|| "?".into());
    let fingerprint = FingerprintLabel(caps.version);
    Ok(ComparisonInput {
        index,
        label: format!(
            "{}   {profile}   fp {} ({fingerprint})",
            path.display(),
            caps.version
        ),
    })
}

pub fn compare(a: &Path, b: &Path, detail: Detail, common: Common) -> anyhow::Result<Report> {
    let ComparisonInput {
        index: left,
        label: header_a,
    } = load(a)?;
    let ComparisonInput {
        index: right,
        label: header_b,
    } = load(b)?;
    let shared: Vec<_> = left.keys().filter(|k| right.contains_key(*k)).collect();
    let changed: Vec<_> = shared
        .iter()
        .copied()
        .filter(|k| left[*k] != right[*k])
        .collect();
    let only_a: Vec<_> = left.keys().filter(|k| !right.contains_key(*k)).collect();
    let only_b: Vec<_> = right.keys().filter(|k| !left.contains_key(*k)).collect();
    let mut out = format!("A: {header_a}\nB: {header_b}\n");
    if changed.is_empty() && only_a.is_empty() && only_b.is_empty() {
        writeln!(out, "  {} common · no differences", shared.len())?;
    } else {
        writeln!(
            out,
            "  {} common ({} caps-changed) · {} only in A · {} only in B",
            shared.len(),
            changed.len(),
            only_a.len(),
            only_b.len()
        )?;
    }
    for (label, marker, keys, index) in [('A', '-', only_a, &left), ('B', '+', only_b, &right)] {
        if !keys.is_empty() {
            writeln!(out, "\nonly in {label} ({}):", keys.len())?;
            for key in keys {
                writeln!(out, "  {marker} {}", ShapeView(key))?;
                if detail.is_full() {
                    render_variants(&mut out, label, &index[key])?;
                }
            }
        }
    }
    if common.is_shown() && !shared.is_empty() {
        writeln!(out, "\ncommon ({}):", shared.len())?;
        for key in shared {
            writeln!(
                out,
                "  {} {}",
                if left[key] == right[key] { '=' } else { '~' },
                ShapeView(key)
            )?;
        }
    }
    if detail.is_full() && !changed.is_empty() {
        writeln!(out, "\ncaps changed ({}):", changed.len())?;
        for key in changed {
            writeln!(out, "  ~ {}", ShapeView(key))?;
            // Whole typed variants also handle repeated component labels without guessing pairing.
            for (label, index) in [('A', &left), ('B', &right)] {
                render_variants(&mut out, label, &index[key])?;
            }
        }
    }
    Ok(Report {
        body: out,
        diagnostics: String::new(),
        outcome: Outcome::Clean,
    })
}
