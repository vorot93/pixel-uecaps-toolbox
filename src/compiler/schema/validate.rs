use super::{
    canonicalize::canonicalize_sources,
    source::{
        CarrierSource, LteDocument, LteFileSource, LteSourceCombo, NrDocument, NrSourceCombo,
        ProfileSource, SourceDocument,
    },
    validated::{
        LegendEntry, NrSelectionIndex, ProfiledRole, ValidatedCarrier, ValidatedLte,
        ValidatedLteCombo, ValidatedNr, ValidatedNrCombo, ValidatedProfile, ValidatedSources,
    },
};
use crate::{
    compiler::{
        decimal_key_entries,
        feature_palette::FeaturePalette,
        lte::RawLteCombo,
        selection::{LteDomain, LteRelation, NrDomain, NrRelation, Sku},
    },
    mapping::{MappingEntry, MappingRoot, map_to_root, root_to_map},
    model::{Family, Tier, matching_anchors, profile},
    raw_nr::{RawNrPayload, RawNrPayloadKey, RawSubBlockKey},
};
use anyhow::{Context, bail, ensure};
use compact_str::CompactString;
use std::collections::{BTreeMap, BTreeSet};
pub(crate) fn parse_sources(text: &str) -> anyhow::Result<ValidatedSources> {
    let source = crate::compiler::read_sources(text).context("parsing the source document")?;
    validate_documents(source)
}

pub(crate) fn validate_documents(source: SourceDocument) -> anyhow::Result<ValidatedSources> {
    // The version check lives in the reader (`kdl_source::checked_version`), not here. It has to
    // run before the document body is mapped: a stale tree fails the *vocabulary* first, so a
    // check at this point only ever saw documents that had already mapped cleanly. `decompose`
    // reaches here with a freshly-ingested document, which carries no version at all — the writer
    // stamps `SOURCE_FORMAT_VERSION` on the way out — so nothing is left for a check here to catch.
    let SourceDocument { nr, lte } = source;
    let bitmask_fingerprints = validate_fingerprint_partition(&nr)?;
    let carriers = validate_carriers(&nr)?;
    validate_mapping_projection(&carriers)?;
    let nr_domain = build_nr_domain(&nr, &carriers)?;
    let input_palette = FeaturePalette::from_rows(nr.dl_features.clone(), nr.ul_features.clone());
    let nr_combo = validate_nr_combos(&nr.combos, &nr_domain, &input_palette)?;
    let selection_index = NrSelectionIndex::build(&nr_combo);
    let palette = input_palette.canonicalized();
    let (lte_files, lte_domain) = validate_lte_files(&lte)?;
    let lte_combo = validate_lte_combos(&lte.combos, &lte_domain)?;
    let mut validated = ValidatedSources {
        nr: ValidatedNr {
            source: nr,
            bitmask_fingerprints,
            carriers,
            domain: nr_domain,
            palette,
            combos: nr_combo,
            selection_index,
        },
        lte: ValidatedLte {
            source: lte,
            files: lte_files,
            domain: lte_domain,
            combos: lte_combo,
        },
    };
    canonicalize_sources(&mut validated)?;
    Ok(validated)
}

fn build_nr_domain(
    nr: &NrDocument,
    carriers: &BTreeMap<String, ValidatedCarrier>,
) -> anyhow::Result<NrDomain> {
    let mut members: BTreeSet<(CompactString, Sku)> = nr
        .bitmask_carriers
        .iter()
        .map(|carrier| (carrier.as_str().into(), Sku::Legacy))
        .collect();
    for (carrier, source) in carriers {
        let anchors = source
            .profiled
            .iter()
            .flat_map(|profiled| profiled.profiles.keys().copied());
        for anchor in anchors {
            members.extend(
                Sku::for_nr_anchor(anchor)
                    .into_iter()
                    .map(|sku| (carrier.as_str().into(), sku)),
            );
        }
    }
    NrDomain::new(members)
}

fn validate_nr_combos(
    sources: &[NrSourceCombo],
    domain: &NrDomain,
    palette: &FeaturePalette,
) -> anyhow::Result<Vec<ValidatedNrCombo>> {
    let mut seen = BTreeSet::new();
    let mut validated = Vec::with_capacity(sources.len());
    for (index, source) in sources.iter().enumerate() {
        ensure!(
            !source.sub_blocks.is_empty(),
            "NR combo {} must contain at least one component",
            index + 1
        );
        let mut sub_blocks = source
            .sub_blocks
            .iter()
            .enumerate()
            .map(|(component_index, component)| {
                component.resolve(palette).with_context(|| {
                    format!(
                        "validating NR combo {} component {}",
                        index + 1,
                        component_index + 1,
                    )
                })
            })
            .collect::<anyhow::Result<Vec<_>>>()?;
        sub_blocks.sort_by_cached_key(|component| RawSubBlockKey::from(component));
        let payload = RawNrPayload {
            header: source.header,
            sub_blocks,
        };
        ensure!(
            seen.insert(RawNrPayloadKey::from(&payload)),
            "duplicate canonical NR payload record at combo {}",
            index + 1
        );
        let relation = NrRelation::from_selection(domain, source.selection.as_deref())
            .with_context(|| format!("validating NR combo {} selection", index + 1))?;
        validated.push(ValidatedNrCombo { payload, relation });
    }
    validated.sort_by_cached_key(|combo| RawNrPayloadKey::from(&combo.payload));
    Ok(validated)
}

fn validate_lte_files(
    lte: &LteDocument,
) -> anyhow::Result<(BTreeMap<u64, LteFileSource>, LteDomain)> {
    ensure!(!lte.files.is_empty(), "LTE files map must be nonempty");
    let mut files = BTreeMap::new();
    let mut members = BTreeSet::new();
    for (&id, source) in decimal_key_entries(&lte.files) {
        files.insert(id, source.clone());
        members.extend(Sku::for_lte_file(id));
    }
    Ok((files, LteDomain::new(members)))
}

impl From<&LteSourceCombo> for RawLteCombo {
    /// A source combo minus its `selection` — the payload identity `validate_lte_combos`
    /// dedups on. Shares the type (and therefore the `Ord`) with `compiler::lte`'s ingest.
    fn from(source: &LteSourceCombo) -> Self {
        Self {
            components: source.components.clone(),
            bcs: source.bcs,
            unknown1: source.unknown1,
            unknown2: source.unknown2,
        }
    }
}

fn validate_lte_combos(
    sources: &[LteSourceCombo],
    domain: &LteDomain,
) -> anyhow::Result<Vec<ValidatedLteCombo>> {
    let mut seen = BTreeSet::new();
    let mut validated = Vec::with_capacity(sources.len());
    for (index, source) in sources.iter().enumerate() {
        ensure!(
            !source.components.is_empty(),
            "LTE combo {} must contain at least one component",
            index + 1
        );
        for component in &source.components {
            ensure!(
                component.band > 0,
                "LTE combo {} component band must be positive",
                index + 1
            );
            // Neither state is representable in the source format, and neither occurs in the
            // corpus.
            // Rejecting here — ahead of `to_kdl` — makes the omit-when-0 rule value-faithful by
            // construction rather than by assumption, and gives a message naming the component
            // instead of the codec's "value 0 has no known bandwidth-class letter".
            ensure!(
                component.dl_bw_class_mimo != 0,
                "LTE combo {} band {} has dl_bw_class_mimo 0; the source format cannot represent \
                 a disabled downlink",
                index + 1,
                component.band
            );
            ensure!(
                component.ul_bw_class_mimo.is_some(),
                "LTE combo {} band {} omits ul_bw_class_mimo; the source format cannot represent \
                 an absent uplink class (an omitted argument means the explicit zero)",
                index + 1,
                component.band
            );
        }
        ensure!(
            seen.insert(RawLteCombo::from(source)),
            "duplicate canonical LTE payload record at combo {}",
            index + 1
        );
        let relation = LteRelation::from_selection(domain, source.selection.as_deref())
            .with_context(|| format!("validating LTE combo {} selection", index + 1))?;
        validated.push(ValidatedLteCombo {
            source: source.clone(),
            relation,
        });
    }
    Ok(validated)
}

/// Range-checks an optional raw id down to `i32`, naming `field` and `carrier` in the error so
/// a too-large `bitmask_id`/`profiled_id` is traceable to its source. Both fields share this
/// exact check; only the field name in the message differs.
fn checked_i32_id(id: Option<i64>, field: &str, carrier: &str) -> anyhow::Result<Option<i32>> {
    id.map(|id| {
        i32::try_from(id).with_context(|| format!("{field} for carrier `{carrier}` must fit int32"))
    })
    .transpose()
}

/// Enforces the structural rules a carrier's profile/signature/tier/mapping/bitmask fields
/// must jointly satisfy: a profiled carrier needs signature+tier and vice versa, `profiled_id`
/// implies profiles, `mapping_id` and `plmns` imply each other, and every carrier must have at
/// least one role (bitmask membership, profiles, or a PLMN mapping).
fn validate_carrier_role(
    carrier: &str,
    source: &CarrierSource,
    bitmask_carriers: &BTreeSet<&str>,
) -> anyhow::Result<Option<(u64, Tier)>> {
    let has_profiles = !source.profiles.is_empty();
    let profiled = if has_profiles {
        let signature = source
            .signature
            .with_context(|| format!("profiled carrier `{carrier}` requires signature"))?;
        let tier = source
            .tier
            .with_context(|| format!("profiled carrier `{carrier}` requires tier"))?;
        Some((signature, tier))
    } else {
        ensure!(
            source.signature.is_none() && source.tier.is_none(),
            "carrier `{carrier}` has signature or tier without profiles"
        );
        None
    };
    ensure!(
        has_profiles || source.profiled_id.is_none(),
        "carrier `{carrier}` has profiled_id but no profiled NR files"
    );
    ensure!(
        source.mapping_id.is_some() == source.plmns.is_some(),
        "carrier `{carrier}` must provide mapping_id and plmns together"
    );
    ensure!(
        bitmask_carriers.contains(carrier) || has_profiles || source.plmns.is_some(),
        "carrier `{carrier}` has no bitmask, profile, or mapping-only role"
    );
    Ok(profiled)
}

/// Builds one carrier's `anchor -> ValidatedProfile` table: each profile key's filename
/// product (`signature * multiplier`) must round-trip through `matching_anchors` to the SAME
/// anchor it was declared under, or the profile is rejected as ambiguous/mismatched.
/// `signature`/`tier` come from `validate_carrier_role`, which is what proved they exist.
fn validated_profiles(
    source_profiles: &BTreeMap<u64, ProfileSource>,
    carrier: &str,
    signature: u64,
    tier: Tier,
) -> anyhow::Result<BTreeMap<u64, ValidatedProfile>> {
    let mut profiles = BTreeMap::new();
    for (&anchor, profile_source) in decimal_key_entries(source_profiles) {
        let Some(profile) = profile(anchor) else {
            bail!("unknown profile anchor {anchor} for carrier `{carrier}`");
        };
        let number = signature
            .checked_mul(profile_source.multiplier)
            .with_context(|| {
                format!("filename product overflow for carrier `{carrier}` profile {anchor}")
            })?;
        let matches = matching_anchors(number);
        match matches.as_slice() {
            [matched] if matched.anchor == anchor => {}
            [] | [_] => bail!(
                "filename product {number} for carrier `{carrier}` has wrong profile anchor; expected {anchor}"
            ),
            _ => bail!(
                "filename product {number} for carrier `{carrier}` profile {anchor} is ambiguous"
            ),
        }
        profiles.insert(
            anchor,
            ValidatedProfile {
                number,
                unknown: profile_source.unknown,
                fingerprint: modern_fingerprint(profile.family, tier),
            },
        );
    }
    Ok(profiles)
}

fn validate_carriers(nr: &NrDocument) -> anyhow::Result<BTreeMap<String, ValidatedCarrier>> {
    let bitmask_carriers: BTreeSet<_> = nr.bitmask_carriers.iter().map(String::as_str).collect();
    let mut mapping_ids = BTreeMap::<u64, &str>::new();
    let mut validated = BTreeMap::new();

    for (carrier, source) in &nr.carriers {
        validate_carrier_name(carrier)?;

        let bitmask_id = checked_i32_id(source.bitmask_id, "bitmask_id", carrier)?;
        if bitmask_id.is_some() {
            ensure!(
                bitmask_carriers.contains(carrier.as_str()),
                "carrier `{carrier}` has bitmask_id but is absent from bitmask_carriers"
            );
        }

        let profiled_role = validate_carrier_role(carrier, source, &bitmask_carriers)?;

        let profiled_id = checked_i32_id(source.profiled_id, "profiled_id", carrier)?;

        if let Some(id) = source.mapping_id
            && let Some(previous) = mapping_ids.insert(id, carrier)
        {
            bail!("mapping_id {id} is used by both carrier `{previous}` and `{carrier}`");
        }

        // `validate_carrier_role` proved mapping_id and plmns imply each other.
        let legend = match (source.mapping_id, source.plmns.clone()) {
            (Some(mapping_id), Some(plmns)) => Some(LegendEntry { mapping_id, plmns }),
            _ => None,
        };
        let profiled = profiled_role
            .map(|(signature, tier)| {
                validated_profiles(&source.profiles, carrier, signature, tier)
                    .map(|profiles| ProfiledRole { profiles })
            })
            .transpose()?;

        validated.insert(
            carrier.clone(),
            ValidatedCarrier {
                bitmask_id,
                profiled_id,
                legend,
                profiled,
            },
        );
    }
    Ok(validated)
}

/// The PLMN legend every validated carrier with a legend entry contributes, in `mapping_id`
/// order — the order the generated `ap_plmn_mapping.binarypb` carries.
///
/// The single source for this projection: `provision`'s `generate_mapping_file` ships these
/// bytes, `decompose`'s `rebuild_mapping` self-checks against them, and
/// [`validate_mapping_projection`] proves they re-encode. Building it three times is how the
/// three could drift.
pub(crate) fn legend_root(carriers: &BTreeMap<String, ValidatedCarrier>) -> MappingRoot {
    let mut mappings: Vec<_> = carriers
        .iter()
        .filter_map(|(name, carrier)| {
            let legend = carrier.legend.as_ref()?;
            Some(MappingEntry {
                id: legend.mapping_id,
                name: name.clone(),
                plmns: legend.plmns.clone(),
            })
        })
        .collect();
    mappings.sort_by_key(|entry| entry.id);
    MappingRoot { mappings }
}

fn validate_mapping_projection(
    carriers: &BTreeMap<String, ValidatedCarrier>,
) -> anyhow::Result<()> {
    let root = legend_root(carriers);
    let map = root_to_map(&root).context("validating source mapping projection")?;
    map_to_root(&map).context("round-tripping source mapping projection")?;
    Ok(())
}

/// The modern (profiled) fingerprint for a `(family, tier)` pair. Thin wrapper over the one
/// source `model::fingerprint_for` (from `model::FINGERPRINTS`), shared by the NR generation
/// and schema-validation paths so the table lives in exactly one place.
fn modern_fingerprint(family: Family, tier: Tier) -> u64 {
    crate::model::fingerprint_for(family, tier)
        .expect("every (family, tier) pair is present in model::FINGERPRINTS")
}

fn validate_fingerprint_partition(nr: &NrDocument) -> anyhow::Result<BTreeMap<String, u64>> {
    ensure!(
        !nr.bitmask_carriers.is_empty(),
        "bitmask_carriers must be nonempty"
    );
    let mut whitelist = BTreeSet::new();
    for carrier in &nr.bitmask_carriers {
        validate_carrier_name(carrier)?;
        ensure!(
            whitelist.insert(carrier.as_str()),
            "duplicate carrier `{carrier}` in bitmask_carriers"
        );
    }

    let mut fingerprints = BTreeSet::new();
    let mut assigned = BTreeSet::new();
    let mut by_carrier = BTreeMap::new();
    for group in &nr.bitmask_fingerprints {
        ensure!(
            fingerprints.insert(group.fingerprint),
            "duplicate fingerprint {} in bitmask_fingerprint",
            group.fingerprint
        );
        ensure!(
            !group.carriers.is_empty(),
            "bitmask fingerprint {} must have a nonempty carrier list",
            group.fingerprint
        );
        for carrier in &group.carriers {
            validate_carrier_name(carrier)?;
            ensure!(
                whitelist.contains(carrier.as_str()),
                "fingerprint-group carrier `{carrier}` is not a bitmask carrier"
            );
            ensure!(
                assigned.insert(carrier.as_str()),
                "carrier `{carrier}` belongs to more than one fingerprint group"
            );
            by_carrier.insert(carrier.clone(), group.fingerprint);
        }
    }
    ensure!(
        assigned == whitelist,
        "bitmask_fingerprint groups must partition bitmask_carriers exactly"
    );
    Ok(by_carrier)
}

/// A carrier name must be a nonempty, already-trimmed canonical identifier. Shared by the
/// compiler's NR ingestion (`nr.rs`) and schema validation.
pub(crate) fn validate_carrier_name(carrier: &str) -> anyhow::Result<()> {
    ensure!(!carrier.is_empty(), "carrier names must be nonempty");
    ensure!(
        carrier.trim() == carrier,
        "carrier name `{carrier}` is not canonical"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use crate::{
        compiler::{
            read_sources,
            schema::{LteSourceCombo, parse_sources, test_support::*, to_kdl},
            selection::Sku,
        },
        proto::{LteComponent, ShannonFeatureSetDlPerCcNr},
        raw_nr::Direction,
    };
    use std::collections::BTreeSet;
    #[test]
    fn parses_the_minimal_version_one_document() {
        parse_sources(&source(MINIMAL_NR, MINIMAL_LTE)).unwrap();
    }

    #[test]
    fn carrier_ids_are_independent_and_mapping_ids_alone_are_unique() {
        let nr = format!(
            r#"{MINIMAL_NR}
c "A" pi=0 mi=7 sg=1 t="main" {{
    ps
    pf "66813533" x=66813533 u=0
}}

c "B" pi=0 mi=8 sg=1 t="main" {{
    ps
    pf "66813533" x=66813533 u=0
}}
"#
        );
        let parsed = parse_sources(&source(&nr, MINIMAL_LTE)).unwrap();
        assert_eq!(parsed.nr.carriers["A"].profiled_id, Some(0));
        assert_eq!(parsed.nr.carriers["B"].profiled_id, Some(0));
        assert_eq!(
            parsed.nr.carriers["A"]
                .legend
                .as_ref()
                .map(|l| l.mapping_id),
            Some(7)
        );
        assert_eq!(
            parsed.nr.carriers["B"]
                .legend
                .as_ref()
                .map(|l| l.mapping_id),
            Some(8)
        );
    }

    #[test]
    fn mapping_id_requires_plmns_and_must_be_unique() {
        let missing = format!("{MINIMAL_NR}\nc \"MAP\" mi=7\n");
        let error = parse_sources(&source(&missing, MINIMAL_LTE))
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("must provide mapping_id and plmns together"),
            "{error}"
        );

        let missing_id = format!("{MINIMAL_NR}\nc \"MAP\" {{\n    ps\n}}\n");
        let error = parse_sources(&source(&missing_id, MINIMAL_LTE))
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("must provide mapping_id and plmns together"),
            "{error}"
        );

        let duplicate =
            format!("{MINIMAL_NR}\nc \"A\" mi=7 {{\n    ps\n}}\nc \"B\" mi=7 {{\n    ps\n}}\n");
        let error = parse_sources(&source(&duplicate, MINIMAL_LTE))
            .unwrap_err()
            .to_string();
        assert!(error.contains("mapping_id 7 is used by both"), "{error}");
    }

    #[test]
    fn canonical_feature_palette_preserves_deduplicates_sorts_and_renumbers() {
        let nr = format!(
            "{MINIMAL_NR}\n\
             df s=3\n\
             df s=1\n\
             df s=3\n\
             uf s=9\n\
             n {{\n    n77 A1\n}}\n\
             n {{\n    n78 A3\n}}\n"
        );
        let parsed = parse_sources(&source(&nr, MINIMAL_LTE)).unwrap();
        let canonical = parsed.to_kdl().unwrap();
        assert_eq!(parsed.nr.palette.dl.len(), 2);
        assert_eq!(parsed.nr.palette.ul.len(), 1);
        assert!(canonical.contains("df s=1"), "{canonical}");
        assert!(canonical.contains("df s=3"), "{canonical}");
        assert!(canonical.contains("uf s=9"), "{canonical}");
        assert_eq!(canonical.matches("n77 A2").count(), 1);
        assert_eq!(canonical.matches("n78 A2").count(), 1);
    }

    #[test]
    fn existing_kdl_preserves_manual_palette_without_expanding_or_injecting() {
        let nr = format!(
            "{MINIMAL_NR}\n\
             df s=1 b=40\n\
             df s=2 b=100\n\
             n {{\n    n77 A1\n}}\n"
        );
        let parsed = parse_sources(&source(&nr, MINIMAL_LTE)).unwrap();
        let canonical = parsed.to_kdl().unwrap();

        assert_eq!(parsed.nr.palette.dl.len(), 2);
        assert!(parsed.nr.palette.ul.is_empty());
        assert!(canonical.contains("df s=1 b=40"), "{canonical}");
        assert!(canonical.contains("df s=2 b=100"), "{canonical}");
        assert!(!canonical.contains("df s=1 b=100"), "{canonical}");
        assert!(!canonical.contains("df s=2 b=40"), "{canonical}");
        assert!(!canonical.contains("s=4"), "{canonical}");
    }

    #[test]
    fn repointing_to_a_prelisted_row_changes_only_the_intended_reference() {
        let nr = format!(
            "{MINIMAL_NR}\n\
             df s=1\n\
             df s=2\n\
             df s=3\n\
             n {{\n    n77 A1\n}}\n\
             n {{\n    n78 A3\n}}\n"
        );
        let baseline = parse_sources(&source(&nr, MINIMAL_LTE)).unwrap();
        let baseline_palette = baseline.nr.palette.dl.clone();
        let baseline = baseline.to_kdl().unwrap();
        let edited = baseline.replacen("n77 A1", "n77 A2", 1);
        assert_ne!(
            edited, baseline,
            "baseline must contain the intended reference"
        );

        let repointed = parse_sources(&edited).unwrap();
        assert_eq!(repointed.nr.palette.dl, baseline_palette);
        assert_eq!(repointed.nr.palette.dl.len(), 3);
        let repointed = repointed.to_kdl().unwrap();
        assert!(repointed.contains("n77 A2"), "{repointed}");
        assert!(repointed.contains("n78 A3"), "{repointed}");
        assert_eq!(repointed, edited);
        assert_eq!(
            repointed,
            baseline.replacen("n77 A1", "n77 A2", 1),
            "canonical bytes must differ only at the repointed combo reference"
        );
    }

    #[test]
    fn canonical_feature_palette_equal_input_positions_share_one_index() {
        let nr = format!(
            "{MINIMAL_NR}\n\
             df s=7\n\
             df s=7\n\
             n {{\n    n77 A1\n}}\n\
             n {{\n    n78 A2\n}}\n"
        );
        let parsed = parse_sources(&source(&nr, MINIMAL_LTE)).unwrap();
        let canonical = parsed.to_kdl().unwrap();

        assert_eq!(parsed.nr.palette.dl.len(), 1);
        assert_eq!(canonical.matches("df s=7").count(), 1);
        assert_eq!(canonical.matches("A1").count(), 2);
    }

    #[test]
    fn feature_palette_preserves_referenced_absent_and_explicit_zero_records() {
        let nr = format!(
            "{MINIMAL_NR}\n\
             df\n\
             df s=0\n\
             n {{\n    n77 A2\n}}\n\
             n {{\n    n78 A1\n}}\n"
        );
        let parsed = parse_sources(&source(&nr, MINIMAL_LTE)).unwrap();
        assert_eq!(parsed.nr.palette.dl.len(), 2);
        assert_eq!(
            parsed.nr.palette.dl[0],
            ShannonFeatureSetDlPerCcNr::default()
        );
        assert_eq!(parsed.nr.palette.dl[1].max_scs, Some(0));
    }

    #[test]
    fn feature_references_reject_zero_out_of_range_and_old_inline_fields() {
        // A 0 reference is now caught at PARSE time by the composite value's own rule, while
        // an out-of-range one is still caught at validation against the catalog length — the
        // reader knows references are 1-based, but not how long the catalog is.
        for (cc_line, expected) in [
            ("n78 A0", "1-based"),
            ("n78 A2", "exceeds the dl catalog length 1"),
            ("n78 A1 A0", "1-based"),
            ("n78 A1 A2", "exceeds the ul catalog length 1"),
        ] {
            let nr = format!("{MINIMAL_NR}\ndf s=3\nuf s=4\nn {{\n    {cc_line}\n}}\n");
            // `{:#}` for the whole chain: a parse-time rejection is wrapped in
            // "parsing the source document", so `to_string()` alone would show only that.
            let error = format!(
                "{:#}",
                parse_sources(&source(&nr, MINIMAL_LTE)).unwrap_err()
            );
            assert!(error.contains(expected), "{error}");
        }

        let old = format!("{MINIMAL_NR}\nn {{\n    n78 dl-max-scs=3\n}}\n");
        assert!(read_sources(&source(&old, MINIMAL_LTE)).is_err());
    }

    #[test]
    fn global_source_palette_can_exceed_one_byte() {
        let mut nr = MINIMAL_NR.to_string();
        for value in 1..=300 {
            nr.push_str(&format!("\ndf b={value}\n"));
            nr.push_str(&format!("\nn {{\n    n{value} A{value}\n}}\n"));
        }
        let parsed = parse_sources(&source(&nr, MINIMAL_LTE)).unwrap();
        assert_eq!(parsed.nr.palette.dl.len(), 300);
    }

    #[test]
    fn global_nr_feature_references_can_exceed_u16() {
        let index = usize::from(u16::MAX) + 1;
        let mut nr = MINIMAL_NR.to_string();
        for value in 1..=index {
            nr.push_str(&format!("\ndf b={value}\n"));
        }
        nr.push_str(&format!("\nn {{\n    n78 A{index}\n}}\n"));

        let parsed = parse_sources(&source(&nr, MINIMAL_LTE)).unwrap();
        assert_eq!(parsed.nr.palette.dl.len(), index);
        assert!(parsed.to_kdl().unwrap().contains("n78 A65536"));
    }

    #[test]
    fn the_version_is_required_and_only_the_current_one_is_supported() {
        let document = source(MINIMAL_NR, MINIMAL_LTE);

        let missing = document.replacen("version 1\n", "", 1);
        assert!(parse_sources(&missing).is_err());

        // `{:#}`, not bare `to_string()`: since the version check moved into the reader
        // (`kdl_source::checked_version`), this error comes from inside `read_sources`, which
        // `parse_sources` wraps in a "parsing the source document" context. Plain `Display` would
        // show only that outer layer; see
        // `a_stale_vocabulary_reports_the_version_not_the_unknown_property` below for the same
        // point made explicitly.
        let unsupported = document.replacen("version 1\n", "version 2\n", 1);
        assert!(
            format!("{:#}", parse_sources(&unsupported).unwrap_err())
                .contains("source-format version 2")
        );
    }

    /// The regression the reader-side check exists for. A real stale tree differs in **both** the
    /// version marker and the vocabulary; before the check moved into the reader, the vocabulary
    /// failed first and the remedy sentence never printed.
    ///
    /// `{:#}`, not bare `to_string()`: `parse_sources` wraps the reader's errors in its own
    /// "parsing the source document" context, so plain `Display` shows only that outer layer,
    /// never the version text underneath. `main.rs` prints top-level errors with `{:#}` too, so
    /// this is the user's-eye view of the fix, not a test-visibility workaround. `assert_nr_error`
    /// below takes the same precaution.
    #[test]
    fn a_stale_vocabulary_reports_the_version_not_the_unknown_property() {
        let stale = source(
            MINIMAL_NR,
            &format!("{MINIMAL_LTE}\nl {{\n    B1 dm=A4 um=off\n}}\n"),
        )
        .replacen("version 1\n", "version 2\n", 1);
        let error = format!("{:#}", parse_sources(&stale).unwrap_err());
        assert!(
            error.contains("source-format version 2") && error.contains("re-run `decompose`"),
            "{error}"
        );
        assert!(!error.contains("missing required property"), "{error}");
    }

    #[test]
    fn numeric_map_keys_use_shortest_decimal_u64_syntax() {
        parse_sources(&source(&profiled_nr("66813533"), MINIMAL_LTE)).unwrap();
        parse_sources(&source(MINIMAL_NR, &lte_with_file_key("400907661"))).unwrap();

        for key in ["066813533", "+66813533", " 66813533", "66813533 ", "anchor"] {
            let error = format!(
                "{:#}",
                parse_sources(&source(&profiled_nr(key), MINIMAL_LTE)).unwrap_err()
            );
            assert!(error.contains("profile key"), "{key:?}: {error}");
        }

        for key in [
            "0400907661",
            "+400907661",
            " 400907661",
            "400907661 ",
            "file",
        ] {
            let error = format!(
                "{:#}",
                parse_sources(&source(MINIMAL_NR, &lte_with_file_key(key))).unwrap_err()
            );
            assert!(error.contains("LTE file key"), "{key:?}: {error}");
        }
    }

    #[test]
    fn profile_validation_errors_follow_decimal_text_order() {
        let nr = format!(
            "{MINIMAL_NR}\nc C sg=1 t=main {{\n pf \"2\" x=2 u=0\n pf \"10\" x=10 u=0\n}}\n"
        );
        let error = format!(
            "{:#}",
            parse_sources(&source(&nr, MINIMAL_LTE)).unwrap_err()
        );
        assert!(error.contains("unknown profile anchor 10"), "{error}");
    }

    fn assert_nr_error(nr: &str, needle: &str) {
        // `{:#}` for the whole chain: a parse-time rejection is wrapped in "parsing the source
        // document", so `to_string()` alone would show only that.
        let error = format!("{:#}", parse_sources(&source(nr, MINIMAL_LTE)).unwrap_err());
        assert!(error.contains(needle), "expected {needle:?} in {error:?}");
    }

    #[test]
    fn fingerprint_groups_are_nonempty_disjoint_and_exhaustive() {
        assert_nr_error(
            r#"
bc "A"
bf 1 {
    c
}
"#,
            "nonempty",
        );

        assert_nr_error(
            r#"
bc "A" "B"
bf 1 {
    c "A"
}
bf 2 {
    c "A" "B"
}
"#,
            "more than one fingerprint",
        );

        assert_nr_error(
            r#"
bc "A" "B"
bf 1 {
    c "A"
}
"#,
            "partition",
        );

        assert_nr_error(
            r#"
bc "A"
bf 1 {
    c "A" "B"
}
"#,
            "fingerprint-group carrier `B` is not a bitmask carrier",
        );

        assert_nr_error(
            r#"
bc "A" "A"
bf 1 {
    c "A"
}
"#,
            "duplicate carrier",
        );

        assert_nr_error(
            r#"
bc "A"
bf 1 {
    c "A"
}
bf 1 {
    c "A"
}
"#,
            "duplicate fingerprint",
        );
    }

    #[test]
    fn carrier_shapes_and_id_ranges_are_validated() {
        assert_nr_error(
            &nr_with_carrier_sections(
                r#"
c "PROFILED" t="main" {
    pf "66813533" x=66813533 u=0
}
"#,
            ),
            "signature",
        );
        assert_nr_error(
            &nr_with_carrier_sections(
                r#"
c "PROFILED" sg=1 {
    pf "66813533" x=66813533 u=0
}
"#,
            ),
            "tier",
        );
        assert_nr_error(
            &nr_with_carrier_sections(
                r#"
c "ORPHAN" sg=1 t="main"
"#,
            ),
            "without profiles",
        );
        assert_nr_error(
            &nr_with_carrier_sections(
                r#"
c "MAPPING" {
    ps
}
"#,
            ),
            "must provide mapping_id and plmns together",
        );
        assert_nr_error(
            &nr_with_carrier_sections(
                r#"
c "MAPPING" mi=7
"#,
            ),
            "must provide mapping_id and plmns together",
        );
        assert_nr_error(
            &nr_with_carrier_sections(
                r#"
c "MAPPING" pi=7
"#,
            ),
            "has profiled_id but no profiled NR files",
        );
        assert_nr_error(
            &nr_with_carrier_sections(
                r#"
c "NOT_LEGACY" bi=1
"#,
            ),
            "bitmask_carriers",
        );
        assert_nr_error(
            &nr_with_carrier_sections(
                r#"
c "LEGACY" bi=2147483648
"#,
            ),
            "int32",
        );
        assert_nr_error(
            &nr_with_carrier_sections(
                r#"
c "PROFILED" pi=2147483648 sg=1 t="main" {
    pf "66813533" x=66813533 u=0
}
"#,
            ),
            "int32",
        );

        parse_sources(&source(
            &nr_with_carrier_sections(
                r#"
c "MAPPING" mi=18446744073709551615 {
    ps
}
"#,
            ),
            MINIMAL_LTE,
        ))
        .unwrap();
    }

    #[test]
    fn profile_products_resolve_exactly_the_keyed_registered_anchor() {
        parse_sources(&source(&profiled_nr("66813533"), MINIMAL_LTE)).unwrap();

        assert_nr_error(&profiled_nr("123"), "unknown profile anchor");
        assert_nr_error(
            &profiled_nr("66813533").replace("x=66813533", "x=1176929627"),
            "wrong profile anchor",
        );
        assert_nr_error(
            &nr_with_carrier_sections(
                r#"
c "PROFILED" sg=1 t="main" {
    pf "167" x=308449 u=0
}
"#,
            ),
            "ambiguous",
        );
        assert_nr_error(
            &nr_with_carrier_sections(
                r#"
c "PROFILED" sg=18446744073709551615 t="main" {
    pf "167" x=2 u=0
}
"#,
            ),
            "overflow",
        );
    }

    #[test]
    fn plmns_are_parsed_but_order_and_duplicates_are_legal() {
        parse_sources(&source(
            &nr_with_carrier_sections(
                r#"
c "MAPPING" mi=7 {
    p mcc=302 mnc=220
    p mcc=250 mnc=1
    p mcc=302 mnc=220
}
"#,
            ),
            MINIMAL_LTE,
        ))
        .unwrap();

        // A syntactically well-formed `plmn` node whose mcc/mnc reconstruct into a string
        // `Plmn::from_str` rejects (mnc has more than 3 digits) now fails inside
        // `read_sources` itself (`read_plmn` re-validates), not in the later semantic
        // carrier validation, so unwrap the full chain rather than using
        // `assert_nr_error`'s plain (unwrapped) `Display`.
        let error = format!(
            "{:#}",
            parse_sources(&source(
                &nr_with_carrier_sections(
                    r#"
c "MAPPING" mi=7 {
    p mcc=302 mnc=99999
}
"#,
                ),
                MINIMAL_LTE,
            ))
            .unwrap_err()
        );
        assert!(error.contains("PLMN"), "{error}");
    }

    #[test]
    fn modern_nr_bitmasks_cannot_be_stored_in_source() {
        let nr = format!("{MINIMAL_NR}\nn bm=1 {{\n    n78\n}}\n");
        assert!(parse_sources(&source(&nr, MINIMAL_LTE)).is_err());
    }

    #[test]
    fn domains_expand_only_registered_models_or_one_synthetic_token() {
        let sources = parse_sources(&source(
            &nr_with_complete_domain(),
            &lte_with_complete_domain(),
        ))
        .unwrap();
        let nr = sources.nr.domain.denorm_members();
        assert_eq!(
            nr,
            BTreeSet::from([
                ("LEGACY".into(), Sku::Legacy),
                ("PROFILED".into(), Sku::Model("G2YBB".into())),
                ("PROFILED".into(), Sku::Prime(8969)),
            ])
        );
        assert!(nr.iter().all(|(carrier, _)| carrier != "MAPPING"));

        let lte: BTreeSet<_> = sources.lte.domain.iter().cloned().collect();
        assert_eq!(
            lte,
            BTreeSet::from([
                Sku::Model("G2YBB".into()),
                Sku::Model("GGX8B".into()),
                Sku::Model("GR83Y".into()),
                Sku::Lte(564_260_317),
            ])
        );
    }

    #[test]
    fn selections_are_resolved_and_cached_during_validation() {
        let nr = format!(
            "{}\nn {{\n    s {{\n        c \"PROFILED\"\n        m \"G2YBB\"\n    }}\n    n78 A\n}}\n",
            nr_with_complete_domain()
        );
        let lte = format!(
            "{}\nl {{\n    s {{\n        m \"G2YBB\" \"lte:564260317\"\n    }}\n    B1 A4\n}}\n",
            lte_with_complete_domain()
        );
        let sources = parse_sources(&source(&nr, &lte)).unwrap();
        assert_eq!(
            sources
                .nr
                .domain
                .denorm_relation(&sources.nr.combos[0].relation),
            BTreeSet::from([("PROFILED".into(), Sku::Model("G2YBB".into()))])
        );
        assert_eq!(
            sources.lte.combos[0]
                .relation
                .iter()
                .cloned()
                .collect::<BTreeSet<_>>(),
            BTreeSet::from([Sku::Model("G2YBB".into()), Sku::Lte(564_260_317)])
        );
    }

    #[test]
    fn nr_payloads_require_valid_components_and_canonicalize_them() {
        // Each case is rejected for its own distinct reason, not just "some error occurred":
        // an empty combo has no components at all. `n0 A`'s bare class letter (no per-CC
        // list) is the all-zero placeholder, a complete DL argument on its own, so it clears
        // the mandatory-DL-argument check and reaches per-component validation, where band 0
        // fails (it has no 3GPP meaning). `B1 A st=5` similarly supplies a complete DL
        // argument but still leaves `st` (`srs-tx-switch`) unconsumed, since that property is
        // NR-only and this sub-block is E-UTRA.
        for (combo_body, expected) in [
            ("", "must contain at least one component"),
            ("n0 A\n", "band must be positive"),
            ("B1 A st=5\n", "unknown property `st`"),
        ] {
            let nr = format!("{MINIMAL_NR}\nn {{\n{combo_body}}}\n");
            // `{:#}` for the whole chain: the third case is rejected while still inside
            // `read_sources` (wrapped in "parsing the source document"), while the first two
            // are rejected by
            // `validate_documents`, downstream and unwrapped. The alternate format finds the
            // substring either way, matching `assert_nr_error` elsewhere in this module.
            let error = format!(
                "{:#}",
                parse_sources(&source(&nr, MINIMAL_LTE)).unwrap_err()
            );
            assert!(
                error.contains(expected),
                "body {combo_body:?} expected {expected:?} in {error:?}"
            );
        }

        let nr = format!("{MINIMAL_NR}\ndf s=3\nn {{\n    n78 A1\n    B1 A\n}}\n");
        let sources = parse_sources(&source(&nr, MINIMAL_LTE)).unwrap();
        let cc = &sources.nr.combos[0].payload.sub_blocks;
        assert_eq!(
            cc.iter()
                .map(|cc| cc.band_label().to_string())
                .collect::<Vec<_>>(),
            ["B1", "n78"]
        );
        assert_eq!(cc[1].selector(Direction::Dl), None);
    }

    #[test]
    fn duplicate_canonical_nr_payload_records_are_rejected() {
        let nr = format!(
            "{MINIMAL_NR}\ndf s=3\ndf s=3\nn {{\n    n78 A1\n    B1 A\n}}\nn {{\n    B1 A\n    n78 A2\n}}\n"
        );
        assert_nr_error(&nr, "duplicate canonical NR payload");
    }

    #[test]
    fn lte_payloads_require_components_preserve_order_and_reject_exact_duplicates() {
        let empty = format!("{MINIMAL_LTE}\nl {{\n}}\n");
        assert!(parse_sources(&source(MINIMAL_NR, &empty)).is_err());

        let duplicate = format!("{MINIMAL_LTE}\nl {{\n    B1 A4\n}}\nl {{\n    B1 A4\n}}\n");
        assert!(
            parse_sources(&source(MINIMAL_NR, &duplicate))
                .unwrap_err()
                .to_string()
                .contains("duplicate canonical LTE payload")
        );

        let ordered = format!(
            "{MINIMAL_LTE}\nl {{\n    B1 A4\n    B3 A4\n}}\nl {{\n    B3 A4\n    B1 A4\n}}\n"
        );
        let sources = parse_sources(&source(MINIMAL_NR, &ordered)).unwrap();
        assert_eq!(sources.lte.combos.len(), 2);
        assert_eq!(sources.lte.combos[0].source.components[0].band, 1);
        assert_eq!(sources.lte.combos[1].source.components[0].band, 3);

        // `bcs` is the surviving optional-presence pair: absent `b` is `None`, `b=""` is
        // `Some(0)`. It is also what keeps these two combos distinct under `RawLteCombo`, now
        // that their components are identical.
        let optional_presence =
            format!("{MINIMAL_LTE}\nl {{\n    B1 A4\n}}\nl b=\"\" {{\n    B1 A4\n}}\n");
        let sources = parse_sources(&source(MINIMAL_NR, &optional_presence)).unwrap();
        assert_eq!(sources.lte.combos.len(), 2);
        assert_eq!(sources.lte.combos[0].source.bcs, None);
        assert_eq!(sources.lte.combos[1].source.bcs, Some(0));
        // An omitted UL argument is the explicit zero, not an absent field — in both combos.
        for combo in &sources.lte.combos {
            assert_eq!(combo.source.components[0].ul_bw_class_mimo, Some(0));
        }
    }

    /// The source format spells UL-disabled by omitting the argument, so `None` has no spelling
    /// left; and with `off` gone, neither does a disabled DL. Both are corpus-absent (0 of 12 159
    /// sub-blocks), so this rejects rather than silently normalising — a foreign file gets a loud
    /// error instead of a quiet re-encode.
    #[test]
    fn lte_components_reject_a_disabled_dl_and_an_absent_ul_class() {
        let base = read_sources(&source(MINIMAL_NR, MINIMAL_LTE)).unwrap();

        let bad_combo = |component: LteComponent| LteSourceCombo {
            selection: None,
            bcs: None,
            unknown1: None,
            unknown2: None,
            components: vec![component],
        };

        let mut disabled_dl = base.clone();
        disabled_dl.lte.combos.push(bad_combo(LteComponent {
            band: 7,
            dl_bw_class_mimo: 0,
            ul_bw_class_mimo: Some(0),
        }));
        let error = to_kdl(&disabled_dl).unwrap_err().to_string();
        assert!(
            error.contains("band 7") && error.contains("dl_bw_class_mimo 0"),
            "{error}"
        );

        let mut absent_ul = base;
        absent_ul.lte.combos.push(bad_combo(LteComponent {
            band: 7,
            dl_bw_class_mimo: 32_769,
            ul_bw_class_mimo: None,
        }));
        let error = to_kdl(&absent_ul).unwrap_err().to_string();
        assert!(
            error.contains("band 7") && error.contains("omits ul_bw_class_mimo"),
            "{error}"
        );
    }

    #[test]
    fn validated_metadata_caches_derived_fingerprints_and_parsed_plmns() {
        let nr = nr_with_carrier_sections(
            r#"
c "PROFILED" pi=7 mi=7 sg=1 t="alt" {
    p mcc=302 mnc=220
    p mcc=250 mnc=1
    p mcc=302 mnc=220
    pf "66813533" x=66813533 u=9
}
"#,
        );
        let sources = parse_sources(&source(&nr, MINIMAL_LTE)).unwrap();
        assert_eq!(sources.nr.bitmask_fingerprints["LEGACY"], 715_188_856);
        let carrier = &sources.nr.carriers["PROFILED"];
        let legend_plmns: Vec<u64> = carrier
            .legend
            .as_ref()
            .expect("carrier has a legend entry")
            .plmns
            .iter()
            .map(|plmn| plmn.to_encoded())
            .collect();
        assert_eq!(legend_plmns, [197_154, 5_435_408, 197_154]);
        let profiled = carrier.profiled.as_ref().expect("carrier is profiled");
        let profile = &profiled.profiles[&66_813_533];
        assert_eq!(profile.number, 66_813_533);
        assert_eq!(profile.fingerprint, 627_223_094);
        assert_eq!(profile.unknown, 9);
    }
}
