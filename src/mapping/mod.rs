//! Reader for `ap_plmn_mapping.binarypb` — the PLMN→carrier legend.

mod error;
mod plmn;
mod schema;

pub use error::Error;
pub use plmn::Plmn;
pub(crate) use schema::{
    MappingEntry, Root as MappingRoot, encode_root_verified, map_to_root, root_to_map,
};

use crate::proto::PlmnMap;
use prost::Message;
use std::{collections::BTreeMap, path::Path};

pub struct CarrierEntry {
    pub index: Option<u64>,
    pub plmns: Vec<u64>,
}

/// A leniently-loaded legend plus the structural anomalies the lenient collapse would
/// otherwise hide. `entries` is exactly what [`load_mapping`] returns (last entry wins per
/// name; empty-named carriers dropped). The write path ([`root_to_map`]) hard-errors on
/// duplicate and empty names, but the read path must stay lenient so a junk legend still
/// yields a best-effort view — so these fields let the *report* surfaces (`check`) flag the
/// corruption instead of auditing it as clean (the read/write asymmetry is the bug).
#[derive(Default)]
pub struct LegendReport {
    pub entries: BTreeMap<String, CarrierEntry>,
    /// Carrier names carried by more than one entry (each listed once), sorted.
    pub duplicate_names: Vec<String>,
    /// Number of carriers dropped from `entries` for having an empty name.
    pub empty_named: usize,
}

/// Load the legend from the directory containing `ap_plmn_mapping.binarypb`.
/// Returns an empty map if the file is missing or unreadable.
pub fn load_mapping(dir: &Path) -> BTreeMap<String, CarrierEntry> {
    load_mapping_report(dir).entries
}

/// Load the legend and report structural anomalies (see [`LegendReport`]). Returns an empty
/// report if the file is missing or unreadable (lenient, like [`load_mapping`]).
pub fn load_mapping_report(dir: &Path) -> LegendReport {
    let Ok(data) = std::fs::read(dir.join("ap_plmn_mapping.binarypb")) else {
        return LegendReport::default();
    };
    let Ok(map) = PlmnMap::decode(&data[..]) else {
        return LegendReport::default();
    };
    let mut entries = BTreeMap::new();
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    let mut empty_named = 0usize;
    for c in map.carriers {
        if c.name.is_empty() {
            empty_named += 1;
            continue;
        }
        *counts.entry(c.name.clone()).or_insert(0) += 1;
        entries.insert(
            c.name,
            CarrierEntry {
                index: Some(c.index),
                plmns: c.plmns,
            },
        );
    }
    let duplicate_names = counts
        .into_iter()
        .filter(|(_, n)| *n > 1)
        .map(|(name, _)| name)
        .collect();
    LegendReport {
        entries,
        duplicate_names,
        empty_named,
    }
}

#[cfg(test)]
mod tests {
    use super::load_mapping;
    use crate::proto::{Carrier, PlmnMap};
    use prost::Message;

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
        let dir = std::env::temp_dir().join(format!("uecaps-maptest-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("ap_plmn_mapping.binarypb"), map.encode_to_vec()).unwrap();

        let loaded = load_mapping(&dir);
        std::fs::remove_dir_all(&dir).ok();

        assert_eq!(loaded.len(), 1);
        let entry = loaded.get("TEST").expect("carrier TEST present");
        assert_eq!(entry.index, Some(63));
        assert_eq!(entry.plmns, vec![5_566_544]);
    }
}
