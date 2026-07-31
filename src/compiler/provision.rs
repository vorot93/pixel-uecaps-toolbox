use std::{collections::BTreeSet, fs, path::Path, str};

use anyhow::{Context, ensure};

use super::{
    GeneratedFile,
    decompose::{validate_bitmask_carrier_basename, validate_profiled_carrier_basename},
    lte::generate_lte_file,
    nr::{NrTarget, generate_nr_files},
    schema::{ValidatedNr, ValidatedSources, legend_root, parse_sources},
    selection::Sku,
};
use crate::{
    atomic::write_bytes_atomic,
    magisk::{ModuleEntry, replacement_module, validate_module_basename},
    mapping::encode_root_verified,
    model::{CapabilityLayout, PhoneModel, known_model_codes, phone_model},
    outcome::Outcome,
    wire::{decode_lte_caps, decode_plmn_map, decode_uecaps},
};

const MAPPING_BASENAME: &str = "ap_plmn_mapping.binarypb";

/// Provision one complete model-specific uecapconfig replacement module, persisted atomically.
pub fn provision(
    model_code: &str,
    source: &Path,
    out: &Path,
    name: Option<&str>,
) -> anyhow::Result<Outcome> {
    let (model, files) = load_and_generate(source, model_code)?;
    write_module(model, files, out, name)
}

/// Provision one registered model's replacement module from **already-parsed** sources. A caller
/// that provisions many models from the same source document — release tooling, or the corpus
/// test — parses the ~19 MB source once with [`load_sources`] and calls this per model, rather than
/// re-parsing and re-validating for every target. `provision` is the single-model convenience
/// wrapper over `load_sources` + this.
pub fn provision_from_sources(
    sources: &ValidatedSources,
    model_code: &str,
    out: &Path,
    name: Option<&str>,
) -> anyhow::Result<Outcome> {
    let model = resolve_model(model_code)?;
    let files = generate_files(sources, model)?;
    write_module(model, files, out, name)
}

/// Assemble a model's generated files into a replacement Magisk ZIP and write it atomically.
fn write_module(
    model: &PhoneModel,
    files: Vec<GeneratedFile>,
    out: &Path,
    name: Option<&str>,
) -> anyhow::Result<Outcome> {
    let inputs = files
        .into_iter()
        .map(|file| (file.basename, file.bytes))
        .collect::<Vec<ModuleEntry>>();
    let default_name = format!("Pixel UE-caps: {}", model.code);
    let zip = replacement_module(&inputs, name.unwrap_or(&default_name))?;
    write_bytes_atomic(out, &zip)?;
    Ok(Outcome::Clean)
}

/// Read and strictly validate the normalized source document.
pub fn load_sources(source: &Path) -> anyhow::Result<ValidatedSources> {
    let bytes = read_source(source)?;
    let text = str::from_utf8(&bytes)
        .with_context(|| format!("{} is not valid UTF-8", source.display()))?;
    parse_sources(text).with_context(|| format!("parsing {}", source.display()))
}

fn read_source(path: &Path) -> anyhow::Result<Vec<u8>> {
    fs::read(path).with_context(|| format!("reading source document {}", path.display()))
}

/// Load the document first, then resolve one real registered model and assemble its files.
pub(crate) fn load_and_generate(
    source: &Path,
    model_code: &str,
) -> anyhow::Result<(&'static PhoneModel, Vec<GeneratedFile>)> {
    let sources = load_sources(source)?;
    let model = resolve_model(model_code)?;
    let files = generate_files(&sources, model)?;
    Ok((model, files))
}

fn resolve_model(model_code: &str) -> anyhow::Result<&'static PhoneModel> {
    phone_model(model_code).with_context(|| {
        format!(
            "unknown model; registered models: {}",
            known_model_codes().join(" ")
        )
    })
}

/// Assemble a complete model-specific `uecapconfig` file set in memory.
pub(crate) fn generate_files(
    sources: &ValidatedSources,
    model: &'static PhoneModel,
) -> anyhow::Result<Vec<GeneratedFile>> {
    let (files, verification) = match model.layout {
        CapabilityLayout::Bitmask => (
            generate_nr_files(&sources.nr, NrTarget::Legacy)?,
            VerificationLayout::Bitmask,
        ),
        CapabilityLayout::Profiled { nr_anchor, lte_id } => {
            let sku = Sku::Model(model.code.into());
            let mut files = generate_nr_files(
                &sources.nr,
                NrTarget::Profile {
                    anchor: nr_anchor,
                    sku: sku.clone(),
                },
            )?;
            files.push(generate_mapping_file(&sources.nr)?);
            files.push(generate_lte_file(&sources.lte, lte_id, &sku)?);
            (
                files,
                VerificationLayout::Profiled {
                    lte_basename: format!("lte_{lte_id}.binarypb"),
                },
            )
        }
    };

    let files = finalize_files(files)?;
    verify_generated_files(&files, &verification)?;
    Ok(files)
}

fn generate_mapping_file(nr: &ValidatedNr) -> anyhow::Result<GeneratedFile> {
    let root = legend_root(&nr.carriers);
    let bytes = encode_root_verified(&root, "generated ap_plmn_mapping.binarypb")?;
    Ok(GeneratedFile {
        basename: MAPPING_BASENAME.into(),
        bytes,
    })
}

fn finalize_files(mut files: Vec<GeneratedFile>) -> anyhow::Result<Vec<GeneratedFile>> {
    files.sort_by(|left, right| left.basename.cmp(&right.basename));
    let mut seen = BTreeSet::new();
    for file in &files {
        ensure_safe_basename(&file.basename)?;
        ensure!(
            seen.insert(file.basename.as_str()),
            "duplicate generated basename `{}`",
            file.basename
        );
    }
    Ok(files)
}

fn ensure_safe_basename(basename: &str) -> anyhow::Result<()> {
    validate_module_basename(basename)
        .with_context(|| format!("generated filename {basename:?} is unsafe"))?;
    ensure!(
        basename.ends_with(".binarypb"),
        "generated basename `{basename}` must end in .binarypb"
    );
    Ok(())
}

enum VerificationLayout {
    Bitmask,
    Profiled { lte_basename: String },
}

/// Verifies one generated NR carrier file: its basename must classify (via `validate_basename`,
/// which distinguishes bitmask/legacy vs. profiled layouts) and its bytes must decode as a valid
/// `.binarypb`. `label` ("legacy"/"profiled") only shapes the error context.
fn verify_generated_nr_file(
    file: &GeneratedFile,
    validate_basename: fn(&str) -> anyhow::Result<()>,
    label: &str,
) -> anyhow::Result<()> {
    validate_basename(&file.basename).with_context(|| {
        format!(
            "validating generated {label} NR basename `{}`",
            file.basename
        )
    })?;
    decode_uecaps(&file.bytes, &format!("generated {}", file.basename))?;
    Ok(())
}

fn verify_generated_files(
    files: &[GeneratedFile],
    layout: &VerificationLayout,
) -> anyhow::Result<()> {
    for file in files {
        match layout {
            VerificationLayout::Bitmask => {
                verify_generated_nr_file(file, validate_bitmask_carrier_basename, "legacy")?
            }
            VerificationLayout::Profiled { .. } if file.basename == MAPPING_BASENAME => {
                decode_plmn_map(&file.bytes, &format!("generated {}", file.basename))?;
            }
            VerificationLayout::Profiled { lte_basename } if file.basename == *lte_basename => {
                decode_lte_caps(&file.bytes, &format!("generated {}", file.basename))?;
            }
            VerificationLayout::Profiled { .. } => {
                verify_generated_nr_file(file, validate_profiled_carrier_basename, "profiled")?
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::{collections::BTreeMap, fs, io::Read};

    use prost::Message;
    use tempfile::{NamedTempFile, tempdir};

    use super::{finalize_files, generate_files, load_and_generate, load_sources, provision};
    use crate::{
        compiler::{
            GeneratedFile,
            decompose::decompose,
            features::{NrSourceSubBlock, SourceNrSubBlock},
            schema::{
                BitmaskFingerprint, CarrierSource, CarrierTier, DecimalU64, LteDocument,
                LteFileSource, LteSourceCombo, NrDocument, NrSourceCombo, ProfileSource,
                SourceDocument, ValidatedSources, parse_sources, to_kdl,
            },
            selection::SelectionRect,
            source_from_kdl,
            test_support::{MiniCorpus, REGISTERED_ANCHOR},
        },
        model::{known_model_codes, phone_model},
        outcome::Outcome,
        proto::{LteComponent, PlmnMap, ShannonFeatureSetDlPerCcNr},
        report::combos::build_combos,
        wire::{decode_lte_caps, decode_plmn_map, decode_uecaps},
    };
    use zip::{CompressionMethod, DateTime, ZipArchive};

    const TARGET_ANCHOR: u64 = 66_813_533;
    const TARGET_LTE_ID: u64 = 400_907_661;
    const SYNTHETIC_ANCHOR: u64 = 8_969;
    const TARGET_MODEL: &str = "G2YBB";

    fn selection(carriers: &[&str], skus: &[&str]) -> Option<Vec<SelectionRect>> {
        Some(vec![SelectionRect {
            carriers: Some(carriers.iter().map(|value| (*value).into()).collect()),
            skus: Some(skus.iter().map(|value| (*value).into()).collect()),
        }])
    }

    fn nr_combo(band: u16, carriers: &[&str], skus: &[&str]) -> NrSourceCombo {
        NrSourceCombo {
            selection: selection(carriers, skus),
            // The four corpus-verified always-`Some` header fields (all but
            // `bcs_intra_endc`) — the compiler self-check re-decodes generated bytes
            // through the strict `raw_nr::from_proto_combo` boundary, which fails closed
            // on a missing one (Task 8).
            power_class: Some(0),
            bcs_nr: Some(0),
            bcs_intra_endc: None,
            bcs_eutra: Some(0),
            intra_band_en_dc_support: Some(0),
            sub_blocks: vec![
                SourceNrSubBlock {
                    band,
                    dl_bw_class: Some(1),
                    ul_bw_class: Some(1),
                    ..Default::default()
                }
                .into(),
            ],
        }
    }

    fn carrier_with_profile(
        profiled_id: i64,
        mapping_id: Option<u64>,
        signature: u64,
        anchor: u64,
        unknown: u64,
        plmns: Option<Vec<String>>,
    ) -> CarrierSource {
        CarrierSource {
            profiled_id: Some(profiled_id),
            mapping_id,
            plmns,
            signature: Some(DecimalU64(signature)),
            tier: Some(CarrierTier::Main),
            profiles: BTreeMap::from([(
                anchor.to_string(),
                ProfileSource {
                    multiplier: DecimalU64(anchor),
                    unknown: DecimalU64(unknown),
                },
            )]),
            ..Default::default()
        }
    }

    fn miniature_source() -> SourceDocument {
        let nr = NrDocument {
            bitmask_carriers: vec!["BETA".into(), "EMPTY_LEGACY".into(), "ALPHA".into()],
            bitmask_fingerprints: vec![BitmaskFingerprint {
                fingerprint: 715_188_856,
                carriers: vec!["EMPTY_LEGACY".into(), "ALPHA".into(), "BETA".into()],
            }],
            carriers: BTreeMap::from([
                (
                    "ALPHA".into(),
                    CarrierSource {
                        bitmask_id: Some(1),
                        ..carrier_with_profile(
                            7,
                            Some(7),
                            11,
                            TARGET_ANCHOR,
                            71,
                            Some(vec!["250-01".into(), "250-01".into()]),
                        )
                    },
                ),
                (
                    "BETA".into(),
                    CarrierSource {
                        bitmask_id: Some(2),
                        ..carrier_with_profile(
                            8,
                            Some(8),
                            13,
                            SYNTHETIC_ANCHOR,
                            81,
                            Some(Vec::new()),
                        )
                    },
                ),
                ("EMPTY_LEGACY".into(), CarrierSource::default()),
                (
                    "MAP_ONLY".into(),
                    CarrierSource {
                        mapping_id: Some(5),
                        plmns: Some(vec!["302-220".into(), "302-220".into()]),
                        ..Default::default()
                    },
                ),
                (
                    "NO_COMBOS".into(),
                    carrier_with_profile(9, None, 17, TARGET_ANCHOR, 91, None),
                ),
            ]),
            dl_features: vec![],
            ul_features: vec![],
            combo: vec![
                nr_combo(1, &["ALPHA", "BETA"], &["legacy"]),
                nr_combo(2, &["ALPHA"], &[TARGET_MODEL]),
                nr_combo(3, &["BETA"], &["prime:8969"]),
            ],
        };
        let lte = LteDocument {
            files: BTreeMap::from([(
                TARGET_LTE_ID.to_string(),
                LteFileSource {
                    fingerprint: 123,
                    bitmask: 456,
                },
            )]),
            combo: vec![LteSourceCombo {
                selection: selection(&[], &[TARGET_MODEL]).map(|mut rectangles| {
                    rectangles[0].carriers = None;
                    rectangles
                }),
                bcs: Some(0),
                unknown1: Some(0),
                unknown2: Some(0),
                components: vec![LteComponent {
                    band: 1,
                    dl_bw_class_mimo: 32_769,
                    ul_bw_class_mimo: Some(0),
                }],
            }],
        };
        SourceDocument { nr, lte }
    }

    fn validated_sources() -> ValidatedSources {
        parse_sources(&source_text()).unwrap()
    }

    /// Write the canonical miniature source into `dir` and return the file it landed in. The
    /// basename comes from `tempfile`: no crate constant names the source document, so no test
    /// should either.
    fn write_sources(dir: &std::path::Path) -> NamedTempFile {
        let file = NamedTempFile::new_in(dir).unwrap();
        fs::write(file.path(), source_text()).unwrap();
        file
    }

    fn source_text() -> String {
        to_kdl(&miniature_source()).unwrap()
    }

    fn assert_provision_prewrite_failure(source_kdl: String, model: &str, expected: &str) {
        let temp = tempdir().unwrap();
        let source = NamedTempFile::new_in(temp.path()).unwrap();
        fs::write(source.path(), source_kdl).unwrap();
        let output = temp.path().join("module.zip");
        fs::write(&output, b"existing module bytes").unwrap();
        let before = directory_names(temp.path());

        let error = provision(model, source.path(), &output, None).unwrap_err();
        let error = format!("{error:#}");
        assert!(error.contains(expected), "unexpected error: {error}");
        assert_eq!(fs::read(&output).unwrap(), b"existing module bytes");
        assert_eq!(directory_names(temp.path()), before);
    }

    fn directory_names(dir: &std::path::Path) -> Vec<String> {
        let mut names = fs::read_dir(dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().into_string().unwrap())
            .collect::<Vec<_>>();
        names.sort_unstable();
        names
    }

    fn rename_carrier(nr: &mut NrDocument, old: &str, new: &str) {
        for carrier in &mut nr.bitmask_carriers {
            if carrier == old {
                *carrier = new.into();
            }
        }
        for group in &mut nr.bitmask_fingerprints {
            for carrier in &mut group.carriers {
                if carrier == old {
                    *carrier = new.into();
                }
            }
        }
        let source = nr.carriers.remove(old).unwrap();
        nr.carriers.insert(new.into(), source);
        for combo in &mut nr.combo {
            for rectangle in combo.selection.iter_mut().flatten() {
                for carrier in rectangle.carriers.iter_mut().flatten() {
                    if carrier == old {
                        *carrier = new.into();
                    }
                }
            }
        }
    }

    fn assert_replacement_zip(zip: &[u8], basenames: &[String]) {
        assert!(
            basenames.windows(2).all(|pair| pair[0] < pair[1]),
            "generated carrier basenames must be strictly sorted"
        );
        let mut archive = ZipArchive::new(std::io::Cursor::new(zip)).unwrap();
        let mut expected_names = vec![
            "module.prop".to_string(),
            "META-INF/com/google/android/update-binary".to_string(),
            "META-INF/com/google/android/updater-script".to_string(),
            "system/vendor/firmware/uecapconfig/.replace".to_string(),
        ];
        expected_names.extend(
            basenames
                .iter()
                .map(|basename| format!("system/vendor/firmware/uecapconfig/{basename}")),
        );

        assert_eq!(archive.len(), expected_names.len());
        for (index, expected_name) in expected_names.iter().enumerate() {
            let mut entry = archive.by_index(index).unwrap();
            assert_eq!(entry.name(), expected_name, "archive entry {index}");
            assert_eq!(
                entry.last_modified(),
                Some(DateTime::default()),
                "{expected_name}"
            );
            assert_eq!(
                entry.unix_mode().map(|mode| mode & 0o777),
                Some(if index == 1 { 0o755 } else { 0o644 }),
                "{expected_name}"
            );
            assert_eq!(
                entry.compression(),
                CompressionMethod::Deflated,
                "{expected_name}"
            );

            let mut bytes = Vec::new();
            entry.read_to_end(&mut bytes).unwrap();
            match index {
                0 => {
                    let text = String::from_utf8(bytes).unwrap();
                    assert!(text.contains("name=Hermetic round trip\n"), "{text}");
                    assert!(text.contains(&basenames.join(", ")), "{text}");
                }
                1 => assert_eq!(bytes, include_bytes!("../magisk/assets/update-binary")),
                2 => assert_eq!(bytes, b"#MAGISK\n"),
                3 => assert!(bytes.is_empty()),
                _ => assert!(!bytes.is_empty(), "{expected_name}"),
            }
        }
    }

    #[test]
    fn full_two_generation_round_trip_is_source_and_zip_byte_deterministic() {
        let first = tempdir().unwrap();
        let second = tempdir().unwrap();
        let (first_bitmask, first_profiled) = MiniCorpus::new().write_to(first.path(), false);
        let (second_bitmask, second_profiled) = MiniCorpus::new().write_to(second.path(), true);
        let first_source = NamedTempFile::new_in(first.path()).unwrap();
        let second_source = NamedTempFile::new_in(second.path()).unwrap();
        for source in [&first_source, &second_source] {
            fs::write(source.path(), b"old source").unwrap();
        }

        decompose(&first_bitmask, &first_profiled, Some(first_source.path())).unwrap();
        decompose(
            &second_bitmask,
            &second_profiled,
            Some(second_source.path()),
        )
        .unwrap();

        let first_text = fs::read_to_string(first_source.path()).unwrap();
        assert_eq!(
            first_text,
            fs::read_to_string(second_source.path()).unwrap()
        );
        assert_eq!(
            to_kdl(&source_from_kdl(&first_text).unwrap()).unwrap(),
            first_text
        );

        let first_legacy = first.path().join("legacy.zip");
        let second_legacy = second.path().join("legacy.zip");
        fs::write(&first_legacy, b"old legacy module").unwrap();
        fs::write(&second_legacy, b"old legacy module").unwrap();
        provision(
            "G0DZQ",
            first_source.path(),
            &first_legacy,
            Some("Hermetic round trip"),
        )
        .unwrap();
        provision(
            "G0DZQ",
            second_source.path(),
            &second_legacy,
            Some("Hermetic round trip"),
        )
        .unwrap();
        let first_legacy = fs::read(first_legacy).unwrap();
        assert_eq!(first_legacy, fs::read(second_legacy).unwrap());
        assert_replacement_zip(
            &first_legacy,
            &["ALPHA.binarypb".into(), "BETA.binarypb".into()],
        );

        let first_profiled_out = first.path().join("profiled.zip");
        let second_profiled_out = second.path().join("profiled.zip");
        fs::write(&first_profiled_out, b"old profiled module").unwrap();
        fs::write(&second_profiled_out, b"old profiled module").unwrap();
        provision(
            TARGET_MODEL,
            first_source.path(),
            &first_profiled_out,
            Some("Hermetic round trip"),
        )
        .unwrap();
        provision(
            TARGET_MODEL,
            second_source.path(),
            &second_profiled_out,
            Some("Hermetic round trip"),
        )
        .unwrap();
        let first_profiled_zip = fs::read(first_profiled_out).unwrap();
        assert_eq!(first_profiled_zip, fs::read(second_profiled_out).unwrap());
        assert_replacement_zip(
            &first_profiled_zip,
            &[
                format!("ALPHA_{}.binarypb", 11 * REGISTERED_ANCHOR),
                format!("BETA_{}.binarypb", 13 * REGISTERED_ANCHOR),
                "ap_plmn_mapping.binarypb".into(),
                format!("lte_{TARGET_LTE_ID}.binarypb"),
            ],
        );
    }

    #[test]
    fn legacy_generation_emits_every_bitmask_carrier_only_with_catch_all_masks() {
        let sources = validated_sources();
        let files = generate_files(&sources, phone_model("G0DZQ").unwrap()).unwrap();

        assert_eq!(
            files
                .iter()
                .map(|file| file.basename.as_str())
                .collect::<Vec<_>>(),
            ["ALPHA.binarypb", "BETA.binarypb", "EMPTY_LEGACY.binarypb"]
        );
        assert!(
            files
                .iter()
                .all(|file| file.basename != "ap_plmn_mapping.binarypb"
                    && !file.basename.starts_with("lte_"))
        );
        for file in &files {
            let caps = decode_uecaps(&file.bytes, &file.basename).unwrap();
            assert!(
                build_combos(&caps)
                    .iter()
                    .all(|combo| combo.bit_mask == Some(65_535))
            );
        }
        let empty = files
            .iter()
            .find(|file| file.basename == "EMPTY_LEGACY.binarypb")
            .unwrap();
        assert!(
            crate::proto::UeCaps::decode(empty.bytes.as_slice())
                .unwrap()
                .combo_groups
                .is_empty()
        );
    }

    #[test]
    fn legacy_generation_rejects_carriers_that_are_not_bitmask_filenames() {
        for carrier in ["ap_plmn_mapping", "FOO_123", "lte_123"] {
            let mut document = miniature_source();
            rename_carrier(&mut document.nr, "EMPTY_LEGACY", carrier);
            let sources = parse_sources(&to_kdl(&document).unwrap()).unwrap();

            let error = generate_files(&sources, phone_model("G0DZQ").unwrap()).unwrap_err();
            let error = format!("{error:#}");
            assert!(error.contains(&format!("{carrier}.binarypb")), "{error}");
        }
    }

    #[test]
    fn profiled_generation_emits_target_nr_complete_mapping_and_exact_lte() {
        let sources = validated_sources();
        let files = generate_files(&sources, phone_model(TARGET_MODEL).unwrap()).unwrap();
        let expected_alpha = format!("ALPHA_{}.binarypb", 11 * TARGET_ANCHOR);
        let expected_empty = format!("NO_COMBOS_{}.binarypb", 17 * TARGET_ANCHOR);

        assert_eq!(
            files
                .iter()
                .map(|file| file.basename.as_str())
                .collect::<Vec<_>>(),
            [
                expected_alpha.as_str(),
                expected_empty.as_str(),
                "ap_plmn_mapping.binarypb",
                "lte_400907661.binarypb",
            ]
        );
        assert!(files.iter().all(|file| !file.basename.starts_with("BETA_")));

        let alpha = files
            .iter()
            .find(|file| file.basename == expected_alpha)
            .unwrap();
        let alpha = decode_uecaps(&alpha.bytes, &alpha.basename).unwrap();
        assert_eq!(alpha.version, 862_505_271);
        assert_eq!(alpha.id, Some(7));
        assert_eq!(alpha.unknown, 71);
        assert_eq!(
            build_combos(&alpha)
                .iter()
                .map(|combo| combo.bit_mask)
                .collect::<Vec<_>>(),
            [Some(0)]
        );

        let empty = files
            .iter()
            .find(|file| file.basename == expected_empty)
            .unwrap();
        let empty = decode_uecaps(&empty.bytes, &empty.basename).unwrap();
        assert_eq!(empty.version, 862_505_271);
        assert_eq!(empty.id, Some(9));
        assert_eq!(empty.unknown, 91);
        assert!(empty.combo_groups.is_empty());

        let mapping = files
            .iter()
            .find(|file| file.basename == "ap_plmn_mapping.binarypb")
            .unwrap();
        let mapping = decode_plmn_map(&mapping.bytes, &mapping.basename).unwrap();
        assert_eq!(
            mapping
                .carriers
                .iter()
                .map(|carrier| (
                    carrier.index,
                    carrier.name.as_str(),
                    carrier.plmns.as_slice()
                ))
                .collect::<Vec<_>>(),
            [
                (5, "MAP_ONLY", &[197_154, 197_154][..]),
                (7, "ALPHA", &[5_435_408, 5_435_408][..]),
                (8, "BETA", &[][..]),
            ]
        );
        assert!(
            mapping
                .carriers
                .iter()
                .all(|carrier| carrier.name != "NO_COMBOS")
        );

        let lte = files
            .iter()
            .find(|file| file.basename == "lte_400907661.binarypb")
            .unwrap();
        let lte = decode_lte_caps(&lte.bytes, &lte.basename).unwrap();
        assert_eq!(lte.fingerprint, 123);
        assert_eq!(lte.bitmask, 456);
        assert_eq!(lte.combos.len(), 1);
        assert_eq!(lte.combos[0].bcs, Some(0));
        assert_eq!(lte.combos[0].unknown1, Some(0));
        assert_eq!(lte.combos[0].unknown2, Some(0));
        assert_eq!(lte.combos[0].components[0].ul_bw_class_mimo, Some(0));
    }

    #[test]
    fn profiled_generation_rejects_carriers_that_parse_as_lte_filenames() {
        for carrier in ["lte", "lte_PRIVATE"] {
            let mut document = miniature_source();
            rename_carrier(&mut document.nr, "NO_COMBOS", carrier);
            let sources = parse_sources(&to_kdl(&document).unwrap()).unwrap();

            let error = generate_files(&sources, phone_model(TARGET_MODEL).unwrap()).unwrap_err();
            let error = format!("{error:#}");
            assert!(error.contains(&format!("{carrier}_")), "{error}");
        }
    }

    #[test]
    fn classifier_validation_ignores_mapping_only_and_unselected_carriers() {
        let mut document = miniature_source();
        rename_carrier(&mut document.nr, "MAP_ONLY", "ap_plmn_mapping");
        rename_carrier(&mut document.nr, "BETA", "lte_NOT_SELECTED");
        let sources = parse_sources(&to_kdl(&document).unwrap()).unwrap();

        let files = generate_files(&sources, phone_model(TARGET_MODEL).unwrap()).unwrap();
        let mapping = files
            .iter()
            .find(|file| file.basename == "ap_plmn_mapping.binarypb")
            .unwrap();
        let mapping = decode_plmn_map(&mapping.bytes, &mapping.basename).unwrap();
        assert!(
            mapping
                .carriers
                .iter()
                .any(|carrier| carrier.name == "ap_plmn_mapping")
        );
        assert!(
            mapping
                .carriers
                .iter()
                .any(|carrier| carrier.name == "lte_NOT_SELECTED")
        );
    }

    #[test]
    fn the_source_is_fully_loaded_and_validated_before_model_resolution() {
        let temp = tempdir().unwrap();
        let source = NamedTempFile::new_in(temp.path()).unwrap();
        fs::write(source.path(), "version 1\nu 1\n").unwrap();

        let error = load_and_generate(source.path(), "NOT-A-MODEL").unwrap_err();
        let error = format!("{error:#}");
        assert!(error.contains("parsing the source document"), "{error}");
        assert!(!error.contains("unknown model"), "{error}");
    }

    #[test]
    fn source_loader_requires_a_present_utf8_strict_document() {
        let temp = tempdir().unwrap();
        let source = write_sources(temp.path());
        assert!(load_sources(source.path()).is_ok());

        fs::remove_file(source.path()).unwrap();
        let error = load_sources(source.path()).unwrap_err().to_string();
        assert!(
            error.contains(&source.path().display().to_string()),
            "{error}"
        );

        fs::write(source.path(), [0xff]).unwrap();
        let error = load_sources(source.path()).unwrap_err().to_string();
        assert!(
            error.contains("UTF-8") && error.contains(&source.path().display().to_string()),
            "{error}"
        );
    }

    #[test]
    fn unknown_model_error_lists_only_registered_real_codes() {
        let temp = tempdir().unwrap();
        let source = write_sources(temp.path());

        for token in ["NOT-A-MODEL", "legacy", "prime:66813533", "lte:400907661"] {
            let error = load_and_generate(source.path(), token)
                .unwrap_err()
                .to_string();
            assert!(error.contains("unknown model"), "{error}");
            for code in known_model_codes() {
                assert!(error.contains(code), "missing {code} from {error}");
            }
            assert!(!error.contains("legacy"), "{error}");
            assert!(!error.contains("prime:"), "{error}");
            assert!(!error.contains("lte:"), "{error}");
        }
    }

    #[test]
    fn final_file_set_is_sorted_and_rejects_duplicate_or_unsafe_basenames() {
        let sorted = finalize_files(vec![
            GeneratedFile {
                basename: "B.binarypb".into(),
                bytes: Vec::new(),
            },
            GeneratedFile {
                basename: "A.binarypb".into(),
                bytes: Vec::new(),
            },
        ])
        .unwrap();
        assert_eq!(sorted[0].basename, "A.binarypb");

        for files in [
            vec![
                GeneratedFile {
                    basename: "A.binarypb".into(),
                    bytes: Vec::new(),
                },
                GeneratedFile {
                    basename: "A.binarypb".into(),
                    bytes: Vec::new(),
                },
            ],
            vec![GeneratedFile {
                basename: "../A.binarypb".into(),
                bytes: Vec::new(),
            }],
            vec![GeneratedFile {
                basename: r"dir\A.binarypb".into(),
                bytes: Vec::new(),
            }],
        ] {
            assert!(finalize_files(files).is_err());
        }
    }

    #[test]
    fn final_file_set_rejects_control_and_unicode_line_separator_basenames() {
        for character in ['\0', '\n', '\r', '\u{2028}', '\u{2029}'] {
            let basename = format!("BAD{character}NAME.binarypb");
            let error = finalize_files(vec![GeneratedFile {
                basename,
                bytes: Vec::new(),
            }])
            .unwrap_err();
            let error = format!("{error:#}");
            assert!(error.contains("control or line-separator"), "{error:?}");
        }
    }

    #[test]
    fn generated_mapping_uses_unpacked_plmn_fields() {
        let sources = validated_sources();
        let files = generate_files(&sources, phone_model(TARGET_MODEL).unwrap()).unwrap();
        let mapping = files
            .iter()
            .find(|file| file.basename == "ap_plmn_mapping.binarypb")
            .unwrap();

        let intended = PlmnMap::decode(mapping.bytes.as_slice()).unwrap();
        assert_eq!(intended.carriers[0].plmns.len(), 2);
        decode_plmn_map(&mapping.bytes, &mapping.basename).unwrap();
    }

    #[test]
    fn provision_writes_closed_replacement_zip_with_default_model_name() {
        let temp = tempdir().unwrap();
        let source = write_sources(temp.path());
        let output = temp.path().join("module.zip");

        assert_eq!(
            provision(TARGET_MODEL, source.path(), &output, None).unwrap(),
            Outcome::Clean
        );

        let bytes = fs::read(&output).unwrap();
        let mut archive = ZipArchive::new(std::io::Cursor::new(bytes)).unwrap();
        let names = (0..archive.len())
            .map(|index| archive.by_index(index).unwrap().name().to_string())
            .collect::<Vec<_>>();
        assert!(names.contains(&"system/vendor/firmware/uecapconfig/.replace".to_string()));
        let mut module_prop = String::new();
        archive
            .by_name("module.prop")
            .unwrap()
            .read_to_string(&mut module_prop)
            .unwrap();
        assert!(
            module_prop.contains("name=Pixel UE-caps: G2YBB\n"),
            "{module_prop}"
        );
    }

    #[test]
    fn provision_failures_preserve_existing_zip_without_temporary_sibling() {
        let temp = tempdir().unwrap();
        let source = write_sources(temp.path());
        let output = temp.path().join("module.zip");
        fs::write(&output, b"original zip bytes").unwrap();
        let original_names = directory_names(temp.path());

        fs::write(source.path(), "version 1\nu 1\n").unwrap();
        let error = provision("NOT-A-MODEL", source.path(), &output, None).unwrap_err();
        assert!(
            format!("{error:#}").contains("parsing the source document"),
            "{error:#}"
        );
        assert_eq!(fs::read(&output).unwrap(), b"original zip bytes");
        assert_eq!(directory_names(temp.path()), original_names);

        fs::write(source.path(), source_text()).unwrap();
        let error = provision("NOT-A-MODEL", source.path(), &output, None).unwrap_err();
        assert!(error.to_string().contains("unknown model"), "{error:#}");
        assert_eq!(fs::read(&output).unwrap(), b"original zip bytes");
        assert_eq!(directory_names(temp.path()), original_names);
    }

    #[test]
    fn source_validation_and_generation_failures_preserve_an_existing_zip() {
        let base = source_text();

        let mut missing_lte = miniature_source();
        missing_lte.lte.files = BTreeMap::from([(
            "92".into(),
            LteFileSource {
                fingerprint: 102,
                bitmask: 202,
            },
        )]);
        missing_lte.lte.combo.clear();
        assert_provision_prewrite_failure(
            to_kdl(&missing_lte).unwrap(),
            TARGET_MODEL,
            "absent from the LTE source domain",
        );

        assert_provision_prewrite_failure(
            base.replacen("pf \"66813533\"", "pf \"066813533\"", 1),
            TARGET_MODEL,
            "shortest-decimal",
        );

        let invalid_selection = base.replacen(
            "c ALPHA BETA\n        m legacy",
            "c ALPHA\n        m prime:8969",
            1,
        );
        assert_ne!(invalid_selection, base);
        assert_provision_prewrite_failure(invalid_selection, TARGET_MODEL, "empty intersection");

        // ALPHA's PLMN list is `["250-01", "250-01"]`, written as two identical
        // `plmn mcc=250 mnc=1` nodes; corrupt the first into an out-of-range MNC so it
        // fails to reconstruct into a valid PLMN.
        let invalid_plmn = base.replacen("p mcc=250 mnc=1", "p mcc=250 mnc=99999", 1);
        assert_ne!(invalid_plmn, base);
        assert_provision_prewrite_failure(invalid_plmn, TARGET_MODEL, "invalid PLMN");

        let overflow = base.replacen("sg=11", "sg=18446744073709551615", 1);
        assert_provision_prewrite_failure(overflow, TARGET_MODEL, "filename product overflow");

        let mut too_many_features = miniature_source();
        too_many_features.nr.dl_features = (1..=256)
            .map(|max_scs| ShannonFeatureSetDlPerCcNr {
                max_scs: Some(max_scs),
                ..Default::default()
            })
            .collect();
        too_many_features.nr.combo = (1..=256)
            .map(|max_scs| {
                let mut combo = nr_combo(1, &["ALPHA"], &["legacy"]);
                let NrSourceSubBlock::Nr(cc) = &mut combo.sub_blocks[0] else {
                    panic!("nr_combo builds an `nr` sub-block")
                };
                cc.dl_feature = vec![max_scs as usize];
                combo
            })
            .collect();
        assert_provision_prewrite_failure(
            to_kdl(&too_many_features).unwrap(),
            "G0DZQ",
            "uses 256 distinct DL feature records; local limit is 255",
        );
    }

    #[test]
    fn escaped_control_carrier_preserves_existing_zip_without_temporary_sibling() {
        let temp = tempdir().unwrap();
        let source = NamedTempFile::new_in(temp.path()).unwrap();
        let mut document = miniature_source();
        rename_carrier(&mut document.nr, "ALPHA", "BAD\nNAME");
        let text = to_kdl(&document).unwrap();
        assert!(text.contains(r#"BAD\nNAME"#), "{text}");
        fs::write(source.path(), text).unwrap();

        let output = temp.path().join("module.zip");
        fs::write(&output, b"original zip bytes").unwrap();
        let original_names = directory_names(temp.path());

        let error = provision("G0DZQ", source.path(), &output, None).unwrap_err();
        assert!(
            format!("{error:#}").contains("control or line-separator"),
            "{error:#}"
        );
        assert_eq!(fs::read(&output).unwrap(), b"original zip bytes");
        assert_eq!(directory_names(temp.path()), original_names);
    }
}
