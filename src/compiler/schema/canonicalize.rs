#[cfg(test)]
use super::{source::SourceDocument, validate::validate_documents};
use super::{
    source::{LteSourceCombo, NrSourceCombo},
    validated::{ValidatedLteCombo, ValidatedNrCombo, ValidatedSources},
};
use crate::{
    compiler::{
        feature_palette::FeaturePalette,
        selection::{LteDomain, NrDomain, NrRelation},
    },
    raw_nr::RawNrPayload,
};
/// Validate + canonicalize + serialize in one step. Now used only by tests (the `decompose`/`provision`
/// paths validate once and reuse via [`ValidatedSources::to_kdl`]); kept as a convenient fixture
/// helper.
#[cfg(test)]
pub(crate) fn to_kdl(source: &SourceDocument) -> anyhow::Result<String> {
    validate_documents(source.clone())?.to_kdl()
}

/// One NR source combo projected from a payload and its relation: the selection in canonical
/// rectangle form, the five combo-header fields verbatim, and per-component catalog references
/// re-derived via `source_sub_block`. The write-side inverse of `validate_nr_combos`/`resolve`.
///
/// The single source for this projection — both the ingest side (`compiler::nr`'s
/// `finish_nr_document`, building the document for the first time) and the canonicalize side
/// ([`nr_source_combos`], rebuilding it from validated data) go through it, so a sixth
/// combo-header field cannot be added to one and forgotten in the other.
pub(crate) fn nr_source_combo(
    payload: &RawNrPayload,
    relation: &NrRelation,
    domain: &NrDomain,
    palette: &FeaturePalette,
) -> anyhow::Result<NrSourceCombo> {
    Ok(NrSourceCombo {
        selection: relation.canonical_selection(domain)?,
        header: payload.header,
        sub_blocks: payload
            .sub_blocks
            .iter()
            .map(|component| palette.source_sub_block(component))
            .collect::<anyhow::Result<Vec<_>>>()?,
    })
}

pub(super) fn nr_source_combos(
    combo: &[ValidatedNrCombo],
    domain: &NrDomain,
    palette: &FeaturePalette,
) -> anyhow::Result<Vec<NrSourceCombo>> {
    combo
        .iter()
        .map(|combo| nr_source_combo(&combo.payload, &combo.relation, domain, palette))
        .collect()
}

/// Rebuilds the LTE half's `combo` list from validated data: only `selection` is re-derived (to
/// its canonical rectangle form); every other field is a straight clone of the validated
/// source combo, since LTE combos carry no catalog references to re-derive.
fn lte_source_combos(
    combo: &[ValidatedLteCombo],
    domain: &LteDomain,
) -> anyhow::Result<Vec<LteSourceCombo>> {
    combo
        .iter()
        .map(|combo| {
            let mut source = combo.source.clone();
            source.selection = combo.relation.canonical_selection(domain)?;
            Ok(source)
        })
        .collect()
}

pub(super) fn canonicalize_sources(validated: &mut ValidatedSources) -> anyhow::Result<()> {
    validated.nr.source.bitmask_carriers.sort_unstable();
    for group in &mut validated.nr.source.bitmask_fingerprints {
        group.carriers.sort_unstable();
    }
    validated
        .nr
        .source
        .bitmask_fingerprints
        .sort_by(|left, right| {
            (left.fingerprint, &left.carriers).cmp(&(right.fingerprint, &right.carriers))
        });

    validated.nr.source.dl_features = validated.nr.palette.dl.clone();
    validated.nr.source.ul_features = validated.nr.palette.ul.clone();
    validated.nr.source.combos = nr_source_combos(
        &validated.nr.combos,
        &validated.nr.domain,
        &validated.nr.palette,
    )?;

    validated.lte.source.combos = lte_source_combos(&validated.lte.combos, &validated.lte.domain)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        compiler::{
            schema::{parse_sources, test_support::*},
            source_from_kdl,
        },
        raw_nr::Direction,
    };
    #[test]
    fn to_kdl_canonicalizes_metadata_payloads_and_selections() {
        let nr_text = format!(
            "{}\ndf s=3\nn {{\n    s {{\n        c \"PROFILED\" \"PROFILED\"\n        m \"G2YBB\" \"G2YBB\"\n    }}\n    n78 A1\n    B1 A\n}}\nn {{\n    s {{\n        c \"LEGACY\"\n    }}\n    B3 A\n}}\n",
            nr_with_complete_domain()
        );
        let lte_text = format!(
            "{}\nl {{\n    s {{\n        m \"lte:564260317\" \"G2YBB\" \"G2YBB\"\n    }}\n    B3 A4\n    B1 A4\n}}\nl {{\n    s {{\n        m \"GR83Y\"\n    }}\n    B7 A4\n}}\n",
            lte_with_complete_domain()
        );
        let text = to_kdl(&source_from_kdl(&source(&nr_text, &lte_text)).unwrap()).unwrap();

        assert!(text.ends_with('\n') && !text.ends_with("\n\n"));

        let canonical = parse_sources(&text).unwrap();
        assert_eq!(
            canonical.nr.combos[0]
                .payload
                .sub_blocks
                .iter()
                .map(|cc| cc.band_label().to_string())
                .collect::<Vec<_>>(),
            ["B1", "n78"]
        );
        assert_eq!(
            canonical.nr.combos[0].payload.sub_blocks[1].selector(Direction::Dl),
            None
        );
        assert_eq!(
            canonical.nr.combos[1].payload.sub_blocks[0]
                .band_label()
                .to_string(),
            "B3"
        );
        assert_eq!(canonical.lte.combos[0].source.components[0].band, 3);
        assert_eq!(canonical.lte.combos[1].source.components[0].band, 7);

        assert_eq!(to_kdl(&source_from_kdl(&text).unwrap()).unwrap(), text);
    }

    #[test]
    fn to_kdl_preserves_plmn_presence_order_duplicates_and_large_mapping_ids() {
        let nr_text = nr_with_carrier_sections(
            r#"
c "ABSENT" bi=1

c "MAP_ONLY" mi=18446744073709551615 {
    ps
}

c "ORDERED" mi=7 {
    p mcc=302 mnc=220
    p mcc=228
    p mcc=302 mnc=220
}
"#,
        )
        .replace("bc \"LEGACY\"", "bc \"ABSENT\" \"LEGACY\"")
        .replace("c \"LEGACY\"", "c \"ABSENT\" \"LEGACY\"");
        let text = to_kdl(&source_from_kdl(&source(&nr_text, MINIMAL_LTE)).unwrap()).unwrap();
        let nr = source_from_kdl(&text).unwrap().nr;
        assert_eq!(nr.carriers["ABSENT"].plmns, None);
        assert_eq!(nr.carriers["MAP_ONLY"].plmns, Some(Vec::new()));
        assert_eq!(
            nr.carriers["ORDERED"].plmns,
            Some(
                ["302-220", "228-ff", "302-220"]
                    .map(|s| s.parse().unwrap())
                    .into()
            )
        );
        assert_eq!(nr.carriers["MAP_ONLY"].profiled_id, None);
        assert_eq!(nr.carriers["MAP_ONLY"].mapping_id, Some(u64::MAX));
        assert!(text.contains("mi=18446744073709551615"));
    }
}
