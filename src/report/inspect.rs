//! Single-file observation with contextual validation and useful partial output.
use super::{
    combos::render_combos,
    detail::Detail,
    identity::project_combinations,
    input::{DecodeKind, Payload, read_input, read_optional_legend},
    validation::{
        Issue, IssueKind, carrier_context_issues, completion, diagnostics, payload_issues,
    },
};
use crate::{
    factor::{factor_display, gcd},
    model::{
        MAPPING_BASENAME, Parsed, decode_plmn, fingerprint_info, matching_anchors, mcc_country,
        parse_name,
    },
    outcome::Outcome,
    proto::{Carrier, PlmnMap},
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
        if let Some(name) = entry.file_name().to_str()
            && let Parsed::Carrier { carrier: c, number } = parse_name(name)
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
        Ok(Some(input)) => {
            let errors = payload_issues(&input);
            let invalid = !errors.is_empty();
            issues.extend(errors);
            if let Payload::Legend(map) = input.payload {
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
}
pub fn inspect(path: &Path, detail: Detail) -> anyhow::Result<Outcome> {
    let base = path.file_name().and_then(|n| n.to_str()).unwrap_or("?");
    let parsed = parse_name(base);
    let kind = match parsed {
        Parsed::Mapping => DecodeKind::Legend,
        Parsed::Lte(_) => DecodeKind::Lte,
        Parsed::Carrier { .. } => DecodeKind::Carrier,
        Parsed::Other => anyhow::bail!("Not a recognised uecaps filename: {base}"),
    };
    let dir = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let mut issues = Vec::new();
    let input = match read_input(path, kind) {
        Ok(input) => {
            issues.extend(payload_issues(&input));
            Some(input)
        }
        Err(error) => {
            issues.push(Issue::input(path, error));
            None
        }
    };
    let mut out = format!("File: {}\n", path.display());
    if input.is_none() {
        out.push_str("Content unavailable; filename-only analysis\n");
    }
    match parsed {
        Parsed::Carrier { carrier, number } => {
            writeln!(
                out,
                "Carrier UE-capability profile\nCarrier: {carrier}\nNumber: {number}"
            )?;
            carrier_identity(&mut out, &mut issues, dir, &carrier);
            let caps = input.as_ref().and_then(|i| match &i.payload {
                Payload::Carrier(c) => Some(c),
                _ => None,
            });
            issues.extend(carrier_context_issues(path, number, caps));
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
                match carrier_signature(dir, &carrier, number) {
                    Ok((sig, count)) => writeln!(
                        out,
                        "Carrier signature: {sig} = {} ({count} sibling files)",
                        factor_display(sig)
                    )?,
                    Err(error) => issues.push(Issue::new(
                        IssueKind::Read,
                        dir,
                        format!("sibling context unavailable: {error:#}"),
                    )),
                }
            }
            if let Some(caps) = caps {
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
        }
        Parsed::Lte(number) => {
            out.push_str("LTE-only fallback config\n");
            for line in super::lte::config_block(number) {
                writeln!(out, "{line}")?;
            }
            if let Some(input) = &input
                && let Payload::Lte(caps) = &input.payload
            {
                writeln!(
                    out,
                    "In-file fingerprint: {}\nbitmask: {}",
                    caps.fingerprint, caps.bitmask
                )?;
                out.push_str(&super::lte::render_lte_combos(caps, detail));
            }
        }
        Parsed::Mapping => {
            if let Some(input) = &input
                && let Payload::Legend(map) = &input.payload
            {
                out.push_str(&render_legend(map));
            }
        }
        Parsed::Other => unreachable!(),
    }
    super::write_report(&out, &diagnostics(&issues))?;
    Ok(completion(!issues.is_empty()))
}
