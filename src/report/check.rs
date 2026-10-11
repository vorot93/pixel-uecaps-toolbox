//! Folder-wide consistency report rendered from shared retained evidence.
use super::{
    Report,
    folder::scan_folder,
    input::{LoadedInput, Payload},
    validation::diagnostics,
};
use crate::{
    model::{FINGERPRINTS, MAPPING_BASENAME, Parsed, Tier, tier_fingerprints, tier_profile_count},
    outcome::Outcome,
};
use std::{fmt::Write, path::Path};

fn anomalies_header() -> String {
    format!(
        "## genuine anomalies (do not fit the {}/{}-profile, {}-fingerprint model)",
        tier_profile_count(Tier::Main),
        tier_profile_count(Tier::Alt),
        FINGERPRINTS.len()
    )
}
fn alt_tier_header() -> String {
    let fps = tier_fingerprints(Tier::Alt)
        .iter()
        .map(u64::to_string)
        .collect::<Vec<_>>()
        .join("/");
    format!(
        "## alt-tier carriers ({} profiles, fingerprints {fps})",
        tier_profile_count(Tier::Alt)
    )
}
pub fn check_folder(dir: &Path) -> anyhow::Result<Report> {
    let report = scan_folder(dir)?;
    let legend = report
        .files
        .iter()
        .find(|f| matches!(f.parsed, Parsed::Mapping));
    let count = match legend {
        Some(f) if !f.errors.is_empty() => "unavailable (invalid legend)".into(),
        Some(f) => match &f.input {
            Some(LoadedInput {
                payload: Payload::Legend(map),
                ..
            }) => map.carriers.len().to_string(),
            _ => unreachable!(),
        },
        None => "absent (optional)".into(),
    };
    let mut out = format!(
        "=== folder check: {} ===\nfiles: {}  |  carriers: {}  |  legend entries: {count}\n\n{}\n",
        dir.display(),
        report.files.len() + report.unsupported.len(),
        report.coverage.len(),
        anomalies_header()
    );
    if report.has_errors() {
        out.push_str(&diagnostics(report.errors()));
    } else {
        out.push_str("   none\n");
    }
    writeln!(
        out,
        "\n## reference stubs (profile + fingerprint, but NO capability payload)\n   {} files",
        report.stubs.len()
    )?;
    for path in &report.stubs {
        writeln!(out, "   {}", path.display())?;
    }
    writeln!(out, "\n{}", alt_tier_header())?;
    let alt = report
        .coverage
        .iter()
        .filter(|c| c.tier == Some(Tier::Alt))
        .map(|c| c.carrier.as_str())
        .collect::<Vec<_>>();
    writeln!(
        out,
        "   {}",
        if alt.is_empty() {
            "none".into()
        } else {
            alt.join(", ")
        }
    )?;
    writeln!(out, "\n## carriers with files but ABSENT from the legend")?;
    match &report.legend_gaps {
        None => writeln!(out, "   unavailable (invalid legend)")?,
        Some(gaps) if gaps.is_empty() => writeln!(out, "   none")?,
        Some(gaps) => {
            for c in gaps {
                writeln!(out, "   {c}")?;
            }
        }
    }
    writeln!(out, "\n## validated profile coverage")?;
    for c in &report.coverage {
        writeln!(
            out,
            "   {:<16} {}/{} profiles ({} tier)",
            c.carrier,
            c.seen,
            c.expected,
            c.tier.map_or("?", Tier::as_str)
        )?;
    }
    writeln!(
        out,
        "\n## non-capability files\n   {MAPPING_BASENAME} : {} (the legend)\n   lte_*.binarypb : {} (LTE-only fallback)",
        usize::from(legend.is_some()),
        report
            .files
            .iter()
            .filter(|f| matches!(f.parsed, Parsed::Lte(_)))
            .count()
    )?;
    for path in &report.unsupported {
        writeln!(out, "   {path:?}: unsupported/not validated")?;
    }
    Ok(Report {
        body: out,
        diagnostics: String::new(),
        outcome: Outcome::from_validation_errors(report.has_errors()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        model::PROFILES,
        proto::{Carrier, Combo, ComboGroup, PlmnMap, UeCaps},
    };
    use prost::Message;
    #[test]
    fn headers_derive_counts_and_fingerprints_from_the_model() {
        assert_eq!(
            anomalies_header(),
            "## genuine anomalies (do not fit the 16/14-profile, 4-fingerprint model)"
        );
        assert_eq!(
            alt_tier_header(),
            "## alt-tier carriers (14 profiles, fingerprints 707802847/627223094)"
        );
    }
    #[test]
    fn duplicate_or_empty_legend_names_are_flagged() {
        let dir = tempfile::tempdir().unwrap();
        let entry = |index, name: &str| Carrier {
            plmns: vec![],
            index,
            name: name.into(),
        };
        let map = PlmnMap {
            carriers: vec![entry(1, "VZW"), entry(2, "VZW"), entry(3, "")],
        };
        std::fs::write(
            dir.path().join("ap_plmn_mapping.binarypb"),
            map.encode_to_vec(),
        )
        .unwrap();
        let report = scan_folder(dir.path()).unwrap();
        assert!(
            report
                .errors()
                .any(|e| e.message.contains("duplicate carrier name"))
        );
        assert!(
            report
                .errors()
                .any(|e| e.message.contains("empty carrier name"))
        );
        assert_eq!(check_folder(dir.path()).unwrap().outcome, Outcome::Rejected);
    }
    #[test]
    fn mixed_tier_fingerprints_are_flagged() {
        let dir = tempfile::tempdir().unwrap();
        for (anchor, version) in [
            (PROFILES[0].anchor, 874_888_686),
            (PROFILES[1].anchor, 707_802_847),
        ] {
            let caps = UeCaps {
                version,
                combo_groups: vec![ComboGroup {
                    combo_header: None,
                    combo: vec![Combo {
                        bitmask: Some(0),
                        sub_blocks: vec![],
                    }],
                }],
                ..Default::default()
            };
            std::fs::write(
                dir.path().join(format!("TEST_{anchor}.binarypb")),
                caps.encode_to_vec(),
            )
            .unwrap();
        }
        let report = scan_folder(dir.path()).unwrap();
        assert!(
            report
                .errors()
                .any(|e| e.message.contains("mixes tier fingerprints"))
        );
        assert_eq!(check_folder(dir.path()).unwrap().outcome, Outcome::Rejected);
    }
}
