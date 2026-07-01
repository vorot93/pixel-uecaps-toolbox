//! Folder-wide consistency check (`check`).

use super::{binarypb_names, read_ue_caps};
use crate::{mapping::load_mapping, model::*, proto::UeCaps};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
};

// For carrier files the fingerprint (field 1) is always present, so "no payload"
// (field 3 absent) is exactly the stub condition.
const fn is_stub(caps: &UeCaps) -> bool {
    caps.combo_groups.is_empty()
}

// --------------------------------------------------------------------------- //
//  Folder-wide consistency check                                               //
// --------------------------------------------------------------------------- //
pub fn check_folder(dir: &Path) -> anyhow::Result<i32> {
    let filenames = binarypb_names(dir)?;

    let mapping = load_mapping(dir);

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

        let tier = tier_votes
            .iter()
            .max_by_key(|(_, n)| **n)
            .map_or("?", |(k, _)| *k);
        if tier == "alt" {
            alt_carriers.push(carrier.clone());
        }
        let expected = if tier == "alt" { 14 } else { 16 };
        if profiles_seen.len() != expected {
            incomplete.push((carrier.clone(), profiles_seen.len(), expected, tier));
        }
    }

    let not_in_legend: Vec<String> = carriers
        .keys()
        .filter(|c| !mapping.contains_key(*c))
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
        mapping.len()
    );

    println!("## genuine anomalies (do not fit the 16/14-profile, 4-fingerprint model)");
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

    println!("\n## alt-tier carriers (14 profiles, fingerprints 707802847/627223094)");
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
