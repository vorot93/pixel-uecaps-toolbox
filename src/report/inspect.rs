//! Single-file analysis (`inspect`): text report for carrier / LTE / mapping files.

use super::read_ue_caps;
use crate::{
    factor::{factor_display, gcd},
    mapping::load_mapping,
    model::{
        Parsed, Profile, decode_plmn, family_desc, family_short, fp_info, matching_anchors,
        mcc_country, parse_name, tier_short,
    },
    outcome::Outcome,
    proto::UeCaps,
    report::{
        combos::{build_combos, print_combos},
        detail::Detail,
    },
};
use anyhow::Context;
use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
};

/// A PLMN integer as its `"<mcc>-<mnc>"` label. `decode_plmn` renders any masked/wildcard
/// nibble as `*`, which reads well as plain text but is not `mapping::Plmn`'s canonical
/// hex form (e.g. the ANY-MNC wildcard is `"**"` here, `"ff"` in `Plmn::Display`).
fn plmn_label(v: u64) -> String {
    // An out-of-range value is named as corrupt rather than silently truncated into a
    // different-but-plausible carrier — see `model::decode_plmn`.
    decode_plmn(v).map_or_else(
        || format!("<invalid PLMN {v}>"),
        |(mcc, mnc)| format!("{mcc}-{mnc}"),
    )
}

/// Distinct countries covered by a PLMN list, in first-seen order.
fn country_summary(plmns: &[u64]) -> Vec<String> {
    let mut seen = BTreeSet::new();
    let mut ordered = Vec::new();
    for &v in plmns {
        let name = match decode_plmn(v) {
            Some((mcc, _)) => mcc_country(&mcc).map_or_else(|| format!("MCC{mcc}"), str::to_string),
            None => "(invalid PLMN)".to_string(),
        };
        if seen.insert(name.clone()) {
            ordered.push(name);
        }
    }
    ordered
}

/// GCD of all sibling files' numbers = the carrier identity embedded in them.
fn carrier_signature(dir: &Path, carrier: &str, fallback: u64) -> (u64, usize) {
    let mut nums = Vec::new();
    if let Ok(rd) = std::fs::read_dir(dir) {
        for entry in rd.flatten() {
            if let Some(name) = entry.file_name().to_str()
                && let Parsed::Carrier { carrier: c, number } = parse_name(name)
                && c == carrier
            {
                nums.push(number);
            }
        }
    }
    if nums.is_empty() {
        nums.push(fallback);
    }
    let g = nums.iter().fold(0u64, |g, &x| gcd(g, x));
    (g, nums.len())
}

// --------------------------------------------------------------------------- //
//  Single-file analysis                                                        //
// --------------------------------------------------------------------------- //
pub fn inspect(path: &Path, detail: Detail) -> anyhow::Result<Outcome> {
    let base = path.file_name().and_then(|s| s.to_str()).unwrap_or("?");
    let dir: PathBuf = match path.parent() {
        Some(p) if !p.as_os_str().is_empty() => p.to_path_buf(),
        _ => PathBuf::from("."),
    };

    Ok(match parse_name(base) {
        Parsed::Mapping => {
            inspect_mapping(&dir);
            Outcome::Clean
        }
        Parsed::Lte(number) => {
            inspect_lte(path, number, detail);
            Outcome::Clean
        }
        Parsed::Carrier { carrier, number } => {
            inspect_carrier(path, &dir, &carrier, number, detail)
        }
        Parsed::Other => {
            eprintln!("Not a recognised uecaps filename: {base}");
            Outcome::Rejected
        }
    })
}

fn inspect_lte(path: &Path, number: u64, detail: Detail) {
    println!("LTE-only fallback config\n");

    let caps = std::fs::read(path)
        .with_context(|| format!("cannot read {}", path.display()))
        .and_then(|d| crate::wire::decode_lte_caps(&d, "LTE fallback config"));

    let fp_line = match &caps {
        Ok(c) => {
            let fp_suffix = match fp_info(c.fingerprint) {
                Some((fam, tier)) => format!(
                    "  [family {}, {} tier]",
                    family_short(fam),
                    tier_short(tier)
                ),
                None => "  [UNKNOWN fingerprint]".to_string(),
            };
            format!("in-file fp : {}{fp_suffix}", c.fingerprint)
        }
        Err(error) => {
            format!("in-file fp : (unavailable — {error:#}; filename-only analysis)")
        }
    };
    println!("{fp_line}");
    for line in super::lte::config_block(number) {
        println!("{line}");
    }

    if detail.is_full() {
        println!();
        println!("Number       : {number}");
        println!("  factored   : {}", factor_display(number));
        if let Ok(c) = &caps {
            println!("bitmask      : {}", c.bitmask);
        }
    }
    println!();

    match &caps {
        Ok(c) => super::lte::print_lte_combos(c, detail),
        Err(error) => println!("LTE band combinations: (unavailable — {error:#})"),
    }
}

/// Carrier-config names that are regional-default / fallback configs (legend-absent by
/// design) rather than operator-specific ones — the alt-tier reference data other operators'
/// stubs delegate to. These are **exact** documented identifiers (README: "the real alt-tier
/// data lives in `EU_COMMON1`"; DESIGN.md reference-stub note), not a `ends_with("COMMON")`
/// guess: that substring both over-matches (a crafted `FOO_COMMON` is not a default) and
/// misses the real default `EU_COMMON1`, which ends in "1".
const REGIONAL_DEFAULT_CARRIERS: &[&str] = &["EU_COMMON1"];

/// Whether `carrier` is a documented regional-default / fallback config.
fn is_regional_default(carrier: &str) -> bool {
    REGIONAL_DEFAULT_CARRIERS.contains(&carrier)
}

/// The carrier line plus its legend entry (PLMNs, countries) or the reason it has none.
fn print_carrier_identity(dir: &Path, carrier: &str, detail: Detail) {
    let mapping = load_mapping(dir);
    println!("Carrier      : {carrier}");
    if let Some(entry) = mapping.get(carrier) {
        if detail.is_full() {
            println!("  mapping idx: {}", entry.index);
        }
        let mut sample: Vec<String> = entry
            .plmns
            .iter()
            .take(10)
            .map(|&v| plmn_label(v))
            .collect();
        if entry.plmns.len() > 10 {
            sample.push("...".into());
        }
        println!("  PLMNs ({}) : {}", entry.plmns.len(), sample.join(", "));
        println!(
            "  countries  : {}",
            country_summary(&entry.plmns).join(", ")
        );
    } else if is_regional_default(carrier) {
        println!("  (regional default / fallback config -- used when no operator-");
        println!("   specific config matches the serving network)");
    } else {
        println!("  (not present in ap_plmn_mapping.binarypb)");
    }
    println!();
}

/// `--full` only: the trailing number, its factorization, and the carrier signature derived
/// from sibling files.
fn print_number_analysis(dir: &Path, carrier: &str, number: u64) {
    println!("Trailing number");
    println!("  value      : {number}");
    println!("  factored   : {}", factor_display(number));
    println!("  meaning    : carrier-identity  x  SKU-profile tag");
    println!();
    let (sig, nsib) = carrier_signature(dir, carrier, number);
    println!("Carrier signature (common factor of all of this carrier's files)");
    println!("  value      : {sig}   = {}", factor_display(sig));
    println!("  derived from: {nsib} sibling file(s) in this directory");
    if sig != 0 && number.is_multiple_of(sig) {
        println!("  SKU portion : {number} / {sig} = {}", number / sig);
    }
    println!();
}

/// The in-file-fingerprint status line for the SKU-profile block, plus its short tier when the
/// fingerprint is recognised: `(tier, "  in-file fp : …  […]")`.
fn fingerprint_status(caps: Option<&UeCaps>, profile: &Profile) -> (Option<&'static str>, String) {
    let Some(v) = caps.map(|c| c.version) else {
        return (
            None,
            "  in-file fp : (file not present; filename-only analysis)".to_string(),
        );
    };
    let Some((ffam, t)) = fp_info(v) else {
        return (None, format!("  in-file fp : {v}  [UNKNOWN fingerprint]"));
    };
    let status = if ffam == profile.family {
        "OK".to_string()
    } else {
        format!("MISMATCH: content is {}", family_desc(ffam))
    };
    (
        Some(tier_short(t)),
        format!("  in-file fp : {v}  [{status}]"),
    )
}

/// The SKU-profile block: which profile the number selects, whether the in-file fingerprint
/// agrees, and (`--full`) the selection rule. Returns `Outcome::Findings` for an unrecognised
/// profile.
fn print_sku_profile(caps: Option<&UeCaps>, carrier: &str, number: u64, detail: Detail) -> Outcome {
    let anchors = matching_anchors(number);
    let mut outcome = Outcome::Clean;
    if anchors.len() != 1 {
        let why = if anchors.is_empty() {
            "no anchor prime divides the number".to_string()
        } else {
            crate::model::ambiguous_anchors(&anchors)
        };
        println!("SKU profile  : UNRECOGNISED ({why})");
        outcome = Outcome::Findings;
    } else {
        let profile = anchors[0];
        let (tier_opt, fp_line) = fingerprint_status(caps, profile);
        println!("SKU profile  : {}", sku_profile_summary(profile, tier_opt));
        if detail.is_full() {
            println!(
                "  anchor prime: {}  ({number} mod {} == 0  OK)",
                profile.anchor, profile.anchor
            );
            let core: Vec<String> = profile.core.iter().map(u64::to_string).collect();
            println!("  full tag   : {}", core.join(" · "));
        }
        println!("{fp_line}");
        if detail.is_full() {
            println!();
            println!("Selection rule");
            println!(
                "  A Pixel whose SKU maps to profile {} loads THIS file, because it is",
                profile.anchor
            );
            println!(
                "  the unique {carrier} file whose number is divisible by {}.",
                profile.anchor
            );
        }
    }
    outcome
}

fn inspect_carrier(path: &Path, dir: &Path, carrier: &str, number: u64, detail: Detail) -> Outcome {
    println!("Carrier UE-capability profile\n");
    print_carrier_identity(dir, carrier, detail);
    if detail.is_full() {
        print_number_analysis(dir, carrier, number);
    }
    let caps = read_ue_caps(path);
    let outcome = print_sku_profile(caps.as_ref().ok(), carrier, number, detail);
    println!();
    match &caps {
        Ok(c) => print_combos(&build_combos(c), detail),
        // Naming the reason matters: strict validation now rejects wire-level corruption that
        // used to decode, and "not readable" would hide both that and a permissions problem.
        Err(error) => println!("Band combinations: (unavailable — {error:#})"),
    }
    outcome
}

fn inspect_mapping(dir: &Path) {
    let mapping = load_mapping(dir);
    println!("File type     : PLMN -> carrier legend (not a capability file)");
    println!("Carriers      : {}", mapping.len());
    println!();
    println!("Maps each network (PLMN) to a carrier-config name; those names are the");
    println!("<CARRIER> prefixes on the other .binarypb files.");
    if !mapping.is_empty() {
        println!();
        println!(
            "  {:<18} {:>4} {:>7}  countries",
            "carrier", "idx", "#PLMNs"
        );
        for (name, entry) in &mapping {
            let countries = country_summary(&entry.plmns);
            let head: Vec<&str> = countries.iter().take(6).map(String::as_str).collect();
            println!(
                "  {:<18} {:>4} {:>7}  {}",
                name,
                entry.index,
                entry.plmns.len(),
                head.join(", ")
            );
        }
    }
}

/// The text after "SKU profile  : " — anchor id, [family/tier], and the known model (if any).
fn sku_profile_summary(profile: &Profile, tier: Option<&str>) -> String {
    let mut s = match tier {
        Some(t) => format!(
            "{}  [family {}, {} tier]",
            profile.anchor,
            family_short(profile.family),
            t
        ),
        None => format!(
            "{}  [family {}]",
            profile.anchor,
            family_short(profile.family)
        ),
    };
    if let Some(m) = profile.model {
        s.push_str(" — ");
        s.push_str(m);
    }
    s
}

#[cfg(test)]
mod tests {
    use super::sku_profile_summary;
    use crate::model::{Family, PROFILES, Tier, family_short, tier_short};

    #[test]
    fn sku_profile_summary_renders_model_inline() {
        let with_model = PROFILES.iter().find(|p| p.anchor == 3_616_442_437).unwrap();
        let no_model = PROFILES.iter().find(|p| p.anchor == 8969).unwrap();
        assert_eq!(
            sku_profile_summary(with_model, Some("main")),
            "3616442437  [family A, main tier] — Pixel 10 Pro XL"
        );
        assert_eq!(
            sku_profile_summary(with_model, None),
            "3616442437  [family A] — Pixel 10 Pro XL"
        );
        assert_eq!(
            sku_profile_summary(no_model, Some("main")),
            "8969  [family A, main tier]"
        );
        assert_eq!(sku_profile_summary(no_model, None), "8969  [family A]");
    }

    #[test]
    fn regional_default_uses_exact_documented_identifier() {
        // The note keys off the exact documented default, not `ends_with("COMMON")`,
        // which both missed EU_COMMON1 (ends in "1") and over-matched crafted `*COMMON`.
        assert!(super::is_regional_default("EU_COMMON1"));
        assert!(!super::is_regional_default("EU_COMMON"));
        assert!(!super::is_regional_default("APAC_COMMON"));
        assert!(!super::is_regional_default("VZW"));
    }

    #[test]
    fn short_renderers() {
        assert_eq!(family_short(Family::A), "A");
        assert_eq!(family_short(Family::B), "B");
        assert_eq!(tier_short(Tier::Main), "main");
        assert_eq!(tier_short(Tier::Alt), "alt");
    }
}
