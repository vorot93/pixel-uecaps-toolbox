//! The reverse-engineered UE-capabilities model.
//!
//! Filenames are `<CARRIER>_<NUMBER>.binarypb`. The NUMBER is a selector key:
//!
//! ```text
//! NUMBER = carrier-identity  ×  SKU-profile tag
//! ```
//!
//! Every carrier ships one file per Pixel-SKU capability profile. A Pixel loads
//! the file whose NUMBER is divisible by its own SKU's profile tag (the `anchor`
//! prime), so the chosen file depends on the exact Pixel SKU.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Family {
    A,
    B,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tier {
    /// 16 profiles; US/EU/APAC majors carry full per-carrier capability data.
    Main,
    /// 14 profiles (no P15/P16); India + emerging markets. Per-operator files
    /// are tiny reference stubs that delegate to the EU_COMMON1 config.
    Alt,
}

pub struct Profile {
    /// Unique prime that divides the number of every file for this profile.
    pub anchor: u64,
    /// Full prime tag (one prime per SKU; several primes = a group of SKUs).
    pub core: &'static [u64],
    pub family: Family,
    /// Known real device model for this SKU profile; `None` when unknown.
    #[allow(dead_code)]
    pub model: Option<&'static str>,
}

/// The 16 capability profiles.
pub static PROFILES: &[Profile] = &[
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

/// The single profile whose anchor prime divides `number` (the normal case).
pub fn identify_profile(number: u64) -> Option<&'static Profile> {
    PROFILES.iter().find(|p| number.is_multiple_of(p.anchor))
}

/// Every profile whose anchor divides `number` (>1 means an ambiguous file).
pub fn matching_anchors(number: u64) -> Vec<&'static Profile> {
    PROFILES
        .iter()
        .filter(|p| number.is_multiple_of(p.anchor))
        .collect()
}

/// A modem LTE-config selection-table entry: the `lte_<id>` filename number, the Shannon
/// firmware family name, the hardware/SKU category codes that select it, and the confirmed
/// Pixel model (`None` when only the raw family is known).
pub struct LteConfig {
    pub id: u64,
    pub family: &'static str,
    pub category_codes: &'static [u32],
    pub model: Option<&'static str>,
}

/// The modem's LTE-config selection table (from `g5400c-main.bin`). The id is the
/// `lte_<id>.binarypb` filename number; selection is hardware/SKU-category driven, not SIM/MCC.
pub static LTE_CONFIGS: &[LteConfig] = &[
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
pub fn lte_config(id: u64) -> Option<&'static LteConfig> {
    LTE_CONFIGS.iter().find(|c| c.id == id)
}

/// A `provision`-able phone: a CLI code mapped to its NR SKU anchor and LTE-config id.
pub struct PhoneModel {
    /// CLI token (Google 5-char model code), e.g. `"GUL82"`.
    pub code: &'static str,
    /// Human label, e.g. `"Pixel 9 (mmWave, US)"`.
    pub display: &'static str,
    /// The SKU anchor prime — selects the carrier NR file (`-> PROFILES[..].anchor`).
    pub nr_anchor: u64,
    /// The LTE-config id — selects the `lte_<id>.binarypb` fallback (`-> LTE_CONFIGS[..].id`).
    pub lte_id: u64,
}

/// The phone models `provision` can build for — keyed by Google's 5-char model code
/// (from the `pixel-bands` crate). Derived from Google's supported-bands page and
/// maintainer-corrected; each code's bands come from `pixel_bands::PIXEL_BANDS`.
pub static PHONE_MODELS: &[PhoneModel] = &[
    PhoneModel {
        code: "G2YBB",
        display: "Pixel 9 (mmWave, US)",
        nr_anchor: 66_813_533,
        lte_id: 400_907_661,
    },
    PhoneModel {
        code: "GUR25",
        display: "Pixel 9 (sub-6, intl)",
        nr_anchor: 1_334_093,
        lte_id: 2_160_127_815,
    },
    PhoneModel {
        code: "GKV4X",
        display: "Pixel 9 (sub-6, NA)",
        nr_anchor: 1_334_093,
        lte_id: 2_160_127_815,
    },
    PhoneModel {
        code: "G6GPR",
        display: "Pixel 9 (sub-6, RoW)",
        nr_anchor: 1_334_093,
        lte_id: 2_160_127_815,
    },
    PhoneModel {
        code: "G1B60",
        display: "Pixel 9 (sub-6, JP)",
        nr_anchor: 1_334_093,
        lte_id: 2_160_127_815,
    },
    PhoneModel {
        code: "GR83Y",
        display: "Pixel 9 Pro (mmWave, US)",
        nr_anchor: 1_176_929_627,
        lte_id: 400_907_661,
    },
    PhoneModel {
        code: "GEC77",
        display: "Pixel 9 Pro (sub-6, RoW)",
        nr_anchor: 1847,
        lte_id: 2_160_127_815,
    },
    PhoneModel {
        code: "GWVK6",
        display: "Pixel 9 Pro (sub-6, JP)",
        nr_anchor: 1847,
        lte_id: 2_160_127_815,
    },
    PhoneModel {
        code: "GGX8B",
        display: "Pixel 9 Pro XL (mmWave, US)",
        nr_anchor: 154_921_957,
        lte_id: 400_907_661,
    },
    PhoneModel {
        code: "GZC4K",
        display: "Pixel 9 Pro XL (sub-6, RoW)",
        nr_anchor: 224_309,
        lte_id: 2_160_127_815,
    },
    PhoneModel {
        code: "GQ57S",
        display: "Pixel 9 Pro XL (sub-6, JP)",
        nr_anchor: 224_309,
        lte_id: 2_160_127_815,
    },
    PhoneModel {
        code: "GGH2X",
        display: "Pixel 9 Pro Fold (RoW)",
        nr_anchor: 6791,
        lte_id: 4_210_990_300,
    },
    PhoneModel {
        code: "GC15S",
        display: "Pixel 9 Pro Fold (JP)",
        nr_anchor: 6791,
        lte_id: 4_210_990_300,
    },
    PhoneModel {
        code: "GU0NP",
        display: "Pixel 10 Pro Fold (Global)",
        nr_anchor: 167,
        lte_id: 2_306_930_561,
    },
    PhoneModel {
        code: "GM66V",
        display: "Pixel 10 Pro Fold (JP)",
        nr_anchor: 167,
        lte_id: 2_306_930_561,
    },
    // Pixel 10 Pro XL: US (mmWave) and RoW/JP (sub-6) share one NR profile (anchor 3616442437) but differ in lte_id.
    PhoneModel {
        code: "GUL82",
        display: "Pixel 10 Pro XL (mmWave, US)",
        nr_anchor: 3_616_442_437,
        lte_id: 1_254_026_417,
    },
    PhoneModel {
        code: "G45RY",
        display: "Pixel 10 Pro XL (sub-6, RoW)",
        nr_anchor: 3_616_442_437,
        lte_id: 4_017_061_044,
    },
    PhoneModel {
        code: "GYPW4",
        display: "Pixel 10 Pro XL (sub-6, JP)",
        nr_anchor: 3_616_442_437,
        lte_id: 4_017_061_044,
    },
];

/// Look up a phone model by its CLI code.
pub fn phone_model(code: &str) -> Option<&'static PhoneModel> {
    PHONE_MODELS.iter().find(|m| m.code == code)
}

/// A device model resolved from its hardware SKU — the fields a caller needs to
/// select and patch its capability files. Owned + `'static` so it crosses the
/// wasm-bindgen boundary cleanly.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ModelInfo {
    pub code: &'static str,
    pub display: &'static str,
    pub lte_id: u64,
    pub nr_anchor: u64,
}

impl From<&PhoneModel> for ModelInfo {
    fn from(m: &PhoneModel) -> Self {
        Self {
            code: m.code,
            display: m.display,
            lte_id: m.lte_id,
            nr_anchor: m.nr_anchor,
        }
    }
}

/// Resolve a `ro.boot.product.hardware.sku` value (e.g. `"GUL82"`) to a known
/// Pixel model. Input is trimmed and upper-cased before lookup. `None` if unknown.
pub fn device_model(sku: &str) -> Option<ModelInfo> {
    let code = sku.trim().to_ascii_uppercase();
    phone_model(&code).map(ModelInfo::from)
}

/// In-file capability fingerprint (protobuf field 1) -> (family, tier).
pub const fn fp_info(fp: u64) -> Option<(Family, Tier)> {
    match fp {
        874_888_686 => Some((Family::A, Tier::Main)),
        862_505_271 => Some((Family::B, Tier::Main)),
        707_802_847 => Some((Family::A, Tier::Alt)),
        627_223_094 => Some((Family::B, Tier::Alt)),
        _ => None,
    }
}

pub const fn family_desc(f: Family) -> &'static str {
    match f {
        Family::A => "capability family A",
        Family::B => "capability family B",
    }
}

/// Tier as a short key: `"main"` / `"alt"`.
pub const fn tier_short(t: Tier) -> &'static str {
    match t {
        Tier::Main => "main",
        Tier::Alt => "alt",
    }
}

/// What a filename refers to.
#[derive(Debug, PartialEq, Eq)]
pub enum Parsed {
    /// `ap_plmn_mapping.binarypb` — the PLMN→carrier legend.
    Mapping,
    /// `lte_<n>.binarypb` — LTE-only fallback, outside the profile scheme.
    Lte(u64),
    /// `<CARRIER>_<NUMBER>.binarypb`.
    Carrier { carrier: String, number: u64 },
    /// Anything else.
    Other,
}

pub fn parse_name(filename: &str) -> Parsed {
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
pub fn decode_plmn(v: u64) -> (String, String) {
    let plmn = crate::mapping::Plmn::from_encoded(v & 0xFF_FFFF).expect("masked to 24 bits");
    let (mcc_n, mnc_n, mnc3) = plmn.nibbles();
    let d = |x: u8| if x < 10 { (b'0' + x) as char } else { '*' };
    let mcc: String = mcc_n.iter().map(|&x| d(x)).collect();
    let mut mnc: String = mnc_n.iter().map(|&x| d(x)).collect();
    if mnc3 != 0xf {
        mnc.push(d(mnc3));
    }
    (mcc, mnc)
}

/// MCC -> country/territory for the regions present in the dataset.
pub fn mcc_country(mcc: &str) -> Option<&'static str> {
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
    fn decodes_plmns() {
        assert_eq!(decode_plmn(5_566_544), ("450".into(), "05".into())); // SKT, Korea
        assert_eq!(decode_plmn(1_245_572), ("311".into(), "480".into())); // Verizon, US
    }

    #[test]
    fn decode_plmn_renders_wildcard_as_star() {
        // 228-ff: both MNC nibbles are hex F -> "**"; the filler third nibble is dropped.
        assert_eq!(
            decode_plmn(2_291_967),
            ("228".to_string(), "**".to_string())
        );
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
    fn phone_model_resolves_and_rejects() {
        assert_eq!(phone_model("GUL82").unwrap().nr_anchor, 3_616_442_437); // Pixel 10 Pro XL US
        assert_eq!(phone_model("GUL82").unwrap().lte_id, 1_254_026_417);
        assert_eq!(phone_model("GC15S").unwrap().lte_id, 4_210_990_300); // Pixel 9 Pro Fold
        assert!(phone_model("p9-us").is_none()); // old hand-rolled code retired
        assert!(phone_model("nope").is_none());
    }

    #[test]
    fn phone_models_are_consistent() {
        use pixel_bands::PIXEL_BANDS;
        use std::collections::BTreeSet;
        assert_eq!(PHONE_MODELS.len(), 18);
        let codes: BTreeSet<&str> = PHONE_MODELS.iter().map(|m| m.code).collect();
        assert_eq!(codes.len(), 18, "codes must be unique");
        for m in PHONE_MODELS {
            assert!(
                PIXEL_BANDS.get(m.code).is_some(),
                "{}: not in PIXEL_BANDS",
                m.code
            );
            assert!(
                PROFILES.iter().any(|p| p.anchor == m.nr_anchor),
                "{}: nr_anchor {} not in PROFILES",
                m.code,
                m.nr_anchor
            );
            assert!(
                LTE_CONFIGS.iter().any(|c| c.id == m.lte_id),
                "{}: lte_id {} not in LTE_CONFIGS",
                m.code,
                m.lte_id
            );
        }
    }

    #[test]
    fn device_model_resolves_known_sku() {
        let m = device_model("GUL82").expect("GUL82 is a known SKU");
        assert_eq!(m.code, "GUL82");
        assert_eq!(m.lte_id, 1_254_026_417);
        assert_eq!(m.nr_anchor, 3_616_442_437);
    }

    #[test]
    fn device_model_normalizes_case_and_whitespace() {
        // getprop output may arrive lower-cased or with a trailing newline.
        assert_eq!(device_model(" gul82\n").map(|m| m.code), Some("GUL82"));
    }

    #[test]
    fn device_model_unknown_is_none() {
        assert!(device_model("ZZ999").is_none());
    }
}
