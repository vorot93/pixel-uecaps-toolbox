use super::{read::read_nr_combo, read_sources, write::write_nr_combo, write_sources};
use crate::{
    compiler::{
        features::SourceSubBlock,
        schema::{LteDocument, NrDocument},
    },
    mapping::Plmn,
    model::Tier,
    proto::{ComboHeader, LteComponent, ShannonFeatureSetDlPerCcNr, ShannonFeatureSetUlPerCcNr},
    raw_nr::SubBlockKind,
};
use anyhow::Result;
use kdl::{KdlDocument, KdlNode};
#[cfg(test)]
mod numeric_key_tests {
    use super::*;

    #[test]
    fn numeric_keys_keep_decimal_text_order_at_both_writer_sites() {
        let source = read_sources("version 1\nbc\nf \"2\" fp=2 bm=2\nf \"10\" fp=10 bm=10\nc C sg=1 t=main {\n pf \"2\" x=2 u=0\n pf \"10\" x=10 u=0\n}\n").unwrap();
        let text = write_sources(&source.nr, &source.lte).unwrap();
        for prefix in ["f", "pf"] {
            assert!(
                text.find(&format!("{prefix} \"10\"")).unwrap()
                    < text.find(&format!("{prefix} \"2\"")).unwrap(),
                "{text}"
            );
        }
    }

    #[test]
    fn malformed_numeric_keys_are_rejected_by_the_reader() {
        for key in ["02", "+2", "18446744073709551616"] {
            for body in [
                format!("f \"{key}\" fp=1 bm=1"),
                format!("c C {{\n pf \"{key}\" x=1 u=1\n}}"),
            ] {
                let error = format!(
                    "{:#}",
                    read_sources(&format!("version 1\nbc\n{body}\n")).unwrap_err()
                );
                assert!(
                    error.contains(key) && error.contains("shortest-decimal u64"),
                    "{error}"
                );
            }
        }
    }

    #[test]
    fn numeric_key_error_phase_preserves_node_checks_and_document_version() {
        for (text, expected, absent) in [
            (
                "version 2\nbc\nf \"02\" fp=1 bm=1\n",
                "version",
                "shortest-decimal",
            ),
            ("version 1\nbc\nf \"02\" fp=1\n", "bm", "shortest-decimal"),
            (
                "version 1\nbc\nf \"02\" fp=1 bm=1 bogus=1\n",
                "bogus",
                "shortest-decimal",
            ),
            (
                "version 1\nbc\nf \"2\" fp=1 bm=1\nf \"2\" fp=1\n",
                "bm",
                "duplicate LTE",
            ),
            (
                "version 1\nbc\nc C {\n pf \"02\" x=1\n}\n",
                "u",
                "shortest-decimal",
            ),
            (
                "version 1\nbc\nc C {\n pf \"02\" x=1 u=1 bogus=1\n}\n",
                "bogus",
                "shortest-decimal",
            ),
            (
                "version 1\nbc\nc C {\n pf \"2\" x=1 u=1\n pf \"2\" x=1\n}\n",
                "u",
                "duplicate profile",
            ),
            (
                "version 1\nbc\nf \"02\" fp=1 bm=1\nf \"3\" fp=1\n",
                "shortest-decimal",
                "missing",
            ),
        ] {
            let error = format!("{:#}", read_sources(text).unwrap_err());
            assert!(
                error.contains(expected) && !error.contains(absent),
                "{error}"
            );
        }
    }
}

#[cfg(test)]
mod nr_tests {
    use super::*;
    use crate::compiler::{
        features::{SourceNrSubBlock, SourceSubBlock},
        schema::{BitmaskFingerprint, CarrierSource, NrDocument, NrSourceCombo, ProfileSource},
        selection::SelectionRect,
    };
    use std::collections::BTreeMap;

    /// The NR half on its own. `write_sources` always takes both halves; these tests exercise
    /// only the NR one, so the LTE half is empty. Returns `Result` rather than unwrapping so it
    /// stays a drop-in for the writer it replaced, whose failures some tests assert on.
    fn nr_only(nr: &NrDocument) -> Result<String> {
        write_sources(nr, &LteDocument::default())
    }

    /// Read a document and keep the NR half. The reader is document-level now, so the LTE half
    /// (empty in every fixture here) is simply dropped.
    fn nr_from(text: &str) -> Result<NrDocument> {
        Ok(read_sources(text)?.nr)
    }

    pub(super) fn sample() -> NrDocument {
        NrDocument {
            bitmask_carriers: vec!["ATT".into(), "VZW".into()],
            bitmask_fingerprints: vec![BitmaskFingerprint {
                fingerprint: 715_188_856,
                carriers: vec!["VZW".into()],
            }],
            carriers: BTreeMap::from([(
                "VZW".to_string(),
                CarrierSource {
                    bitmask_id: Some(1),
                    profiled_id: Some(0),
                    mapping_id: Some(u64::MAX),
                    plmns: Some(vec!["311-480".parse().unwrap()]),
                    signature: Some(1),
                    tier: Some(Tier::Main),
                    profiles: BTreeMap::from([(
                        66_813_533,
                        ProfileSource {
                            multiplier: 66_813_533,
                            unknown: 0,
                        },
                    )]),
                },
            )]),
            dl_features: vec![ShannonFeatureSetDlPerCcNr {
                max_scs: Some(3),
                max_mimo: Some(2),
                max_bw: Some(100),
                max_mod_order: Some(2),
                bw_90mhz_supported: Some(true),
            }],
            ul_features: vec![ShannonFeatureSetUlPerCcNr {
                max_scs: Some(2),
                max_mimo_cb: Some(1),
                max_bw: Some(100),
                max_mod_order: None,
                bw_90mhz_supported: None,
                max_mimo_non_cb: None,
            }],
            combos: vec![NrSourceCombo {
                selection: Some(vec![SelectionRect {
                    carriers: Some(vec!["VZW".into()]),
                    skus: Some(vec!["legacy".into()]),
                }]),
                header: ComboHeader {
                    power_class: Some(3),
                    bcs_nr: Some(1),
                    ..Default::default()
                },
                sub_blocks: vec![
                    SourceNrSubBlock {
                        band: 78,
                        dl_bw_class: Some(1),
                        // 1-based: the sole catalog entry. (This was `vec![0]`, an invalid
                        // reference the old format round-tripped without noticing because
                        // nothing checked it until `validate_documents`.)
                        dl_feature_indices: vec![1],
                        ..Default::default()
                    }
                    .into(),
                ],
            }],
        }
    }

    #[test]
    fn nr_round_trips_byte_identically() {
        let doc = sample();
        let text = nr_only(&doc).unwrap();
        let back = nr_from(&text).expect("read back");
        assert_eq!(nr_only(&back).unwrap(), text, "byte-identity");
        // spot-check the readable shape:
        assert!(text.contains("c VZW bi=1"), "{text}");
        assert!(text.contains("pf \"66813533\" x=66813533 u=0"), "{text}");
        assert!(text.contains("n78"), "{text}");
        assert!(
            text.contains("n78 A1"),
            "class and per-CC list merge into one value: {text}"
        );
        let parsed: KdlDocument = text.parse().unwrap();
        let combo = parsed
            .get(crate::compiler::kdl_keys::doc::NR_COMBO)
            .unwrap();
        let component = combo.children().unwrap().get("n78").unwrap();
        let properties: Vec<_> = component
            .entries()
            .iter()
            .filter_map(|entry| entry.name().map(|name| name.value()))
            .collect();
        assert_eq!(properties, Vec::<&str>::new());
        assert!(text.contains("mi=18446744073709551615"), "{text}");
    }

    #[test]
    fn nr_sub_block_repeated_features_and_lte_placeholder_round_trip() {
        // Two-CC NR sub-block: the comma-separated per-CC list in the DL positional argument
        // (`B5,8`) reads back as one usize per CC. LTE sub-block: no per-CC references at all.
        // The all-zero placeholder selector that the binary carries for it is NOT part of the
        // source model — the reader leaves it out entirely (`SourceSubBlock::resolve`
        // derives it from the DL positional argument's class, here `A` (class 1), on the
        // provision path; see `resolve_derives_the_omitted_placeholder` in
        // `compiler::features`) — and re-emitting must stay a byte-identical fixed point.
        let text = "version 1\nbc ATT\nn {\n    n48 B5,8\n    B66 A\n}\n";
        let doc = nr_from(text).expect("parse");
        let cc = &doc.combos[0].sub_blocks;
        let SourceSubBlock::Nr(nr) = &cc[0] else {
            panic!("first sub-block is an `nr` node, got {:?}", cc[0])
        };
        assert_eq!(nr.dl_feature_indices, vec![5, 8], "repeated dl_feature");
        // The `lte` variant has no per-CC feature list to be empty — that is the point.
        assert!(matches!(cc[1], SourceSubBlock::Lte(_)));
        assert_eq!(
            nr_only(&doc).unwrap(),
            text,
            "re-emitting must not resurrect the omitted LTE placeholder (fixed point)"
        );
    }

    #[test]
    fn nr_sized_indices_preserve_values_above_u16_and_lte_rejects_them() {
        let nr_text = "version 1\nbc ATT\nn {\n    n78 A65536\n}\n";
        let nr = nr_from(nr_text).expect("NR index above u16 parses");
        let [SourceSubBlock::Nr(component)] = &nr.combos[0].sub_blocks[..] else {
            panic!(
                "expected one NR sub-block, got {:?}",
                nr.combos[0].sub_blocks
            )
        };
        assert_eq!(
            component.dl_feature_indices,
            vec![usize::from(u16::MAX) + 1]
        );
        assert_eq!(
            nr_only(&nr).unwrap(),
            nr_text,
            "NR value remains byte-stable"
        );

        let lte_error = nr_from("version 1\nbc ATT\nn {\n    B66 A65536\n}\n")
            .unwrap_err()
            .to_string();
        assert!(lte_error.contains("LTE DL feature index"), "{lte_error}");
        assert!(lte_error.contains("u16"), "{lte_error}");
    }

    #[test]
    fn lte_sub_block_scalar_feature_names_and_ul_omit_when_zero_round_trip() {
        // On an `lte` node the proto-4/5 index is the single trailing number in the
        // positional direction value, single-valued; an omitted UL argument means index `0`
        // and re-defaults to `Some(0)`. No per-CC list is read on LTE. Byte-identical fixed
        // point.
        let text = "version 1\nbc ATT\nn {\n    B7 B1 A2\n    B66 A3\n}\n";
        let doc = nr_from(text).expect("parse");
        let cc = &doc.combos[0].sub_blocks;
        // Both are `lte` nodes, so neither can carry a per-CC feature list at all — the
        // variant simply has no such field.
        let [SourceSubBlock::Lte(first), SourceSubBlock::Lte(second)] = &cc[..] else {
            panic!("both sub-blocks are `lte` nodes, got {cc:?}")
        };
        assert_eq!(first.dl_feature_index, Some(1));
        assert_eq!(first.ul_feature_index, Some(2));
        assert_eq!(second.dl_feature_index, Some(3));
        assert_eq!(
            second.ul_feature_index,
            Some(0),
            "absent ul_feature on an lte node defaults to Some(0)"
        );
        assert_eq!(
            nr_only(&doc).unwrap(),
            text,
            "byte-identical fixed point: ul=0 stays omitted"
        );
    }

    #[test]
    fn carrier_plmns_use_structured_plmn_nodes() {
        let mut doc = sample();
        doc.carriers.get_mut("VZW").unwrap().plmns = Some(
            ["311-480", "310-04", "310-004", "228-ff", "310-04"]
                .map(|s| s.parse().unwrap())
                .into(),
        );
        let text = nr_only(&doc).unwrap();
        assert!(text.contains("p mcc=311 mnc=480"), "{text}");
        assert!(text.contains("p mcc=310 mnc=4 mnc-digits=3"), "{text}");
        assert!(
            text.contains("p mcc=228\n") || text.contains("p mcc=228 "),
            "{text}"
        );
        assert!(
            !text.contains("ps "),
            "old plmns list node must be gone: {text}"
        );
        // round-trips
        let back = nr_from(&text).unwrap();
        let carrier = back.carriers.values().next().unwrap();
        assert_eq!(
            carrier.plmns.as_deref().unwrap(),
            ["311-480", "310-04", "310-004", "228-ff", "310-04"]
                .map(|s| s.parse::<Plmn>().unwrap())
        );
    }

    #[test]
    fn carrier_plmns_empty_some_round_trips_distinct_from_none() {
        let mut doc = sample();
        doc.carriers.get_mut("VZW").unwrap().plmns = Some(Vec::new());
        doc.carriers
            .insert("NOPLMN".to_string(), CarrierSource::default());
        let text = nr_only(&doc).unwrap();
        let back = nr_from(&text).unwrap();
        assert_eq!(
            back.carriers["VZW"].plmns,
            Some(Vec::new()),
            "present-but-empty PLMN list must survive the round trip: {text}"
        );
        assert_eq!(
            back.carriers["NOPLMN"].plmns, None,
            "carrier with no plmns/plmn nodes at all must read back as None: {text}"
        );
    }

    #[test]
    fn nr_rejects_a_plmns_node_carrying_stale_list_args() {
        // A `plmns` node must be a *bare* marker (present-but-empty PLMN list). A stale
        // pre-migration `plmns "a" "b"` list node — this writer's old shape, and something
        // a hand-editor could still type — must be a hard parse error, never a silent
        // `Some(vec![])` that drops the listed PLMNs.
        let text = "version 1\nbc \"LEGACY\"\nc \"MAP\" mi=7 {\n    ps \"310-260\"\n}\n";
        let err = format!("{:#}", nr_from(text).unwrap_err());
        assert!(err.contains("ps"), "{err}");
    }

    #[test]
    fn nr_rejects_unknown_property() {
        let unmodified = nr_only(&sample()).unwrap();
        let text = unmodified.replace("n78", "n78 bogus=9");
        assert_ne!(text, unmodified);
        assert!(nr_from(&text).is_err());
    }

    /// This began as the sharpest instance of the duplicate-property hole: `dl-feature` was a
    /// per-CC *list* on an `nr` node but a single scalar on an `lte` node, so the identical
    /// spelling meant "both values" under one and "silently keep the last" under the other.
    ///
    /// Merging class and list into one value removes the repeated key entirely, so the mistake
    /// it guarded now takes a different shape — a multi-index value on an E-UTRA sub-block,
    /// which carries one `parseLteFeatureIndex` scalar whatever its class.
    #[test]
    fn lte_sub_block_rejects_a_repeated_feature_property() {
        let text = "version 1\nbc ATT\nn {\n    B66 B3,4\n}\n";

        let error = nr_from(text).unwrap_err().to_string();

        assert!(error.contains("at most one index"), "{error}");
        assert!(error.contains("E-UTRA"), "{error}");
    }

    /// The band is the node name's suffix, so a sub-block line reads as its 3GPP band
    /// designation — the same convention `SubBlockKind::band_label` uses everywhere else.
    #[test]
    fn sub_block_node_name_carries_the_band() {
        let text = "version 1\nbc ATT\nn {\n    n257 G1,1 A1\n    B66 A2\n}\n";

        let doc = nr_from(text).expect("bands parse out of the node name");
        let combo = &doc.combos[0];

        assert_eq!(combo.sub_blocks[0].band(), 257);
        assert_eq!(combo.sub_blocks[0].kind(), SubBlockKind::Nr);
        assert_eq!(combo.sub_blocks[1].band(), 66);
        assert_eq!(combo.sub_blocks[1].kind(), SubBlockKind::Lte);

        let out = nr_only(&doc).unwrap();
        assert!(out.contains("n257 "), "{out}");
        assert!(out.contains("B66 "), "{out}");
    }

    /// A malformed band must be rejected, not silently read as band 0 or waved through as an
    /// unknown child.
    #[test]
    fn malformed_sub_block_node_names_are_rejected() {
        for good in ["n78", "B66"] {
            nr_from(&format!("version 1\nbc ATT\nn {{\n    {good} A\n}}\n")).unwrap();
        }
        for bad in ["nr", "n257x", "nrfoo", "lte", "x99", "n99999999"] {
            let text = format!("version 1\nbc ATT\nn {{\n    {bad} A\n}}\n");
            let error = format!("{:#}", nr_from(&text).unwrap_err());
            assert!(
                error.contains(&format!("`n` has unknown child node `{bad}`")),
                "{error}"
            );
            assert!(!error.contains("unknown property"), "{error}");
        }
    }

    #[test]
    fn compiler_source_rejects_removed_props() {
        // The four escape-hatch surface keys were dropped (proto machinery kept). The strict
        // reader must now reject each as an unknown property.
        for key in [
            "dl-feature-index=1",
            "ul-feature-index=1",
            "dl-cc-id=1",
            "ul-cc-id=1",
        ] {
            let text = format!("version 1\nbc ATT\nn {{\n    n78 A {key}\n}}\n");
            let err = nr_from(&text).unwrap_err().to_string();
            assert!(
                err.contains("unknown property"),
                "{key} should be rejected: {err}"
            );
        }
    }

    #[test]
    fn nr_rejects_unknown_top_level_node() {
        let text = format!("{}\nmystery 1\n", nr_only(&sample()).unwrap());
        assert!(nr_from(&text).is_err());
    }

    /// Duplicates are rejected, and the message names the *concept*, not the key — a rename
    /// that let this degrade to `duplicate c` would violate DESIGN.md's diagnostics rule.
    #[test]
    fn nr_rejects_duplicate_carrier_by_name_not_by_key() {
        let text = format!("{}\nc VZW\n", nr_only(&sample()).unwrap());
        let error = format!("{:#}", nr_from(&text).unwrap_err());
        assert!(error.contains("duplicate carrier"), "{error}");
        assert!(!error.contains("duplicate c "), "{error}");
    }

    #[test]
    fn carrier_and_combo_nodes_use_distinct_document_keys() {
        let text = nr_only(&sample()).unwrap();
        let parsed: KdlDocument = text.parse().unwrap();
        use crate::compiler::kdl_keys::doc;
        assert_eq!(
            parsed
                .get(doc::CARRIER)
                .unwrap()
                .get(0)
                .and_then(|v| v.as_string()),
            Some("VZW")
        );
        assert_eq!(
            parsed
                .nodes()
                .iter()
                .filter(|node| node.name().value() == doc::NR_COMBO)
                .count(),
            sample().combos.len()
        );
    }

    #[test]
    fn nr_rejects_unknown_cc_kind() {
        let unmodified = nr_only(&sample()).unwrap();
        let text = unmodified.replace("n78", "bogus78");
        assert_ne!(text, unmodified);
        assert!(nr_from(&text).is_err());
    }

    #[test]
    fn nr_rejects_unknown_tier() {
        let unmodified = nr_only(&sample()).unwrap();
        let text = unmodified.replace("t=main", "t=bogus");
        assert_ne!(text, unmodified);
        assert!(nr_from(&text).is_err());
    }

    #[test]
    fn nr_rejects_bare_numeric_profile_key() {
        // A map key (the profile anchor) must be a quoted string arg, never a bare
        // integer — the reader rejects `profile 66813533` in favor of `profile "66813533"`.
        let unmodified = nr_only(&sample()).unwrap();
        let text = unmodified.replace("pf \"66813533\"", "pf 66813533");
        assert_ne!(text, unmodified);
        assert!(nr_from(&text).is_err());
    }

    fn combo_with(
        bcs_intra_endc: Option<u32>,
        intra_band_en_dc_support: Option<i32>,
    ) -> NrSourceCombo {
        NrSourceCombo {
            selection: None,
            header: ComboHeader {
                power_class: Some(0),
                bcs_nr: Some(0),
                bcs_intra_endc,
                bcs_eutra: Some(0),
                intra_band_en_dc_support,
            },
            sub_blocks: vec![],
        }
    }

    fn parse_combo(text: &str) -> KdlNode {
        let doc: KdlDocument = text.parse().expect("combo KDL parses");
        doc.nodes().first().expect("one combo node").clone()
    }

    #[test]
    fn bcs_intra_endc_derivable_zero_is_omitted_and_restored() {
        // Some(0) + intra=1: derived == actual → omitted on disk, re-derived on read.
        let node = write_nr_combo(&combo_with(Some(0), Some(1))).unwrap();
        let text = node.to_string();
        assert_eq!(
            node.get(crate::compiler::kdl_keys::nr_combo::BCS_INTRA_ENDC),
            None,
            "{text}"
        );
        let back = read_nr_combo(&parse_combo(&text)).unwrap();
        assert_eq!(back.header.bcs_intra_endc, Some(0));
        assert_eq!(back.header.intra_band_en_dc_support, Some(1));
    }

    #[test]
    fn bcs_intra_endc_exceptional_zero_stays_explicit() {
        // Some(0) + intra=0: derived is None → the zero is written explicitly (the ~20),
        // spelled as the empty BCS index list.
        let node = write_nr_combo(&combo_with(Some(0), Some(0))).unwrap();
        let text = node.to_string();
        assert_eq!(
            node.get(crate::compiler::kdl_keys::nr_combo::BCS_INTRA_ENDC)
                .and_then(|v| v.as_string()),
            Some(""),
            "{text}"
        );
        let back = read_nr_combo(&parse_combo(&text)).unwrap();
        assert_eq!(back.header.bcs_intra_endc, Some(0));
        assert_eq!(back.header.intra_band_en_dc_support, Some(0));
    }

    #[test]
    fn bcs_intra_endc_nonzero_stays_explicit() {
        let node = write_nr_combo(&combo_with(Some(7), Some(1))).unwrap();
        let text = node.to_string();
        assert_eq!(
            node.get(crate::compiler::kdl_keys::nr_combo::BCS_INTRA_ENDC)
                .and_then(|v| v.as_string()),
            Some("b29,30,31"),
            "{text}"
        );
        let back = read_nr_combo(&parse_combo(&text)).unwrap();
        assert_eq!(back.header.bcs_intra_endc, Some(7));
    }

    #[test]
    fn bcs_intra_endc_none_without_intra1_is_omitted() {
        let node = write_nr_combo(&combo_with(None, Some(0))).unwrap();
        let text = node.to_string();
        assert_eq!(
            node.get(crate::compiler::kdl_keys::nr_combo::BCS_INTRA_ENDC),
            None,
            "{text}"
        );
        let back = read_nr_combo(&parse_combo(&text)).unwrap();
        assert_eq!(back.header.bcs_intra_endc, None);
    }

    #[test]
    fn bcs_intra_endc_none_with_intra1_fails_closed() {
        // The single unrepresentable state (0 corpus cases): omission would re-derive
        // Some(0), so the writer must bail rather than silently corrupt.
        let err = write_nr_combo(&combo_with(None, Some(1))).unwrap_err();
        assert!(
            format!("{err:#}").contains("cannot be represented"),
            "{err:#}"
        );
    }

    /// `bn`/`be` spell their zero by omitting the property, so an explicit empty value would
    /// be a second spelling of the same value and is refused with the remedy.
    #[test]
    fn bcs_nr_and_eutra_round_trip_as_index_lists() {
        let text = "version 1\nbc ATT\nn bn=b0,1 be=b0 {\n    n78 A\n}\n";
        let doc = nr_from(text).expect("parse");
        assert_eq!(doc.combos[0].header.bcs_nr, Some(3_221_225_472));
        assert_eq!(doc.combos[0].header.bcs_eutra, Some(2_147_483_648));
        assert_eq!(
            nr_only(&doc).unwrap(),
            text,
            "index lists are a fixed point"
        );

        // An omitted property is the zero, and stays omitted on the way back out.
        let zero = "version 1\nbc ATT\nn {\n    n78 A\n}\n";
        let doc = nr_from(zero).expect("parse");
        assert_eq!(doc.combos[0].header.bcs_nr, Some(0));
        assert_eq!(nr_only(&doc).unwrap(), zero);

        for key in ["bn", "be"] {
            let text = format!("version 1\nbc ATT\nn {key}=\"\" {{\n    n78 A\n}}\n");
            let error = nr_from(&text).unwrap_err().to_string();
            assert!(error.contains("omitting the property"), "{error}");
        }
    }

    /// `bi` is derived-and-omitted when it matches, so the only values written are the
    /// exceptions — including the explicit empty set, which `""` spells.
    #[test]
    fn bcs_intra_endc_round_trips_as_an_index_list() {
        // Nonzero, written explicitly.
        let text = "version 1\nbc ATT\nn bi=b0,1 ie=1 {\n    n78 A\n}\n";
        let doc = nr_from(text).expect("parse");
        assert_eq!(doc.combos[0].header.bcs_intra_endc, Some(3_221_225_472));
        assert_eq!(nr_only(&doc).unwrap(), text);

        // The exceptional zero: `ie` is not 1, so `Some(0)` is NOT derivable and must be
        // spelled. The empty set is `""`.
        let text = "version 1\nbc ATT\nn bi=\"\" {\n    n78 A\n}\n";
        let doc = nr_from(text).expect("parse");
        assert_eq!(doc.combos[0].header.bcs_intra_endc, Some(0));
        assert_eq!(nr_only(&doc).unwrap(), text);
    }

    /// Stating the value the reader would derive anyway gives that value two spellings. The
    /// writer never emits one; this refuses a hand-edited document that does.
    #[test]
    fn an_explicitly_derivable_bcs_intra_endc_is_rejected() {
        // `ie=1` derives `Some(0)`, so an explicit empty value is the redundant spelling.
        let error = nr_from("version 1\nbc ATT\nn bi=\"\" ie=1 {\n    n78 A\n}\n")
            .unwrap_err()
            .to_string();
        assert!(error.contains("omit"), "{error}");
    }

    #[test]
    fn bw_class_is_direction_first_and_old_spelling_rejected() {
        // Directions are positional, DL then UL, and round-trip byte-identically.
        let text = "version 1\nbc ATT\nn {\n    n78 A A\n}\n";
        let doc = nr_from(text).expect("parse positional spelling");
        assert_eq!(
            nr_only(&doc).unwrap(),
            text,
            "positional spelling is a fixed point"
        );

        // A superseded spelling leaves the node with NO positional argument, so the reader
        // stops at the missing DL and never reaches the unknown-property check. That is the
        // diagnostic a stale document actually gets.
        for old in [
            "version 1\nbc ATT\nn {\n    n78 bw-class-dl=1 bw-class-ul=1\n}\n",
            "version 1\nbc ATT\nn {\n    n78 d=A u=A\n}\n",
        ] {
            let err = nr_from(old).unwrap_err().to_string();
            assert!(err.contains("missing its DL"), "got: {err}");
        }

        // Once a DL argument is present, a leftover key is reported as the unknown property
        // it is — the strict reader still has no alias for it.
        let err = nr_from("version 1\nbc ATT\nn {\n    n78 A d=A\n}\n")
            .unwrap_err()
            .to_string();
        assert!(err.contains("unknown property `d`"), "got: {err}");
    }

    #[test]
    /// Argument order is load-bearing for byte-identity: the first positional is DL, the
    /// second UL. Nothing else distinguishes them, so this is the only thing pinning it.
    fn nr_emits_direction_grouped_order() {
        let text = "version 1\nbc ATT\nn {\n    n78 A2 A3\n}\n";
        let doc = nr_from(text).expect("parse");
        let out = nr_only(&doc).unwrap();
        let dl = out.find("A2").expect("DL argument present");
        let ul = out.find("A3").expect("UL argument present");
        assert!(dl < ul, "expected dl before ul:\n{out}");
    }

    /// An absent DL would shift UL into the first argument and silently change the
    /// sub-block's meaning, so the reader refuses a sub-block with no arguments at all.
    #[test]
    fn a_sub_block_without_a_dl_argument_is_rejected() {
        let error = nr_from("version 1\nbc ATT\nn {\n    n78\n}\n")
            .unwrap_err()
            .to_string();
        assert!(error.contains("missing"), "{error}");
    }

    /// A third argument has no meaning and must not be silently dropped.
    #[test]
    fn a_sub_block_with_a_third_argument_is_rejected() {
        let error = nr_from("version 1\nbc ATT\nn {\n    n78 A2 A3 A4\n}\n")
            .unwrap_err()
            .to_string();
        assert!(error.contains("extra argument"), "{error}");
    }
}

#[cfg(test)]
mod lte_tests {
    use super::*;
    use crate::compiler::{
        schema::{LteDocument, LteFileSource, LteSourceCombo},
        selection::SelectionRect,
    };
    use std::collections::BTreeMap;

    /// The LTE half on its own — the mirror of `nr_tests::nr_only`. The empty NR half still
    /// emits its `bc` node (an empty list, not an omission), which is why every inline fixture
    /// below carries a bare `bc` line: without one the reader rejects the document outright.
    fn lte_only(lte: &LteDocument) -> Result<String> {
        write_sources(&NrDocument::default(), lte)
    }

    /// Read a document and keep the LTE half — the mirror of `nr_tests::nr_from`.
    fn lte_from(text: &str) -> Result<LteDocument> {
        Ok(read_sources(text)?.lte)
    }

    pub(super) fn sample() -> LteDocument {
        LteDocument {
            files: BTreeMap::from([(
                3,
                LteFileSource {
                    fingerprint: 715_188_856,
                    bitmask: 1,
                },
            )]),
            combos: vec![LteSourceCombo {
                selection: Some(vec![SelectionRect {
                    carriers: None,
                    skus: Some(vec!["legacy".into()]),
                }]),
                bcs: Some(2),
                unknown1: None,
                unknown2: None,
                components: vec![
                    LteComponent {
                        band: 1,
                        dl_bw_class_mimo: 32769,
                        ul_bw_class_mimo: Some(32769),
                    },
                    LteComponent {
                        band: 3,
                        dl_bw_class_mimo: 32769,
                        // Omit-when-0: this renders with no `u` at all.
                        ul_bw_class_mimo: Some(0),
                    },
                ],
            }],
        }
    }

    #[test]
    fn lte_round_trips_byte_identically() {
        let doc = sample();
        let text = lte_only(&doc).unwrap();
        let back = lte_from(&text).expect("read back");
        assert_eq!(lte_only(&back).unwrap(), text, "byte-identity");
        assert!(text.contains("f \"3\" fp=715188856 bm=1"), "{text}");
        assert!(text.contains("B1 A4 A4"), "{text}");
        assert!(text.contains("B3 A4\n"), "{text}");
    }

    /// An LTE combo's `b` distinguishes an explicit zero from an absent field — DESIGN.md
    /// requires that fidelity for the bit-for-bit LTE round trip — so unlike `bn`/`be` its
    /// empty set IS spelled, as `""`.
    #[test]
    fn lte_bcs_round_trips_as_an_index_list_and_keeps_absent_distinct() {
        let text = "version 1\nbc\nl b=b0,1 u1=0 u2=0 {\n    B1 A4\n}\n";
        let doc = lte_from(text).expect("parse");
        assert_eq!(doc.combos[0].bcs, Some(3_221_225_472));
        assert_eq!(lte_only(&doc).unwrap(), text);

        let explicit_zero = "version 1\nbc\nl b=\"\" u1=0 u2=0 {\n    B1 A4\n}\n";
        let doc = lte_from(explicit_zero).expect("parse");
        assert_eq!(doc.combos[0].bcs, Some(0));
        assert_eq!(lte_only(&doc).unwrap(), explicit_zero);

        let absent = "version 1\nbc\nl u1=0 u2=0 {\n    B1 A4\n}\n";
        let doc = lte_from(absent).expect("parse");
        assert_eq!(
            doc.combos[0].bcs, None,
            "an absent `b` is None, not Some(0)"
        );
        assert_eq!(lte_only(&doc).unwrap(), absent);
    }

    /// The guard exists because `LteCombo.bcs` is `uint64`; a value above 2^32 has never been
    /// observed and has no spelling, so the writer refuses it rather than truncating.
    #[test]
    fn an_lte_bcs_wider_than_32_bits_fails_closed() {
        let mut doc = lte_from("version 1\nbc\nl u1=0 u2=0 {\n    B1 A4\n}\n").expect("parse");
        doc.combos[0].bcs = Some(u64::from(u32::MAX) + 1);
        let error = lte_only(&doc).unwrap_err().to_string();
        assert!(error.contains("32-bit"), "{error}");
    }

    #[test]
    fn lte_rejects_unknown_property() {
        // Built directly rather than via `sample()`'s round-tripped output: `sample()`'s
        // `bcs = Some(2)` now spells as `b=b30` (2 is bit 1 of the 32-bit word, so index
        // 31-1 = 30), so a `.replace("b=2", …)` on the writer's own output would silently
        // become a no-op once the index-list codec landed.
        let text = "version 1\nbc\nl b=b30 u1=0 u2=0 bogus=9 {\n    B1 A4\n}\n";
        assert!(lte_from(text).is_err());
    }

    #[test]
    fn lte_rejects_superseded_direction_property_spellings() {
        // Companion to `bw_class_is_direction_first_and_old_spelling_rejected`, which covers only
        // the NR combo pair. Three generations of now-dead property spelling are checked here:
        // the direction-last `md`/`mu`, the `dm`/`um` that carried the class+MIMO encoding in the
        // key, and the property form `d`/`u` itself — superseded by the positional spelling
        // `lte_directions_are_positional_and_the_old_keys_are_rejected` covers.
        for dead in ["md", "mu", "dm", "um", "d", "u"] {
            // A valid positional sub-block with a leftover dead property appended, so the reader
            // reports the unknown property rather than a missing required one.
            let text = format!("version 1\nbc\nl {{\n    B1 A4 A2 {dead}=A4\n}}\n");
            let err = lte_from(&text).unwrap_err().to_string();
            assert!(
                err.contains(&format!("unknown property `{dead}`")),
                "{dead} must be rejected, got: {err}"
            );
        }
    }

    /// LTE sub-blocks spell their directions positionally too, and the superseded `d=`/`u=`
    /// keys are rejected rather than silently ignored.
    #[test]
    fn lte_directions_are_positional_and_the_old_keys_are_rejected() {
        let text = "version 1\nbc\nl {\n    B1 A4 A2\n}\n";
        let doc = lte_from(text).expect("parse positional spelling");
        assert_eq!(lte_only(&doc).unwrap(), text, "positional is a fixed point");

        // `B1 d=A4` has no positional argument at all, so the reader stops at the missing DL.
        let error = lte_from("version 1\nbc\nl {\n    B1 d=A4\n}\n")
            .unwrap_err()
            .to_string();
        assert!(error.contains("missing its DL"), "{error}");

        // With DL present, the leftover key is reported as the unknown property it is.
        let error = lte_from("version 1\nbc\nl {\n    B1 A4 u=A2\n}\n")
            .unwrap_err()
            .to_string();
        assert!(error.contains("unknown property `u`"), "{error}");
    }

    /// A DL class+MIMO value is mandatory; without it the UL value would slide into first
    /// place and be read as the downlink.
    #[test]
    fn an_lte_sub_block_without_a_dl_argument_is_rejected() {
        let error = lte_from("version 1\nbc\nl {\n    B1\n}\n")
            .unwrap_err()
            .to_string();
        assert!(error.contains("missing"), "{error}");
    }

    /// The LTE counterpart to `nr_tests::a_sub_block_with_a_third_argument_is_rejected`:
    /// a third argument has no meaning and must not be silently dropped. Both readers share the
    /// same `NodeReader::finish` mechanism, so the `n` and `l` sub-block readers are pinned
    /// symmetrically here.
    #[test]
    fn an_lte_sub_block_with_a_third_argument_is_rejected() {
        let error = lte_from("version 1\nbc\nl {\n    B1 A4 A2 A2\n}\n")
            .unwrap_err()
            .to_string();
        assert!(error.contains("extra argument"), "{error}");
    }

    #[test]
    fn lte_rejects_duplicate_file() {
        let text = format!("{}\nf \"3\" fp=1 bm=1\n", lte_only(&sample()).unwrap());
        assert!(lte_from(&text).is_err());
    }

    #[test]
    fn lte_rejects_bare_numeric_file_key() {
        // The LTE file id is a map key: a quoted string arg, not a bare integer.
        let unmodified = lte_only(&sample()).unwrap();
        let text = unmodified.replace("f \"3\"", "f 3");
        assert_ne!(text, unmodified);
        assert!(lte_from(&text).is_err());
    }
}

#[cfg(test)]
mod source_tests {
    use super::*;

    /// One document, both halves, in canonical order: version, the whitelists and identity
    /// nodes, the two catalogs, then every NR combo, then every LTE combo.
    #[test]
    fn source_round_trips_byte_identically() {
        let nr = super::nr_tests::sample();
        let lte = super::lte_tests::sample();
        let text = write_sources(&nr, &lte).unwrap();
        let parsed = read_sources(&text).unwrap();
        let reserialized = write_sources(&parsed.nr, &parsed.lte).unwrap();
        assert_eq!(text, reserialized);
    }

    /// Exactly one version marker, and the node order is the canonical one.
    #[test]
    fn node_order_is_canonical_with_one_version_marker() {
        let text = write_sources(&super::nr_tests::sample(), &super::lte_tests::sample()).unwrap();
        // Top-level node names sit at column 0 — and so does the `}` that closes a block node,
        // which is why it is filtered out too rather than read as a node name.
        let kinds: Vec<&str> = text
            .lines()
            .filter(|line| !line.starts_with(char::is_whitespace) && !line.is_empty())
            .filter(|line| *line != "}")
            .map(|line| line.split_whitespace().next().unwrap())
            .map(|name| name.trim_end_matches('{'))
            .collect();
        assert_eq!(
            kinds.iter().filter(|k| **k == "version").count(),
            1,
            "{kinds:?}"
        );
        let rank = |k: &str| match k {
            "version" => 0,
            "bc" => 1,
            "bf" => 2,
            "c" => 3,
            "f" => 4,
            "df" => 5,
            "uf" => 6,
            "n" => 7,
            "l" => 8,
            other => panic!("unexpected top-level node `{other}`"),
        };
        let ranks: Vec<u8> = kinds.iter().map(|k| rank(k)).collect();
        assert!(ranks.windows(2).all(|w| w[0] <= w[1]), "{kinds:?}");
    }

    /// Two `version` markers is the failure mode of naively concatenating two old documents.
    #[test]
    fn two_version_markers_are_rejected() {
        let text = format!(
            "{}\nversion 1\n",
            write_sources(&super::nr_tests::sample(), &super::lte_tests::sample()).unwrap()
        );
        let error = format!("{:#}", read_sources(&text).unwrap_err());
        assert!(error.contains("duplicate `version`"), "{error}");
    }

    #[test]
    fn a_document_without_a_version_marker_is_rejected() {
        let unmodified =
            write_sources(&super::nr_tests::sample(), &super::lte_tests::sample()).unwrap();
        let text = unmodified.replacen("version 1\n", "", 1);
        assert_ne!(text, unmodified);
        let error = format!("{:#}", read_sources(&text).unwrap_err());
        assert!(error.contains("missing `version`"), "{error}");
    }

    /// `checked_version` *scans* for the marker rather than reading the first node, so the version
    /// is diagnosed even in a document that puts it somewhere else. Every other fixture in the
    /// suite opens with `version`, so without this one a "simplification" to `doc.nodes().first()`
    /// would silently defeat the scan — and defeat it exactly where it matters, since a stale tree
    /// whose vocabulary also changed would go back to failing on the vocabulary instead.
    #[test]
    fn the_version_marker_is_found_wherever_it_sits() {
        let good = read_sources("bc ATT\nversion 1\n").expect("`version` may follow another node");
        // The body still maps: the scan neither swallowed the preceding node nor stopped at it.
        assert_eq!(good.nr.bitmask_carriers, ["ATT"]);

        let error = format!("{:#}", read_sources("bc ATT\nversion 2\n").unwrap_err());
        assert!(error.contains("source-format version 2"), "{error}");
    }

    /// A `bf` group's carrier dedented to top level becomes a second declaration of a carrier
    /// that already exists — caught as a duplicate rather than silently absorbed. This hazard
    /// is specific to `cr` -> `c`: before the rename the dedented node was simply unknown.
    #[test]
    fn a_dedented_fingerprint_carrier_is_rejected_as_a_duplicate() {
        let text = "version 1\nbc ALPHA\nbf 1 {\n    c ALPHA\n}\nc ALPHA\nc ALPHA\n";
        let error = format!("{:#}", read_sources(text).unwrap_err());
        assert!(error.contains("duplicate carrier"), "{error}");
    }

    /// The same sub-block text means two different things depending on which combo encloses it:
    /// in an `n` combo a bandwidth class plus a per-CC feature index (`parse_direction`); in an
    /// `l` combo a bandwidth class plus a MIMO-width bitfield (`parse_class_mimo`). Nothing in
    /// the sub-block line says which. The enclosing `n`/`l` node settles it — the disambiguator
    /// the two separate documents used to provide. Nothing else in the suite ties the two codecs
    /// together, so an editor who noticed the shared spelling and "unified" them would break
    /// nothing visible elsewhere — only every real LTE combo, silently. The collision is a
    /// deliberate trade, not an oversight: the enclosing combo fixes the interpretation, the same
    /// way a sub-block node name carries no radio-kind tag. This test is where that trade is meant
    /// to be learned before anyone "fixes" it. See DESIGN.md.
    #[test]
    fn identical_sub_block_text_means_different_things_under_n_and_l() {
        // The exact same positional text, `C2`, under each combo kind — in ONE document now.
        let text = "version 1\nbc\nn {\n    B66 C2\n}\nl b=\"\" {\n    B66 C2\n}\n";
        let parsed = read_sources(text).expect("the source document parses");

        // Under `n`: `C2` is bandwidth class C (3) plus per-CC feature index 2. A `B66` node
        // always parses to the `Lte` variant of `SourceSubBlock`.
        let SourceSubBlock::Lte(sub_block) = &parsed.nr.combos[0].sub_blocks[0] else {
            panic!(
                "a `B66` node parses to the `Lte` variant, got {:?}",
                parsed.nr.combos[0].sub_blocks[0]
            )
        };
        assert_eq!(sub_block.band, 66);
        assert_eq!(sub_block.dl_bw_class, Some(3));
        assert_eq!(sub_block.dl_feature_index, Some(2));

        // Under `l`: the identical text `C2` is class C + 2x2 MIMO, the bitfield 8192.
        let component = &parsed.lte.combos[0].components[0];
        assert_eq!(component.band, 66);
        assert_eq!(component.dl_bw_class_mimo, 8192);

        // Spell out that these are different INTERPRETATIONS of identical text, not merely
        // different incidental numbers: the NR value is a small 1-based catalog reference, the
        // LTE one a bitfield. If the two positional codecs were ever unified, this is the
        // assertion that would catch it.
        assert_ne!(
            i64::from(sub_block.dl_feature_index.unwrap()),
            i64::from(component.dl_bw_class_mimo),
            "the per-CC feature index and the class+MIMO bitfield must stay disjoint decodings \
             of the same `C2` text — if they ever match, the two codecs have merged"
        );

        // `B66 A` parses under `n`: class A (1), with an empty per-CC list — the all-zero
        // placeholder that `features::resolve` re-materializes later. The resulting
        // `dl_feature` is incidental to this test's point and deliberately not pinned here.
        let placeholder = read_sources("version 1\nbc ATT\nn {\n    B66 A\n}\n")
            .expect("`A` parses as a class with no per-CC list");
        let SourceSubBlock::Lte(placeholder_sub_block) = &placeholder.nr.combos[0].sub_blocks[0]
        else {
            panic!("still a `B66`/`Lte` sub-block")
        };
        assert_eq!(placeholder_sub_block.dl_bw_class, Some(1));

        // The identical `B66 A` is rejected under `l`: a class+MIMO value always needs the MIMO
        // digit that the NR bare-class placeholder spelling never carries.
        let error = read_sources("version 1\nbc\nl {\n    B66 A\n}\n")
            .unwrap_err()
            .to_string();
        assert!(error.contains("MIMO width"), "{error}");
    }
}
