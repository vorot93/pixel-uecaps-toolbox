//! KDL (de)serialization for the PLMN legend (`mapping decode`/`encode`).

use anyhow::{Context, Result, bail};
use kdl::{KdlDocument, KdlEntry, KdlNode};

use crate::{
    kdl_support::{NodeReader, finish_doc, plmn_to_node, read_plmn},
    mapping::schema::{MappingEntry, Root},
};

const VERSION: i128 = 1;

pub(crate) fn root_to_kdl(root: &Root) -> Result<String> {
    let mut doc = KdlDocument::new();

    let mut version = KdlNode::new("version");
    version.push(KdlEntry::new(VERSION));
    doc.nodes_mut().push(version);

    for m in &root.mappings {
        let mut node = KdlNode::new("mapping");
        node.push(KdlEntry::new_prop("id", i128::from(m.id)));
        node.push(KdlEntry::new_prop("name", m.name.as_str()));
        if !m.plmns.is_empty() {
            let kids = node.ensure_children();
            for p in &m.plmns {
                kids.nodes_mut().push(plmn_to_node(p)?);
            }
        }
        doc.nodes_mut().push(node);
    }
    Ok(finish_doc(doc))
}

pub(crate) fn root_from_kdl(text: &str) -> Result<Root> {
    let doc: KdlDocument = text.parse().context("mapping legend is not valid KDL")?;
    let mut version: Option<i128> = None;
    let mut mappings = Vec::new();
    for node in doc.nodes() {
        match node.name().value() {
            "version" => {
                if version.is_some() {
                    bail!("duplicate `version`");
                }
                let mut r = NodeReader::new(node);
                version = Some(r.key_int::<i128>()?);
                r.finish()?;
            }
            "mapping" => mappings.push(read_mapping(node)?),
            other => bail!("unknown top-level node `{other}` in the mapping legend"),
        }
    }
    match version {
        Some(VERSION) => {}
        Some(v) => {
            bail!("unsupported mapping legend version {v} (this build understands {VERSION})")
        }
        None => bail!("mapping legend missing `version`"),
    }
    Ok(Root { mappings })
}

fn read_mapping(node: &KdlNode) -> Result<MappingEntry> {
    let mut r = NodeReader::new(node);
    let id = r.req_int::<u64>("id")?;
    let name = r.req_str("name")?;
    let plmns = r
        .children("plmn")
        .iter()
        .map(|n| read_plmn(n))
        .collect::<Result<Vec<_>>>()?;
    r.finish()?;
    Ok(MappingEntry { id, name, plmns })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mapping::schema::{MappingEntry, Root};

    fn sample() -> Root {
        Root {
            mappings: vec![
                MappingEntry {
                    id: 1,
                    name: "VZW".into(),
                    plmns: vec!["311-480".into(), "310-004".into(), "228-ff".into()],
                },
                MappingEntry {
                    id: 2,
                    name: "ATT".into(),
                    plmns: vec!["310-410".into()],
                },
            ],
        }
    }

    #[test]
    fn legend_round_trips_through_kdl() {
        let text = root_to_kdl(&sample()).unwrap();
        assert!(text.starts_with("version 1"), "{text}");
        assert!(text.contains("mapping id=1 name=VZW"), "{text}");
        assert!(text.contains("plmn mcc=310 mnc=4 mnc-digits=3"), "{text}");
        assert!(
            text.contains("plmn mcc=228\n") || text.contains("plmn mcc=228 "),
            "{text}"
        );
        assert_eq!(root_from_kdl(&text).unwrap(), sample());
    }

    #[test]
    fn rejects_missing_version() {
        assert!(root_from_kdl("mapping id=1 name=VZW\n").is_err());
    }

    #[test]
    fn rejects_unknown_top_node() {
        assert!(root_from_kdl("version 1\nbogus 1\n").is_err());
    }

    #[test]
    fn empty_plmns_round_trips() {
        // A carrier with no PLMNs is a childless `mapping` node; it must read back as an
        // empty Vec, distinct from its neighbors — not error, not absorb the next carrier.
        let root = Root {
            mappings: vec![
                MappingEntry {
                    id: 7,
                    name: "EMPTY".into(),
                    plmns: vec![],
                },
                MappingEntry {
                    id: 8,
                    name: "ONE".into(),
                    plmns: vec!["310-410".into()],
                },
            ],
        };
        let text = root_to_kdl(&root).unwrap();
        assert!(
            !text.contains("EMPTY {"),
            "empty plmns → no children block: {text}"
        );
        assert_eq!(root_from_kdl(&text).unwrap(), root);
    }
}
