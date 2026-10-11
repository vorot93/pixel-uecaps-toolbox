//! Single-file observation with contextual validation and useful partial output.
use super::{
    Report,
    combos::render_combos,
    detail::Detail,
    identity::project_combinations,
    input::{FileEvidence, Observation, load_validated, read_optional_legend},
    validation::{Issue, IssueKind, diagnostics, legend_issues},
};
use crate::{
    factor::{factor_display, gcd},
    model::{
        FileKind, MAPPING_BASENAME, classify_filename, decode_plmn, fingerprint_info,
        matching_anchors, mcc_country,
    },
    outcome::Outcome,
    proto::{Carrier, LteCaps, PlmnMap, UeCaps},
};
use std::{collections::BTreeSet, fmt::Write, path::Path};

fn plmn_label(value: u64) -> String {
    decode_plmn(value).map_or_else(
        || format!("<invalid PLMN {value}>"),
        |(mcc, mnc)| format!("{mcc}-{mnc}"),
    )
}
fn render_entry(out: &mut String, index: usize, c: &Carrier) {
    let plmns = c
        .plmns
        .iter()
        .map(|&v| plmn_label(v))
        .collect::<Vec<_>>()
        .join(", ");
    let countries: BTreeSet<_> = c
        .plmns
        .iter()
        .filter_map(|&v| decode_plmn(v))
        .map(|(mcc, _)| {
            mcc_country(&mcc)
                .map(str::to_owned)
                .unwrap_or_else(|| format!("MCC{mcc}"))
        })
        .collect();
    let _ = writeln!(
        out,
        "  entry {}: {:?} index={} PLMNs ({}) [{plmns}] countries: {}",
        index + 1,
        c.name,
        c.index,
        c.plmns.len(),
        countries.into_iter().collect::<Vec<_>>().join(", ")
    );
}
fn render_legend(map: &PlmnMap) -> String {
    let mut out = format!(
        "PLMN -> carrier legend\nCarriers : {} (original entries)\n",
        map.carriers.len()
    );
    for (i, c) in map.carriers.iter().enumerate() {
        render_entry(&mut out, i, c);
    }
    out
}
fn carrier_signature(dir: &Path, carrier: &str, fallback: u64) -> anyhow::Result<(u64, usize)> {
    let mut nums = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        if let FileKind::Carrier { carrier: c, number } = classify_filename(&entry.path())
            && c == carrier
        {
            nums.push(number);
        }
    }
    if nums.is_empty() {
        nums.push(fallback);
    }
    Ok((nums.iter().fold(0, |a, &n| gcd(a, n)), nums.len()))
}
fn carrier_identity(out: &mut String, issues: &mut Vec<Issue>, dir: &Path, carrier: &str) {
    match read_optional_legend(dir) {
        Err(error) => {
            issues.push(Issue::input(&dir.join(MAPPING_BASENAME), error));
            out.push_str("Legend identity: unavailable\n");
        }
        Ok(None) => out.push_str("Legend: absent (optional)\n"),
        Ok(Some(map)) => {
            let errors = legend_issues(&dir.join(MAPPING_BASENAME), &map);
            let invalid = !errors.is_empty();
            issues.extend(errors);
            let matches: Vec<_> = map
                .carriers
                .iter()
                .enumerate()
                .filter(|(_, c)| c.name == carrier)
                .collect();
            if invalid {
                out.push_str(
                    "Legend identity: invalid; observed entries below are not authoritative\n",
                );
            }
            if matches.len() > 1 {
                out.push_str("Conflicting legend entries:\n");
            }
            if matches.is_empty() && !invalid {
                if carrier == "EU_COMMON1" {
                    out.push_str("Regional default / fallback config\n");
                } else {
                    out.push_str("Carrier not present in legend\n");
                }
            }
            for (i, c) in matches {
                render_entry(out, i, c);
            }
        }
    }
}
pub fn inspect(path: &Path, detail: Detail) -> anyhow::Result<Report> {
    let base = path.file_name().and_then(|n| n.to_str()).unwrap_or("?");
    let evidence = load_validated(path, classify_filename(path))
        .ok_or_else(|| anyhow::anyhow!("Not a recognised uecaps filename: {base}"))?;
    let mut out = format!("File: {}\n", path.display());
    let issues = match evidence {
        FileEvidence::Carrier {
            carrier,
            number,
            mut observation,
        } => {
            render_carrier_file(&mut out, &carrier, number, &mut observation, detail)?;
            observation.issues
        }
        FileEvidence::Lte { id, observation } => {
            render_lte_file(&mut out, id, &observation, detail)?;
            observation.issues
        }
        FileEvidence::Legend(observation) => {
            render_legend_file(&mut out, &observation);
            observation.issues
        }
    };
    Ok(Report {
        body: out,
        diagnostics: diagnostics(&issues),
        outcome: Outcome::from_validation_errors(!issues.is_empty()),
    })
}

fn note_unavailable<T>(out: &mut String, observation: &Observation<T>) {
    if observation.value.is_none() {
        out.push_str("Content unavailable; filename-only analysis\n");
    }
}

fn render_carrier_file(
    out: &mut String,
    carrier: &str,
    number: u64,
    observation: &mut Observation<UeCaps>,
    detail: Detail,
) -> std::fmt::Result {
    note_unavailable(out, observation);
    let dir = observation
        .path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    writeln!(
        out,
        "Carrier UE-capability profile\nCarrier: {carrier}\nNumber: {number}"
    )?;
    carrier_identity(out, &mut observation.issues, dir, carrier);
    for profile in matching_anchors(number) {
        writeln!(
            out,
            "SKU profile anchor: {} ({})",
            profile.anchor,
            profile.model.unwrap_or("unregistered model")
        )?;
        if detail.is_full() {
            writeln!(
                out,
                "Selection rule: {number} mod {} = 0; core {:?}",
                profile.anchor, profile.core
            )?;
        }
    }
    if detail.is_full() {
        writeln!(out, "Factored: {}", factor_display(number))?;
        match carrier_signature(dir, carrier, number) {
            Ok((sig, count)) => writeln!(
                out,
                "Carrier signature: {sig} = {} ({count} sibling files)",
                factor_display(sig)
            )?,
            Err(error) => observation.issues.push(Issue::new(
                IssueKind::Read,
                dir,
                format!("sibling context unavailable: {error:#}"),
            )),
        }
    }
    if let Some(caps) = &observation.value {
        let fp = fingerprint_info(caps.version)
            .map(|(f, t)| format!("{}/{}", f.short(), t.as_str()))
            .unwrap_or_else(|| "UNKNOWN".into());
        writeln!(out, "In-file fingerprint: {} ({fp})", caps.version)?;
        if caps.combo_groups.is_empty() {
            out.push_str("Band combinations: none (reference stub)\n");
        } else if let Ok(combos) = project_combinations(caps) {
            out.push_str(&render_combos(&combos, detail));
        } else {
            out.push_str(
                "Band combinations unavailable: invalid NR references (see diagnostics)\n",
            );
        }
    }
    Ok(())
}

fn render_lte_file(
    out: &mut String,
    number: u64,
    observation: &Observation<LteCaps>,
    detail: Detail,
) -> std::fmt::Result {
    note_unavailable(out, observation);
    out.push_str("LTE-only fallback config\n");
    for line in super::lte::config_block(number) {
        writeln!(out, "{line}")?;
    }
    if let Some(caps) = &observation.value {
        writeln!(
            out,
            "In-file fingerprint: {}\nbitmask: {}",
            caps.fingerprint, caps.bitmask
        )?;
        out.push_str(&super::lte::render_lte_combos(caps, detail));
    }
    Ok(())
}

fn render_legend_file(out: &mut String, observation: &Observation<PlmnMap>) {
    note_unavailable(out, observation);
    if let Some(map) = &observation.value {
        out.push_str(&render_legend(map));
    }
}
