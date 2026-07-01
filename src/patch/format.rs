//! The TOML combo-patch format: a `kind`-tagged ADT over the carrier (`nr`) and LTE patches.

use crate::report::combos::Combo;
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
pub(crate) struct PatchDoc<E> {
    pub(crate) kind: Kind,
    pub(crate) version: u32,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) delete: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) set: Vec<E>,
}

/// One "set this key to these combos" operation. `kind` ("add"/"change") is
/// informational. Generic over the combo type — `Combo` (NR) or `LtePatchCombo` (LTE).
#[derive(serde::Serialize, serde::Deserialize, Debug)]
pub(crate) struct Entry<C> {
    pub(crate) key: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) kind: Option<String>,
    pub(crate) combo: Vec<C>,
}

/// Carrier/NR patch (`kind = "nr"`) and its set entry.
pub(crate) type NrPatch = PatchDoc<SetEntry>;
pub(crate) type SetEntry = Entry<Combo>;

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

/// Parse a patch: peek `kind`, parse the matching variant, reject an unrecognized `version`.
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
    Ok(patch)
}

/// LTE-fallback patch (`kind = "lte"`) and its set entry.
pub(crate) type LtePatch = PatchDoc<LteSetEntry>;
pub(crate) type LteSetEntry = Entry<LtePatchCombo>;

#[derive(serde::Serialize, serde::Deserialize, Debug)]
pub(crate) struct LtePatchCombo {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) bands: Option<String>, // readable label; informational, ignored on apply
    pub(crate) components: Vec<LtePatchComponent>,
    pub(crate) bcs: u64,
    pub(crate) unknown1: u64,
    pub(crate) unknown2: u64,
}

#[derive(serde::Serialize, serde::Deserialize, Debug)]
pub(crate) struct LtePatchComponent {
    pub(crate) band: i32,
    pub(crate) bw_class_mimo_dl: i32,
    pub(crate) bw_class_mimo_ul: i32,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::report::combos::Cc; // Combo comes via `use super::*`

    fn sample_nr() -> Patch {
        Patch::Nr(NrPatch {
            kind: Kind::Nr,
            version: 1,
            delete: vec!["n41A".to_string()],
            set: vec![SetEntry {
                key: "n2A".to_string(),
                kind: Some("add".to_string()),
                combo: vec![Combo {
                    bit_mask: 0,
                    cc: vec![Cc {
                        band: 10002,
                        bw_class_dl: Some(1),
                        dl_max_bw_mhz: Some(40),
                        dl_mimo: Some("4x4".to_string()),
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
        let Patch::Nr(back) = from_toml(&text).unwrap() else {
            panic!("expected nr variant")
        };
        assert_eq!(back.version, 1);
        assert_eq!(back.delete, vec!["n41A".to_string()]);
        assert_eq!(back.set[0].key, "n2A");
        assert_eq!(back.set[0].combo[0].cc[0].band, 10002);
    }

    #[test]
    fn lte_patch_round_trips_with_kind() {
        let p = Patch::Lte(LtePatch {
            kind: Kind::Lte,
            version: 1,
            delete: vec!["B5A↓".to_string()],
            set: vec![LteSetEntry {
                key: "B7A↓".to_string(),
                kind: Some("add".to_string()),
                combo: vec![LtePatchCombo {
                    bands: Some("B7A↓".to_string()),
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
        let Patch::Lte(lp) = from_toml(&text).unwrap() else {
            panic!("expected lte variant")
        };
        assert_eq!(lp.version, 1);
        assert_eq!(lp.delete, vec!["B5A↓".to_string()]);
        assert_eq!(lp.set[0].key, "B7A↓");
        assert_eq!(lp.set[0].kind.as_deref(), Some("add"));
        assert_eq!(lp.set[0].combo[0].bcs, 7);
        assert_eq!(lp.set[0].combo[0].unknown1, 8);
        assert_eq!(lp.set[0].combo[0].unknown2, 9);
    }

    #[test]
    fn unknown_version_is_rejected() {
        let text = "kind = \"nr\"\nversion = 2\n";
        assert!(from_toml(text).is_err());
    }
}
