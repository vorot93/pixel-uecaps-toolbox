use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::OsStr,
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, ensure};
use prost::Message;

use super::{
    lte::{DecodedLteFile, generate_lte_file, ingest_lte},
    nr::{DecodedNrFile, NrTarget, generate_nr_files, ingest_nr},
    schema::{LteDocument, NrDocument, ValidatedSources, parse_sources, validate_documents},
    selection::Sku,
};
use crate::{
    atomic::prepare_sibling_atomic,
    magisk::validate_module_basename,
    mapping::{map_to_root, root_to_map},
    model::{lte_model_codes, profile_model_codes},
    proto::{Carrier, PlmnMap},
    wire::{decode_lte_caps, decode_plmn_map, decode_uecaps},
};

#[derive(Clone, Debug, PartialEq, Eq)]
enum BitmaskInputName {
    Carrier(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum ProfiledInputName {
    Carrier { carrier: String, number: u64 },
    Mapping,
    Lte { id: u64 },
}

#[derive(Clone, Debug)]
struct ClassifiedFile<T> {
    basename: String,
    path: PathBuf,
    kind: T,
}

/// Decode one complete legacy bitmask folder and one complete profiled folder into the two
/// canonical compiler source documents. No output path is touched by this pure orchestration
/// seam.
pub(crate) fn decode_documents(
    bitmask_dir: &Path,
    profiled_dir: &Path,
) -> anyhow::Result<(NrDocument, LteDocument, String, String)> {
    let bitmask_files = classify_bitmask_dir(bitmask_dir)?;
    let profiled_files = classify_profiled_dir(profiled_dir)?;

    let mut legacy = Vec::with_capacity(bitmask_files.len());
    for file in bitmask_files {
        let BitmaskInputName::Carrier(carrier) = file.kind;
        let bytes = read_file(&file.path, &file.basename)?;
        let caps = decode_uecaps(&bytes, &file.basename)?;
        legacy.push(DecodedNrFile {
            carrier,
            number: None,
            caps,
        });
    }

    let mut profiled = Vec::new();
    let mut lte_files = Vec::new();
    let mut original_lte = BTreeMap::new();
    let mut original_mapping = None;
    let mut mapping = None;
    for file in profiled_files {
        let bytes = read_file(&file.path, &file.basename)?;
        match file.kind {
            ProfiledInputName::Carrier { carrier, number } => {
                let caps = decode_uecaps(&bytes, &file.basename)?;
                profiled.push(DecodedNrFile {
                    carrier,
                    number: Some(number),
                    caps,
                });
            }
            ProfiledInputName::Mapping => {
                let decoded = decode_plmn_map(&bytes, &file.basename)?;
                let root = map_to_root(&decoded).context("decoding profiled PLMN mapping")?;
                root_to_map(&root).context("validating profiled PLMN mapping IDs and names")?;
                ensure_mapping_order(&decoded)?;
                original_mapping = Some(bytes);
                mapping = Some(root);
            }
            ProfiledInputName::Lte { id } => {
                let caps = decode_lte_caps(&bytes, &file.basename)?;
                ensure!(
                    original_lte.insert(id, bytes.clone()).is_none(),
                    "duplicate LTE file ID {id}"
                );
                lte_files.push(DecodedLteFile {
                    id,
                    original: bytes,
                    caps,
                });
            }
        }
    }

    let mapping = mapping.expect("profiled classifier requires one mapping");
    let original_mapping = original_mapping.expect("profiled classifier requires one mapping");
    let nr = ingest_nr(legacy, profiled, &mapping).context("normalizing NR carrier files")?;
    let lte = ingest_lte(lte_files).context("normalizing LTE files")?;

    // Serialization is itself a validation boundary. Canonicalize the ingest once, reparse the
    // emitted source through the strict public schema, and require byte-idempotence before using
    // the validated representation for every internal generation self-check. The reparse's
    // `validated.to_kdl()` serializes an already-canonical source, so no third `validate_documents`
    // pass is needed — the assertions below still prove the emitted documents are a fixed point.
    let (nr_text, lte_text) = validate_documents(nr, lte)?.to_kdl()?;
    let validated = parse_sources(&nr_text, &lte_text).context("reparsing decoded sources")?;
    let (canonical_nr, canonical_lte) = validated.to_kdl()?;
    ensure!(
        canonical_nr.as_bytes() == nr_text.as_bytes(),
        "nr.kdl changed when reparsed and reserialized"
    );
    ensure!(
        canonical_lte.as_bytes() == lte_text.as_bytes(),
        "lte.kdl changed when reparsed and reserialized"
    );

    verify_internal_targets(&validated, &original_mapping, &original_lte)?;
    // `nr_text`/`lte_text` are the canonical documents already validated above (reparse +
    // reserialize byte-idempotent). Return them so `decompose` need not recompute to_kdl (E4).
    Ok((validated.nr.source, validated.lte.source, nr_text, lte_text))
}

/// Decompose both required folders and atomically replace the two canonical source documents.
/// Validation, encoding, and self-verification finish before the output directory is created.
pub fn decompose(bitmask_dir: &Path, profiled_dir: &Path, out_dir: &Path) -> anyhow::Result<i32> {
    let (_nr, _lte, nr_text, lte_text) = decode_documents(bitmask_dir, profiled_dir)?;
    let nr_bytes = nr_text.into_bytes();
    let lte_bytes = lte_text.into_bytes();

    if out_dir.exists() {
        ensure!(
            out_dir.is_dir(),
            "decompose output {} must be a directory",
            out_dir.display()
        );
    } else {
        fs::create_dir_all(out_dir).with_context(|| {
            format!("creating decompose output directory {}", out_dir.display())
        })?;
    }

    let nr_path = out_dir.join("nr.kdl");
    let lte_path = out_dir.join("lte.kdl");
    let prepared_nr = prepare_sibling_atomic(&nr_path, |writer| {
        writer.write_all(&nr_bytes)?;
        Ok(())
    })?;
    let prepared_lte = prepare_sibling_atomic(&lte_path, |writer| {
        writer.write_all(&lte_bytes)?;
        Ok(())
    })?;
    prepared_nr.persist()?;
    prepared_lte.persist()?;
    Ok(0)
}

fn classify_bitmask_dir(dir: &Path) -> anyhow::Result<Vec<ClassifiedFile<BitmaskInputName>>> {
    let files = classify_directory(dir, "bitmask", classify_bitmask_name)?;
    ensure!(
        !files.is_empty(),
        "bitmask input must contain at least one unnumbered carrier .binarypb file"
    );
    Ok(files)
}

fn classify_profiled_dir(dir: &Path) -> anyhow::Result<Vec<ClassifiedFile<ProfiledInputName>>> {
    let files = classify_directory(dir, "profiled", classify_profiled_name)?;
    let mappings = files
        .iter()
        .filter(|file| matches!(file.kind, ProfiledInputName::Mapping))
        .count();
    let lte = files
        .iter()
        .filter(|file| matches!(file.kind, ProfiledInputName::Lte { .. }))
        .count();
    let carriers = files
        .iter()
        .filter(|file| matches!(file.kind, ProfiledInputName::Carrier { .. }))
        .count();
    ensure!(
        mappings == 1,
        "profiled input must contain exactly one ap_plmn_mapping.binarypb (found {mappings})"
    );
    ensure!(
        lte != 0,
        "profiled input must contain at least one lte_<id>.binarypb file"
    );
    ensure!(
        carriers != 0,
        "profiled input must contain at least one numbered carrier .binarypb file"
    );
    Ok(files)
}

fn classify_directory<T>(
    dir: &Path,
    label: &str,
    classify: impl Fn(&str) -> anyhow::Result<T>,
) -> anyhow::Result<Vec<ClassifiedFile<T>>> {
    let metadata = fs::metadata(dir)
        .with_context(|| format!("reading {label} input directory {}", dir.display()))?;
    ensure!(
        metadata.is_dir(),
        "{label} input {} must be a directory",
        dir.display()
    );

    let mut files = Vec::new();
    for entry in fs::read_dir(dir)
        .with_context(|| format!("listing {label} input directory {}", dir.display()))?
    {
        let entry = entry
            .with_context(|| format!("reading an entry from {label} input {}", dir.display()))?;
        let file_name = entry.file_name();
        let basename = match file_name.to_str() {
            Some(basename) if basename.ends_with(".binarypb") => basename.to_owned(),
            Some(_) => continue,
            None if Path::new(&file_name).extension() == Some(OsStr::new("binarypb")) => {
                anyhow::bail!("{label} input contains a non-UTF-8 .binarypb filename")
            }
            None => continue,
        };
        validate_module_basename(&basename)
            .with_context(|| format!("validating {label} input basename"))?;
        let kind = classify(&basename)?;
        files.push(ClassifiedFile {
            basename,
            path: entry.path(),
            kind,
        });
    }
    files.sort_by(|left, right| left.basename.cmp(&right.basename));
    Ok(files)
}

fn classify_bitmask_name(basename: &str) -> anyhow::Result<BitmaskInputName> {
    let stem = basename
        .strip_suffix(".binarypb")
        .expect("caller filters exact binarypb extension");
    ensure!(
        !stem.is_empty(),
        "unsupported bitmask .binarypb filename `{basename}`: carrier name must be nonempty"
    );
    ensure!(
        stem != "ap_plmn_mapping",
        "unsupported cross-layout file `{basename}` in bitmask input"
    );
    if let Some((_, suffix)) = stem.rsplit_once('_')
        && !suffix.is_empty()
        && suffix.bytes().all(|byte| byte.is_ascii_digit())
    {
        anyhow::bail!("unsupported numbered file `{basename}` in bitmask input");
    }
    Ok(BitmaskInputName::Carrier(stem.into()))
}

/// Require a generated legacy NR basename to classify exactly as a bitmask carrier input.
pub(super) fn validate_bitmask_carrier_basename(basename: &str) -> anyhow::Result<()> {
    let BitmaskInputName::Carrier(_) = classify_bitmask_name(basename)?;
    Ok(())
}

fn classify_profiled_name(basename: &str) -> anyhow::Result<ProfiledInputName> {
    if basename == "ap_plmn_mapping.binarypb" {
        return Ok(ProfiledInputName::Mapping);
    }
    let stem = basename
        .strip_suffix(".binarypb")
        .expect("caller filters exact binarypb extension");
    if let Some(decimal) = stem.strip_prefix("lte_") {
        let id = parse_filename_number(decimal, basename, "LTE file ID")?;
        return Ok(ProfiledInputName::Lte { id });
    }
    let Some((carrier, decimal)) = stem.rsplit_once('_') else {
        anyhow::bail!("unsupported cross-layout file `{basename}` in profiled input");
    };
    ensure!(
        !carrier.is_empty(),
        "unsupported profiled filename `{basename}`: carrier name must be nonempty"
    );
    let number = parse_filename_number(decimal, basename, "carrier number")?;
    Ok(ProfiledInputName::Carrier {
        carrier: carrier.into(),
        number,
    })
}

/// Require a generated modern NR basename to classify as a profiled carrier rather than the
/// reserved mapping or LTE forms.
pub(super) fn validate_profiled_carrier_basename(basename: &str) -> anyhow::Result<()> {
    match classify_profiled_name(basename)? {
        ProfiledInputName::Carrier { .. } => Ok(()),
        ProfiledInputName::Mapping => {
            anyhow::bail!("generated profiled NR basename `{basename}` is reserved for the mapping")
        }
        ProfiledInputName::Lte { .. } => {
            anyhow::bail!("generated profiled NR basename `{basename}` classifies as LTE")
        }
    }
}

fn parse_filename_number(decimal: &str, basename: &str, field: &str) -> anyhow::Result<u64> {
    let value = decimal.parse::<u64>().with_context(|| {
        format!("profiled filename `{basename}` {field} `{decimal}` does not fit u64")
    })?;
    ensure!(
        value.to_string() == decimal,
        "profiled filename `{basename}` {field} must be shortest decimal"
    );
    Ok(value)
}

fn read_file(path: &Path, basename: &str) -> anyhow::Result<Vec<u8>> {
    fs::read(path).with_context(|| format!("reading supported input file `{basename}`"))
}

fn ensure_mapping_order(mapping: &PlmnMap) -> anyhow::Result<()> {
    ensure!(
        mapping
            .carriers
            .windows(2)
            .all(|pair| pair[0].index < pair[1].index),
        "PLMN mapping entries must already be in increasing mapping_id order"
    );
    Ok(())
}

fn verify_internal_targets(
    sources: &ValidatedSources,
    original_mapping: &[u8],
    original_lte: &BTreeMap<u64, Vec<u8>>,
) -> anyhow::Result<()> {
    let legacy = generate_nr_files(&sources.nr, NrTarget::Legacy)
        .context("self-verifying the legacy NR target")?;
    let mut expected_legacy = sources
        .nr
        .source
        .bitmask_carriers
        .iter()
        .map(|carrier| format!("{carrier}.binarypb"))
        .collect::<Vec<_>>();
    expected_legacy.sort_unstable();
    ensure!(
        legacy
            .iter()
            .map(|file| file.basename.clone())
            .collect::<Vec<_>>()
            == expected_legacy,
        "legacy NR target generated an unexpected file set"
    );
    for file in &legacy {
        decode_uecaps(&file.bytes, &format!("generated {}", file.basename))?;
    }

    let anchors = sources
        .nr
        .carriers
        .values()
        .flat_map(|carrier| carrier.profiles.keys().copied())
        .collect::<BTreeSet<_>>();
    for anchor in anchors {
        let sku = profile_model_codes(anchor)
            .first()
            .map_or(Sku::Prime(anchor), |code| Sku::Model((*code).into()));
        let generated = generate_nr_files(&sources.nr, NrTarget::Profile { anchor, sku })
            .with_context(|| format!("self-verifying NR profile anchor {anchor}"))?;
        let mut expected = sources
            .nr
            .carriers
            .iter()
            .filter_map(|(carrier, source)| {
                source
                    .profiles
                    .get(&anchor)
                    .map(|profile| format!("{carrier}_{}.binarypb", profile.number))
            })
            .collect::<Vec<_>>();
        expected.sort_unstable();
        ensure!(
            generated
                .iter()
                .map(|file| file.basename.clone())
                .collect::<Vec<_>>()
                == expected,
            "NR profile anchor {anchor} generated an unexpected file set"
        );
        for file in &generated {
            decode_uecaps(&file.bytes, &format!("generated {}", file.basename))?;
        }
    }

    for id in sources.lte.files.keys().copied() {
        let sku = lte_model_codes(id)
            .first()
            .map_or(Sku::Lte(id), |code| Sku::Model((*code).into()));
        let generated = generate_lte_file(&sources.lte, id, &sku)
            .with_context(|| format!("self-verifying LTE file ID {id}"))?;
        let original = original_lte
            .get(&id)
            .expect("every validated LTE file retains its original bytes");
        ensure!(
            generated.bytes == *original,
            "LTE self-verification for lte_{id}.binarypb was not byte-identical"
        );
    }

    let rebuilt_mapping = rebuild_mapping(sources)?;
    ensure!(
        rebuilt_mapping == original_mapping,
        "PLMN mapping decode self-verification was not byte-identical"
    );
    Ok(())
}

fn rebuild_mapping(sources: &ValidatedSources) -> anyhow::Result<Vec<u8>> {
    let mut carriers = sources
        .nr
        .carriers
        .iter()
        .filter_map(|(name, source)| {
            source.plmns.as_ref().map(|plmns| Carrier {
                plmns: plmns.clone(),
                index: source
                    .mapping_id
                    .expect("validated PLMN carrier has mapping_id"),
                name: name.clone(),
            })
        })
        .collect::<Vec<_>>();
    carriers.sort_by_key(|carrier| carrier.index);
    let mapping = PlmnMap { carriers };
    let root = map_to_root(&mapping).context("building self-verification PLMN mapping")?;
    let mapping = root_to_map(&root).context("validating self-verification PLMN mapping")?;
    let bytes = mapping.encode_to_vec();
    let decoded = decode_plmn_map(&bytes, "rebuilt ap_plmn_mapping.binarypb")?;
    ensure!(
        decoded == mapping,
        "rebuilt PLMN mapping changed values during encoding"
    );
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use std::fs;

    use prost::Message;
    use tempfile::tempdir;

    use super::{decode_documents, decompose};
    use crate::{
        compiler::{
            schema::{parse_sources, to_kdl},
            test_support::{
                FIRST_LTE_ID, MiniCorpus, SECOND_LTE_ID, SYNTHETIC_ANCHOR, decode_lte,
                decode_mapping, decode_nr, inject_unknown_nr_cc_field,
                make_lte_encoding_noncanonical, replace_lte, replace_mapping, replace_nr,
            },
        },
        proto::{
            Carrier, ComboGroup, ShannonFeatureSetDlPerCcNr, ShannonFeatureSetUlPerCcNr,
            combo_group::ComboHeader,
        },
    };

    fn profile_name(carrier: &str, signature: u64, anchor: u64) -> String {
        format!("{carrier}_{}.binarypb", signature * anchor)
    }

    fn assert_prewrite_failure(corpus: MiniCorpus, expected: &str) -> String {
        let temp = tempdir().unwrap();
        let (bitmask, profiled) = corpus.write_to(temp.path(), false);
        let out = temp.path().join("out");
        fs::create_dir(&out).unwrap();
        fs::write(out.join("nr.kdl"), b"old nr\n").unwrap();
        fs::write(out.join("lte.kdl"), b"old lte\n").unwrap();

        let error = decompose(&bitmask, &profiled, &out).unwrap_err();
        let error = format!("{error:#}");
        assert!(error.contains(expected), "unexpected error: {error}");
        assert_eq!(fs::read(out.join("nr.kdl")).unwrap(), b"old nr\n");
        assert_eq!(fs::read(out.join("lte.kdl")).unwrap(), b"old lte\n");
        let mut names = fs::read_dir(&out)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().into_string().unwrap())
            .collect::<Vec<_>>();
        names.sort_unstable();
        assert_eq!(names, ["lte.kdl", "nr.kdl"]);
        error
    }

    fn mapping_file(corpus: &mut MiniCorpus) -> &mut crate::compiler::test_support::FixtureFile {
        corpus.profiled_file_mut("ap_plmn_mapping.binarypb")
    }

    fn lte_file(
        corpus: &mut MiniCorpus,
        id: u64,
    ) -> &mut crate::compiler::test_support::FixtureFile {
        corpus.profiled_file_mut(&format!("lte_{id}.binarypb"))
    }

    #[test]
    fn complete_fixture_decodes_both_required_folders() {
        let temp = tempdir().unwrap();
        let corpus = MiniCorpus::new();
        let expected = corpus.expected.clone();
        let (bitmask, profiled) = corpus.write_to(temp.path(), false);

        let (nr, lte, _, _) = decode_documents(&bitmask, &profiled).unwrap();

        assert_eq!(nr.bitmask_carriers, expected.bitmask_carriers);
        for (carrier, anchors) in expected.profiles {
            let actual = nr.carriers[&carrier]
                .profiles
                .keys()
                .map(|key| key.parse::<u64>().unwrap())
                .collect::<std::collections::BTreeSet<_>>();
            assert_eq!(actual, anchors);
        }
        for (carrier, plmns) in expected.plmns {
            assert_eq!(nr.carriers[&carrier].plmns.as_ref(), Some(&plmns));
        }
        assert_eq!(nr.combo.len(), expected.nr_payloads);
        assert_eq!(
            lte.files
                .keys()
                .map(|key| key.parse::<u64>().unwrap())
                .collect::<Vec<_>>(),
            expected.lte_ids
        );
        assert_eq!(lte.combo.len(), expected.lte_payloads);
        let (nr_kdl, lte_kdl) = to_kdl(&nr, &lte).unwrap();
        assert_eq!(nr_kdl, expected.nr_kdl);
        assert_eq!(lte_kdl, expected.lte_kdl);
    }

    #[test]
    fn both_inputs_must_be_directories() {
        let temp = tempdir().unwrap();
        let (bitmask, profiled) = MiniCorpus::new().write_to(temp.path(), false);
        let regular_file = temp.path().join("not-a-directory");
        fs::write(&regular_file, b"x").unwrap();

        let error = decode_documents(&regular_file, &profiled)
            .unwrap_err()
            .to_string();
        assert!(error.contains("bitmask input"), "{error}");
        assert!(error.contains("directory"), "{error}");

        let error = decode_documents(&bitmask, &regular_file)
            .unwrap_err()
            .to_string();
        assert!(error.contains("profiled input"), "{error}");
        assert!(error.contains("directory"), "{error}");
    }

    #[test]
    fn bitmask_folder_accepts_only_nonempty_unnumbered_binarypb_names() {
        for invalid in [
            "ap_plmn_mapping.binarypb",
            "ALPHA_123.binarypb",
            "lte_91.binarypb",
            ".binarypb",
        ] {
            let mut corpus = MiniCorpus::new();
            corpus.bitmask[0].basename = invalid.into();
            let error = assert_prewrite_failure(corpus, "bitmask");
            assert!(error.contains(invalid), "{error}");
        }
    }

    #[test]
    fn profiled_folder_requires_mapping_lte_and_numbered_carriers() {
        let mut without_mapping = MiniCorpus::new();
        without_mapping.remove_profiled(|file| file.basename == "ap_plmn_mapping.binarypb");
        assert_prewrite_failure(without_mapping, "exactly one ap_plmn_mapping.binarypb");

        let mut without_lte = MiniCorpus::new();
        without_lte.remove_profiled(|file| file.basename.starts_with("lte_"));
        assert_prewrite_failure(without_lte, "at least one lte_<id>.binarypb");

        let mut without_carriers = MiniCorpus::new();
        without_carriers.remove_profiled(|file| {
            file.basename != "ap_plmn_mapping.binarypb" && !file.basename.starts_with("lte_")
        });
        assert_prewrite_failure(without_carriers, "at least one numbered carrier");
    }

    #[test]
    fn profiled_folder_rejects_cross_layout_and_unsupported_binarypb_names() {
        for invalid in ["LEGACY.binarypb", "lte_bad.binarypb", "CARRIER_.binarypb"] {
            let mut corpus = MiniCorpus::new();
            corpus.profiled[0].basename = invalid.into();
            let error = assert_prewrite_failure(corpus, "profiled");
            assert!(error.contains(invalid), "{error}");
        }
    }

    #[test]
    fn unrelated_files_are_ignored_and_creation_order_does_not_change_output() {
        let first = tempdir().unwrap();
        let second = tempdir().unwrap();
        let (first_bitmask, first_profiled) = MiniCorpus::new().write_to(first.path(), false);
        let (second_bitmask, second_profiled) = MiniCorpus::new().write_to(second.path(), true);
        fs::write(first_bitmask.join("README.txt"), b"ignored").unwrap();
        fs::write(first_profiled.join("notes.txt"), b"ignored").unwrap();
        fs::create_dir(first_profiled.join("nested")).unwrap();

        let first_out = first.path().join("out");
        let second_out = second.path().join("out");
        assert_eq!(
            decompose(&first_bitmask, &first_profiled, &first_out).unwrap(),
            0
        );
        assert_eq!(
            decompose(&second_bitmask, &second_profiled, &second_out).unwrap(),
            0
        );
        assert_eq!(
            fs::read(first_out.join("nr.kdl")).unwrap(),
            fs::read(second_out.join("nr.kdl")).unwrap()
        );
        assert_eq!(
            fs::read(first_out.join("lte.kdl")).unwrap(),
            fs::read(second_out.join("lte.kdl")).unwrap()
        );
    }

    #[test]
    fn prefix_related_carrier_names_use_sorted_expected_basenames() {
        let mut corpus = MiniCorpus::new();
        corpus.rename_carrier("ALPHA", "EU_COMMON");
        corpus.rename_carrier("BETA", "EU_COMMON1");
        let temp = tempdir().unwrap();
        let (bitmask, profiled) = corpus.write_to(temp.path(), false);
        decode_documents(&bitmask, &profiled).unwrap();
    }

    #[test]
    fn decompose_writes_exactly_two_newline_terminated_idempotent_documents() {
        let temp = tempdir().unwrap();
        let (bitmask, profiled) = MiniCorpus::new().write_to(temp.path(), false);
        let out = temp.path().join("source");

        assert_eq!(decompose(&bitmask, &profiled, &out).unwrap(), 0);

        let mut names = fs::read_dir(&out)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().into_string().unwrap())
            .collect::<Vec<_>>();
        names.sort_unstable();
        assert_eq!(names, ["lte.kdl", "nr.kdl"]);
        let nr = fs::read_to_string(out.join("nr.kdl")).unwrap();
        let lte = fs::read_to_string(out.join("lte.kdl")).unwrap();
        assert!(nr.ends_with('\n') && !nr.ends_with("\n\n"));
        assert!(lte.ends_with('\n') && !lte.ends_with("\n\n"));
        let parsed = parse_sources(&nr, &lte).unwrap();
        let (canonical_nr, canonical_lte) = to_kdl(&parsed.nr.source, &parsed.lte.source).unwrap();
        assert_eq!(canonical_nr, nr);
        assert_eq!(canonical_lte, lte);
    }

    #[test]
    fn mapping_must_already_be_in_increasing_mapping_id_order() {
        let mut corpus = MiniCorpus::new();
        let file = mapping_file(&mut corpus);
        let mut mapping = decode_mapping(file);
        mapping.carriers.reverse();
        replace_mapping(file, &mapping);

        assert_prewrite_failure(corpus, "in increasing mapping_id order");
    }

    #[test]
    fn unknown_wire_fields_fail_before_outputs_change() {
        let mut corpus = MiniCorpus::new();
        corpus
            .bitmask_file_mut("ALPHA.binarypb")
            .bytes
            .extend_from_slice(&[0x98, 0x06, 0x01]);
        assert_prewrite_failure(corpus, "field #99 is not modeled");

        let mut nested = MiniCorpus::new();
        inject_unknown_nr_cc_field(nested.bitmask_file_mut("ALPHA.binarypb"));
        let error = assert_prewrite_failure(nested, "field #15 is not modeled");
        assert!(error.contains("SubBlock"), "{error}");
    }

    #[test]
    fn value_bearing_empty_nr_group_fails_before_outputs_change() {
        let mut corpus = MiniCorpus::new();
        let file = corpus.bitmask_file_mut("ALPHA.binarypb");
        let mut caps = decode_nr(file);
        caps.combo_groups.push(ComboGroup {
            combo_header: Some(ComboHeader {
                power_class: Some(3),
                ..Default::default()
            }),
            combo: Vec::new(),
        });
        replace_nr(file, &caps);

        assert_prewrite_failure(corpus, "empty combo group");
    }

    #[test]
    fn semantically_empty_nr_layout_remains_decodable() {
        let mut corpus = MiniCorpus::new();
        let file = corpus.bitmask_file_mut("ALPHA.binarypb");
        let mut caps = decode_nr(file);
        caps.combo_groups.push(ComboGroup::default());
        caps.dl_feature_per_cc_list
            .push(ShannonFeatureSetDlPerCcNr::default());
        caps.ul_feature_per_cc_list
            .push(ShannonFeatureSetUlPerCcNr::default());
        replace_nr(file, &caps);
        let temp = tempdir().unwrap();
        let (bitmask, profiled) = corpus.write_to(temp.path(), false);

        assert_eq!(
            decompose(&bitmask, &profiled, &temp.path().join("out")).unwrap(),
            0
        );
    }

    #[test]
    fn unsafe_carrier_input_basenames_fail_before_outputs_change() {
        for (carrier, expected) in [
            ("A\\B", "path separators"),
            ("A\nB", "control or line-separator"),
            ("A\u{2028}B", "control or line-separator"),
        ] {
            let mut bitmask = MiniCorpus::new();
            bitmask.bitmask[0].basename = format!("{carrier}.binarypb");
            assert_prewrite_failure(bitmask, expected);

            let mut profiled = MiniCorpus::new();
            profiled
                .profiled_file_mut(&profile_name("ALPHA", 11, SYNTHETIC_ANCHOR))
                .basename = format!("{carrier}_{}.binarypb", 11 * SYNTHETIC_ANCHOR);
            assert_prewrite_failure(profiled, expected);
        }
    }

    #[test]
    fn lte_self_verification_mismatch_fails_before_outputs_change() {
        let mut corpus = MiniCorpus::new();
        make_lte_encoding_noncanonical(lte_file(&mut corpus, FIRST_LTE_ID));

        assert_prewrite_failure(corpus, "was not byte-identical");
    }

    #[test]
    fn malformed_mapping_and_duplicate_mapping_metadata_fail_before_writes() {
        let mut malformed = MiniCorpus::new();
        let file = mapping_file(&mut malformed);
        let mut mapping = decode_mapping(file);
        mapping.carriers[0].plmns.push(0x0100_0000);
        replace_mapping(file, &mapping);
        assert_prewrite_failure(malformed, "PLMN");

        let mut duplicate_id = MiniCorpus::new();
        let file = mapping_file(&mut duplicate_id);
        let mut mapping = decode_mapping(file);
        mapping.carriers[1].index = mapping.carriers[0].index;
        replace_mapping(file, &mapping);
        assert_prewrite_failure(duplicate_id, "duplicate carrier id");

        let mut duplicate_name = MiniCorpus::new();
        let file = mapping_file(&mut duplicate_name);
        let mut mapping = decode_mapping(file);
        mapping.carriers[1].name = mapping.carriers[0].name.clone();
        replace_mapping(file, &mapping);
        assert_prewrite_failure(duplicate_name, "duplicate mapping name");
    }

    #[test]
    fn inconsistent_profile_ids_and_tiers_fail_before_writes() {
        let mut ids = MiniCorpus::new();
        let name = profile_name("ALPHA", 11, SYNTHETIC_ANCHOR);
        let file = ids.profiled_file_mut(&name);
        let mut caps = decode_nr(file);
        caps.id = Some(9);
        replace_nr(file, &caps);
        assert_prewrite_failure(ids, "inconsistent field 2 IDs");

        let mut tiers = MiniCorpus::new();
        let file = tiers.profiled_file_mut(&name);
        let mut caps = decode_nr(file);
        caps.version = 707_802_847;
        replace_nr(file, &caps);
        assert_prewrite_failure(tiers, "inconsistent fingerprint tiers");
    }

    #[test]
    fn unknown_and_ambiguous_profile_anchors_fail_before_writes() {
        let mut unknown = MiniCorpus::new();
        unknown
            .profiled_file_mut(&profile_name("ALPHA", 11, SYNTHETIC_ANCHOR))
            .basename = "ALPHA_19.binarypb".into();
        assert_prewrite_failure(unknown, "matched 0");

        let mut ambiguous = MiniCorpus::new();
        ambiguous
            .profiled_file_mut(&profile_name("ALPHA", 11, SYNTHETIC_ANCHOR))
            .basename = "ALPHA_308449.binarypb".into();
        assert_prewrite_failure(ambiguous, "matched 2");
    }

    #[test]
    fn unsupported_bitmask_and_metadata_assumptions_fail_before_writes() {
        let mut modern_bitmask = MiniCorpus::new();
        let name = profile_name("ALPHA", 11, SYNTHETIC_ANCHOR);
        let file = modern_bitmask.profiled_file_mut(&name);
        let mut caps = decode_nr(file);
        caps.combo_groups[0].combo[0].bitmask = Some(1);
        replace_nr(file, &caps);
        assert_prewrite_failure(modern_bitmask, "unsupported nonzero bitmask");

        let mut legacy_unknown = MiniCorpus::new();
        let file = legacy_unknown.bitmask_file_mut("ALPHA.binarypb");
        let mut caps = decode_nr(file);
        caps.unknown = 1;
        replace_nr(file, &caps);
        assert_prewrite_failure(legacy_unknown, "unsupported field 9 value");

        let mut unknown_fingerprint = MiniCorpus::new();
        let file = unknown_fingerprint.profiled_file_mut(&name);
        let mut caps = decode_nr(file);
        caps.version = 123;
        replace_nr(file, &caps);
        assert_prewrite_failure(unknown_fingerprint, "unknown fingerprint");
    }

    #[test]
    fn overflowing_filename_numbers_fail_before_writes() {
        let mut corpus = MiniCorpus::new();
        corpus
            .profiled_file_mut(&profile_name("ALPHA", 11, SYNTHETIC_ANCHOR))
            .basename = "ALPHA_18446744073709551616.binarypb".into();
        assert_prewrite_failure(corpus, "does not fit u64");
    }

    #[test]
    fn duplicate_canonical_nr_payloads_fail_before_writes() {
        let mut corpus = MiniCorpus::new();
        let file = corpus.profiled_file_mut(&profile_name("ALPHA", 11, SYNTHETIC_ANCHOR));
        let mut caps = decode_nr(file);
        caps.combo_groups.push(caps.combo_groups[0].clone());
        replace_nr(file, &caps);
        assert_prewrite_failure(corpus, "duplicate canonical NR payload");
    }

    #[test]
    fn lte_duplicates_and_cycles_fail_before_writes() {
        let mut duplicate = MiniCorpus::new();
        let file = lte_file(&mut duplicate, FIRST_LTE_ID);
        let mut caps = decode_lte(file);
        caps.combos.push(caps.combos[0].clone());
        replace_lte(file, &caps);
        assert_prewrite_failure(duplicate, "duplicate LTE payload");

        let mut cycle = MiniCorpus::new();
        let first = decode_lte(lte_file(&mut cycle, FIRST_LTE_ID));
        let file = lte_file(&mut cycle, SECOND_LTE_ID);
        let mut second = decode_lte(file);
        second.combos = vec![first.combos[1].clone(), first.combos[0].clone()];
        replace_lte(file, &second);
        assert_prewrite_failure(cycle, "cycle");
    }

    #[test]
    fn profiled_mapping_bytes_are_self_verified_with_duplicate_and_empty_plmns() {
        let temp = tempdir().unwrap();
        let corpus = MiniCorpus::new();
        let original = corpus
            .profiled
            .iter()
            .find(|file| file.basename == "ap_plmn_mapping.binarypb")
            .unwrap()
            .bytes
            .clone();
        let (bitmask, profiled) = corpus.write_to(temp.path(), false);

        decode_documents(&bitmask, &profiled).unwrap();

        let mapping = crate::wire::decode_plmn_map(&original, "fixture mapping").unwrap();
        assert_eq!(mapping.carriers[0].plmns, [5_435_408, 5_435_408]);
        assert_eq!(
            mapping.carriers[1],
            Carrier {
                plmns: Vec::new(),
                index: 8,
                name: "BETA".into(),
            }
        );
        assert_eq!(mapping.encode_to_vec(), original);
    }
}
