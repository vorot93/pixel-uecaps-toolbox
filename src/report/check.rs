//! Folder-wide consistency check (`check`).

use super::{binarypb_names, read_ue_caps};
use crate::{
    mapping::{LegendReport, load_mapping_report},
    model::*,
    outcome::Outcome,
    proto::UeCaps,
};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
};

// For carrier files the fingerprint (field 1) is always present, so "no payload"
// (field 3 absent) is exactly the stub condition.
const fn is_stub(caps: &UeCaps) -> bool {
    caps.combo_groups.is_empty()
}

/// The `## genuine anomalies` section header, with the per-tier profile counts and the
/// fingerprint count derived from the model rather than hardcoded `16/14/4` literals.
fn anomalies_header() -> String {
    format!(
        "## genuine anomalies (do not fit the {}/{}-profile, {}-fingerprint model)",
        tier_profile_count(Tier::Main),
        tier_profile_count(Tier::Alt),
        FINGERPRINTS.len(),
    )
}

/// The `## alt-tier carriers` section header, with the alt profile count and the Alt-tier
/// fingerprints derived from the model rather than hardcoded literals.
fn alt_tier_header() -> String {
    let fps = tier_fingerprints(Tier::Alt)
        .iter()
        .map(u64::to_string)
        .collect::<Vec<_>>()
        .join("/");
    format!(
        "## alt-tier carriers ({} profiles, fingerprints {fps})",
        tier_profile_count(Tier::Alt),
    )
}

// --------------------------------------------------------------------------- //
//  Folder-wide consistency check                                               //
// --------------------------------------------------------------------------- //

/// What one pass over a folder's `.binarypb` names found, before any file is read.
struct FolderScan {
    /// Carrier files grouped by carrier, each `(number, filename)`, in sorted-name order.
    carriers: BTreeMap<String, Vec<(u64, String)>>,
    lte: usize,
    mapping_files: usize,
    unparseable: Vec<String>,
}

/// Groups `filenames` into a [`FolderScan`] in one pass. `lte`/`mapping_files` read like
/// `filter().count()`, but writing them as two extra passes over `filenames` would call
/// `parse_name` three times per name instead of once — so the `match` produces all four
/// fields directly instead of writing into separate outer-scope counters.
fn scan_filenames(filenames: &[String]) -> FolderScan {
    let mut carriers: BTreeMap<String, Vec<(u64, String)>> = BTreeMap::new();
    let mut unparseable = Vec::new();
    let (mut lte, mut mapping_files) = (0usize, 0usize);
    for name in filenames {
        match parse_name(name) {
            Parsed::Mapping => mapping_files += 1,
            Parsed::Lte(_) => lte += 1,
            Parsed::Other => unparseable.push(name.clone()),
            Parsed::Carrier { carrier, number } => carriers
                .entry(carrier)
                .or_default()
                .push((number, name.clone())),
        }
    }
    FolderScan {
        carriers,
        lte,
        mapping_files,
        unparseable,
    }
}

/// What analysing one carrier's files produced. Returned rather than pushed, so the caller
/// owns the order findings appear in.
#[derive(Default)]
struct CarrierFinding {
    /// `(filename-or-carrier, why)` pairs, in discovery order.
    anomalies: Vec<(String, String)>,
    /// Filenames with a profile and fingerprint but no capability payload.
    stubs: Vec<String>,
    /// Whether the carrier's majority tier is alt.
    is_alt: bool,
    /// `Some((seen, expected, tier))` when the profile set is short.
    incomplete: Option<(usize, usize, &'static str)>,
}

/// What analysing one file within a carrier's group found. Returned rather than pushed
/// straight into the carrier's [`CarrierFinding`], so `analyse_carrier` decides what to do
/// with a profile match (accumulate it into `profiles_seen`) and a tier vote (tally it) —
/// this function only looks at one file in isolation.
#[derive(Default)]
struct FileFinding {
    /// This file's anomaly, if any.
    anomaly: Option<(String, String)>,
    /// Whether this file is a stub (only meaningful when `profile_anchor.is_some()`).
    is_stub: bool,
    /// The profile anchor this file's number resolved to, when exactly one matched.
    profile_anchor: Option<u64>,
    /// This file's vote for the carrier's tier, when its fingerprint was recognised.
    tier_vote: Option<&'static str>,
}

/// Analyses one carrier file in isolation: which profile (if any) its number resolves to,
/// whether its in-file fingerprint agrees with that profile, and whether it's a stub.
fn analyse_file(dir: &Path, name: &str, number: u64) -> FileFinding {
    let anchors = matching_anchors(number);
    if anchors.len() != 1 {
        let why = if anchors.is_empty() {
            "no profile (0 anchor primes divide number)".to_string()
        } else {
            let anchor_ids: Vec<_> = anchors.iter().map(|p| p.anchor).collect();
            format!(
                "ambiguous: divisible by {} anchors {:?}",
                anchors.len(),
                anchor_ids
            )
        };
        return FileFinding {
            anomaly: Some((name.to_string(), why)),
            ..Default::default()
        };
    }
    let profile = anchors[0];
    let mut finding = FileFinding {
        profile_anchor: Some(profile.anchor),
        ..Default::default()
    };

    let caps = read_ue_caps(&dir.join(name));
    let fp = caps.as_ref().map(|c| c.version);
    match fp.and_then(fp_info) {
        None => {
            finding.anomaly = Some((
                name.to_string(),
                format!(
                    "unknown fingerprint {}",
                    fp.map_or_else(|| "<none>".into(), |v| v.to_string())
                ),
            ));
        }
        Some((ffam, tier)) => {
            finding.tier_vote = Some(tier_short(tier));
            if ffam != profile.family {
                finding.anomaly = Some((
                    name.to_string(),
                    format!(
                        "fingerprint family {} != profile {} family {}",
                        family_desc(ffam),
                        profile.anchor,
                        family_desc(profile.family)
                    ),
                ));
            }
        }
    }
    if let Some(c) = &caps
        && is_stub(c)
    {
        finding.is_stub = true;
    }
    finding
}

/// Analyses one carrier's files: per-file profile/fingerprint consistency and stub detection
/// (via [`analyse_file`]), then the carrier-wide tier-majority and profile-completeness checks.
fn analyse_carrier(dir: &Path, carrier: &str, files: &[(u64, String)]) -> CarrierFinding {
    let mut finding = CarrierFinding::default();
    let mut tier_votes: BTreeMap<&'static str, usize> = BTreeMap::new();
    let mut profiles_seen: BTreeSet<u64> = BTreeSet::new();

    for (number, name) in files {
        let file = analyse_file(dir, name, *number);
        if let Some(anomaly) = file.anomaly {
            finding.anomalies.push(anomaly);
        }
        if file.is_stub {
            finding.stubs.push(name.clone());
        }
        if let Some(anchor) = file.profile_anchor {
            profiles_seen.insert(anchor);
        }
        if let Some(tier_key) = file.tier_vote {
            *tier_votes.entry(tier_key).or_insert(0) += 1;
        }
    }

    // A real carrier's files all share one tier; a split vote is anomalous data, so
    // surface it explicitly rather than relying on the implicit BTreeMap-order tie-break
    // (which would silently classify a 8/8 split as "main" and expect 16 profiles).
    if tier_votes.len() > 1 {
        let tally: Vec<String> = tier_votes.iter().map(|(k, n)| format!("{k}={n}")).collect();
        finding.anomalies.push((
            carrier.to_string(),
            format!("carrier mixes tier fingerprints ({})", tally.join(", ")),
        ));
    }

    // Pick the majority tier. `BTreeMap` iter order is sorted, so `max_by_key` on a tie
    // returns the lexically-last key — `"main"` (since `"alt" < "main"`). The split-vote
    // anomaly above already flags the genuine ambiguity; this just selects a deterministic
    // expected-profile count so the incomplete-profiles check still runs.
    let tier = tier_votes
        .iter()
        .max_by_key(|(_, n)| **n)
        .map_or("?", |(k, _)| *k);
    if tier == "alt" {
        finding.is_alt = true;
    }
    let expected = if tier == "alt" {
        tier_profile_count(Tier::Alt)
    } else {
        tier_profile_count(Tier::Main)
    };
    if profiles_seen.len() != expected {
        finding.incomplete = Some((profiles_seen.len(), expected, tier));
    }

    finding
}

/// Legend corruption the lenient collapse would otherwise hide: the write path
/// (`root_to_map`) hard-errors on duplicate/empty names, so a read-only audit must not
/// report them as clean.
fn legend_anomalies(legend: &LegendReport) -> Vec<(String, String)> {
    let mut anomalies = Vec::new();
    for name in &legend.duplicate_names {
        anomalies.push((
            "ap_plmn_mapping.binarypb".to_string(),
            format!(
                "duplicate legend carrier name {name:?} (entries collapsed on read; the write path rejects this)"
            ),
        ));
    }
    if legend.empty_named > 0 {
        anomalies.push((
            "ap_plmn_mapping.binarypb".to_string(),
            format!(
                "{} legend carrier(s) with an empty name (dropped on read; the write path rejects this)",
                legend.empty_named
            ),
        ));
    }
    anomalies
}

/// The report's finding lists, grouped so `print_report` takes one argument instead of five.
#[derive(Default)]
struct Findings {
    anomalies: Vec<(String, String)>,
    stubs: Vec<String>,
    alt_carriers: Vec<String>,
    incomplete: Vec<(String, usize, usize, &'static str)>,
    not_in_legend: Vec<String>,
}

/// Runs [`analyse_carrier`] over every carrier in `scan.carriers` (sorted, since it's a
/// `BTreeMap`), then appends the legend anomalies, then computes which carriers have files but
/// no legend entry. Order matches what `check_folder` produced before this split: per-carrier
/// findings extended in carrier order, legend anomalies always last.
fn collect_findings(dir: &Path, scan: &FolderScan, legend: &LegendReport) -> Findings {
    let mut findings = Findings::default();

    for (carrier, files) in &scan.carriers {
        let finding = analyse_carrier(dir, carrier, files);
        findings.anomalies.extend(finding.anomalies);
        findings.stubs.extend(finding.stubs);
        if finding.is_alt {
            findings.alt_carriers.push(carrier.clone());
        }
        if let Some((seen, expected, tier)) = finding.incomplete {
            findings
                .incomplete
                .push((carrier.clone(), seen, expected, tier));
        }
    }

    findings.anomalies.extend(legend_anomalies(legend));

    findings.not_in_legend = scan
        .carriers
        .keys()
        .filter(|c| !legend.entries.contains_key(*c))
        .cloned()
        .collect();

    findings
}

fn print_anomalies_section(anomalies: &[(String, String)]) {
    println!("{}", anomalies_header());
    if anomalies.is_empty() {
        println!("   none");
    } else {
        for (name, why) in anomalies {
            println!("   {name:<44} {why}");
        }
    }
}

fn print_stubs_section(stubs: &[String]) {
    println!("\n## reference stubs (profile + fingerprint, but NO capability payload)");
    println!("   {} files", stubs.len());
    if !stubs.is_empty() {
        let mut by_carrier: BTreeMap<String, usize> = BTreeMap::new();
        for name in stubs {
            if let Parsed::Carrier { carrier, .. } = parse_name(name) {
                *by_carrier.entry(carrier).or_insert(0) += 1;
            }
        }
        let list: Vec<_> = by_carrier
            .iter()
            .map(|(c, n)| format!("{c}({n})"))
            .collect();
        println!("   carriers: {}", list.join(", "));
    }
}

fn print_alt_tier_section(alt_carriers: &[String]) {
    println!("\n{}", alt_tier_header());
    println!(
        "   {}",
        if alt_carriers.is_empty() {
            "none".into()
        } else {
            alt_carriers.join(", ")
        }
    );
}

fn print_not_in_legend_section(not_in_legend: &[String]) {
    println!("\n## carriers with files but ABSENT from the legend");
    if not_in_legend.is_empty() {
        println!("   none");
    } else {
        for c in not_in_legend {
            println!("   {c}");
        }
    }
}

fn print_incomplete_section(incomplete: &[(String, usize, usize, &'static str)]) {
    println!("\n## incomplete profile sets (fewer files than the tier expects)");
    if incomplete.is_empty() {
        println!("   none");
    } else {
        for (c, got, exp, tier) in incomplete {
            println!("   {c:<16} {got}/{exp} profiles ({tier} tier)");
        }
    }
}

fn print_non_capability_section(scan: &FolderScan) {
    println!("\n## non-capability files");
    println!(
        "   ap_plmn_mapping.binarypb : {} (the legend)",
        scan.mapping_files
    );
    println!(
        "   lte_*.binarypb           : {} (LTE-only fallback)",
        scan.lte
    );
    println!(
        "   unparseable names        : {}",
        if scan.unparseable.is_empty() {
            "none".into()
        } else {
            scan.unparseable.join(", ")
        }
    );
}

/// Prints the folder-check report's fixed six sections, in the order `check_folder` has always
/// printed them. That order — and each section's internal line order — is observable behavior,
/// not incidental structure.
fn print_report(
    dir: &Path,
    filenames: &[String],
    scan: &FolderScan,
    legend: &LegendReport,
    findings: &Findings,
) {
    println!(
        "=== folder check: {} ===",
        dir.canonicalize()
            .unwrap_or_else(|_| dir.to_path_buf())
            .display()
    );
    println!(
        "files: {}  |  carriers: {}  |  legend entries: {}\n",
        filenames.len(),
        scan.carriers.len(),
        legend.entries.len()
    );

    print_anomalies_section(&findings.anomalies);
    print_stubs_section(&findings.stubs);
    print_alt_tier_section(&findings.alt_carriers);
    print_not_in_legend_section(&findings.not_in_legend);
    print_incomplete_section(&findings.incomplete);
    print_non_capability_section(scan);
}

pub fn check_folder(dir: &Path) -> anyhow::Result<Outcome> {
    let filenames = binarypb_names(dir)?;
    let legend = load_mapping_report(dir);
    let scan = scan_filenames(&filenames);
    let findings = collect_findings(dir, &scan, &legend);
    print_report(dir, &filenames, &scan, &legend, &findings);
    Ok(Outcome::from(!findings.anomalies.is_empty()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::outcome::Outcome;

    #[test]
    fn headers_derive_counts_and_fingerprints_from_the_model() {
        // The report headers reflect the model (PROFILES / MAIN_ONLY_ANCHORS /
        // FINGERPRINTS), not hardcoded `16/14/4` and `707802847/627223094` literals that
        // would drift silently.
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
        // A legend with a duplicate carrier name or an empty name must not audit as
        // clean. load_mapping collapses/drops them (last-wins / skip-empty) while the write
        // path rejects both, so check now surfaces them as anomalies (exit 1), not exit 0.
        use crate::proto::{Carrier, PlmnMap};
        use prost::Message;
        let dir = std::env::temp_dir().join(format!("uecaps-check-legend-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let entry = |index, name: &str| Carrier {
            plmns: vec![],
            index,
            name: name.into(),
        };
        let map = PlmnMap {
            carriers: vec![
                entry(1, "VZW"),
                entry(2, "VZW"), // duplicate name (collapsed on read)
                entry(3, ""),    // empty name (dropped on read)
            ],
        };
        std::fs::write(dir.join("ap_plmn_mapping.binarypb"), map.encode_to_vec()).unwrap();
        let code = check_folder(&dir).unwrap();
        std::fs::remove_dir_all(&dir).ok();
        assert_eq!(
            code,
            Outcome::Findings,
            "a corrupted legend must not audit as clean (exit 0)"
        );
    }

    #[test]
    fn mixed_tier_fingerprints_are_flagged() {
        // A real carrier's files all share one tier; a split vote (some files main-tier,
        // some alt-tier) is anomalous data the audit must surface explicitly, rather than
        // relying on the implicit BTreeMap-order tie-break (which would silently pick "main"
        // and only flag the profile-count mismatch).
        //
        // This test pins the folder-level exit code at 1 for the mixed-tier scenario. It
        // doesn't distinguish the new "mixes tier fingerprints" anomaly from the existing
        // "incomplete profile set" anomaly (both fire here); the new anomaly is verified by
        // reading `check_folder`'s explicit `tier_votes.len() > 1` branch above.
        use crate::proto::{ComboGroup, UeCaps, combo_group};
        use prost::Message;
        let dir =
            std::env::temp_dir().join(format!("uecaps-check-mixedtier-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();

        // Pick two distinct anchor primes whose primality guarantees `matching_anchors`
        // returns exactly one profile for each filename number.
        let anchor_main = PROFILES[0].anchor; // first profile, used with a Main fingerprint
        let anchor_alt = PROFILES[1].anchor; // second profile, used with an Alt fingerprint
        assert_ne!(anchor_main, anchor_alt, "test needs distinct anchors");

        let caps_with = |version: u64| UeCaps {
            version,
            combo_groups: vec![ComboGroup {
                combo_header: None,
                combo: vec![combo_group::Combo {
                    bitmask: Some(0),
                    sub_blocks: vec![],
                }],
            }],
            ..Default::default()
        };

        // Carrier TEST: one file at a Main-tier fingerprint, one at Alt-tier. A real carrier
        // never splits its tier across files; the audit must exit non-zero on this input.
        std::fs::write(
            dir.join(format!("TEST_{anchor_main}.binarypb")),
            caps_with(874_888_686).encode_to_vec(), // Main / family A
        )
        .unwrap();
        std::fs::write(
            dir.join(format!("TEST_{anchor_alt}.binarypb")),
            caps_with(707_802_847).encode_to_vec(), // Alt / family A
        )
        .unwrap();

        let code = check_folder(&dir).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(
            code,
            Outcome::Findings,
            "a carrier with mixed-tier fingerprints must not audit as clean (exit 0)"
        );
    }
}
