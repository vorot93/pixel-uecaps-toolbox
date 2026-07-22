//! One-file KDL slices of `nr.kdl` / `lte.kdl` — the capability branches of the
//! `decode` command. Write-only: a slice carries no cross-file metadata and no
//! `selection`, so it is not a `build` input.

use super::{
    kdl_source::{emit_dl_feature, emit_lte_combo, emit_nr_combo, emit_ul_feature},
    lte::lte_source_from_one_file,
    nr::nr_source_from_one_file,
};
use crate::{
    kdl_support::finish_doc,
    proto::{LteCaps, UeCaps},
};
use kdl::{KdlDocument, KdlEntry, KdlNode};
use prost::Message;

/// A carrier file's slice: `version 1` + the file's own dl-feature/ul-feature
/// catalogs + its combos, emitted through the compiler's own writers so the spelling
/// matches `nr.kdl` exactly. Lenient — undecodable bytes yield
/// [`version_only_document`] and exit 1.
pub(crate) fn nr_slice(bytes: &[u8]) -> anyhow::Result<(String, i32)> {
    let Ok(caps) = UeCaps::decode(bytes) else {
        return Ok((version_only_document(), 1));
    };
    let (dl, ul, combos) = nr_source_from_one_file(&caps);
    let mut doc = versioned_document();
    for f in &dl {
        doc.nodes_mut().push(emit_dl_feature(f));
    }
    for f in &ul {
        doc.nodes_mut().push(emit_ul_feature(f));
    }
    for combo in &combos {
        doc.nodes_mut().push(emit_nr_combo(combo)?);
    }
    Ok((finish_doc(doc), 0))
}

/// An `lte_*` file's slice: `version 1` + its combos, via the compiler's own writer.
/// Lenient, like [`nr_slice`].
pub(crate) fn lte_slice(bytes: &[u8]) -> (String, i32) {
    let Ok(caps) = LteCaps::decode(bytes) else {
        return (version_only_document(), 1);
    };
    let mut doc = versioned_document();
    for combo in &lte_source_from_one_file(&caps) {
        doc.nodes_mut().push(emit_lte_combo(combo));
    }
    (finish_doc(doc), 0)
}

/// What an unreadable capability file emits: just `version 1`, no combos, no
/// catalogs. Intentionally empty so a stale round-trip cannot fabricate data the
/// file does not contain; the diagnostic belongs to `inspect`'s text report.
fn version_only_document() -> String {
    finish_doc(versioned_document())
}

/// A fresh document whose only node is `version 1`.
fn versioned_document() -> KdlDocument {
    let mut doc = KdlDocument::new();
    let mut version = KdlNode::new("version");
    version.push(KdlEntry::new(1i128));
    doc.nodes_mut().push(version);
    doc
}

#[cfg(test)]
mod tests {
    use super::{lte_slice, nr_slice};
    use crate::proto::{
        ComboGroup, LteCaps, LteCombo, LteComponent, UeCaps, combo_group,
        combo_group::combo::SubBlock,
    };
    use prost::Message;

    fn carrier_bytes() -> Vec<u8> {
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
    fn nr_slice_matches_the_nr_kdl_shape() {
        let (text, code) = nr_slice(&carrier_bytes()).unwrap();
        assert_eq!(code, 0, "decodable bytes exit 0");
        assert!(text.starts_with("version 1"), "{text}");
        assert!(text.contains("nr 78"), "{text}");
        // No diagnostic envelope and no display-only extensions: this is a slice of
        // nr.kdl, not the text report.
        assert!(!text.contains("type=carrier"), "{text}");
        assert!(!text.contains("dl-scs-khz"), "{text}");
        assert!(!text.contains("fingerprint-status"), "{text}");
    }

    #[test]
    fn lte_slice_matches_the_lte_kdl_shape() {
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
        let (text, code) = lte_slice(&caps.encode_to_vec());
        assert_eq!(code, 0, "decodable bytes exit 0");
        assert!(text.starts_with("version 1"), "{text}");
        assert!(text.contains("subblock 1 dl-bw-class-mimo=32768"), "{text}");
        // ul-bw-class-mimo is None -- omitted, matching lte.kdl's presence semantics.
        assert!(!text.contains("ul-bw-class-mimo"), "{text}");
        assert!(!text.contains("config-family"), "{text}");
        assert!(!text.contains("fingerprint="), "{text}");
    }

    #[test]
    fn undecodable_bytes_yield_a_version_only_document_and_code_one() {
        // Truncated field 3 -- UeCaps::decode fails.
        let (text, code) = nr_slice(&[0x1a, 0x05, 0x01]).unwrap();
        assert_eq!(code, 1, "an unreadable file must exit 1");
        assert!(
            text.starts_with("version 1") && !text.contains("combo"),
            "an empty slice must not fabricate data: {text}"
        );
    }
}
