//! Reader for `ap_plmn_mapping.binarypb` — the PLMN→carrier legend.

mod error;
mod plmn;
mod schema;

pub use error::Error;
pub use plmn::Plmn;
pub(crate) use schema::{
    MappingEntry, Root as MappingRoot, encode_root_verified, map_to_root, root_to_map,
};

use std::{collections::BTreeMap, path::Path};

pub struct CarrierEntry {
    pub index: u64,
    pub plmns: Vec<u64>,
}

/// A best-effort legend plus the structural anomalies the lenient collapse would otherwise
/// hide. `entries` is exactly what [`load_mapping`] returns (last entry wins per name;
/// empty-named carriers dropped).
///
/// Decoding is strict — the same fail-closed scan the compiler uses — but the *result* stays
/// lenient: a junk legend yields an empty view plus a populated anomaly field rather than an
/// error return, so a folder with one bad file can still be audited. [`root_to_map`]
/// hard-errors on duplicate names, empty names, and duplicate indices; every one of those three
/// now has a field here, so `check` reports what the write path would refuse instead of
/// auditing it as clean.
#[derive(Default)]
pub struct LegendReport {
    pub entries: BTreeMap<String, CarrierEntry>,
    /// Carrier names carried by more than one entry (each listed once), sorted.
    pub duplicate_names: Vec<String>,
    /// Number of carriers dropped from `entries` for having an empty name.
    pub empty_named: usize,
    /// Carrier `index` values carried by more than one entry (each listed once), sorted.
    /// [`root_to_map`] rejects these as [`Error::DuplicateId`]; without this field the audit
    /// surfaces had no way to see the third of the write path's three invariants, so a legend
    /// the compiler refuses was reported as clean.
    pub duplicate_indices: Vec<u64>,
    /// Why the legend could not be read or strictly validated, if it could not be. Kept as a
    /// report field rather than an error return so a folder with a corrupt legend still yields
    /// a best-effort view — but `check` can now say so instead of showing an empty legend.
    pub decode_error: Option<String>,
}

/// Load the legend from the directory containing `ap_plmn_mapping.binarypb`.
/// Returns an empty map if the file is missing or unreadable.
pub fn load_mapping(dir: &Path) -> BTreeMap<String, CarrierEntry> {
    load_mapping_report(dir).entries
}

/// Load the legend and report structural anomalies (see [`LegendReport`]). Returns an empty
/// report if the file is missing or unreadable (lenient, like [`load_mapping`]).
pub fn load_mapping_report(dir: &Path) -> LegendReport {
    let path = dir.join("ap_plmn_mapping.binarypb");
    let data = match std::fs::read(&path) {
        Ok(data) => data,
        // A missing legend is normal (many folders have none), so it is not an error to report.
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return LegendReport::default();
        }
        Err(error) => {
            return LegendReport {
                decode_error: Some(format!("cannot read {}: {error}", path.display())),
                ..Default::default()
            };
        }
    };
    // Strictly validated, like the compiler's read of the same file: an unknown field, a wrong
    // wire type or a packed `plmns` list means the legend cannot be trusted, and the audit
    // surfaces must say so rather than silently show an empty legend.
    let map = match crate::wire::decode_plmn_map(&data, "ap_plmn_mapping.binarypb") {
        Ok(map) => map,
        Err(error) => {
            return LegendReport {
                decode_error: Some(format!("{error:#}")),
                ..Default::default()
            };
        }
    };
    let mut entries = BTreeMap::new();
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    let mut index_counts: BTreeMap<u64, usize> = BTreeMap::new();
    let mut empty_named = 0usize;
    for c in map.carriers {
        // Counted before the empty-name skip: a duplicate index is a duplicate regardless of
        // whether its entry survives into `entries`.
        *index_counts.entry(c.index).or_insert(0) += 1;
        if c.name.is_empty() {
            empty_named += 1;
            continue;
        }
        *counts.entry(c.name.clone()).or_insert(0) += 1;
        entries.insert(
            c.name,
            CarrierEntry {
                index: c.index,
                plmns: c.plmns,
            },
        );
    }
    let duplicate_names = counts
        .into_iter()
        .filter(|(_, n)| *n > 1)
        .map(|(name, _)| name)
        .collect();
    let duplicate_indices = index_counts
        .into_iter()
        .filter(|(_, n)| *n > 1)
        .map(|(index, _)| index)
        .collect();
    LegendReport {
        entries,
        duplicate_names,
        empty_named,
        duplicate_indices,
        decode_error: None,
    }
}

#[cfg(test)]
mod tests {
    use super::{load_mapping, load_mapping_report};
    use crate::proto::{Carrier, PlmnMap};
    use prost::Message;

    fn write_legend(map: &PlmnMap) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("ap_plmn_mapping.binarypb"),
            map.encode_to_vec(),
        )
        .unwrap();
        dir
    }

    /// `root_to_map` rejects three conditions; `LegendReport` used to carry only two, so a
    /// legend with colliding carrier indices — which `provision`/`decompose` hard-fail on with
    /// `DuplicateId` — was audited as clean by `check`.
    #[test]
    fn reports_duplicate_carrier_indices() {
        let dir = write_legend(&PlmnMap {
            carriers: vec![
                Carrier {
                    plmns: vec![1_245_572],
                    index: 311_480,
                    name: "VZW".into(),
                },
                Carrier {
                    plmns: vec![197_154],
                    index: 311_480,
                    name: "OTHER".into(),
                },
            ],
        });

        let report = load_mapping_report(dir.path());

        assert_eq!(report.duplicate_indices, vec![311_480]);
        assert!(report.duplicate_names.is_empty());
        assert_eq!(report.decode_error, None);
    }

    #[test]
    fn distinct_indices_are_not_reported() {
        let dir = write_legend(&PlmnMap {
            carriers: vec![
                Carrier {
                    plmns: vec![1_245_572],
                    index: 1,
                    name: "A".into(),
                },
                Carrier {
                    plmns: vec![197_154],
                    index: 2,
                    name: "B".into(),
                },
            ],
        });

        assert!(load_mapping_report(dir.path()).duplicate_indices.is_empty());
    }

    /// A wire-corrupt legend must be named as corrupt, not shown as an empty legend.
    #[test]
    fn reports_a_wire_invalid_legend_instead_of_an_empty_one() {
        let dir = tempfile::tempdir().unwrap();
        let mut bytes = PlmnMap::default().encode_to_vec();
        bytes.extend([0x78, 0x01]); // field 15, not modeled
        std::fs::write(dir.path().join("ap_plmn_mapping.binarypb"), bytes).unwrap();

        let report = load_mapping_report(dir.path());

        let error = report.decode_error.expect("corrupt legend reports why");
        assert!(error.contains("field #15"), "{error}");
        assert!(report.entries.is_empty());
    }

    /// A missing legend is ordinary, not an anomaly to report.
    #[test]
    fn a_missing_legend_is_not_an_error() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(load_mapping_report(dir.path()).decode_error, None);
    }

    #[test]
    fn decodes_one_carrier() {
        // Exercise load_mapping itself: encode a one-entry legend, write it to a
        // temp dir as ap_plmn_mapping.binarypb, and load it back through the function.
        let map = PlmnMap {
            carriers: vec![Carrier {
                plmns: vec![5_566_544], // 450-05
                index: 63,
                name: "TEST".into(),
            }],
        };
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("ap_plmn_mapping.binarypb"),
            map.encode_to_vec(),
        )
        .unwrap();

        let loaded = load_mapping(dir.path());

        assert_eq!(loaded.len(), 1);
        let entry = loaded.get("TEST").expect("carrier TEST present");
        assert_eq!(entry.index, 63);
        assert_eq!(entry.plmns, vec![5_566_544]);
    }
}
