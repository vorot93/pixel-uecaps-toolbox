//! Single-file analysis (`inspect`): text + TOML for carrier / LTE / mapping files.

use super::read_ue_caps;
use crate::{
    factor::{factorize, format_factors, gcd},
    mapping::load_mapping,
    model::*,
    report::combos::{Combo, build_combos, print_combos},
};
use anyhow::Context;
use prost::Message;
use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
};

/// A PLMN integer as its `"<mcc>-<mnc>"` label.
fn plmn_label(v: u64) -> String {
    let (mcc, mnc) = decode_plmn(v);
    format!("{mcc}-{mnc}")
}

/// A mapping index as text, or `"-"` when absent.
fn idx_str(index: Option<u64>) -> String {
    index.map_or_else(|| "-".into(), |i| i.to_string())
}

/// Distinct countries covered by a PLMN list, in first-seen order.
fn country_summary(plmns: &[u64]) -> Vec<String> {
    let mut seen = BTreeSet::new();
    let mut ordered = Vec::new();
    for &v in plmns {
        let (mcc, _) = decode_plmn(v);
        let name = mcc_country(&mcc).map_or_else(|| format!("MCC{mcc}"), str::to_string);
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
pub fn inspect(path: &Path, full: bool, as_toml: bool) -> anyhow::Result<i32> {
    let base = path.file_name().and_then(|s| s.to_str()).unwrap_or("?");
    let dir: PathBuf = match path.parent() {
        Some(p) if !p.as_os_str().is_empty() => p.to_path_buf(),
        _ => PathBuf::from("."),
    };

    if as_toml {
        return inspect_toml(path, &dir, base);
    }

    Ok(match parse_name(base) {
        Parsed::Mapping => {
            inspect_mapping(&dir);
            0
        }
        Parsed::Lte(number) => inspect_lte(path, number, full),
        Parsed::Carrier { carrier, number } => inspect_carrier(path, &dir, &carrier, number, full),
        Parsed::Other => {
            eprintln!("Not a recognised uecaps filename: {base}");
            2
        }
    })
}

fn inspect_toml(path: &Path, dir: &Path, base: &str) -> anyhow::Result<i32> {
    match parse_name(base) {
        Parsed::Mapping => {
            let carrier = load_mapping(dir)
                .into_iter()
                .map(|(name, e)| MapCarrier {
                    name,
                    index: e.index,
                    countries: country_summary(&e.plmns),
                })
                .collect();
            let v = MappingToml {
                file: base.to_string(),
                kind: "mapping".into(),
                carrier,
            };
            print!(
                "{}",
                toml::to_string(&v).context("serialise mapping to TOML")?
            );
            Ok(0)
        }
        Parsed::Lte(number) => {
            let caps = std::fs::read(path)
                .ok()
                .and_then(|d| crate::proto::LteCaps::decode(&d[..]).ok());
            let (fingerprint, bitmask, combos) = match &caps {
                Some(c) => (c.fingerprint, c.bitmask, super::lte::lte_combos_toml(c)),
                None => (0, 0, Vec::new()),
            };
            let cfg = crate::model::lte_config(number);
            let v = LteToml {
                file: base.to_string(),
                kind: "lte".into(),
                fingerprint,
                bitmask,
                combo_count: combos.len(),
                combos,
                config_family: cfg.map(|c| c.family.to_string()),
                config_model: cfg.and_then(|c| c.model).map(String::from),
                category_codes: cfg
                    .map(|c| {
                        c.category_codes
                            .iter()
                            .map(|x| format!("0x{x:X}"))
                            .collect()
                    })
                    .unwrap_or_default(),
            };
            print!("{}", toml::to_string(&v).context("serialise lte to TOML")?);
            Ok(0)
        }
        Parsed::Carrier { carrier, number } => {
            let v = inspect_view(path, dir, &carrier, number, base);
            print!(
                "{}",
                toml::to_string(&v).context("serialise analysis to TOML")?
            );
            Ok(0)
        }
        Parsed::Other => {
            eprintln!("Not a recognised uecaps filename: {base}");
            Ok(2)
        }
    }
}

fn inspect_view(path: &Path, dir: &Path, carrier: &str, number: u64, base: &str) -> InspectToml {
    let mapping = load_mapping(dir);
    let (mapping_index, plmns, countries) = match mapping.get(carrier) {
        Some(e) => (
            e.index,
            e.plmns.iter().map(|&v| plmn_label(v)).collect(),
            country_summary(&e.plmns),
        ),
        None => (None, Vec::new(), Vec::new()),
    };
    let (sig, _) = carrier_signature(dir, carrier, number);
    let sku_portion = if sig != 0 && number.is_multiple_of(sig) {
        Some(number / sig)
    } else {
        None
    };
    let caps = read_ue_caps(path);
    let fingerprint = caps.as_ref().map_or(0, |c| c.version);
    let prof = identify_profile(number);
    let (profile, family) = match prof {
        Some(p) => (p.anchor.to_string(), family_short(p.family).to_string()),
        None => (String::new(), String::new()),
    };
    let (tier, fingerprint_status) = match fp_info(fingerprint) {
        Some((ffam, t)) => {
            let status = match prof {
                Some(p) if p.family == ffam => "ok",
                Some(_) => "mismatch",
                None => "unknown",
            };
            (tier_short(t).to_string(), status.to_string())
        }
        None => (String::new(), "unknown".to_string()),
    };
    let combo = caps.as_ref().map(build_combos).unwrap_or_default();
    let component_count = combo.iter().map(|c| c.cc.len()).sum();
    InspectToml {
        file: base.to_string(),
        kind: "carrier".into(),
        carrier: carrier.to_string(),
        mapping_index,
        plmns,
        countries,
        number,
        factored: format_factors(&factorize(number)),
        carrier_signature: sig,
        sku_portion,
        profile,
        model: prof.and_then(|p| p.model).map(String::from),
        family,
        tier,
        fingerprint,
        fingerprint_status,
        combo_group_count: caps.as_ref().map_or(0, |c| c.combo_groups.len()),
        combo_count: combo.len(),
        component_count,
        combo,
    }
}

fn inspect_lte(path: &Path, number: u64, full: bool) -> i32 {
    println!("LTE-only fallback config\n");

    let caps = std::fs::read(path)
        .ok()
        .and_then(|d| crate::proto::LteCaps::decode(&d[..]).ok());

    let fp = caps.as_ref().map_or(0, |c| c.fingerprint);
    let fp_suffix = match fp_info(fp) {
        Some((fam, tier)) => format!(
            "  [family {}, {} tier]",
            family_short(fam),
            tier_short(tier)
        ),
        None => "  [UNKNOWN fingerprint]".to_string(),
    };
    println!("in-file fp : {fp}{fp_suffix}");
    for line in super::lte::config_block(number) {
        println!("{line}");
    }

    if full {
        println!();
        println!("Number       : {number}");
        println!("  factored   : {}", format_factors(&factorize(number)));
        if let Some(c) = &caps {
            println!("bitmask      : {}", c.bitmask);
        }
    }
    println!();

    match &caps {
        Some(c) => super::lte::print_lte_combos(c, full),
        None => println!("LTE band combinations: (file not readable)"),
    }
    0
}

fn inspect_carrier(path: &Path, dir: &Path, carrier: &str, number: u64, full: bool) -> i32 {
    println!("Carrier UE-capability profile\n");

    let mapping = load_mapping(dir);
    println!("Carrier      : {carrier}");
    if let Some(entry) = mapping.get(carrier) {
        if full {
            let idx = idx_str(entry.index);
            println!("  mapping idx: {idx}");
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
    } else if carrier.ends_with("COMMON") {
        println!("  (regional default / fallback config -- used when no operator-");
        println!("   specific config matches the serving network)");
    } else {
        println!("  (not present in ap_plmn_mapping.binarypb)");
    }
    println!();

    if full {
        println!("Trailing number");
        println!("  value      : {number}");
        println!("  factored   : {}", format_factors(&factorize(number)));
        println!("  meaning    : carrier-identity  x  SKU-profile tag");
        println!();
        let (sig, nsib) = carrier_signature(dir, carrier, number);
        println!("Carrier signature (common factor of all of this carrier's files)");
        println!(
            "  value      : {sig}   = {}",
            format_factors(&factorize(sig))
        );
        println!("  derived from: {nsib} sibling file(s) in this directory");
        if sig != 0 && number.is_multiple_of(sig) {
            println!("  SKU portion : {number} / {sig} = {}", number / sig);
        }
        println!();
    }

    let caps = read_ue_caps(path);
    let anchors = matching_anchors(number);
    let mut ret = 0;
    if anchors.len() != 1 {
        let why = if anchors.is_empty() {
            "no anchor prime divides the number".to_string()
        } else {
            let anchor_ids: Vec<_> = anchors.iter().map(|p| p.anchor).collect();
            format!(
                "ambiguous: divisible by {} anchors {:?}",
                anchors.len(),
                anchor_ids
            )
        };
        println!("SKU profile  : UNRECOGNISED ({why})");
        ret = 1;
    } else {
        let profile = anchors[0];
        let fp = caps.as_ref().map(|c| c.version);
        let (tier_opt, fp_line) = match fp {
            Some(v) => match fp_info(v) {
                Some((ffam, t)) => {
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
                None => (None, format!("  in-file fp : {v}  [UNKNOWN fingerprint]")),
            },
            None => (
                None,
                "  in-file fp : (file not present; filename-only analysis)".to_string(),
            ),
        };
        println!("SKU profile  : {}", sku_profile_summary(profile, tier_opt));
        if full {
            println!(
                "  anchor prime: {}  ({number} mod {} == 0  OK)",
                profile.anchor, profile.anchor
            );
            let core: Vec<String> = profile.core.iter().map(u64::to_string).collect();
            println!("  full tag   : {}", core.join(" · "));
        }
        println!("{fp_line}");
        if full {
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
    println!();

    match &caps {
        Some(c) => print_combos(&build_combos(c), full),
        None => println!("Band combinations: (file not readable)"),
    }
    ret
}

fn inspect_mapping(dir: &Path) -> i32 {
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
            let idx = idx_str(entry.index);
            let countries = country_summary(&entry.plmns);
            let head: Vec<&str> = countries.iter().take(6).map(String::as_str).collect();
            println!(
                "  {:<18} {:>4} {:>7}  {}",
                name,
                idx,
                entry.plmns.len(),
                head.join(", ")
            );
        }
    }
    0
}

const fn family_short(f: Family) -> &'static str {
    match f {
        Family::A => "A",
        Family::B => "B",
    }
}

const fn tier_short(t: Tier) -> &'static str {
    match t {
        Tier::Main => "main",
        Tier::Alt => "alt",
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

/// Complete structured analysis of a carrier file (`inspect --toml`).
#[derive(serde::Serialize)]
struct InspectToml {
    file: String,
    #[serde(rename = "type")]
    kind: String,
    carrier: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    mapping_index: Option<u64>,
    plmns: Vec<String>,
    countries: Vec<String>,
    number: u64,
    factored: String,
    carrier_signature: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    sku_portion: Option<u64>,
    profile: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    model: Option<String>,
    family: String,
    tier: String,
    fingerprint: u64,
    fingerprint_status: String,
    combo_group_count: usize,
    combo_count: usize,
    component_count: usize,
    combo: Vec<Combo>,
}

#[derive(serde::Serialize)]
struct LteToml {
    file: String,
    #[serde(rename = "type")]
    kind: String,
    fingerprint: u64,
    bitmask: u64,
    combo_count: usize,
    combos: Vec<super::lte::LteComboToml>,
    #[serde(skip_serializing_if = "Option::is_none")]
    config_family: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    config_model: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    category_codes: Vec<String>,
}

#[derive(serde::Serialize)]
struct MapCarrier {
    name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    index: Option<u64>,
    countries: Vec<String>,
}

#[derive(serde::Serialize)]
struct MappingToml {
    file: String,
    #[serde(rename = "type")]
    kind: String,
    carrier: Vec<MapCarrier>,
}

#[cfg(test)]
mod tests {
    use super::{InspectToml, family_short, sku_profile_summary, tier_short};
    use crate::{
        model::{Family, PROFILES, Tier},
        report::combos::{Cc, Combo},
    };

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
    fn short_renderers() {
        assert_eq!(family_short(Family::A), "A");
        assert_eq!(family_short(Family::B), "B");
        assert_eq!(tier_short(Tier::Main), "main");
        assert_eq!(tier_short(Tier::Alt), "alt");
    }

    #[test]
    fn toml_carrier_analysis() {
        let mut view = InspectToml {
            file: "VZW_1.binarypb".into(),
            kind: "carrier".into(),
            carrier: "VZW".into(),
            mapping_index: Some(1),
            plmns: vec!["310-004".into()],
            countries: vec!["USA".into()],
            number: 193_698_151_252_893,
            factored: "3^5 · 7^2".into(),
            carrier_signature: 85_523,
            sku_portion: Some(2_264_866_191),
            profile: "3616442437".into(),
            model: Some("Pixel 10 Pro XL".into()),
            family: "A".into(),
            tier: "main".into(),
            fingerprint: 874_888_686,
            fingerprint_status: "ok".into(),
            combo_group_count: 1,
            combo_count: 1,
            component_count: 1,
            combo: vec![Combo {
                group: 1,
                index: 1,
                bands: "n1A".into(),
                power_class: Some(0),
                bcs_nr: None,
                bcs_intra_endc: None,
                bcs_eutra: None,
                intra_band_en_dc_support: None,
                bit_mask: 0,
                cc: vec![Cc {
                    band: "n1".into(),
                    bw_class_dl: Some(1),
                    bw_class_ul: Some(1),
                    dl_feature_index: Some(1),
                    ul_feature_index: Some(1),
                    dl_feature_per_cc_ids: Some(vec![0x0b]),
                    ul_feature_per_cc_ids: Some(vec![0x07]),
                    dl_feature_per_cc: None,
                    ul_feature_per_cc: None,
                    srs_tx_switch: None,
                    dl_scs_khz: Some(30),
                    dl_mimo: Some("4x4".into()),
                    dl_max_bw_mhz: Some(100),
                    dl_mod_order: Some("QAM256".into()),
                    dl_bw90mhz: Some(true),
                    ul_scs_khz: Some(30),
                    ul_mimo_cb: Some("Yes".into()),
                    ul_mimo_non_cb: Some(1),
                    ul_max_bw_mhz: Some(100),
                    ul_mod_order: Some("QAM256".into()),
                    ul_bw90mhz: None,
                }],
            }],
        };
        let out = toml::to_string(&view).unwrap();
        assert!(out.contains("type = \"carrier\""));
        assert!(out.contains("fingerprint_status = \"ok\""));
        assert!(out.contains("[[combo.cc]]"));
        assert!(!out.contains("srs_tx_switch")); // None omitted
        assert!(!out.contains("band_label"));
        assert!(!out.contains("nr ="));
        let parsed: toml::Value = toml::from_str(&out).unwrap();
        assert_eq!(parsed["type"].as_str(), Some("carrier"));
        assert_eq!(parsed["combo"][0]["cc"][0]["band"].as_str(), Some("n1"));
        assert!(out.contains("dl_max_bw_mhz = 100"));
        assert!(out.contains("dl_mimo = \"4x4\""));
        assert!(out.contains("dl_feature_per_cc_ids = [11]"), "{out}");
        assert!(out.contains("ul_feature_per_cc_ids = [7]"), "{out}");
        assert!(!out.contains("dl_feature_per_cc_ids = \"0b\""), "{out}");
        assert!(!out.contains("ul_feature_per_cc_ids = \"07\""), "{out}");
        assert!(!out.contains("ul_bw90mhz")); // None omitted
        assert!(out.contains("model = \"Pixel 10 Pro XL\""));
        view.model = None;
        let out_none = toml::to_string(&view).unwrap();
        assert!(!out_none.contains("model ="));
    }

    #[test]
    fn lte_toml_carries_config_fields() {
        let cfg = crate::model::lte_config(2_160_127_815).unwrap();
        let v = super::LteToml {
            file: "lte_2160127815.binarypb".into(),
            kind: "lte".into(),
            fingerprint: 862_505_271,
            bitmask: 0,
            combo_count: 0,
            combos: Vec::new(),
            config_family: Some(cfg.family.to_string()),
            config_model: cfg.model.map(String::from),
            category_codes: cfg
                .category_codes
                .iter()
                .map(|x| format!("0x{x:X}"))
                .collect(),
        };
        let s = toml::to_string(&v).unwrap();
        assert!(s.contains("config_family = \"sub6\""));
        assert!(s.contains("config_model = \"Pixel 9 / 9 Pro / 9 Pro XL, sub-6 (RoW)\""));
        assert!(s.contains("category_codes = [\"0x112\", \"0x122\", \"0x142\"]"));

        // known id WITHOUT a confirmed model (sta5_jp): family + codes serialize, config_model omitted
        let cfg2 = crate::model::lte_config(1_534_561_764).unwrap();
        assert_eq!(cfg2.family, "sta5_jp");
        assert_eq!(cfg2.model, None);
        let v3 = super::LteToml {
            file: "lte_1534561764.binarypb".into(),
            kind: "lte".into(),
            fingerprint: 0,
            bitmask: 0,
            combo_count: 0,
            combos: Vec::new(),
            config_family: Some(cfg2.family.to_string()),
            config_model: cfg2.model.map(String::from),
            category_codes: cfg2
                .category_codes
                .iter()
                .map(|x| format!("0x{x:X}"))
                .collect(),
        };
        let s3 = toml::to_string(&v3).unwrap();
        assert!(s3.contains("config_family = \"sta5_jp\""));
        assert!(s3.contains("category_codes = [\"0x814\"]"));
        assert!(!s3.contains("config_model"));

        let unknown = super::LteToml {
            file: "lte_1.binarypb".into(),
            kind: "lte".into(),
            fingerprint: 0,
            bitmask: 0,
            combo_count: 0,
            combos: Vec::new(),
            config_family: None,
            config_model: None,
            category_codes: Vec::new(),
        };
        let s2 = toml::to_string(&unknown).unwrap();
        assert!(!s2.contains("config_family"));
        assert!(!s2.contains("category_codes"));
        assert!(!s2.contains("config_model"));
    }
}
