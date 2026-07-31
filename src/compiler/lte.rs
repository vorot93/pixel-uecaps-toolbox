use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Context, bail, ensure};
use prost::Message;

use super::{
    GeneratedFile,
    schema::{LteDocument, LteFileSource, LteSourceCombo, ValidatedLte, ValidatedLteCombo},
    selection::{LteDomain, LteRelation, SelectionRect, Sku},
};
use crate::{
    model::lte_model_codes,
    proto::{LteCaps, LteCombo, LteComponent},
    wire::decode_lte_caps,
};

/// An LTE combo's identity: everything but its `selection`. Field order is load-bearing — the
/// derived `Ord` drives `topological_order`'s `BTreeSet::pop_first`, which fixes the source
/// document's LTE combo order and hence the generated LTE bytes. Do not reorder.
///
/// This is also the dedup key `validate_lte_combos` builds from an `LteSourceCombo`, so the
/// ingest side and the validate side compare payloads by exactly the same definition.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct RawLteCombo {
    pub(crate) components: Vec<LteComponent>,
    pub(crate) bcs: Option<u64>,
    pub(crate) unknown1: Option<u64>,
    pub(crate) unknown2: Option<u64>,
}

impl From<&LteCombo> for RawLteCombo {
    fn from(combo: &LteCombo) -> Self {
        Self {
            components: combo.components.clone(),
            bcs: combo.bcs,
            unknown1: combo.unknown1,
            unknown2: combo.unknown2,
        }
    }
}

impl From<&LteSourceCombo> for LteCombo {
    fn from(source: &LteSourceCombo) -> Self {
        Self {
            components: source.components.clone(),
            bcs: source.bcs,
            unknown1: source.unknown1,
            unknown2: source.unknown2,
        }
    }
}

impl RawLteCombo {
    /// This payload as an `LteSourceCombo` with the given canonical selection attached.
    fn to_source(&self, selection: Option<Vec<SelectionRect>>) -> LteSourceCombo {
        LteSourceCombo {
            selection,
            bcs: self.bcs,
            unknown1: self.unknown1,
            unknown2: self.unknown2,
            components: self.components.clone(),
        }
    }
}

pub(crate) struct DecodedLteFile {
    pub(crate) id: u64,
    pub(crate) original: Vec<u8>,
    pub(crate) caps: LteCaps,
}

/// Groups `files` by id, erroring on a duplicate — the input is a flat `Vec` (arrival order
/// from the profiled folder scan) but everything downstream needs random access by id.
fn group_files_by_id(files: Vec<DecodedLteFile>) -> anyhow::Result<BTreeMap<u64, DecodedLteFile>> {
    let mut files_by_id = BTreeMap::new();
    for file in files {
        let id = file.id;
        if files_by_id.insert(id, file).is_some() {
            bail!("duplicate LTE file ID {id}");
        }
    }
    Ok(files_by_id)
}

/// The cross-file accumulations one scan over every LTE file builds: each file's persisted
/// source metadata (string-keyed, matching the source document's file-key spelling), the
/// applicability domain, which SKUs use each distinct payload, and the same-file adjacency
/// edges that fix
/// payloads' relative order across files (topologically sorted next, by the caller).
#[derive(Default)]
struct LteScan {
    file_sources: BTreeMap<String, LteFileSource>,
    domain_members: BTreeSet<Sku>,
    payload_skus: BTreeMap<RawLteCombo, BTreeSet<Sku>>,
    edges: BTreeMap<RawLteCombo, BTreeSet<RawLteCombo>>,
}

/// Scans every file in id order, converting each combo to its canonical `RawLteCombo`,
/// erroring on a within-file duplicate payload, and recording every same-file consecutive pair
/// as a precedence edge (`sequence.windows(2)`) — the constraint `topological_order` resolves
/// into one global combo order.
fn scan_lte_files(files_by_id: &BTreeMap<u64, DecodedLteFile>) -> anyhow::Result<LteScan> {
    let mut scan = LteScan::default();
    for (id, file) in files_by_id {
        let basename = format!("lte_{id}.binarypb");
        let decoded = decode_lte_caps(&file.original, &basename)?;
        ensure!(
            decoded == file.caps,
            "decoded values for {basename} do not match its supplied LTE message"
        );

        scan.file_sources.insert(
            id.to_string(),
            LteFileSource {
                fingerprint: file.caps.fingerprint,
                bitmask: file.caps.bitmask,
            },
        );

        let skus = skus_for_file(*id);
        scan.domain_members.extend(skus.iter().cloned());
        let mut seen = BTreeSet::new();
        let mut sequence = Vec::with_capacity(file.caps.combos.len());
        for (index, combo) in file.caps.combos.iter().enumerate() {
            ensure!(
                !combo.components.is_empty(),
                "LTE payload {} in {basename} has no components",
                index + 1
            );
            for component in &combo.components {
                ensure!(
                    component.band > 0,
                    "LTE payload {} in {basename} has nonpositive band {}",
                    index + 1,
                    component.band
                );
            }

            let payload = RawLteCombo::from(combo);
            ensure!(
                seen.insert(payload.clone()),
                "duplicate LTE payload in {basename} at combo {}",
                index + 1
            );
            scan.payload_skus
                .entry(payload.clone())
                .or_default()
                .extend(skus.iter().cloned());
            scan.edges.entry(payload.clone()).or_default();
            sequence.push(payload);
        }
        for adjacent in sequence.windows(2) {
            scan.edges
                .entry(adjacent[0].clone())
                .or_default()
                .insert(adjacent[1].clone());
        }
    }
    Ok(scan)
}

/// Realizes the topologically-ordered raw payloads as parallel `LteSourceCombo`/
/// `ValidatedLteCombo` lists (same index in both) — the order the ordering pass fixed becomes
/// each combo's position.
fn canonical_lte_combos(
    order: Vec<RawLteCombo>,
    mut payload_skus: BTreeMap<RawLteCombo, BTreeSet<Sku>>,
    domain: &LteDomain,
) -> anyhow::Result<(Vec<LteSourceCombo>, Vec<ValidatedLteCombo>)> {
    let mut validated_combos = Vec::with_capacity(order.len());
    let mut source_combos = Vec::with_capacity(order.len());
    for payload in order {
        let members = payload_skus
            .remove(&payload)
            .expect("every ordered LTE payload has applicability");
        let relation = LteRelation::new(members);
        let source = payload.to_source(relation.canonical_selection(domain)?);
        source_combos.push(source.clone());
        validated_combos.push(ValidatedLteCombo { source, relation });
    }
    Ok((source_combos, validated_combos))
}

/// Regenerates every file for every one of its applicable SKUs and requires byte-identity
/// against the original — the ingest-side half of the round-trip guarantee (`decompose`'s
/// `verify_internal_targets` covers the other, generation-triggered half).
fn verify_lte_ingest(
    files_by_id: &BTreeMap<u64, DecodedLteFile>,
    validated: &ValidatedLte,
) -> anyhow::Result<()> {
    for (id, file) in files_by_id {
        for sku in skus_for_file(*id) {
            let generated = generate_lte_file(validated, *id, &sku)
                .with_context(|| format!("self-verifying lte_{id}.binarypb for `{}`", sku))?;
            ensure!(
                generated.bytes == file.original,
                "LTE decode self-verification for lte_{id}.binarypb and `{}` was not byte-identical",
                sku
            );
        }
    }
    Ok(())
}

pub(crate) fn ingest_lte(files: Vec<DecodedLteFile>) -> anyhow::Result<LteDocument> {
    ensure!(!files.is_empty(), "at least one LTE file is required");

    let files_by_id = group_files_by_id(files)?;
    let scan = scan_lte_files(&files_by_id)?;

    let order = topological_order(&scan.edges)?;
    let domain = LteDomain::new(scan.domain_members);
    let (source_combos, validated_combos) =
        canonical_lte_combos(order, scan.payload_skus, &domain)?;

    let source = LteDocument {
        files: scan.file_sources,
        combo: source_combos,
    };
    let parsed_files = files_by_id
        .iter()
        .map(|(id, file)| {
            (
                *id,
                LteFileSource {
                    fingerprint: file.caps.fingerprint,
                    bitmask: file.caps.bitmask,
                },
            )
        })
        .collect();
    let validated = ValidatedLte {
        source: source.clone(),
        files: parsed_files,
        domain,
        combo: validated_combos,
    };

    verify_lte_ingest(&files_by_id, &validated)?;

    Ok(source)
}

pub(crate) fn generate_lte_file(
    lte: &ValidatedLte,
    id: u64,
    sku: &Sku,
) -> anyhow::Result<GeneratedFile> {
    validate_target(id, sku)?;
    ensure!(
        lte.domain.iter().any(|eligible| eligible == sku),
        "SKU token `{}` is absent from the LTE source domain",
        sku
    );
    let metadata = lte
        .files
        .get(&id)
        .with_context(|| format!("LTE file ID {id} is absent from the source whitelist"))?;
    let combos = lte
        .combo
        .iter()
        .filter(|combo| combo.relation.iter().any(|selected| selected == sku))
        .map(|combo| LteCombo::from(&combo.source))
        .collect();
    let caps = LteCaps {
        fingerprint: metadata.fingerprint,
        combos,
        bitmask: metadata.bitmask,
    };
    let basename = format!("lte_{id}.binarypb");
    let bytes = caps.encode_to_vec();
    let decoded = decode_lte_caps(&bytes, &basename)?;
    ensure!(
        decoded == caps,
        "generated {basename} changed LTE values during encoding"
    );
    ensure!(
        decoded.encode_to_vec() == bytes,
        "generated {basename} was not byte-stable after decoding"
    );
    Ok(GeneratedFile { basename, bytes })
}

fn skus_for_file(id: u64) -> BTreeSet<Sku> {
    let models = lte_model_codes(id);
    if models.is_empty() {
        BTreeSet::from([Sku::Lte(id)])
    } else {
        models
            .into_iter()
            .map(|code| Sku::Model(code.into()))
            .collect()
    }
}

fn validate_target(id: u64, sku: &Sku) -> anyhow::Result<()> {
    let eligible = skus_for_file(id);
    ensure!(
        eligible.contains(sku),
        "SKU token `{}` does not select LTE file ID {id}",
        sku
    );
    Ok(())
}

fn topological_order(
    edges: &BTreeMap<RawLteCombo, BTreeSet<RawLteCombo>>,
) -> anyhow::Result<Vec<RawLteCombo>> {
    let mut indegree = edges
        .keys()
        .cloned()
        .map(|payload| (payload, 0_usize))
        .collect::<BTreeMap<_, _>>();
    for successors in edges.values() {
        for successor in successors {
            *indegree
                .get_mut(successor)
                .expect("every LTE successor is also an ordering node") += 1;
        }
    }

    let mut ready = indegree
        .iter()
        .filter(|(_, count)| **count == 0)
        .map(|(payload, _)| payload.clone())
        .collect::<BTreeSet<_>>();
    let mut ordered = Vec::with_capacity(indegree.len());
    while let Some(payload) = ready.pop_first() {
        ordered.push(payload.clone());
        for successor in &edges[&payload] {
            let count = indegree
                .get_mut(successor)
                .expect("every LTE successor retains its indegree");
            *count -= 1;
            if *count == 0 {
                ready.insert(successor.clone());
            }
        }
    }

    ensure!(
        ordered.len() == indegree.len(),
        "LTE ordering constraints contain a cycle ({} payloads remain)",
        indegree.len() - ordered.len()
    );
    Ok(ordered)
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};

    use prost::Message;

    use super::{DecodedLteFile, RawLteCombo, generate_lte_file, ingest_lte};
    use crate::{
        compiler::{
            schema::{
                BitmaskFingerprint, LteDocument, NrDocument, SOURCE_FORMAT_VERSION, SourceDocument,
                ValidatedLte, parse_sources, to_kdl,
            },
            selection::Sku,
        },
        model::lte_model_codes,
        proto::{LteCaps, LteCombo, LteComponent},
    };

    fn component(band: i32) -> LteComponent {
        LteComponent {
            band,
            // A real corpus bitfield (class A, 4x4). `32_768 + band` was only valid for band 0
            // or 1; components are already distinguished by `band`.
            dl_bw_class_mimo: 32_769,
            ul_bw_class_mimo: Some(0),
        }
    }

    fn combo(bands: &[i32]) -> LteCombo {
        LteCombo {
            components: bands.iter().copied().map(component).collect(),
            bcs: Some(0),
            unknown1: Some(0),
            unknown2: Some(0),
        }
    }

    fn caps(fingerprint: u64, bitmask: u64, combos: Vec<LteCombo>) -> LteCaps {
        LteCaps {
            fingerprint,
            combos,
            bitmask,
        }
    }

    fn decoded(id: u64, caps: LteCaps) -> DecodedLteFile {
        DecodedLteFile {
            id,
            original: caps.encode_to_vec(),
            caps,
        }
    }

    fn minimal_nr() -> NrDocument {
        NrDocument {
            bitmask_carriers: vec!["LEGACY".into()],
            bitmask_fingerprints: vec![BitmaskFingerprint {
                fingerprint: 1,
                carriers: vec!["LEGACY".into()],
            }],
            carriers: BTreeMap::new(),
            dl_features: vec![],
            ul_features: vec![],
            combo: Vec::new(),
        }
    }

    fn validated(document: &LteDocument) -> ValidatedLte {
        let text = to_kdl(&SourceDocument {
            version: SOURCE_FORMAT_VERSION,
            nr: minimal_nr(),
            lte: document.clone(),
        })
        .unwrap();
        parse_sources(&text).unwrap().lte
    }

    fn bands(document: &LteDocument) -> Vec<i32> {
        document
            .combo
            .iter()
            .map(|combo| combo.components[0].band)
            .collect()
    }

    #[test]
    fn raw_identity_preserves_component_order_and_optional_presence() {
        let forward = RawLteCombo::from(&combo(&[1, 3]));
        let reverse = RawLteCombo::from(&combo(&[3, 1]));
        assert_ne!(forward, reverse);

        // `None` vs `Some(0)` stays a raw-identity distinction even though the source format
        // cannot spell `None`: it omits `u` for `Some(0)`, and `validate_lte_combos` rejects a
        // `None` outright. Different layers, both claims true.
        let absent = RawLteCombo::from(&LteCombo {
            components: vec![LteComponent {
                band: 1,
                dl_bw_class_mimo: 32_768,
                ul_bw_class_mimo: None,
            }],
            bcs: None,
            unknown1: None,
            unknown2: None,
        });
        let present_zero = RawLteCombo::from(&LteCombo {
            components: vec![LteComponent {
                band: 1,
                dl_bw_class_mimo: 32_768,
                ul_bw_class_mimo: Some(0),
            }],
            bcs: Some(0),
            unknown1: Some(0),
            unknown2: Some(0),
        });
        assert_ne!(absent, present_zero);
        assert_eq!(
            BTreeSet::from([forward, reverse, absent, present_zero]).len(),
            4
        );
    }

    #[test]
    fn exact_payloads_deduplicate_across_files() {
        let shared = combo(&[1, 3]);
        let document = ingest_lte(vec![
            decoded(91, caps(11, 21, vec![shared.clone()])),
            decoded(92, caps(12, 22, vec![shared])),
        ])
        .unwrap();

        assert_eq!(document.combo.len(), 1);
        assert_eq!(document.combo[0].selection, None);
    }

    #[test]
    fn duplicate_exact_payload_inside_one_file_is_rejected() {
        let repeated = combo(&[1]);
        let error = ingest_lte(vec![decoded(
            91,
            caps(1, 2, vec![repeated.clone(), repeated]),
        )])
        .unwrap_err()
        .to_string();

        assert!(error.contains("duplicate LTE payload"), "{error}");
        assert!(error.contains("lte_91.binarypb"), "{error}");
    }

    #[test]
    fn shared_constraints_form_one_global_order() {
        let a = combo(&[1]);
        let b = combo(&[2]);
        let c = combo(&[3]);
        let document = ingest_lte(vec![
            decoded(91, caps(1, 11, vec![a.clone(), b, c.clone()])),
            decoded(92, caps(2, 12, vec![a, c])),
        ])
        .unwrap();

        assert_eq!(bands(&document), vec![1, 2, 3]);
    }

    #[test]
    fn topological_ties_use_full_raw_payload_order() {
        let lower = combo(&[1]);
        let higher = combo(&[2]);
        let sink = combo(&[3]);
        let document = ingest_lte(vec![
            decoded(92, caps(2, 12, vec![higher, sink.clone()])),
            decoded(91, caps(1, 11, vec![lower, sink])),
        ])
        .unwrap();

        assert_eq!(bands(&document), vec![1, 2, 3]);
    }

    #[test]
    fn mustang_style_identical_sequences_keep_distinct_metadata() {
        let sequence = vec![combo(&[1]), combo(&[2]), combo(&[3])];
        let first = decoded(91, caps(101, 201, sequence.clone()));
        let second = decoded(92, caps(102, 202, sequence));
        let first_original = first.original.clone();
        let second_original = second.original.clone();
        let document = ingest_lte(vec![first, second]).unwrap();

        assert_eq!(document.combo.len(), 3);
        assert!(document.combo.iter().all(|combo| combo.selection.is_none()));
        let lte = validated(&document);
        assert_eq!(
            generate_lte_file(&lte, 91, &Sku::Lte(91)).unwrap().bytes,
            first_original
        );
        assert_eq!(
            generate_lte_file(&lte, 92, &Sku::Lte(92)).unwrap().bytes,
            second_original
        );
    }

    #[test]
    fn conflicting_file_orders_are_rejected_as_a_cycle() {
        let a = combo(&[1]);
        let b = combo(&[2]);
        let error = ingest_lte(vec![
            decoded(91, caps(1, 11, vec![a.clone(), b.clone()])),
            decoded(92, caps(2, 12, vec![b, a])),
        ])
        .unwrap_err()
        .to_string();

        assert!(error.contains("cycle"), "{error}");
    }

    #[test]
    fn file_ids_expand_to_registered_models_or_one_synthetic_token() {
        const REGISTERED_ID: u64 = 4_210_990_300;
        let registered = combo(&[1]);
        let synthetic = combo(&[2]);
        let document = ingest_lte(vec![
            decoded(REGISTERED_ID, caps(1, 11, vec![registered])),
            decoded(91, caps(2, 12, vec![synthetic])),
        ])
        .unwrap();

        let expected_registered: Vec<_> = lte_model_codes(REGISTERED_ID)
            .into_iter()
            .map(str::to_owned)
            .collect();
        assert_eq!(
            document.combo[0].selection.as_ref().unwrap()[0].skus,
            Some(expected_registered)
        );
        assert_eq!(
            document.combo[1].selection.as_ref().unwrap()[0].skus,
            Some(vec!["lte:91".into()])
        );
    }

    #[test]
    fn generation_filters_global_order_restores_metadata_and_is_byte_identical() {
        let shared = combo(&[1]);
        // A real corpus bitfield (class B, 4x4). This combo only has to differ from `shared`; a
        // disabled DL and an absent UL are states the source format cannot represent and
        // `validate_lte_combos` rejects, and `validated()` below round-trips through the source
        // format.
        let first_only = LteCombo {
            components: vec![LteComponent {
                band: 2,
                dl_bw_class_mimo: 16_385,
                ul_bw_class_mimo: Some(0),
            }],
            bcs: None,
            unknown1: Some(0),
            unknown2: None,
        };
        let second_only = combo(&[3]);
        let first = decoded(91, caps(101, 201, vec![shared.clone(), first_only.clone()]));
        let second = decoded(92, caps(102, 202, vec![shared, second_only]));
        let expected = first.original.clone();
        let document = ingest_lte(vec![first, second]).unwrap();
        let generated = generate_lte_file(&validated(&document), 91, &Sku::Lte(91)).unwrap();

        assert_eq!(generated.basename, "lte_91.binarypb");
        assert_eq!(generated.bytes, expected);
        assert_eq!(
            LteCaps::decode(generated.bytes.as_slice())
                .unwrap()
                .fingerprint,
            101
        );
    }

    /// The byte-identity self-check's *residual* job. `wire::scan` now rejects every other
    /// non-canonical encoding (unknown field, wrong wire type, out-of-range varint, duplicate
    /// singular field, descending tag order, overlong varint), which leaves exactly one form it
    /// cannot reject: an explicit zero for a bare, non-`optional` scalar. prost drops that on
    /// re-encode — see `src/proto.rs`'s note on the bit-identity messages — so only a byte
    /// comparison catches it. Reordered fields are covered by `wire`'s own tests now.
    #[test]
    fn decode_self_verification_rejects_noncanonical_original_bytes() {
        let caps = caps(1, 0, Vec::new());
        // field 1 (fingerprint) = 1, then field 3 (bitmask) = an explicit 0 that the
        // canonical encoding omits. Ascending order, no duplicates, in range: wire-legal.
        let original = vec![0x08, 0x01, 0x18, 0x00];
        assert_eq!(LteCaps::decode(original.as_slice()).unwrap(), caps);
        assert_eq!(caps.encode_to_vec(), vec![0x08, 0x01]);
        crate::wire::ensure_modeled(&original, crate::wire::RootMessage::LteCaps)
            .expect("the strict scanner cannot reject an explicit zero");

        let error = ingest_lte(vec![DecodedLteFile {
            id: 91,
            original,
            caps,
        }])
        .unwrap_err()
        .to_string();
        assert!(error.contains("byte-identical"), "{error}");
    }

    #[test]
    fn duplicate_file_ids_are_rejected() {
        let error = ingest_lte(vec![
            decoded(91, caps(1, 11, vec![combo(&[1])])),
            decoded(91, caps(2, 12, vec![combo(&[2])])),
        ])
        .unwrap_err()
        .to_string();
        assert!(error.contains("duplicate LTE file ID 91"), "{error}");
    }
}
