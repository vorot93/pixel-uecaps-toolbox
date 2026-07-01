use super::{Error, Plmn};
use crate::proto::{Carrier, PlmnMap};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

#[derive(Debug, Serialize, Deserialize)]
pub struct Root {
    #[serde(default, rename = "mapping")]
    pub mappings: Vec<MappingEntry>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct MappingEntry {
    pub id: u64,
    pub name: String,
    pub plmns: Vec<String>,
}

/// proto → editable model (decode). Preserves entry and `plmns` order verbatim.
pub fn map_to_root(map: &PlmnMap) -> Result<Root, Error> {
    let mut mappings = Vec::with_capacity(map.carriers.len());
    for c in &map.carriers {
        let plmns = c
            .plmns
            .iter()
            .map(|&v| Ok(Plmn::from_encoded(v)?.to_string()))
            .collect::<Result<Vec<_>, Error>>()?;
        mappings.push(MappingEntry {
            id: c.index,
            name: c.name.clone(),
            plmns,
        });
    }
    Ok(Root { mappings })
}

/// editable model → proto (encode). Validates unique ids/names and non-empty names;
/// keeps entry order and duplicate PLMNs verbatim.
pub fn root_to_map(root: &Root) -> Result<PlmnMap, Error> {
    let mut seen_ids: HashSet<u64> = HashSet::new();
    let mut seen_names: HashSet<&str> = HashSet::new();
    let mut carriers = Vec::with_capacity(root.mappings.len());
    for MappingEntry { id, name, plmns } in &root.mappings {
        if name.is_empty() {
            return Err(Error::EmptyName(*id));
        }
        if !seen_ids.insert(*id) {
            return Err(Error::DuplicateId(*id));
        }
        if !seen_names.insert(name.as_str()) {
            return Err(Error::DuplicateName(name.clone()));
        }
        let plmns = plmns
            .iter()
            .map(|s| Ok(s.parse::<Plmn>()?.to_encoded()))
            .collect::<Result<Vec<_>, Error>>()?;
        carriers.push(Carrier {
            plmns,
            index: *id,
            name: name.clone(),
        });
    }
    Ok(PlmnMap { carriers })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_duplicate_id() {
        let root = Root {
            mappings: vec![
                MappingEntry {
                    id: 1,
                    name: "A".into(),
                    plmns: vec![],
                },
                MappingEntry {
                    id: 1,
                    name: "B".into(),
                    plmns: vec![],
                },
            ],
        };
        assert!(matches!(root_to_map(&root), Err(Error::DuplicateId(1))));
    }

    #[test]
    fn rejects_duplicate_name() {
        let root = Root {
            mappings: vec![
                MappingEntry {
                    id: 1,
                    name: "A".into(),
                    plmns: vec![],
                },
                MappingEntry {
                    id: 2,
                    name: "A".into(),
                    plmns: vec![],
                },
            ],
        };
        assert!(matches!(root_to_map(&root), Err(Error::DuplicateName(_))));
    }

    #[test]
    fn rejects_empty_name() {
        let root = Root {
            mappings: vec![MappingEntry {
                id: 1,
                name: String::new(),
                plmns: vec![],
            }],
        };
        assert!(matches!(root_to_map(&root), Err(Error::EmptyName(1))));
    }

    #[test]
    fn map_to_root_decodes_fields_and_plmns() {
        let map = PlmnMap {
            carriers: vec![Carrier {
                plmns: vec![5_435_408],
                index: 7,
                name: "X".into(),
            }],
        };
        let root = map_to_root(&map).unwrap();
        assert_eq!(root.mappings[0].id, 7);
        assert_eq!(root.mappings[0].name, "X");
        assert_eq!(root.mappings[0].plmns, vec!["250-01".to_string()]);
    }
}
