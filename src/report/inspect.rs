//! Single-file analysis (`inspect`): text + KDL for carrier / LTE / mapping files.

use super::read_ue_caps;
use crate::{
    compiler::{
        emit_dl_feature, emit_lte_combo, emit_nr_combo, emit_ul_feature,
        features::{DlFeatureSource, UlFeatureSource},
        schema::{LteSourceCombo, NrSourceCombo},
    },
    factor::{factor_display, gcd},
    kdl_support::{finish_doc, opt_int_prop, str_list_node},
    mapping::load_mapping,
    model::*,
    report::combos::{build_combos, print_combos},
};
use kdl::{KdlDocument, KdlEntry, KdlNode};
use prost::Message;
use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
};

/// A PLMN integer as its `"<mcc>-<mnc>"` label. `decode_plmn` renders any masked/wildcard
/// nibble as `*`, which reads well as plain text but is not `mapping::Plmn`'s canonical
/// hex form (e.g. the ANY-MNC wildcard is `"**"` here, `"ff"` in `Plmn::Display`).
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
pub fn inspect(path: &Path, full: bool, as_kdl: bool) -> anyhow::Result<i32> {
    let base = path.file_name().and_then(|s| s.to_str()).unwrap_or("?");
    let dir: PathBuf = match path.parent() {
        Some(p) if !p.as_os_str().is_empty() => p.to_path_buf(),
        _ => PathBuf::from("."),
    };

    if as_kdl {
        let (text, code) = inspect_kdl(path, &dir, base)?;
        print!("{text}");
        return Ok(code);
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

/// The write-only `inspect --kdl` dump: builds compiler source DTOs from one file
/// (`nr_source_from_one_file` / `lte_source_from_one_file`) and emits them via the
/// compiler's `pub(crate)` writers. The Mapping branch is unchanged.
fn inspect_kdl(path: &Path, dir: &Path, base: &str) -> anyhow::Result<(String, i32)> {
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
            let v = MappingKdl {
                file: base.to_string(),
                kind: "mapping".into(),
                carrier,
            };
            Ok((v.to_kdl(), 0))
        }
        Parsed::Lte(number) => {
            let caps = std::fs::read(path)
                .ok()
                .and_then(|d| crate::proto::LteCaps::decode(&d[..]).ok());
            let readable = caps.is_some();
            let text = match caps {
                None => version_only_document(),
                Some(caps) => {
                    let combos = crate::compiler::lte_source_from_one_file(&caps);
                    render_lte_slice(&combos)
                }
            };
            let _ = number;
            Ok((text, i32::from(!readable)))
        }
        Parsed::Carrier {
            carrier: _,
            number: _,
        } => {
            // Single-file nr.kdl slice: version 1, per-file dl-feature/ul-feature
            // catalogs, per-file combos. No diagnostic envelope (text report has it);
            // no cross-file metadata (not synthesizable from one file).
            let caps = read_ue_caps(path);
            let code = i32::from(caps.is_none());
            let text = match caps {
                None => version_only_document(),
                Some(caps) => {
                    let (dl, ul, combos) = crate::compiler::nr_source_from_one_file(&caps);
                    render_nr_slice(&dl, &ul, &combos)?
                }
            };
            Ok((text, code))
        }
        Parsed::Other => {
            eprintln!("Not a recognised uecaps filename: {base}");
            Ok((String::new(), 2))
        }
    }
}

/// Render the per-file NR slice as `version 1` + dl-feature/ul-feature catalogs +
/// combos, all via the compiler's `pub(crate)` writers. No diagnostic envelope.
fn render_nr_slice(
    dl: &[DlFeatureSource],
    ul: &[UlFeatureSource],
    combos: &[NrSourceCombo],
) -> anyhow::Result<String> {
    let mut doc = KdlDocument::new();
    let mut version = KdlNode::new("version");
    version.push(KdlEntry::new(1i128));
    doc.nodes_mut().push(version);
    for f in dl {
        doc.nodes_mut().push(emit_dl_feature(f));
    }
    for f in ul {
        doc.nodes_mut().push(emit_ul_feature(f));
    }
    for combo in combos {
        doc.nodes_mut().push(emit_nr_combo(combo)?);
    }
    Ok(finish_doc(doc))
}

/// Render the per-file LTE slice as `version 1` + combos, via the compiler's
/// `pub(crate)` writer. No diagnostic envelope.
fn render_lte_slice(combos: &[LteSourceCombo]) -> String {
    let mut doc = KdlDocument::new();
    let mut version = KdlNode::new("version");
    version.push(KdlEntry::new(1i128));
    doc.nodes_mut().push(version);
    for combo in combos {
        doc.nodes_mut().push(emit_lte_combo(combo));
    }
    finish_doc(doc)
}

/// What inspect --kdl emits for an unreadable file: just `version 1` (no combos,
/// no catalogs). The text report carries the diagnostic; the slice is intentionally
/// empty so a stale `--kdl | build` round-trip can't fabricate data.
fn version_only_document() -> String {
    let mut doc = KdlDocument::new();
    let mut version = KdlNode::new("version");
    version.push(KdlEntry::new(1i128));
    doc.nodes_mut().push(version);
    finish_doc(doc)
}

fn inspect_lte(path: &Path, number: u64, full: bool) -> i32 {
    println!("LTE-only fallback config\n");

    let caps = std::fs::read(path)
        .ok()
        .and_then(|d| crate::proto::LteCaps::decode(&d[..]).ok());

    let fp_line = match &caps {
        Some(c) => {
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
        None => "in-file fp : (file not readable; filename-only analysis)".to_string(),
    };
    println!("{fp_line}");
    for line in super::lte::config_block(number) {
        println!("{line}");
    }

    if full {
        println!();
        println!("Number       : {number}");
        println!("  factored   : {}", factor_display(number));
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
    } else if is_regional_default(carrier) {
        println!("  (regional default / fallback config -- used when no operator-");
        println!("   specific config matches the serving network)");
    } else {
        println!("  (not present in ap_plmn_mapping.binarypb)");
    }
    println!();

    if full {
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

struct MapCarrier {
    name: String,
    index: Option<u64>,
    countries: Vec<String>,
}

impl MapCarrier {
    fn to_node(&self) -> KdlNode {
        let mut node = KdlNode::new("carrier");
        node.push(KdlEntry::new_prop("name", self.name.as_str()));
        opt_int_prop(&mut node, "index", self.index.map(i128::from));
        node.ensure_children()
            .nodes_mut()
            .push(str_list_node("countries", &self.countries));
        node
    }
}

struct MappingKdl {
    file: String,
    kind: String,
    carrier: Vec<MapCarrier>,
}

impl MappingKdl {
    fn to_kdl(&self) -> String {
        let mut node = KdlNode::new(self.kind.as_str());
        node.push(KdlEntry::new_prop("file", self.file.as_str()));
        node.push(KdlEntry::new_prop("type", self.kind.as_str()));
        if !self.carrier.is_empty() {
            let kids = node.ensure_children();
            for c in &self.carrier {
                kids.nodes_mut().push(c.to_node());
            }
        }
        let mut doc = KdlDocument::new();
        doc.nodes_mut().push(node);
        finish_doc(doc)
    }
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
        // M6: the note keys off the exact documented default, not `ends_with("COMMON")`,
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

    fn caps_bytes() -> Vec<u8> {
        use crate::proto::{ComboGroup, UeCaps, combo_group, combo_group::combo::SubBlock};
        use prost::Message;
        UeCaps {
            version: 874_888_686,
            combo_groups: vec![ComboGroup {
                combo_header: None,
                combo: vec![combo_group::Combo {
                    bitmask: Some(0),
                    sub_blocks: vec![SubBlock {
                        band: 10078,
                        dl_bw_class: Some(1),
                        ul_bw_class: Some(1),
                        ..Default::default()
                    }],
                }],
            }],
            ..Default::default()
        }
        .encode_to_vec()
    }

    #[test]
    fn inspect_kdl_carrier_emits_nr_kdl_slice() {
        let dir = std::env::temp_dir().join(format!("uecaps-kdl-slice-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let base = "VZW_3616442437.binarypb";
        let path = dir.join(base);
        std::fs::write(&path, caps_bytes()).unwrap();

        let (text, _code) = super::inspect_kdl(&path, &dir, base).unwrap();
        std::fs::remove_dir_all(&dir).ok();

        // Shape matches a one-file slice of nr.kdl.
        assert!(text.starts_with("version 1"), "{text}");
        assert!(text.contains("nr 78"), "{text}");
        assert!(!text.contains("type=carrier"), "{text}"); // envelope gone
        assert!(!text.contains("dl-scs-khz"), "{text}"); // display extension gone
        assert!(!text.contains("fingerprint-status"), "{text}");
        // The sample's only component has no resolved feature records (catalog empty):
        // no dl-feature/ul-feature top-level nodes, no dl-feature=N on the cc.
    }

    #[test]
    fn inspect_kdl_readable_file_exits_zero_even_when_ambiguous() {
        // R8 history: 3347 * 3539 is divisible by two anchors → ambiguous. Under the
        // one-file-slice shape, ambiguity is no longer a --kdl-level exit condition
        // (the diagnostic envelope that carried it is gone — the text report has it);
        // a readable file with an ambiguous NUMBER still exits 0.
        let dir = std::env::temp_dir().join(format!("uecaps-r8-kdl-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let number = 3347u64 * 3539;
        let base = format!("VZW_{number}.binarypb");
        let path = dir.join(&base);
        std::fs::write(&path, caps_bytes()).unwrap();

        let (_text, code) = super::inspect_kdl(&path, &dir, &base).unwrap();
        std::fs::remove_dir_all(&dir).ok();
        assert_eq!(
            code, 0,
            "a readable carrier file with an ambiguous NUMBER still exits 0; \
             the diagnostic moves to the text report"
        );
    }

    #[test]
    fn inspect_kdl_lte_emits_lte_kdl_slice() {
        use crate::proto::{LteCaps, LteCombo, LteComponent};
        use prost::Message;

        let dir = std::env::temp_dir().join(format!("uecaps-kdl-lte-slice-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let base = "lte_2160127815.binarypb";
        let path = dir.join(base);
        let caps = LteCaps {
            fingerprint: 862_505_271,
            bitmask: 0,
            combos: vec![LteCombo {
                components: vec![LteComponent {
                    band: 1,
                    dl_bw_class_mimo: 32768,
                    ul_bw_class_mimo: None,
                }],
                bcs: None,
                unknown1: None,
                unknown2: None,
            }],
        };
        std::fs::write(&path, caps.encode_to_vec()).unwrap();

        let (text, code) = super::inspect_kdl(&path, &dir, base).unwrap();
        std::fs::remove_dir_all(&dir).ok();

        assert_eq!(code, 0, "a readable LTE file exits 0");
        assert!(text.starts_with("version 1"), "{text}");
        assert!(text.contains("subblock 1 dl-bw-class-mimo=32768"), "{text}");
        // ul-bw-class-mimo is None — must be omitted (presence semantics match lte.kdl)
        assert!(!text.contains("ul-bw-class-mimo"), "{text}");
        // No diagnostic envelope
        assert!(!text.contains("config-family"), "{text}");
        assert!(!text.contains("fingerprint="), "{text}");
    }

    #[test]
    fn inspect_kdl_unreadable_carrier_exits_nonzero() {
        let dir = std::env::temp_dir().join(format!("uecaps-kdl-bad-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let base = "VZW_3616442437.binarypb";
        let path = dir.join(base);
        // Truncated field 3 — UeCaps::decode fails.
        std::fs::write(&path, [0x1a, 0x05, 0x01]).unwrap();

        let (text, code) = super::inspect_kdl(&path, &dir, base).unwrap();
        std::fs::remove_dir_all(&dir).ok();
        assert_eq!(code, 1, "an unreadable file must exit 1");
        assert!(
            text.starts_with("version 1") && !text.contains("combo"),
            "unreadable file yields version-only output: {text}"
        );
    }
}
