//! The TOML combo-patch format: a `kind`-tagged ADT over the carrier (`nr`) and LTE patches.

use crate::{
    proto::{LteCombo, LteComponent, ShannonFeatureSetDlPerCcNr, ShannonFeatureSetUlPerCcNr},
    report::{
        combos::{
            Cc, Combo, NR_BAND_OFFSET, band_label, combo_key, dl_mimo_label, mod_order_label,
            raw_band, render_component, scs_khz, ul_mimo_cb_label,
        },
        lte::lte_combo_key,
    },
};
use anyhow::Context;

/// Patch discriminator, serialized as the top-level `kind` string.
#[derive(serde::Serialize, serde::Deserialize, Debug, PartialEq, Eq)]
pub(crate) enum Kind {
    #[serde(rename = "nr")]
    Nr,
    #[serde(rename = "lte")]
    Lte,
}

/// A combo patch — one of the two formats, discriminated by `kind`.
#[derive(Debug)]
pub(crate) enum Patch {
    Nr(NrPatch),
    Lte(LtePatch),
}

/// A combo patch document: delete the listed keys, set the listed keys to full
/// definitions. Generic over the set-entry type — `SetEntry` (NR) or `LteSetEntry`
/// (LTE); the `kind` field discriminates the two.
#[derive(serde::Serialize, serde::Deserialize, Debug)]
#[serde(bound(
    serialize = "E: serde::Serialize",
    deserialize = "E: serde::Deserialize<'de>"
))]
#[serde(deny_unknown_fields)]
pub(crate) struct PatchDoc<E> {
    pub(crate) kind: Kind,
    pub(crate) version: u32,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) delete: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) set: Vec<E>,
}

/// One "set this derived key to these combos" operation. `kind` ("add"/"change")
/// is informational. Generic over the combo type — `PatchCombo` (NR) or
/// `LtePatchCombo` (LTE).
#[derive(serde::Serialize, serde::Deserialize, Debug)]
#[serde(deny_unknown_fields)]
pub(crate) struct Entry<C> {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) kind: Option<String>,
    pub(crate) combo: Vec<C>,
}

/// Carrier/NR patch (`kind = "nr"`) and its set entry.
pub(crate) type NrPatch = PatchDoc<SetEntry>;
pub(crate) type SetEntry = Entry<PatchCombo>;

const FORMAT_VERSION: u32 = 1;

impl Patch {
    /// The patch's format version, regardless of kind.
    const fn version(&self) -> u32 {
        match self {
            Self::Nr(p) => p.version,
            Self::Lte(p) => p.version,
        }
    }
}

/// Serialize a patch to TOML text (the variant struct carries the `kind` field).
pub(crate) fn to_toml(patch: &Patch) -> anyhow::Result<String> {
    match patch {
        Patch::Nr(p) => toml::to_string_pretty(p),
        Patch::Lte(p) => toml::to_string_pretty(p),
    }
    .context("serializing patch TOML")
}

/// Parse a patch: peek `kind`, parse the matching variant, reject an unrecognized `version`,
/// and validate each set entry's derived key.
pub(crate) fn from_toml(text: &str) -> anyhow::Result<Patch> {
    #[derive(serde::Deserialize)]
    struct KindOnly {
        kind: Kind,
    }
    let KindOnly { kind } = toml::from_str(text).context("reading patch kind")?;
    let patch = match kind {
        Kind::Nr => Patch::Nr(toml::from_str(text).context("parsing nr patch TOML")?),
        Kind::Lte => Patch::Lte(toml::from_str(text).context("parsing lte patch TOML")?),
    };
    let version = patch.version();
    if version != FORMAT_VERSION {
        anyhow::bail!(
            "unsupported patch version {version} (this build understands version {FORMAT_VERSION})"
        );
    }
    validate_patch(&patch)?;
    Ok(patch)
}

fn validate_patch(patch: &Patch) -> anyhow::Result<()> {
    match patch {
        Patch::Nr(p) => {
            for entry in &p.set {
                set_entry_key(entry)?;
            }
        }
        Patch::Lte(p) => {
            for entry in &p.set {
                lte_set_entry_key(entry)?;
            }
        }
    }
    Ok(())
}

fn derived_entry_key<T>(
    combos: &[T],
    key_of: impl Fn(&T) -> anyhow::Result<String>,
) -> anyhow::Result<String> {
    let mut iter = combos.iter();
    let first = iter.next().context("set entry has no combo variants")?;
    let key = key_of(first)?;
    if key.is_empty() {
        anyhow::bail!("set entry derives an empty key");
    }
    for combo in iter {
        let other = key_of(combo)?;
        if other != key {
            anyhow::bail!("set entry mixes derived keys {key:?} and {other:?}");
        }
    }
    Ok(key)
}

/// The derived key for one NR/carrier set entry.
pub(crate) fn set_entry_key(entry: &SetEntry) -> anyhow::Result<String> {
    derived_entry_key(&entry.combo, |combo| Ok(combo_key(&combo.to_combo()?)))
}

/// Convert one NR/carrier set entry's combos into the internal raw-band model.
pub(crate) fn set_entry_combos(entry: &SetEntry) -> anyhow::Result<Vec<Combo>> {
    entry.combo.iter().map(PatchCombo::to_combo).collect()
}

/// Convert one serialized LTE patch combo into the proto shape used by the LTE
/// combo-key renderer and patch applier.
pub(crate) fn lte_combo_from_patch(p: &LtePatchCombo) -> LteCombo {
    LteCombo {
        components: p
            .components
            .iter()
            .map(|x| LteComponent {
                band: x.band,
                bw_class_mimo_dl: x.bw_class_mimo_dl,
                bw_class_mimo_ul: Some(x.bw_class_mimo_ul),
            })
            .collect(),
        bcs: Some(p.bcs),
        unknown1: Some(p.unknown1),
        unknown2: Some(p.unknown2),
    }
}

/// The derived key for one LTE set entry.
pub(crate) fn lte_set_entry_key(entry: &LteSetEntry) -> anyhow::Result<String> {
    derived_entry_key(&entry.combo, |combo| {
        Ok(lte_combo_key(&lte_combo_from_patch(combo)))
    })
}

fn is_zero_usize(n: &usize) -> bool {
    *n == 0
}

/// Per-component radio kind for an NR/carrier patch combo.
#[derive(serde::Serialize, serde::Deserialize, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum CcKind {
    #[serde(rename = "lte")]
    #[default]
    Lte,
    #[serde(rename = "nr")]
    Nr,
}

impl CcKind {
    const fn raw_band(self, band: i32) -> i32 {
        match self {
            Self::Lte => band,
            Self::Nr => NR_BAND_OFFSET + band,
        }
    }
}

/// One flat component in an NR/carrier patch combo. `band` is the human band
/// number (`78`, not the protobuf's internal `10078`); `kind` supplies `B`/`n`.
#[derive(serde::Serialize, serde::Deserialize, Clone, Default, Debug)]
#[serde(deny_unknown_fields)]
pub(crate) struct PatchCc {
    pub(crate) kind: CcKind,
    pub(crate) band: i32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) bw_class_dl: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) bw_class_ul: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) dl_feature_index: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) ul_feature_index: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) dl_feature_per_cc_ids: Option<Vec<u8>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) ul_feature_per_cc_ids: Option<Vec<u8>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) srs_tx_switch: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) dl_max_scs: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) dl_max_mimo: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) dl_max_bw: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) dl_max_mod_order: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) dl_bw_90mhz_supported: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) ul_max_scs: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) ul_max_mimo_cb: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) ul_max_bw: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) ul_max_mod_order: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) ul_bw_90mhz_supported: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) ul_max_mimo_non_cb: Option<i32>,
}

impl PatchCc {
    pub(crate) fn raw_band(&self) -> i32 {
        self.kind.raw_band(self.band)
    }

    pub(crate) fn dl_feature_set(&self) -> Option<ShannonFeatureSetDlPerCcNr> {
        let present = self.dl_max_scs.is_some()
            || self.dl_max_mimo.is_some()
            || self.dl_max_bw.is_some()
            || self.dl_max_mod_order.is_some()
            || self.dl_bw_90mhz_supported.is_some();
        present.then_some(ShannonFeatureSetDlPerCcNr {
            max_scs: self.dl_max_scs,
            max_mimo: self.dl_max_mimo,
            max_bw: self.dl_max_bw,
            max_mod_order: self.dl_max_mod_order,
            bw_90mhz_supported: self.dl_bw_90mhz_supported,
        })
    }

    pub(crate) fn ul_feature_set(&self) -> Option<ShannonFeatureSetUlPerCcNr> {
        let present = self.ul_max_scs.is_some()
            || self.ul_max_mimo_cb.is_some()
            || self.ul_max_bw.is_some()
            || self.ul_max_mod_order.is_some()
            || self.ul_bw_90mhz_supported.is_some()
            || self.ul_max_mimo_non_cb.is_some();
        present.then_some(ShannonFeatureSetUlPerCcNr {
            max_scs: self.ul_max_scs,
            max_mimo_cb: self.ul_max_mimo_cb,
            max_bw: self.ul_max_bw,
            max_mod_order: self.ul_max_mod_order,
            bw_90mhz_supported: self.ul_bw_90mhz_supported,
            max_mimo_non_cb: self.ul_max_mimo_non_cb,
        })
    }

    pub(crate) fn from_cc(cc: &Cc) -> Self {
        let raw = raw_band(&cc.band).expect("report component band is canonical");
        let is_nr = raw >= NR_BAND_OFFSET;
        let kind = if is_nr { CcKind::Nr } else { CcKind::Lte };
        let band = if is_nr { raw - NR_BAND_OFFSET } else { raw };
        let dl = cc.dl_feature_per_cc.as_ref();
        let ul = cc.ul_feature_per_cc.as_ref();
        Self {
            kind,
            band,
            bw_class_dl: cc.bw_class_dl,
            bw_class_ul: cc.bw_class_ul,
            dl_feature_index: cc.dl_feature_index,
            ul_feature_index: cc.ul_feature_index,
            dl_feature_per_cc_ids: cc.dl_feature_per_cc_ids.clone(),
            ul_feature_per_cc_ids: cc.ul_feature_per_cc_ids.clone(),
            srs_tx_switch: cc.srs_tx_switch,
            dl_max_scs: dl.and_then(|f| f.max_scs),
            dl_max_mimo: dl.and_then(|f| f.max_mimo),
            dl_max_bw: dl.and_then(|f| f.max_bw),
            dl_max_mod_order: dl.and_then(|f| f.max_mod_order),
            dl_bw_90mhz_supported: dl.and_then(|f| f.bw_90mhz_supported),
            ul_max_scs: ul.and_then(|f| f.max_scs),
            ul_max_mimo_cb: ul.and_then(|f| f.max_mimo_cb),
            ul_max_bw: ul.and_then(|f| f.max_bw),
            ul_max_mod_order: ul.and_then(|f| f.max_mod_order),
            ul_bw_90mhz_supported: ul.and_then(|f| f.bw_90mhz_supported),
            ul_max_mimo_non_cb: ul.and_then(|f| f.max_mimo_non_cb),
        }
    }

    pub(crate) fn validate(&self) -> anyhow::Result<()> {
        anyhow::ensure!(self.band > 0, "component band must be positive");
        anyhow::ensure!(
            self.band < NR_BAND_OFFSET,
            "component band must be the plain band number, not raw protobuf encoding"
        );
        if self.kind == CcKind::Lte && self.has_nr_only_fields() {
            anyhow::bail!("LTE component B{} carries NR-only fields", self.band);
        }
        Ok(())
    }

    fn has_nr_only_fields(&self) -> bool {
        // Feature-set indexes are references used by both LTE and NR components.
        // The expanded feature-set fields below are NR-specific patch data.
        self.srs_tx_switch.is_some()
            || self.dl_max_scs.is_some()
            || self.dl_max_mimo.is_some()
            || self.dl_max_bw.is_some()
            || self.dl_max_mod_order.is_some()
            || self.dl_bw_90mhz_supported.is_some()
            || self.ul_max_scs.is_some()
            || self.ul_max_mimo_cb.is_some()
            || self.ul_max_bw.is_some()
            || self.ul_max_mod_order.is_some()
            || self.ul_bw_90mhz_supported.is_some()
            || self.ul_max_mimo_non_cb.is_some()
    }

    pub(crate) fn to_cc(&self) -> anyhow::Result<Cc> {
        self.validate()?;
        let raw = self.raw_band();
        let dl = self.dl_feature_set();
        let ul = self.ul_feature_set();
        Ok(Cc {
            band: band_label(raw),
            bw_class_dl: self.bw_class_dl,
            bw_class_ul: self.bw_class_ul,
            dl_feature_index: self.dl_feature_index,
            ul_feature_index: self.ul_feature_index,
            dl_feature_per_cc_ids: self.dl_feature_per_cc_ids.clone(),
            ul_feature_per_cc_ids: self.ul_feature_per_cc_ids.clone(),
            dl_feature_per_cc: dl,
            ul_feature_per_cc: ul,
            srs_tx_switch: self.srs_tx_switch,
            dl_scs_khz: dl.as_ref().and_then(|f| f.max_scs).and_then(scs_khz),
            dl_mimo: dl.as_ref().and_then(|f| f.max_mimo).map(dl_mimo_label),
            dl_max_bw_mhz: dl.as_ref().and_then(|f| f.max_bw),
            dl_mod_order: dl
                .as_ref()
                .and_then(|f| f.max_mod_order)
                .map(mod_order_label),
            dl_bw90mhz: dl.as_ref().and_then(|f| f.bw_90mhz_supported),
            ul_scs_khz: ul.as_ref().and_then(|f| f.max_scs).and_then(scs_khz),
            ul_mimo_cb: ul
                .as_ref()
                .and_then(|f| f.max_mimo_cb)
                .map(ul_mimo_cb_label),
            ul_mimo_non_cb: ul.as_ref().and_then(|f| f.max_mimo_non_cb),
            ul_max_bw_mhz: ul.as_ref().and_then(|f| f.max_bw),
            ul_mod_order: ul
                .as_ref()
                .and_then(|f| f.max_mod_order)
                .map(mod_order_label),
            ul_bw90mhz: ul.as_ref().and_then(|f| f.bw_90mhz_supported),
        })
    }

    pub(crate) fn band_label(&self) -> String {
        match self.kind {
            CcKind::Lte => format!("B{}", self.band),
            CcKind::Nr => format!("n{}", self.band),
        }
    }

    fn component_label(&self) -> String {
        render_component(
            self.kind.raw_band(self.band),
            self.bw_class_dl,
            self.bw_class_ul,
        )
    }
}

/// One variant under a set entry in an NR/carrier patch.
#[derive(serde::Serialize, serde::Deserialize, Clone, Default, Debug)]
#[serde(deny_unknown_fields)]
pub(crate) struct PatchCombo {
    #[serde(default, skip_serializing_if = "is_zero_usize")]
    pub(crate) group: usize,
    #[serde(default, skip_serializing_if = "is_zero_usize")]
    pub(crate) index: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) power_class: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) bcs_nr: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) bcs_intra_endc: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) bcs_eutra: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) intra_band_en_dc_support: Option<i32>,
    pub(crate) bit_mask: u32,
    pub(crate) cc: Vec<PatchCc>,
}

impl PatchCombo {
    pub(crate) fn from_combo(combo: &Combo) -> Self {
        Self {
            group: combo.group,
            index: combo.index,
            power_class: combo.power_class,
            bcs_nr: combo.bcs_nr,
            bcs_intra_endc: combo.bcs_intra_endc,
            bcs_eutra: combo.bcs_eutra,
            intra_band_en_dc_support: combo.intra_band_en_dc_support,
            bit_mask: combo.bit_mask,
            cc: combo.cc.iter().map(PatchCc::from_cc).collect(),
        }
    }

    pub(crate) fn to_combo(&self) -> anyhow::Result<Combo> {
        let cc: Vec<Cc> = self
            .cc
            .iter()
            .map(PatchCc::to_cc)
            .collect::<anyhow::Result<_>>()?;
        Ok(Combo {
            group: self.group,
            index: self.index,
            bands: self
                .cc
                .iter()
                .map(PatchCc::component_label)
                .collect::<Vec<_>>()
                .join(" + "),
            power_class: self.power_class,
            bcs_nr: self.bcs_nr,
            bcs_intra_endc: self.bcs_intra_endc,
            bcs_eutra: self.bcs_eutra,
            intra_band_en_dc_support: self.intra_band_en_dc_support,
            bit_mask: self.bit_mask,
            cc,
        })
    }
}

/// LTE-fallback patch (`kind = "lte"`) and its set entry.
pub(crate) type LtePatch = PatchDoc<LteSetEntry>;
pub(crate) type LteSetEntry = Entry<LtePatchCombo>;

#[derive(serde::Serialize, serde::Deserialize, Debug)]
#[serde(deny_unknown_fields)]
pub(crate) struct LtePatchCombo {
    pub(crate) components: Vec<LtePatchComponent>,
    pub(crate) bcs: u64,
    pub(crate) unknown1: u64,
    pub(crate) unknown2: u64,
}

#[derive(serde::Serialize, serde::Deserialize, Debug)]
#[serde(deny_unknown_fields)]
pub(crate) struct LtePatchComponent {
    pub(crate) band: i32,
    pub(crate) bw_class_mimo_dl: i32,
    pub(crate) bw_class_mimo_ul: i32,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_nr() -> Patch {
        Patch::Nr(NrPatch {
            kind: Kind::Nr,
            version: 1,
            delete: vec!["n41A".to_string()],
            set: vec![SetEntry {
                kind: Some("add".to_string()),
                combo: vec![PatchCombo {
                    group: 0,
                    index: 0,
                    bit_mask: 0,
                    cc: vec![PatchCc {
                        kind: CcKind::Nr,
                        band: 2,
                        bw_class_dl: Some(1),
                        bw_class_ul: Some(1),
                        dl_max_bw: Some(40),
                        dl_max_mimo: Some(2),
                        dl_max_mod_order: Some(2),
                        ..Default::default()
                    }],
                    ..Default::default()
                }],
            }],
        })
    }

    #[test]
    fn nr_patch_round_trips_with_kind() {
        let text = to_toml(&sample_nr()).unwrap();
        assert!(text.contains("kind = \"nr\""));
        assert!(!text.contains("key ="), "{text}");
        assert!(!text.contains("bands ="), "{text}");
        assert!(!text.contains("band_label"), "{text}");
        assert!(!text.contains("nr ="), "{text}");
        assert!(text.contains("band = 2"), "{text}");
        assert!(text.contains("dl_max_bw = 40"), "{text}");
        assert!(text.contains("dl_max_mimo = 2"), "{text}");
        assert!(text.contains("dl_max_mod_order = 2"), "{text}");
        assert!(!text.contains("dl_max_bw_mhz"), "{text}");
        assert!(!text.contains("dl_mimo"), "{text}");
        assert!(!text.contains("dl_mod_order"), "{text}");
        let Patch::Nr(back) = from_toml(&text).unwrap() else {
            panic!("expected nr variant")
        };
        assert_eq!(back.version, 1);
        assert_eq!(back.delete, vec!["n41A".to_string()]);
        assert_eq!(set_entry_key(&back.set[0]).unwrap(), "n2A");
        assert_eq!(back.set[0].combo[0].cc[0].kind, CcKind::Nr);
        assert_eq!(back.set[0].combo[0].cc[0].band, 2);
    }

    #[test]
    fn lte_patch_round_trips_with_kind() {
        let p = Patch::Lte(LtePatch {
            kind: Kind::Lte,
            version: 1,
            delete: vec!["B5A↓".to_string()],
            set: vec![LteSetEntry {
                kind: Some("add".to_string()),
                combo: vec![LtePatchCombo {
                    components: vec![LtePatchComponent {
                        band: 7,
                        bw_class_mimo_dl: 32768,
                        bw_class_mimo_ul: 0,
                    }],
                    bcs: 7,
                    unknown1: 8,
                    unknown2: 9,
                }],
            }],
        });
        let text = to_toml(&p).unwrap();
        assert!(text.contains("kind = \"lte\""));
        assert!(!text.contains("key ="), "{text}");
        assert!(!text.contains("bands ="), "{text}");
        let Patch::Lte(lp) = from_toml(&text).unwrap() else {
            panic!("expected lte variant")
        };
        assert_eq!(lp.version, 1);
        assert_eq!(lp.delete, vec!["B5A↓".to_string()]);
        assert_eq!(lte_set_entry_key(&lp.set[0]).unwrap(), "B7A↓");
        assert_eq!(lp.set[0].kind.as_deref(), Some("add"));
        assert_eq!(lp.set[0].combo[0].bcs, 7);
        assert_eq!(lp.set[0].combo[0].unknown1, 8);
        assert_eq!(lp.set[0].combo[0].unknown2, 9);
    }

    #[test]
    fn nr_patch_requires_component_kind() {
        let text = r#"
kind = "nr"
version = 1

[[set]]
kind = "add"

[[set.combo]]
bit_mask = 0

[[set.combo.cc]]
band = 2
bw_class_dl = 1
bw_class_ul = 1
"#;
        let err = format!("{:#}", from_toml(text).unwrap_err());
        assert!(err.contains("kind"), "unexpected error: {err}");
    }

    #[test]
    fn nr_patch_rejects_old_decoded_nr_cap_fields() {
        let text = r#"
kind = "nr"
version = 1

[[set]]
kind = "add"

[[set.combo]]
bit_mask = 0

[[set.combo.cc]]
kind = "nr"
band = 78
bw_class_dl = 1
bw_class_ul = 1
dl_mimo = "4x4"
"#;
        let err = format!("{:#}", from_toml(text).unwrap_err());
        assert!(
            err.contains("unknown field") || err.contains("unexpected key"),
            "{err}"
        );
    }

    #[test]
    fn set_entry_with_mixed_variant_keys_is_rejected() {
        let text = r#"
kind = "nr"
version = 1

[[set]]
kind = "change"

[[set.combo]]
bit_mask = 0

[[set.combo.cc]]
kind = "nr"
band = 2
bw_class_dl = 1
bw_class_ul = 1

[[set.combo]]
bit_mask = 0

[[set.combo.cc]]
kind = "nr"
band = 78
bw_class_dl = 1
bw_class_ul = 1
"#;
        let err = from_toml(text).unwrap_err().to_string();
        assert!(
            err.contains("mixes derived keys"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn lte_component_rejects_nr_only_fields() {
        let text = r#"
kind = "nr"
version = 1

[[set]]
kind = "add"

[[set.combo]]
bit_mask = 0

[[set.combo.cc]]
kind = "lte"
band = 66
bw_class_dl = 1
bw_class_ul = 1
dl_max_bw = 100
"#;
        let err = from_toml(text).unwrap_err().to_string();
        assert!(err.contains("NR-only"), "unexpected error: {err}");
    }

    #[test]
    fn lte_component_allows_feature_indexes() {
        let text = r#"
kind = "nr"
version = 1

[[set]]
kind = "add"

[[set.combo]]
bit_mask = 0

[[set.combo.cc]]
kind = "lte"
band = 1
bw_class_dl = 1
bw_class_ul = 1
dl_feature_index = 1
ul_feature_index = 2
"#;
        let Patch::Nr(p) = from_toml(text).unwrap() else {
            panic!("expected nr variant")
        };
        let cc = &p.set[0].combo[0].cc[0];
        assert_eq!(cc.kind, CcKind::Lte);
        assert_eq!(cc.dl_feature_index, Some(1));
        assert_eq!(cc.ul_feature_index, Some(2));
        assert_eq!(set_entry_key(&p.set[0]).unwrap(), "B1A");
    }

    #[test]
    fn nr_patch_round_trips_numeric_feature_per_cc_ids() {
        let text = r#"
kind = "nr"
version = 1

[[set]]
kind = "add"

[[set.combo]]
bit_mask = 0

[[set.combo.cc]]
kind = "nr"
band = 78
bw_class_dl = 1
bw_class_ul = 1
dl_feature_per_cc_ids = [11]
ul_feature_per_cc_ids = [7]
"#;
        let Patch::Nr(p) = from_toml(text).unwrap() else {
            panic!("expected nr variant")
        };
        let cc = &p.set[0].combo[0].cc[0];
        assert_eq!(cc.dl_feature_per_cc_ids, Some(vec![11]));
        assert_eq!(cc.ul_feature_per_cc_ids, Some(vec![7]));

        let round = to_toml(&Patch::Nr(p)).unwrap();
        let value: toml::Value = toml::from_str(&round).unwrap();
        let cc_value = &value["set"][0]["combo"][0]["cc"][0];
        assert_eq!(cc_value["dl_feature_per_cc_ids"][0].as_integer(), Some(11));
        assert_eq!(cc_value["ul_feature_per_cc_ids"][0].as_integer(), Some(7));
        assert!(!round.contains("dl_feature_per_cc_ids = \"0b\""), "{round}");
    }

    #[test]
    fn nr_patch_rejects_hex_string_feature_per_cc_ids() {
        let text = r#"
kind = "nr"
version = 1

[[set]]
kind = "add"

[[set.combo]]
bit_mask = 0

[[set.combo.cc]]
kind = "nr"
band = 78
bw_class_dl = 1
bw_class_ul = 1
dl_feature_per_cc_ids = "0b"
"#;
        let err = format!("{:#}", from_toml(text).unwrap_err());
        assert!(
            err.contains("invalid type") || err.contains("expected"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn unknown_version_is_rejected() {
        let text = "kind = \"nr\"\nversion = 2\n";
        assert!(from_toml(text).is_err());
    }
}
