//! Reader for `ap_plmn_mapping.binarypb` — the PLMN→carrier legend.

mod edit;
mod error;
mod plmn;
mod schema;

pub use edit::{add_plmns_strict, decode, encode, inject};
pub use error::Error;
pub use plmn::Plmn;

use crate::proto::PlmnMap;
use prost::Message;
use std::{collections::BTreeMap, path::Path};

pub struct CarrierEntry {
    pub index: Option<u64>,
    pub plmns: Vec<u64>,
}

/// Load the legend from the directory containing `ap_plmn_mapping.binarypb`.
/// Returns an empty map if the file is missing or unreadable.
pub fn load_mapping(dir: &Path) -> BTreeMap<String, CarrierEntry> {
    let Ok(data) = std::fs::read(dir.join("ap_plmn_mapping.binarypb")) else {
        return BTreeMap::new();
    };
    let Ok(map) = PlmnMap::decode(&data[..]) else {
        return BTreeMap::new();
    };
    map.carriers
        .into_iter()
        .filter(|c| !c.name.is_empty())
        .map(|c| {
            (
                c.name,
                CarrierEntry {
                    index: Some(c.index),
                    plmns: c.plmns,
                },
            )
        })
        .collect()
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
