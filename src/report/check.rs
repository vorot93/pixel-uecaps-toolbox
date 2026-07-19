//! Folder-wide consistency check (`check`).

use super::{binarypb_names, read_ue_caps};
use crate::{mapping::load_mapping_report, model::*, proto::UeCaps};
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
/// fingerprint count derived from the model rather than hardcoded `16/14/4` literals (M7).
fn anomalies_header() -> String {
    format!(
        "## genuine anomalies (do not fit the {}/{}-profile, {}-fingerprint model)",
        tier_profile_count(Tier::Main),
        tier_profile_count(Tier::Alt),
        FINGERPRINTS.len(),
    )
}

/// The `## alt-tier carriers` section header, with the alt profile count and the Alt-tier
/// fingerprints derived from the model rather than hardcoded literals (M7).
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
pub fn check_folder(dir: &Path) -> anyhow::Result<i32> {
    let filenames = binarypb_names(dir)?;

    let legend = load_mapping_report(dir);

    let mut carriers: BTreeMap<String, Vec<(u64, String)>> = BTreeMap::new();
    let mut lte = 0usize;
    let mut mapping_files = 0usize;
    let mut unparseable: Vec<String> = Vec::new();

    for name in &filenames {
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

    let mut anomalies: Vec<(String, String)> = Vec::new();
    let mut stubs: Vec<String> = Vec::new();
    let mut alt_carriers: Vec<String> = Vec::new();
    let mut incomplete: Vec<(String, usize, usize, &'static str)> = Vec::new();

    for (carrier, files) in &carriers {
        let mut tier_votes: BTreeMap<&'static str, usize> = BTreeMap::new();
        let mut profiles_seen: BTreeSet<u64> = BTreeSet::new();

        for (number, name) in files {
            let anchors = matching_anchors(*number);
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
                anomalies.push((name.clone(), why));
                continue;
            }
            let profile = anchors[0];
            profiles_seen.insert(profile.anchor);

            let caps = read_ue_caps(&dir.join(name));
            let fp = caps.as_ref().map(|c| c.version);
            match fp.and_then(fp_info) {
                None => anomalies.push((
                    name.clone(),
                    format!(
                        "unknown fingerprint {}",
                        fp.map_or_else(|| "<none>".into(), |v| v.to_string())
                    ),
                )),
                Some((ffam, tier)) => {
                    let tier_key = tier_short(tier);
                    *tier_votes.entry(tier_key).or_insert(0) += 1;
                    if ffam != profile.family {
                        anomalies.push((
                            name.clone(),
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
                stubs.push(name.clone());
            }
        }

        // A real carrier's files all share one tier; a split vote is anomalous data, so
        // surface it explicitly rather than relying on the implicit BTreeMap-order tie-break
        // (which would silently classify a 8/8 split as "main" and expect 16 profiles).
        if tier_votes.len() > 1 {
            let tally: Vec<String> = tier_votes.iter().map(|(k, n)| format!("{k}={n}")).collect();
            anomalies.push((
                carrier.clone(),
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
            alt_carriers.push(carrier.clone());
        }
        let expected = if tier == "alt" {
            tier_profile_count(Tier::Alt)
        } else {
            tier_profile_count(Tier::Main)
        };
        if profiles_seen.len() != expected {
            incomplete.push((carrier.clone(), profiles_seen.len(), expected, tier));
        }
    }

    // Surface legend corruption the lenient collapse would otherwise hide (M1): the write
    // path (`root_to_map`) hard-errors on duplicate/empty names, so a read-only audit must
    // not report them as clean.
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

    let not_in_legend: Vec<String> = carriers
        .keys()
        .filter(|c| !legend.entries.contains_key(*c))
        .cloned()
        .collect();

    // ---- report ----
    println!(
        "=== folder check: {} ===",
        dir.canonicalize()
            .unwrap_or_else(|_| dir.to_path_buf())
            .display()
    );
    println!(
        "files: {}  |  carriers: {}  |  legend entries: {}\n",
        filenames.len(),
        carriers.len(),
        legend.entries.len()
    );

    println!("{}", anomalies_header());
    if anomalies.is_empty() {
        println!("   none");
    } else {
        for (name, why) in &anomalies {
            println!("   {name:<44} {why}");
        }
    }

    println!("\n## reference stubs (profile + fingerprint, but NO capability payload)");
    println!("   {} files", stubs.len());
    if !stubs.is_empty() {
        let mut by_carrier: BTreeMap<String, usize> = BTreeMap::new();
        for name in &stubs {
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

    println!("\n{}", alt_tier_header());
    println!(
        "   {}",
        if alt_carriers.is_empty() {
            "none".into()
        } else {
            alt_carriers.join(", ")
        }
    );

    println!("\n## carriers with files but ABSENT from the legend");
    if not_in_legend.is_empty() {
        println!("   none");
    } else {
        for c in &not_in_legend {
            println!("   {c}");
        }
    }

    println!("\n## incomplete profile sets (fewer files than the tier expects)");
    if incomplete.is_empty() {
        println!("   none");
    } else {
        for (c, got, exp, tier) in &incomplete {
            println!("   {c:<16} {got}/{exp} profiles ({tier} tier)");
        }
    }

    println!("\n## non-capability files");
    println!("   ap_plmn_mapping.binarypb : {mapping_files} (the legend)");
    println!("   lte_*.binarypb           : {lte} (LTE-only fallback)");
    println!(
        "   unparseable names        : {}",
        if unparseable.is_empty() {
            "none".into()
        } else {
            unparseable.join(", ")
        }
    );

    if anomalies.is_empty() { Ok(0) } else { Ok(1) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn headers_derive_counts_and_fingerprints_from_the_model() {
        // M7: the report headers reflect the model (PROFILES / MAIN_ONLY_ANCHORS /
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
        // M1: a legend with a duplicate carrier name or an empty name must not audit as
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
            code, 1,
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
            code, 1,
            "a carrier with mixed-tier fingerprints must not audit as clean (exit 0)"
        );
    }
}
