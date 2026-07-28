//! The reverse-engineered UE-capabilities model.
//!
//! Profiled Exynos 5400 filenames are `<CARRIER>_<NUMBER>.binarypb`. The NUMBER is a
//! selector key:
//!
//! ```text
//! NUMBER = carrier-identity  ×  SKU-profile tag
//! ```
//!
//! Every profiled carrier ships one file per Pixel-SKU capability profile. A Pixel
//! loads the file whose NUMBER is divisible by its own SKU's profile tag (the
//! `anchor` prime), so the chosen file depends on the exact Pixel SKU. Older Tensor
//! Pixels instead use unnumbered carrier files with per-combination model bitmasks.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Family {
    A,
    B,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Tier {
    /// 16 profiles; US/EU/APAC majors carry full per-carrier capability data.
    Main,
    /// 14 profiles (no P15/P16); India + emerging markets. Per-operator files
    /// are tiny reference stubs that delegate to the EU_COMMON1 config.
    Alt,
}

pub(crate) struct Profile {
    /// Unique prime that divides the number of every file for this profile.
    pub anchor: u64,
    /// Full prime tag (one prime per SKU; several primes = a group of SKUs).
    pub core: &'static [u64],
    pub family: Family,
    /// Known real device model for this SKU profile; `None` when unknown.
    pub model: Option<&'static str>,
}

/// The 16 capability profiles.
pub(crate) static PROFILES: &[Profile] = &[
    Profile {
        anchor: 167,
        core: &[67, 167],
        family: Family::A,
        model: Some("Pixel 10 Pro Fold"),
    },
    Profile {
        anchor: 1847,
        core: &[83, 1847],
        family: Family::B,
        model: Some("Pixel 9 Pro (5G Sub-6 GHz)"),
    },
    Profile {
        anchor: 8969,
        core: &[233, 281, 8969],
        family: Family::A,
        model: None,
    },
    Profile {
        anchor: 688_679,
        core: &[331, 688_679],
        family: Family::A,
        model: None,
    },
    Profile {
        anchor: 224_309,
        core: &[293, 224_309],
        family: Family::B,
        model: Some("Pixel 9 Pro XL (5G Sub 6 GHz)"),
    },
    Profile {
        anchor: 196_911_437,
        core: &[196_911_437],
        family: Family::A,
        model: None,
    },
    Profile {
        anchor: 3_616_442_437,
        core: &[3_616_442_437],
        family: Family::A,
        model: Some("Pixel 10 Pro XL"),
    },
    Profile {
        anchor: 66_813_533,
        core: &[66_813_533],
        family: Family::B,
        model: Some("Pixel 9 (5G mmWave + Sub 6 GHz)"),
    },
    Profile {
        anchor: 1_176_929_627,
        core: &[1_176_929_627],
        family: Family::B,
        model: Some("Pixel 9 Pro (5G mmWave + Sub 6 GHz)"),
    },
    Profile {
        anchor: 154_921_957,
        core: &[154_921_957],
        family: Family::B,
        model: Some("Pixel 9 Pro XL (5G mmWave + Sub 6 GHz)"),
    },
    Profile {
        anchor: 3347,
        core: &[193, 3347],
        family: Family::A,
        model: None,
    },
    Profile {
        anchor: 1_002_739,
        core: &[97, 1_002_739],
        family: Family::A,
        model: None,
    },
    Profile {
        anchor: 6791,
        core: &[509, 6791],
        family: Family::B,
        model: Some("Pixel 9 Pro Fold"),
    },
    Profile {
        anchor: 1_334_093,
        core: &[3209, 1_334_093],
        family: Family::B,
        model: Some("Pixel 9 (5G Sub-6 GHz)"),
    },
    Profile {
        anchor: 2_912_407,
        core: &[2_912_407],
        family: Family::A,
        model: None,
    },
    Profile {
        anchor: 3539,
        core: &[89, 1013, 3539],
        family: Family::A,
        model: None,
    },
];

/// Anchors present only in the **Main** tier. The Alt tier ships 14 of the 16 profiles —
/// it lacks these two (P15/P16). Documented in README/AGENTS ("14 (no 2912407/3539)").
/// This gives the Alt-tier subset a code representation so the expected count is derived,
/// not a `14` literal that would drift silently if `PROFILES` changed.
pub(crate) const MAIN_ONLY_ANCHORS: &[u64] = &[2_912_407, 3539];

/// Expected profile-file count for a tier: `PROFILES.len()` for Main (16), minus the
/// Main-only anchors for Alt (14).
pub(crate) fn tier_profile_count(tier: Tier) -> usize {
    match tier {
        Tier::Main => PROFILES.len(),
        Tier::Alt => PROFILES.len() - MAIN_ONLY_ANCHORS.len(),
    }
}

/// The single profile whose anchor prime divides `number` (the normal case).
///
/// `None` for `0`, which [`matching_profiles`] handles for both callers: `0.is_multiple_of(x)`
/// is true for every nonzero `x`, so an unguarded search would spuriously return the *first*
/// profile and an unguarded filter would return all of them.
pub(crate) fn identify_profile(number: u64) -> Option<&'static Profile> {
    matching_profiles(number).next()
}

/// Every profile whose anchor divides `number` (>1 means an ambiguous file).
pub(crate) fn matching_anchors(number: u64) -> Vec<&'static Profile> {
    matching_profiles(number).collect()
}

/// The single rule both of the above ask: which profiles' anchor primes divide `number`.
///
/// Sharing it is the point. `identify_profile` carried the `number == 0` guard and
/// `matching_anchors` did not, so the two disagreed about the same input: `0` is a multiple of
/// every nonzero anchor, and `matching_anchors(0)` returned all 16 profiles. A
/// `<CARRIER>_0.binarypb` was therefore reported as "ambiguous: divisible by 16 anchors" by
/// `check`/`inspect` and given a wholly fabricated 16-column row by `matrix`, instead of being
/// recognized as the degenerate filename that belongs to no profile.
fn matching_profiles(number: u64) -> impl Iterator<Item = &'static Profile> {
    PROFILES
        .iter()
        .filter(move |p| number != 0 && number.is_multiple_of(p.anchor))
}

/// The "ambiguous SKU" message shared by the `check` and `inspect` report paths: a number
/// divisible by more than one anchor prime cannot pick a single profile.
pub(crate) fn ambiguous_anchors(anchors: &[&'static Profile]) -> String {
    let anchor_ids: Vec<_> = anchors.iter().map(|p| p.anchor).collect();
    format!(
        "ambiguous: divisible by {} anchors {:?}",
        anchors.len(),
        anchor_ids
    )
}

/// A modem LTE-config selection-table entry: the `lte_<id>` filename number, the Shannon
/// firmware family name, the hardware/SKU category codes that select it, and the confirmed
/// Pixel model (`None` when only the raw family is known).
pub(crate) struct LteConfig {
    pub id: u64,
    pub family: &'static str,
    pub category_codes: &'static [u32],
    pub model: Option<&'static str>,
}

/// The modem's LTE-config selection table (from `g5400c-main.bin`). The id is the
/// `lte_<id>.binarypb` filename number; selection is hardware/SKU-category driven, not SIM/MCC.
pub(crate) static LTE_CONFIGS: &[LteConfig] = &[
    LteConfig {
        id: 400_907_661,
        family: "mmw",
        category_codes: &[0x111, 0x121, 0x141],
        model: Some("Pixel 9 / 9 Pro / 9 Pro XL, mmWave (US)"),
    },
    LteConfig {
        id: 2_160_127_815,
        family: "sub6",
        category_codes: &[0x112, 0x122, 0x142],
        model: Some("Pixel 9 / 9 Pro / 9 Pro XL, sub-6 (RoW)"),
    },
    LteConfig {
        id: 4_210_990_300,
        family: "ct3",
        category_codes: &[0x181],
        model: Some("Pixel 9 Pro Fold"),
    },
    LteConfig {
        id: 564_260_317,
        family: "tki3",
        category_codes: &[0x211],
        model: None,
    },
    LteConfig {
        id: 1_254_026_417,
        family: "mmw_p25",
        category_codes: &[0x411, 0x421, 0x441],
        model: Some("Pixel 10 / 10 Pro / 10 Pro XL, mmWave (US)"),
    },
    LteConfig {
        id: 4_017_061_044,
        family: "sub6_p25",
        category_codes: &[0x412, 0x422, 0x442],
        model: Some("Pixel 10 / 10 Pro / 10 Pro XL, sub-6 (RoW)"),
    },
    LteConfig {
        id: 2_306_930_561,
        family: "rg5",
        category_codes: &[0x481],
        model: Some("Pixel 10 Pro Fold"),
    },
    LteConfig {
        id: 844_857_560,
        family: "sta5_na",
        category_codes: &[0x812],
        model: None,
    },
    LteConfig {
        id: 1_534_561_764,
        family: "sta5_jp",
        category_codes: &[0x814],
        model: None,
    },
];

/// The modem selection-table entry for an `lte_<id>` file, if known.
pub(crate) fn lte_config(id: u64) -> Option<&'static LteConfig> {
    LTE_CONFIGS.iter().find(|c| c.id == id)
}

/// The on-device capability-file layout selected by a phone model.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CapabilityLayout {
    /// Unnumbered carrier files with per-combination model bitmasks.
    Bitmask,
    /// Numbered carrier profiles plus a hardware-selected LTE fallback file.
    Profiled { nr_anchor: u64, lte_id: u64 },
}

/// A known phone model and the capability-file layout it selects.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PhoneModel {
    /// CLI token (Google 5-char model code), e.g. `"GUL82"`.
    pub code: &'static str,
    pub layout: CapabilityLayout,
}

impl PhoneModel {
    /// Whether this model uses the legacy unnumbered bitmask layout.
    pub const fn is_bitmask(self) -> bool {
        matches!(self.layout, CapabilityLayout::Bitmask)
    }
}

/// Registered provision targets keyed by Google's 5-character hardware code. The bitmask
/// entries are the complete legacy Tensor set in the pinned `pixel-bands` snapshot;
/// the profiled entries retain the exact evidence-backed anchor/LTE-id mappings the
/// compiler resolves a `provision` target against.
pub static PHONE_MODELS: &[PhoneModel] = &[
    PhoneModel {
        code: "G0DZQ",
        layout: CapabilityLayout::Bitmask,
    },
    PhoneModel {
        code: "G1AZG",
        layout: CapabilityLayout::Bitmask,
    },
    PhoneModel {
        code: "G1MNW",
        layout: CapabilityLayout::Bitmask,
    },
    PhoneModel {
        code: "G3Y12",
        layout: CapabilityLayout::Bitmask,
    },
    PhoneModel {
        code: "G576D",
        layout: CapabilityLayout::Bitmask,
    },
    PhoneModel {
        code: "G82U8",
        layout: CapabilityLayout::Bitmask,
    },
    PhoneModel {
        code: "G8HHN",
        layout: CapabilityLayout::Bitmask,
    },
    PhoneModel {
        code: "G8V0U",
        layout: CapabilityLayout::Bitmask,
    },
    PhoneModel {
        code: "G9BQD",
        layout: CapabilityLayout::Bitmask,
    },
    PhoneModel {
        code: "G9FPL",
        layout: CapabilityLayout::Bitmask,
    },
    PhoneModel {
        code: "G9S9B",
        layout: CapabilityLayout::Bitmask,
    },
    PhoneModel {
        code: "GB17L",
        layout: CapabilityLayout::Bitmask,
    },
    PhoneModel {
        code: "GB62Z",
        layout: CapabilityLayout::Bitmask,
    },
    PhoneModel {
        code: "GB7N6",
        layout: CapabilityLayout::Bitmask,
    },
    PhoneModel {
        code: "GC3VE",
        layout: CapabilityLayout::Bitmask,
    },
    PhoneModel {
        code: "GE2AE",
        layout: CapabilityLayout::Bitmask,
    },
    PhoneModel {
        code: "GE9DP",
        layout: CapabilityLayout::Bitmask,
    },
    PhoneModel {
        code: "GF5KQ",
        layout: CapabilityLayout::Bitmask,
    },
    PhoneModel {
        code: "GFE4J",
        layout: CapabilityLayout::Bitmask,
    },
    PhoneModel {
        code: "GHL1X",
        layout: CapabilityLayout::Bitmask,
    },
    PhoneModel {
        code: "GKWS6",
        layout: CapabilityLayout::Bitmask,
    },
    PhoneModel {
        code: "GLU0G",
        layout: CapabilityLayout::Bitmask,
    },
    PhoneModel {
        code: "GO3Z5",
        layout: CapabilityLayout::Bitmask,
    },
    PhoneModel {
        code: "GOB96",
        layout: CapabilityLayout::Bitmask,
    },
    PhoneModel {
        code: "GP4BC",
        layout: CapabilityLayout::Bitmask,
    },
    PhoneModel {
        code: "GPJ41",
        layout: CapabilityLayout::Bitmask,
    },
    PhoneModel {
        code: "GQML3",
        layout: CapabilityLayout::Bitmask,
    },
    PhoneModel {
        code: "GR1YH",
        layout: CapabilityLayout::Bitmask,
    },
    PhoneModel {
        code: "GTF7P",
        layout: CapabilityLayout::Bitmask,
    },
    PhoneModel {
        code: "GVU6C",
        layout: CapabilityLayout::Bitmask,
    },
    PhoneModel {
        code: "GWKK3",
        layout: CapabilityLayout::Bitmask,
    },
    PhoneModel {
        code: "GX7AS",
        layout: CapabilityLayout::Bitmask,
    },
    PhoneModel {
        code: "GXQ96",
        layout: CapabilityLayout::Bitmask,
    },
    PhoneModel {
        code: "GZPFO",
        layout: CapabilityLayout::Bitmask,
    },
    PhoneModel {
        code: "G2YBB",
        layout: CapabilityLayout::Profiled {
            nr_anchor: 66_813_533,
            lte_id: 400_907_661,
        },
    },
    PhoneModel {
        code: "GUR25",
        layout: CapabilityLayout::Profiled {
            nr_anchor: 1_334_093,
            lte_id: 2_160_127_815,
        },
    },
    PhoneModel {
        code: "GKV4X",
        layout: CapabilityLayout::Profiled {
            nr_anchor: 1_334_093,
            lte_id: 2_160_127_815,
        },
    },
    PhoneModel {
        code: "G6GPR",
        layout: CapabilityLayout::Profiled {
            nr_anchor: 1_334_093,
            lte_id: 2_160_127_815,
        },
    },
    PhoneModel {
        code: "G1B60",
        layout: CapabilityLayout::Profiled {
            nr_anchor: 1_334_093,
            lte_id: 2_160_127_815,
        },
    },
    PhoneModel {
        code: "GR83Y",
        layout: CapabilityLayout::Profiled {
            nr_anchor: 1_176_929_627,
            lte_id: 400_907_661,
        },
    },
    PhoneModel {
        code: "GEC77",
        layout: CapabilityLayout::Profiled {
            nr_anchor: 1847,
            lte_id: 2_160_127_815,
        },
    },
    PhoneModel {
        code: "GWVK6",
        layout: CapabilityLayout::Profiled {
            nr_anchor: 1847,
            lte_id: 2_160_127_815,
        },
    },
    PhoneModel {
        code: "GGX8B",
        layout: CapabilityLayout::Profiled {
            nr_anchor: 154_921_957,
            lte_id: 400_907_661,
        },
    },
    PhoneModel {
        code: "GZC4K",
        layout: CapabilityLayout::Profiled {
            nr_anchor: 224_309,
            lte_id: 2_160_127_815,
        },
    },
    PhoneModel {
        code: "GQ57S",
        layout: CapabilityLayout::Profiled {
            nr_anchor: 224_309,
            lte_id: 2_160_127_815,
        },
    },
    PhoneModel {
        code: "GGH2X",
        layout: CapabilityLayout::Profiled {
            nr_anchor: 6791,
            lte_id: 4_210_990_300,
        },
    },
    PhoneModel {
        code: "GC15S",
        layout: CapabilityLayout::Profiled {
            nr_anchor: 6791,
            lte_id: 4_210_990_300,
        },
    },
    PhoneModel {
        code: "GU0NP",
        layout: CapabilityLayout::Profiled {
            nr_anchor: 167,
            lte_id: 2_306_930_561,
        },
    },
    PhoneModel {
        code: "GM66V",
        layout: CapabilityLayout::Profiled {
            nr_anchor: 167,
            lte_id: 2_306_930_561,
        },
    },
    // Pixel 10 Pro XL: US (mmWave) and RoW/JP (sub-6) share one NR profile (anchor 3616442437) but differ in lte_id.
    PhoneModel {
        code: "GUL82",
        layout: CapabilityLayout::Profiled {
            nr_anchor: 3_616_442_437,
            lte_id: 1_254_026_417,
        },
    },
    PhoneModel {
        code: "G45RY",
        layout: CapabilityLayout::Profiled {
            nr_anchor: 3_616_442_437,
            lte_id: 4_017_061_044,
        },
    },
    PhoneModel {
        code: "GYPW4",
        layout: CapabilityLayout::Profiled {
            nr_anchor: 3_616_442_437,
            lte_id: 4_017_061_044,
        },
    },
];

/// Look up a phone model by its CLI code after trimming and ASCII upper-casing it.
pub(crate) fn phone_model(code: &str) -> Option<&'static PhoneModel> {
    let code = code.trim();
    PHONE_MODELS
        .iter()
        .find(|m| m.code.eq_ignore_ascii_case(code))
}

/// Registered model codes matching `keep`, in lexical order — the shared body of the three
/// registry lookups below.
fn sorted_model_codes(keep: impl Fn(&PhoneModel) -> bool) -> Vec<&'static str> {
    let mut codes: Vec<_> = PHONE_MODELS
        .iter()
        .filter(|m| keep(m))
        .map(|m| m.code)
        .collect();
    codes.sort_unstable();
    codes
}

/// Registered profiled model codes that select `anchor`, in lexical order.
pub(crate) fn profile_model_codes(anchor: u64) -> Vec<&'static str> {
    sorted_model_codes(
        |m| matches!(m.layout, CapabilityLayout::Profiled { nr_anchor, .. } if nr_anchor == anchor),
    )
}

/// Registered profiled model codes that select LTE file `id`, in lexical order.
pub(crate) fn lte_model_codes(id: u64) -> Vec<&'static str> {
    sorted_model_codes(
        |m| matches!(m.layout, CapabilityLayout::Profiled { lte_id, .. } if lte_id == id),
    )
}

/// Every registered model code, in lexical order.
pub(crate) fn known_model_codes() -> Vec<&'static str> {
    sorted_model_codes(|_| true)
}

/// Every known in-file capability fingerprint (protobuf field 1) with its (family, tier).
/// The single source for [`fp_info`] and for deriving per-tier fingerprint sets, so report
/// strings (e.g. `check`'s headers) don't hardcode the values. Currently 4: 2 families ×
/// 2 tiers.
pub(crate) const FINGERPRINTS: &[(u64, Family, Tier)] = &[
    (874_888_686, Family::A, Tier::Main),
    (862_505_271, Family::B, Tier::Main),
    (707_802_847, Family::A, Tier::Alt),
    (627_223_094, Family::B, Tier::Alt),
];

/// In-file capability fingerprint (protobuf field 1) -> (family, tier).
pub(crate) const fn fp_info(fp: u64) -> Option<(Family, Tier)> {
    let mut i = 0;
    while i < FINGERPRINTS.len() {
        let (id, family, tier) = FINGERPRINTS[i];
        if id == fp {
            return Some((family, tier));
        }
        i += 1;
    }
    None
}

/// The fingerprint for a `(family, tier)` pair — the forward of [`fp_info`], from the same
/// [`FINGERPRINTS`] table. `None` if the pair is not a known combination (all four are).
pub(crate) fn fingerprint_for(family: Family, tier: Tier) -> Option<u64> {
    FINGERPRINTS
        .iter()
        .find(|(_, f, t)| *f == family && *t == tier)
        .map(|(id, _, _)| *id)
}

/// The known fingerprints for `tier`, in table order (family A before B).
pub(crate) fn tier_fingerprints(tier: Tier) -> Vec<u64> {
    FINGERPRINTS
        .iter()
        .filter(|(_, _, t)| *t == tier)
        .map(|(id, _, _)| *id)
        .collect()
}

pub(crate) const fn family_desc(f: Family) -> &'static str {
    match f {
        Family::A => "capability family A",
        Family::B => "capability family B",
    }
}

/// Family as a short label: `"A"` / `"B"` (the compact form; cf. `family_desc`).
pub(crate) const fn family_short(f: Family) -> &'static str {
    match f {
        Family::A => "A",
        Family::B => "B",
    }
}

/// Tier as a short key: `"main"` / `"alt"`.
pub(crate) const fn tier_short(t: Tier) -> &'static str {
    match t {
        Tier::Main => "main",
        Tier::Alt => "alt",
    }
}

/// What a filename refers to.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Parsed {
    /// `ap_plmn_mapping.binarypb` — the PLMN→carrier legend.
    Mapping,
    /// `lte_<n>.binarypb` — LTE-only fallback, outside the profile scheme.
    Lte(u64),
    /// `<CARRIER>_<NUMBER>.binarypb`.
    Carrier { carrier: String, number: u64 },
    /// Anything else.
    Other,
}

pub(crate) fn parse_name(filename: &str) -> Parsed {
    let base = std::path::Path::new(filename)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or(filename);
    if base == "ap_plmn_mapping.binarypb" {
        return Parsed::Mapping;
    }
    let Some(stem) = base.strip_suffix(".binarypb") else {
        return Parsed::Other;
    };
    let Some((prefix, num)) = stem.rsplit_once('_') else {
        return Parsed::Other;
    };
    let Ok(number) = num.parse::<u64>() else {
        return Parsed::Other;
    };
    match prefix {
        "lte" => Parsed::Lte(number),
        "" => Parsed::Other,
        _ => Parsed::Carrier {
            carrier: prefix.to_string(),
            number,
        },
    }
}

/// Decode a 3GPP packed-BCD PLMN integer into (MCC, MNC). Filler/hex nibbles
/// (0xA-0xF) render as `*` (wildcard, or the 2-digit-MNC marker for MNC digit 3).
/// Shares the packed-BCD layout with the canonical `mapping::Plmn`.
///
/// `None` for a value that does not fit a PLMN's 24 bits. `Carrier.plmns` is `uint64` on the
/// wire, so those extra bits are real data: masking them away (as this once did) renders a
/// corrupt entry as a different but entirely plausible carrier, which is the opposite of what
/// an audit surface should do. The compiler path rejects the same input via
/// `Error::PlmnOutOfRange`, and this now agrees with it.
pub(crate) fn decode_plmn(v: u64) -> Option<(String, String)> {
    let plmn = crate::mapping::Plmn::from_encoded(v).ok()?;
    let (mcc_n, mnc_n, mnc3) = plmn.nibbles();
    let d = |x: u8| if x < 10 { (b'0' + x) as char } else { '*' };
    let mcc: String = mcc_n.iter().map(|&x| d(x)).collect();
    let mut mnc: String = mnc_n.iter().map(|&x| d(x)).collect();
    if mnc3 != 0xf {
        mnc.push(d(mnc3));
    }
    Some((mcc, mnc))
}

/// MCC -> country/territory for the regions present in the dataset.
pub(crate) fn mcc_country(mcc: &str) -> Option<&'static str> {
    Some(match mcc {
        "302" => "Canada",
        "310" | "311" | "312" | "313" | "316" => "USA",
        "334" => "Mexico",
        "724" => "Brazil",
        "730" => "Chile",
        "732" => "Colombia",
        "202" => "Greece",
        "204" => "Netherlands",
        "206" => "Belgium",
        "208" => "France",
        "212" => "Monaco",
        "213" => "Andorra",
        "214" => "Spain",
        "216" => "Hungary",
        "218" => "Bosnia",
        "219" => "Croatia",
        "220" => "Serbia",
        "222" => "Italy",
        "226" => "Romania",
        "228" => "Switzerland",
        "230" => "Czechia",
        "231" => "Slovakia",
        "232" => "Austria",
        "234" | "235" => "UK",
        "238" => "Denmark",
        "240" => "Sweden",
        "242" => "Norway",
        "244" => "Finland",
        "246" => "Lithuania",
        "247" => "Latvia",
        "248" => "Estonia",
        "250" => "Russia",
        "255" => "Ukraine",
        "260" => "Poland",
        "262" => "Germany",
        "266" => "Gibraltar",
        "268" => "Portugal",
        "270" => "Luxembourg",
        "272" => "Ireland",
        "274" => "Iceland",
        "276" => "Albania",
        "278" => "Malta",
        "280" => "Cyprus",
        "284" => "Bulgaria",
        "286" => "Turkey",
        "293" => "Slovenia",
        "294" => "N.Macedonia",
        "295" => "Liechtenstein",
        "647" => "Reunion (FR)",
        "404" | "405" | "406" => "India",
        "410" => "Pakistan",
        "413" => "Sri Lanka",
        "414" => "Myanmar",
        "419" => "Kuwait",
        "420" => "Saudi Arabia",
        "425" => "Israel",
        "426" => "Bahrain",
        "427" => "Qatar",
        "440" | "441" => "Japan",
        "450" => "South Korea",
        "452" => "Vietnam",
        "454" => "Hong Kong",
        "455" => "Macau",
        "456" => "Cambodia",
        "457" => "Laos",
        "460" => "China",
        "466" => "Taiwan",
        "470" => "Bangladesh",
        "472" => "Maldives",
        "502" => "Malaysia",
        "505" => "Australia",
        "510" => "Indonesia",
        "515" => "Philippines",
        "520" => "Thailand",
        "525" => "Singapore",
        "528" => "Brunei",
        "530" => "New Zealand",
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const BITMASK_CODES: &[&str] = &[
        "G0DZQ", "G1AZG", "G1MNW", "G3Y12", "G576D", "G82U8", "G8HHN", "G8V0U", "G9BQD", "G9FPL",
        "G9S9B", "GB17L", "GB62Z", "GB7N6", "GC3VE", "GE2AE", "GE9DP", "GF5KQ", "GFE4J", "GHL1X",
        "GKWS6", "GLU0G", "GO3Z5", "GOB96", "GP4BC", "GPJ41", "GQML3", "GR1YH", "GTF7P", "GVU6C",
        "GWKK3", "GX7AS", "GXQ96", "GZPFO",
    ];

    const PROFILED_MAPPINGS: &[(&str, u64, u64)] = &[
        ("G2YBB", 66_813_533, 400_907_661),
        ("GUR25", 1_334_093, 2_160_127_815),
        ("GKV4X", 1_334_093, 2_160_127_815),
        ("G6GPR", 1_334_093, 2_160_127_815),
        ("G1B60", 1_334_093, 2_160_127_815),
        ("GR83Y", 1_176_929_627, 400_907_661),
        ("GEC77", 1847, 2_160_127_815),
        ("GWVK6", 1847, 2_160_127_815),
        ("GGX8B", 154_921_957, 400_907_661),
        ("GZC4K", 224_309, 2_160_127_815),
        ("GQ57S", 224_309, 2_160_127_815),
        ("GGH2X", 6791, 4_210_990_300),
        ("GC15S", 6791, 4_210_990_300),
        ("GU0NP", 167, 2_306_930_561),
        ("GM66V", 167, 2_306_930_561),
        ("GUL82", 3_616_442_437, 1_254_026_417),
        ("G45RY", 3_616_442_437, 4_017_061_044),
        ("GYPW4", 3_616_442_437, 4_017_061_044),
    ];

    const UNREGISTERED_PROFILED_CODES: &[&str] = &[
        "GLBW0", "GL066", "GK2MP", "G4QUR", "GN4F5", "GEHN3", "GE1GQ", "GV0BP", "G4H7L",
    ];

    #[test]
    fn registry_profiled_mappings_are_preserved() {
        let actual: Vec<_> = PHONE_MODELS
            .iter()
            .filter_map(|m| match m.layout {
                CapabilityLayout::Bitmask => None,
                CapabilityLayout::Profiled { nr_anchor, lte_id } => {
                    Some((m.code, nr_anchor, lte_id))
                }
            })
            .collect();
        assert_eq!(actual, PROFILED_MAPPINGS);
    }

    #[test]
    fn registry_bitmask_codes_are_exact() {
        let mut actual: Vec<_> = PHONE_MODELS
            .iter()
            .filter(|m| m.layout == CapabilityLayout::Bitmask)
            .map(|m| m.code)
            .collect();
        actual.sort_unstable();
        assert_eq!(actual, BITMASK_CODES);
    }

    #[test]
    fn registry_known_codes_are_sorted_and_exact() {
        let mut expected = BITMASK_CODES.to_vec();
        expected.extend(PROFILED_MAPPINGS.iter().map(|(code, _, _)| *code));
        expected.sort_unstable();
        assert_eq!(known_model_codes(), expected);
    }

    #[test]
    fn registry_reverse_lookups_are_sorted_and_exact() {
        for profile in PROFILES {
            let mut expected: Vec<_> = PROFILED_MAPPINGS
                .iter()
                .filter(|(_, nr_anchor, _)| *nr_anchor == profile.anchor)
                .map(|(code, _, _)| *code)
                .collect();
            expected.sort_unstable();
            assert_eq!(profile_model_codes(profile.anchor), expected);
        }
        assert!(profile_model_codes(u64::MAX).is_empty());

        for config in LTE_CONFIGS {
            let mut expected: Vec<_> = PROFILED_MAPPINGS
                .iter()
                .filter(|(_, _, lte_id)| *lte_id == config.id)
                .map(|(code, _, _)| *code)
                .collect();
            expected.sort_unstable();
            assert_eq!(lte_model_codes(config.id), expected);
        }
        assert!(lte_model_codes(u64::MAX).is_empty());
    }

    #[test]
    fn registry_lookup_normalizes_case_and_whitespace() {
        assert_eq!(phone_model(" gul82\n").map(|m| m.code), Some("GUL82"));
        assert_eq!(phone_model(" g0dzq\t").map(|m| m.code), Some("G0DZQ"));
    }

    #[test]
    fn registry_unregistered_profiled_codes_stay_excluded() {
        let known = known_model_codes();
        let bitmask: std::collections::BTreeSet<_> = PHONE_MODELS
            .iter()
            .filter(|m| m.layout == CapabilityLayout::Bitmask)
            .map(|m| m.code)
            .collect();

        for code in UNREGISTERED_PROFILED_CODES {
            assert!(
                phone_model(code).is_none(),
                "{code} unexpectedly registered"
            );
            assert!(!known.contains(code), "{code} unexpectedly known");
            assert!(!bitmask.contains(code), "{code} misclassified as bitmask");
        }
    }

    #[test]
    fn profiles_are_complete_and_unique() {
        assert_eq!(PROFILES.len(), 16);
        let anchors: std::collections::BTreeSet<u64> = PROFILES.iter().map(|p| p.anchor).collect();
        assert_eq!(anchors.len(), 16);
    }

    #[test]
    fn identifies_vzw_profiles() {
        assert_eq!(identify_profile(193_698_151_252_893).unwrap().anchor, 167);
        assert_eq!(identify_profile(251_107_217_711_255).unwrap().anchor, 8969);
        assert_eq!(
            identify_profile(185_245_025_092_061).unwrap().anchor,
            196_911_437
        );
        assert_eq!(
            identify_profile(326_540_974_641_771).unwrap().anchor,
            2_912_407
        );
        assert_eq!(
            identify_profile(301_963_657_469_763).unwrap().anchor,
            1_176_929_627
        );
    }

    #[test]
    fn identify_profile_rejects_zero() {
        // `0.is_multiple_of(x)` is true for every nonzero `x`; without an explicit guard,
        // `identify_profile(0)` would spuriously return the first profile. `<CARRIER>_0.binarypb`
        // is a real degenerate filename and must not be classified as belonging to any profile.
        assert!(identify_profile(0).is_none());
    }

    #[test]
    fn known_models_are_recorded() {
        let with_model = PROFILES.iter().find(|p| p.anchor == 3_616_442_437).unwrap();
        assert_eq!(with_model.model, Some("Pixel 10 Pro XL"));
        // anchor 167 now carries a model, so use a genuinely model-less profile.
        let no_model = PROFILES.iter().find(|p| p.anchor == 8969).unwrap();
        assert_eq!(no_model.model, None);
    }

    #[test]
    fn fingerprints_map_to_tiers() {
        assert_eq!(fp_info(874_888_686), Some((Family::A, Tier::Main)));
        assert_eq!(fp_info(627_223_094), Some((Family::B, Tier::Alt)));
        assert_eq!(fp_info(123), None);
    }

    #[test]
    fn tier_profile_counts_are_derived() {
        // Main tier: 16, alt tier: 14 (main minus the two Main-only anchors) — derived from
        // PROFILES so they can't drift from a `14`/`16` literal in `check`.
        assert_eq!(tier_profile_count(Tier::Main), 16);
        assert_eq!(tier_profile_count(Tier::Alt), 14);
        assert_eq!(
            tier_profile_count(Tier::Alt),
            PROFILES.len() - MAIN_ONLY_ANCHORS.len()
        );
        // The subtraction is only valid if the Main-only anchors actually exist in PROFILES.
        for &a in MAIN_ONLY_ANCHORS {
            assert!(
                PROFILES.iter().any(|p| p.anchor == a),
                "MAIN_ONLY_ANCHOR {a} not in PROFILES"
            );
        }
    }

    #[test]
    fn fingerprints_table_drives_fp_info_and_tier_sets() {
        // FINGERPRINTS is the single source; fp_info must agree with it, and the
        // per-tier sets must match the values `check`'s headers render.
        assert_eq!(FINGERPRINTS.len(), 4);
        for &(id, fam, tier) in FINGERPRINTS {
            assert_eq!(fp_info(id), Some((fam, tier)));
        }
        assert_eq!(
            tier_fingerprints(Tier::Main),
            vec![874_888_686, 862_505_271]
        );
        assert_eq!(tier_fingerprints(Tier::Alt), vec![707_802_847, 627_223_094]);
    }

    /// `identify_profile` guards `number == 0` with a comment explaining exactly why; its
    /// sibling did not, so `matching_anchors(0)` returned every profile — `0` is a multiple of
    /// everything. A `<CARRIER>_0.binarypb` was reported as "ambiguous: divisible by 16
    /// anchors" and given a full row of 16 present-marks in `matrix`, all fabricated.
    #[test]
    fn matching_anchors_rejects_zero_like_identify_profile() {
        assert!(identify_profile(0).is_none());
        assert!(
            matching_anchors(0).is_empty(),
            "0 belongs to no profile, so no anchor may match it"
        );
        // A real number still matches, so the guard is not over-broad.
        assert!(!matching_anchors(PROFILES[0].anchor).is_empty());
    }

    /// The two functions must agree about every input, not just zero — they are the same
    /// question asked two ways.
    #[test]
    fn identify_profile_agrees_with_matching_anchors() {
        for number in [0u64, 1, 167, 8969, 98_659, 3_347, 6_694] {
            let single = identify_profile(number).map(|p| p.anchor);
            let all = matching_anchors(number);
            match single {
                None => assert!(
                    all.is_empty(),
                    "{number}: identify_profile found none but matching_anchors found {}",
                    all.len()
                ),
                Some(anchor) => assert!(
                    all.iter().any(|p| p.anchor == anchor),
                    "{number}: matching_anchors omitted the anchor identify_profile chose"
                ),
            }
        }
    }

    /// `Profile.anchor` duplicates `core.last()` in all 16 rows. Nothing tied them together, so
    /// a typo in either column would make profile *matching* (which uses `anchor`) and profile
    /// *display* (which shows `core`) describe different SKUs, silently.
    #[test]
    fn every_profiles_anchor_is_its_last_core_prime() {
        for profile in PROFILES {
            assert_eq!(
                Some(profile.anchor),
                profile.core.last().copied(),
                "profile with core {:?} has anchor {} but its last core prime differs",
                profile.core,
                profile.anchor
            );
        }
    }

    #[test]
    fn decodes_plmns() {
        // Pin every documented PLMN packed-BCD vector from DESIGN.md directly at the
        // rendering layer (`decode_plmn`), independently of `mapping::Plmn::Display`. If the
        // 2-digit-MNC vs 3-digit-MNC rendering is ever refactored away from `mapping::Plmn`,
        // these assertions catch a regression at the documented values.
        let d = |v| decode_plmn(v).expect("documented vectors are all in range");
        assert_eq!(d(197_154), ("302".into(), "220".into())); // 3-digit MNC
        assert_eq!(d(5_435_408), ("250".into(), "01".into())); // 2-digit MNC (N3 = F filler)
        assert_eq!(d(5_566_544), ("450".into(), "05".into())); // SKT, Korea
        assert_eq!(d(10_090_905), ("999".into(), "99".into())); // 2-digit MNC
        assert_eq!(d(1_245_572), ("311".into(), "480".into())); // Verizon, US
    }

    #[test]
    fn decode_plmn_renders_wildcard_as_star() {
        // 228-ff: both MNC nibbles are hex F -> "**"; the filler third nibble is dropped.
        assert_eq!(
            decode_plmn(2_291_967),
            Some(("228".to_string(), "**".to_string()))
        );
    }

    /// `Carrier.plmns` is `uint64` on the wire, so a legend can carry more than the 24 bits a
    /// PLMN has. Masking those bits away turned corruption into a *plausible* carrier: the
    /// value below differs from Verizon's 311-480 only above bit 24, and used to render as
    /// Verizon with no warning. The compiler path has always rejected it
    /// (`Error::PlmnOutOfRange`); the audit path must not disagree.
    #[test]
    fn decode_plmn_rejects_a_value_wider_than_24_bits() {
        assert_eq!(decode_plmn(1_245_572), Some(("311".into(), "480".into())));
        assert_eq!(decode_plmn(0x100_0000 | 1_245_572), None);
        assert!(crate::mapping::Plmn::from_encoded(0x100_0000 | 1_245_572).is_err());
    }

    #[test]
    fn lte_config_maps_known_ids() {
        let sub6 = lte_config(2_160_127_815).unwrap();
        assert_eq!(sub6.family, "sub6");
        assert_eq!(sub6.category_codes, &[0x112, 0x122, 0x142]);
        assert_eq!(sub6.model, Some("Pixel 9 / 9 Pro / 9 Pro XL, sub-6 (RoW)"));
        let sta5jp = lte_config(1_534_561_764).unwrap();
        assert_eq!(sta5jp.family, "sta5_jp");
        assert_eq!(sta5jp.model, None);
        assert!(lte_config(123).is_none());
        assert_eq!(LTE_CONFIGS.len(), 9);
        let ids: std::collections::BTreeSet<u64> = LTE_CONFIGS.iter().map(|c| c.id).collect();
        assert_eq!(ids.len(), 9);
    }

    #[test]
    fn parses_names() {
        assert_eq!(
            parse_name("VZW_193698151252893.binarypb"),
            Parsed::Carrier {
                carrier: "VZW".into(),
                number: 193_698_151_252_893
            }
        );
        assert_eq!(
            parse_name("/some/dir/3_IE_1249420795691880.binarypb"),
            Parsed::Carrier {
                carrier: "3_IE".into(),
                number: 1_249_420_795_691_880
            }
        );
        assert_eq!(
            parse_name("lte_844857560.binarypb"),
            Parsed::Lte(844_857_560)
        );
        assert_eq!(parse_name("ap_plmn_mapping.binarypb"), Parsed::Mapping);
        assert_eq!(parse_name("README.md"), Parsed::Other);
        assert_eq!(parse_name("no_number_here.binarypb"), Parsed::Other);
    }

    #[test]
    fn registry_phone_model_resolves_and_rejects() {
        assert_eq!(
            phone_model("GUL82").unwrap().layout,
            CapabilityLayout::Profiled {
                nr_anchor: 3_616_442_437,
                lte_id: 1_254_026_417,
            }
        );
        assert_eq!(
            phone_model("GC15S").unwrap().layout,
            CapabilityLayout::Profiled {
                nr_anchor: 6791,
                lte_id: 4_210_990_300,
            }
        );
        assert!(phone_model("p9-us").is_none()); // old hand-rolled code retired
        assert!(phone_model("nope").is_none());
    }

    #[test]
    fn registry_phone_models_are_consistent() {
        use pixel_bands::PIXEL_BANDS;
        use std::collections::BTreeSet;
        assert_eq!(PHONE_MODELS.len(), 52);
        let codes: BTreeSet<&str> = PHONE_MODELS.iter().map(|m| m.code).collect();
        assert_eq!(codes.len(), PHONE_MODELS.len(), "codes must be unique");
        for m in PHONE_MODELS {
            assert!(
                PIXEL_BANDS.get(m.code).is_some(),
                "{}: not in PIXEL_BANDS",
                m.code
            );
            if let CapabilityLayout::Profiled { nr_anchor, lte_id } = m.layout {
                assert!(
                    PROFILES.iter().any(|p| p.anchor == nr_anchor),
                    "{}: nr_anchor {nr_anchor} not in PROFILES",
                    m.code,
                );
                assert!(
                    LTE_CONFIGS.iter().any(|c| c.id == lte_id),
                    "{}: lte_id {lte_id} not in LTE_CONFIGS",
                    m.code,
                );
            }
        }
    }
}
