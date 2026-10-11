use super::{
    lte::{DecodedLteFile, IngestedLte, generate_lte_file, ingest_lte},
    nr::{LegacyNrFile, NrTarget, ProfiledNrFile, generate_nr_files, ingest_nr},
    schema::{
        SourceDocument, ValidatedLte, ValidatedNr, ValidatedSources, legend_root, parse_sources,
        validate_documents,
    },
    selection::Sku,
};
use crate::{
    atomic::write_bytes_atomic,
    magisk::validate_module_basename,
    mapping::{MappingRoot, map_to_root, root_to_map},
    model::{MAPPING_BASENAME, MAPPING_STEM},
    outcome::Outcome,
    proto::PlmnMap,
    wire::{decode_plmn_map, decode_uecaps},
};
use anyhow::{Context, ensure};
use prost::Message;
use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::OsStr,
    fs,
    path::{Path, PathBuf},
};

#[derive(Clone, Debug, PartialEq, Eq)]
enum ProfiledInputName {
    Carrier(CarrierInput),
    Mapping,
    Lte { id: u64 },
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct CarrierInput {
    carrier: String,
    number: u64,
}

struct ProfiledInputs {
    mapping: ClassifiedFile<()>,
    carriers: Vec<ClassifiedFile<CarrierInput>>,
    lte: Vec<ClassifiedFile<u64>>,
}

#[derive(Clone, Debug)]
struct ClassifiedFile<T> {
    basename: String,
    path: PathBuf,
    kind: T,
}

/// Original file bytes keyed by basename — what the NR self-checks compare regenerated output
/// against, so a tie to the actual input folder exists rather than a re-derivation from the
/// same source field the generator reads.
type OriginalNrBytes = BTreeMap<String, Vec<u8>>;

struct OriginalInputs {
    mapping: Vec<u8>,
    lte: BTreeMap<u64, Vec<u8>>,
    legacy_nr: OriginalNrBytes,
    profiled_nr: OriginalNrBytes,
}

/// Every bitmask-folder file, decoded into its `LegacyNrFile` capture, plus each file's
/// original bytes keyed by basename so `verify_legacy_nr_target` can check its regenerated file
/// set against what the folder actually held.
fn decode_legacy_files(
    files: Vec<ClassifiedFile<String>>,
) -> anyhow::Result<(Vec<LegacyNrFile>, OriginalNrBytes)> {
    let mut legacy = Vec::with_capacity(files.len());
    let mut original = BTreeMap::new();
    for file in files {
        let carrier = file.kind;
        let bytes = read_file(&file.path, &file.basename)?;
        let caps = decode_uecaps(&bytes, &file.basename)?;
        original.insert(file.basename, bytes);
        legacy.push(LegacyNrFile { carrier, caps });
    }
    Ok((legacy, original))
}

/// The cross-file accumulations one pass over the profiled folder builds: every NR carrier
/// capture, every LTE capture, the single PLMN mapping (both its parsed root and original
/// bytes). LTE captures retain their own original bytes until ingest verification finishes.
struct ProfiledDecode {
    nr: Vec<ProfiledNrFile>,
    lte: Vec<DecodedLteFile>,
    mapping: MappingRoot,
    original_mapping: Vec<u8>,
    /// Original NR files by basename, for `verify_profiled_nr_targets`' file-set comparison.
    original_nr: OriginalNrBytes,
}

enum ProfiledWork {
    Carrier(ClassifiedFile<CarrierInput>),
    Lte(ClassifiedFile<u64>),
}

impl ProfiledWork {
    fn basename(&self) -> &str {
        match self {
            Self::Carrier(file) => &file.basename,
            Self::Lte(file) => &file.basename,
        }
    }
}

#[derive(Default)]
struct ProfiledCaptures {
    nr: Vec<ProfiledNrFile>,
    lte: Vec<DecodedLteFile>,
    original_nr: OriginalNrBytes,
    seen_lte: BTreeSet<u64>,
}

impl ProfiledCaptures {
    fn decode(&mut self, files: Vec<ProfiledWork>) -> anyhow::Result<()> {
        for file in files {
            match file {
                ProfiledWork::Carrier(file) => {
                    let bytes = read_file(&file.path, &file.basename)?;
                    self.nr.push(ProfiledNrFile {
                        carrier: file.kind.carrier,
                        number: file.kind.number,
                        caps: decode_uecaps(&bytes, &file.basename)?,
                    });
                    self.original_nr.insert(file.basename, bytes);
                }
                ProfiledWork::Lte(file) => {
                    let bytes = read_file(&file.path, &file.basename)?;
                    let decoded = DecodedLteFile::decode(file.kind, bytes)?;
                    let id = decoded.id();
                    ensure!(self.seen_lte.insert(id), "duplicate LTE file ID {id}");
                    self.lte.push(decoded);
                }
            }
        }
        Ok(())
    }
}

/// Decodes every classified profiled-folder file into its role: an NR carrier capture, the
/// single PLMN mapping (validated and order-checked here, since a stale-order profiled mapping
/// must be rejected before it reaches `ingest_nr`), or a per-id LTE capture. Exactly one
/// mapping file is present structurally. Work on either side retains basename error order.
fn decode_profiled_files(files: ProfiledInputs) -> anyhow::Result<ProfiledDecode> {
    let mut work: Vec<_> = files
        .carriers
        .into_iter()
        .map(ProfiledWork::Carrier)
        .chain(files.lte.into_iter().map(ProfiledWork::Lte))
        .collect();
    work.sort_by(|left, right| left.basename().cmp(right.basename()));
    let later = work
        .split_off(work.partition_point(|file| file.basename() < files.mapping.basename.as_str()));
    let mut captures = ProfiledCaptures::default();
    captures.decode(work)?;

    let original_mapping = read_file(&files.mapping.path, &files.mapping.basename)?;
    let decoded = decode_plmn_map(&original_mapping, &files.mapping.basename)?;
    let mapping = map_to_root(&decoded).context("decoding profiled PLMN mapping")?;
    root_to_map(&mapping).context("validating profiled PLMN mapping IDs and names")?;
    ensure_mapping_order(&decoded)?;

    captures.decode(later)?;
    Ok(ProfiledDecode {
        nr: captures.nr,
        lte: captures.lte,
        mapping,
        original_mapping,
        original_nr: captures.original_nr,
    })
}

/// Decode one complete legacy bitmask folder and one complete profiled folder into the canonical
/// compiler source document. No output path is touched by this pure orchestration seam.
pub(crate) fn decode_documents(bitmask_dir: &Path, profiled_dir: &Path) -> anyhow::Result<String> {
    let bitmask_files = classify_bitmask_dir(bitmask_dir)?;
    let profiled_files = classify_profiled_dir(profiled_dir)?;

    let (legacy, original_legacy_nr) = decode_legacy_files(bitmask_files)?;
    let ProfiledDecode {
        nr: profiled,
        lte: lte_files,
        mapping,
        original_mapping,
        original_nr: original_profiled_nr,
    } = decode_profiled_files(profiled_files)?;

    let nr = ingest_nr(legacy, profiled, &mapping).context("normalizing NR carrier files")?;
    let IngestedLte {
        source: lte,
        originals: original_lte,
    } = ingest_lte(lte_files).context("normalizing LTE files")?;
    let originals = OriginalInputs {
        mapping: original_mapping,
        lte: original_lte,
        legacy_nr: original_legacy_nr,
        profiled_nr: original_profiled_nr,
    };

    // Serialization is itself a validation boundary. Canonicalize the ingest once, reparse the
    // emitted source through the strict public schema, and require byte-idempotence before using
    // the validated representation for every internal generation self-check. The reparse's
    // `validated.to_kdl()` serializes an already-canonical source, so no third `validate_documents`
    // pass is needed — the assertion below still proves the emitted document is a fixed point.
    let text = validate_documents(SourceDocument { nr, lte })?.to_kdl()?;
    let validated = parse_sources(&text).context("reparsing the decoded source")?;
    let canonical = validated.to_kdl()?;
    ensure!(
        canonical.as_bytes() == text.as_bytes(),
        "the source document changed when reparsed and reserialized"
    );

    verify_internal_targets(&validated, &originals)?;
    // `text` is the canonical document already validated above (reparse + reserialize
    // byte-idempotent). Return it so `decompose` need not recompute `to_kdl`.
    Ok(text)
}

/// Decompose both required folders and emit the canonical source document — atomically replacing
/// `out` when given, or on stdout when not. Validation, encoding, and self-verification all
/// finish before either happens.
pub fn decompose(
    bitmask_dir: &Path,
    profiled_dir: &Path,
    out: Option<&Path>,
) -> anyhow::Result<Outcome> {
    let text = decode_documents(bitmask_dir, profiled_dir)?;
    match out {
        // Through the crate's atomic writer like every other file output, so a failure part-way
        // cannot leave a truncated document in place of a previously good one, and replacing an
        // existing file keeps its mode.
        Some(path) => write_bytes_atomic(path, text.as_bytes())
            .with_context(|| format!("cannot write {}", path.display()))?,
        None => print!("{text}"),
    }
    Ok(Outcome::Clean)
}

fn classify_bitmask_dir(dir: &Path) -> anyhow::Result<Vec<ClassifiedFile<String>>> {
    let files = classify_directory(dir, "bitmask", classify_bitmask_name)?;
    ensure!(
        !files.is_empty(),
        "bitmask input must contain at least one unnumbered carrier .binarypb file"
    );
    Ok(files)
}

fn classify_profiled_dir(dir: &Path) -> anyhow::Result<ProfiledInputs> {
    let files = classify_directory(dir, "profiled", classify_profiled_name)?;
    let mut mappings = Vec::new();
    let mut lte = Vec::new();
    let mut carriers = Vec::new();
    for ClassifiedFile {
        basename,
        path,
        kind,
    } in files
    {
        match kind {
            ProfiledInputName::Mapping => mappings.push(ClassifiedFile {
                basename,
                path,
                kind: (),
            }),
            ProfiledInputName::Lte { id } => lte.push(ClassifiedFile {
                basename,
                path,
                kind: id,
            }),
            ProfiledInputName::Carrier(kind) => carriers.push(ClassifiedFile {
                basename,
                path,
                kind,
            }),
        }
    }
    let count = mappings.len();
    let [mapping] = <[_; 1]>::try_from(mappings).map_err(|_| {
        anyhow::anyhow!(
            "profiled input must contain exactly one {MAPPING_BASENAME} (found {count})"
        )
    })?;
    ensure!(
        !lte.is_empty(),
        "profiled input must contain at least one lte_<id>.binarypb file"
    );
    ensure!(
        !carriers.is_empty(),
        "profiled input must contain at least one numbered carrier .binarypb file"
    );
    Ok(ProfiledInputs {
        mapping,
        carriers,
        lte,
    })
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

/// A bitmask-folder basename is just its carrier name — there is only one shape, unlike the
/// profiled folder's three ([`ProfiledInputName`]).
fn classify_bitmask_name(basename: &str) -> anyhow::Result<String> {
    let stem = basename
        .strip_suffix(".binarypb")
        .with_context(|| format!("bitmask basename `{basename}` must end with .binarypb"))?;
    ensure!(
        !stem.is_empty(),
        "unsupported bitmask .binarypb filename `{basename}`: carrier name must be nonempty"
    );
    ensure!(
        stem != MAPPING_STEM,
        "unsupported cross-layout file `{basename}` in bitmask input"
    );
    if let Some((_, suffix)) = stem.rsplit_once('_')
        && !suffix.is_empty()
        && suffix.bytes().all(|byte| byte.is_ascii_digit())
    {
        anyhow::bail!("unsupported numbered file `{basename}` in bitmask input");
    }
    Ok(stem.into())
}

/// Require a generated legacy NR basename to classify exactly as a bitmask carrier input.
pub(super) fn validate_bitmask_carrier_basename(basename: &str) -> anyhow::Result<()> {
    classify_bitmask_name(basename).map(drop)
}

fn classify_profiled_name(basename: &str) -> anyhow::Result<ProfiledInputName> {
    if basename == MAPPING_BASENAME {
        return Ok(ProfiledInputName::Mapping);
    }
    let stem = basename
        .strip_suffix(".binarypb")
        .with_context(|| format!("profiled basename `{basename}` must end with .binarypb"))?;
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
    Ok(ProfiledInputName::Carrier(CarrierInput {
        carrier: carrier.into(),
        number,
    }))
}

/// Require a generated modern NR basename to classify as a profiled carrier rather than the
/// reserved mapping or LTE forms.
pub(super) fn validate_profiled_carrier_basename(basename: &str) -> anyhow::Result<()> {
    match classify_profiled_name(basename)? {
        ProfiledInputName::Carrier(_) => Ok(()),
        ProfiledInputName::Mapping => {
            anyhow::bail!("generated profiled NR basename `{basename}` is reserved for the mapping")
        }
        ProfiledInputName::Lte { .. } => {
            anyhow::bail!("generated profiled NR basename `{basename}` classifies as LTE")
        }
    }
}

fn parse_filename_number(decimal: &str, basename: &str, field: &str) -> anyhow::Result<u64> {
    // Two arms, because "does not fit u64" is only true of one of them. The single arm this
    // replaces fired for *any* unparseable text, so `lte_bad.binarypb` reported the ID `bad`
    // as not fitting u64 and `CARRIER_.binarypb` said the same of the empty string. That
    // wording is the stated reason CONTRIBUTING gives for keeping this two-step form over an
    // `Option` primitive — "a distinct, tested `does not fit u64` vs `must be shortest decimal`
    // message" — so it has to actually be distinct.
    ensure!(
        !decimal.is_empty() && decimal.bytes().all(|b| b.is_ascii_digit()),
        "profiled filename `{basename}` {field} `{decimal}` is not a decimal number"
    );
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

/// Self-verifies the legacy NR target: regenerating it must reproduce exactly the file set the
/// bitmask folder held, and every regenerated file must decode.
///
/// **Not** a byte comparison, unlike every other target here, and the reason is worth stating
/// because it is the one place where byte-identity genuinely does not apply. The legacy layout
/// is deliberately *normalized* rather than round-tripped: `ingest_nr` discards each combo's
/// input bitmask and merges the duplicates that discard creates, and `generate_legacy_files`
/// re-emits every combo with the all-ones sentinel (`InputLayout::Legacy::bitmask()` == 65535).
/// Real bitmask-folder files carry ~150 distinct mask values and never 65535, so requiring
/// byte-identity here would fail on every genuine input. The authoritative per-SKU selection
/// lives in the profiled folder, which `verify_profiled_nr_targets` does check byte-for-byte.
///
/// What this fixes is the tautology: the file set used to be compared against a list built by
/// mapping the same `nr.source.bitmask_carriers` field `generate_legacy_files` iterates, through
/// the same `format!`, then sorted the same way — a re-derivation that could not disagree. It is
/// now compared against the basenames the folder actually contained, so an `ingest_nr` that
/// dropped or renamed a carrier fails here.
fn verify_legacy_nr_target(
    sources: &ValidatedNr,
    original_nr: &OriginalNrBytes,
) -> anyhow::Result<()> {
    let legacy = generate_nr_files(sources, NrTarget::Legacy)
        .context("self-verifying the legacy NR target")?;
    let produced = legacy
        .iter()
        .map(|file| file.basename.as_str())
        .collect::<BTreeSet<_>>();
    let present = original_nr
        .keys()
        .map(String::as_str)
        .collect::<BTreeSet<_>>();
    ensure!(
        produced == present,
        "legacy NR target regenerated {produced:?}, but the bitmask folder contained {present:?}"
    );
    for file in &legacy {
        decode_uecaps(&file.bytes, &format!("generated {}", file.basename))?;
    }
    Ok(())
}

/// Self-verifies every profiled NR anchor: the union of every anchor's regenerated output must
/// be exactly the file set the profiled folder held, and every regenerated file must decode.
///
/// Like its legacy twin, this used to compare basenames against a list re-derived from the same
/// `profile.number` the generator already `ensure!`s equal, so it could not fail. It is now
/// compared against the basenames the folder actually contained.
///
/// Also like its legacy twin, it is **not** a byte comparison, and the reason is measured
/// rather than assumed: regenerating a real profiled file does not reproduce its input bytes.
/// `1_1_DE_3379443364558429875.binarypb` comes back 390 bytes shorter, and at the first
/// difference the regenerated combo begins with E-UTRA band 1 where the original begins with
/// NR band 10001 — because ingest sorts each combo's sub-blocks by `RawSubBlockKey` and
/// deduplicates combos, while local generation filters and renumbers the feature catalogs. Real
/// input is simply not in that canonical form. NR is a *normalizing* pipeline, not a
/// round-tripping one; byte-identity holds only for LTE (`verify_lte_targets`) and the PLMN
/// mapping, both of which are checked that way here.
fn verify_profiled_nr_targets(
    sources: &ValidatedNr,
    original_nr: &OriginalNrBytes,
) -> anyhow::Result<()> {
    let anchors = sources
        .carriers
        .values()
        .flat_map(|carrier| {
            carrier
                .profiled
                .iter()
                .flat_map(|p| p.profiles.keys().copied())
        })
        .collect::<BTreeSet<_>>();
    let mut regenerated = BTreeSet::new();
    for anchor in anchors {
        let sku = Sku::representative_for_nr_anchor(anchor);
        let generated = generate_nr_files(sources, NrTarget::Profile { anchor, sku })
            .with_context(|| format!("self-verifying NR profile anchor {anchor}"))?;
        for file in &generated {
            ensure!(
                original_nr.contains_key(&file.basename),
                "NR profile anchor {anchor} generated {}, which the profiled folder did not \
                 contain",
                file.basename
            );
            decode_uecaps(&file.bytes, &format!("generated {}", file.basename))?;
            regenerated.insert(file.basename.clone());
        }
    }
    // Completeness across all anchors together: no profiled input may go unregenerated.
    for basename in original_nr.keys() {
        ensure!(
            regenerated.contains(basename),
            "no NR profile anchor regenerated {basename}, which the input folder contained"
        );
    }
    Ok(())
}

/// Self-verifies every LTE file: regenerating it must be BYTE-IDENTICAL to the original.
/// Unlike the NR targets (file-set + decodability), LTE's self-check is the stricter one,
/// since there is no profile-anchor fan-out here to reconcile against.
fn verify_lte_targets(
    lte: &ValidatedLte,
    original_lte: &BTreeMap<u64, Vec<u8>>,
) -> anyhow::Result<()> {
    for id in lte.files.keys().copied() {
        let sku = Sku::for_lte_file(id)
            .pop_first()
            .expect("SKU expansion is nonempty");
        let generated = generate_lte_file(lte, id, &sku)
            .with_context(|| format!("self-verifying LTE file ID {id}"))?;
        let original = original_lte
            .get(&id)
            .expect("every validated LTE file retains its original bytes");
        ensure!(
            generated.bytes == *original,
            "LTE self-verification for lte_{id}.binarypb was not byte-identical"
        );
    }
    Ok(())
}

/// Runs the four independent generation round-trips that must hold before decomposed sources
/// are trusted: the legacy NR target, every profiled NR anchor, every LTE file, and the PLMN
/// mapping — each regenerated from the just-validated sources and compared against what the
/// input folders actually contained.
///
/// The two NR targets compare *file sets*; LTE and the mapping compare *bytes*. That split is
/// not laziness: NR is a normalizing pipeline (combos sorted and deduplicated at ingest, feature
/// catalogs filtered and renumbered during local generation, legacy masks replaced by the
/// all-ones sentinel), so a real input file does not regenerate to its own bytes and byte-identity
/// is not an available invariant there. See [`verify_legacy_nr_target`] and
/// [`verify_profiled_nr_targets`] for the measurements.
///
/// What changed is that the NR checks now touch the input at all. Both previously compared
/// generated basenames against a list re-derived from the very source field the generator
/// iterates, so they could not fail — an `ingest_nr` that dropped or renamed a file would have
/// passed every check here.
fn verify_internal_targets(
    sources: &ValidatedSources,
    originals: &OriginalInputs,
) -> anyhow::Result<()> {
    verify_legacy_nr_target(&sources.nr, &originals.legacy_nr)?;
    verify_profiled_nr_targets(&sources.nr, &originals.profiled_nr)?;
    verify_lte_targets(&sources.lte, &originals.lte)?;

    let rebuilt_mapping = rebuild_mapping(sources)?;
    ensure!(
        rebuilt_mapping == originals.mapping,
        "PLMN mapping round-trip self-verification was not byte-identical"
    );
    Ok(())
}

fn rebuild_mapping(sources: &ValidatedSources) -> anyhow::Result<Vec<u8>> {
    // The same id-ordered projection `provision` ships, so this self-check compares the
    // original bytes against exactly what generation would produce.
    let root = legend_root(&sources.nr.carriers);
    let mapping = root_to_map(&root).context("validating self-verification PLMN mapping")?;
    let bytes = mapping.encode_to_vec();
    let decoded = decode_plmn_map(&bytes, &format!("rebuilt {MAPPING_BASENAME}"))?;
    ensure!(
        decoded == mapping,
        "rebuilt PLMN mapping changed values during encoding"
    );
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::{decode_documents, decompose};
    use crate::{
        compiler::{
            schema::{parse_sources, to_kdl},
            source_from_kdl,
            test_support::{
                FIRST_LTE_ID, MiniCorpus, SECOND_LTE_ID, SYNTHETIC_ANCHOR, decode_lte,
                decode_mapping, decode_nr, inject_unknown_nr_cc_field,
                make_lte_encoding_noncanonical, replace_lte, replace_mapping, replace_nr,
            },
        },
        outcome::Outcome,
        proto::{
            Carrier, ComboGroup, ComboHeader, ShannonFeatureSetDlPerCcNr,
            ShannonFeatureSetUlPerCcNr,
        },
    };
    use prost::Message;
    use std::{collections::BTreeMap, fs};
    use tempfile::{NamedTempFile, tempdir};

    fn profile_name(carrier: &str, signature: u64, anchor: u64) -> String {
        format!("{carrier}_{}.binarypb", signature * anchor)
    }

    fn assert_prewrite_failure(corpus: MiniCorpus, expected: &str) -> String {
        let temp = tempdir().unwrap();
        let (bitmask, profiled) = corpus.write_to(temp.path(), false);
        let out = NamedTempFile::new_in(temp.path()).unwrap();
        fs::write(out.path(), b"old source\n").unwrap();

        let error = decompose(&bitmask, &profiled, Some(out.path())).unwrap_err();
        let error = format!("{error:#}");
        assert!(error.contains(expected), "unexpected error: {error}");
        assert_eq!(fs::read(out.path()).unwrap(), b"old source\n");
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

        let text = decode_documents(&bitmask, &profiled).unwrap();
        let source = source_from_kdl(&text).unwrap();

        assert_eq!(source.nr.bitmask_carriers, expected.bitmask_carriers);
        for (carrier, anchors) in expected.profiles {
            let actual = source.nr.carriers[&carrier]
                .profiles
                .keys()
                .copied()
                .collect::<std::collections::BTreeSet<_>>();
            assert_eq!(actual, anchors);
        }
        for (carrier, plmns) in expected.plmns {
            assert_eq!(source.nr.carriers[&carrier].plmns.as_ref(), Some(&plmns));
        }
        assert_eq!(source.nr.combos.len(), expected.nr_payloads);
        assert_eq!(
            crate::compiler::decimal_key_entries(&source.lte.files)
                .into_iter()
                .map(|(&id, _)| id)
                .collect::<Vec<_>>(),
            expected.lte_ids
        );
        assert_eq!(source.lte.combos.len(), expected.lte_payloads);
        assert_eq!(to_kdl(&source).unwrap(), expected.source_kdl);
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
    fn profiled_decode_failures_follow_basename_order_across_roles() {
        let mut corpus = MiniCorpus::new();
        let early = profile_name("ALPHA", 11, SYNTHETIC_ANCHOR);
        corpus.profiled_file_mut(&early).bytes = vec![0x80];
        mapping_file(&mut corpus).bytes = vec![0x80];
        let error = assert_prewrite_failure(corpus, &early);
        assert!(!error.contains("validating ap_plmn_mapping"), "{error}");

        let mut corpus = MiniCorpus::new();
        mapping_file(&mut corpus).bytes = vec![0x80];
        lte_file(&mut corpus, FIRST_LTE_ID).bytes = vec![0x80];
        let error = assert_prewrite_failure(corpus, "validating ap_plmn_mapping.binarypb");
        assert!(!error.contains("validating lte_"), "{error}");
    }

    #[test]
    fn basename_validators_reject_bad_suffixes_without_panicking() {
        for name in ["ALPHA", "ALPHA.binarypb.bak", "ALPHA_1.pb"] {
            for validate in [
                super::validate_bitmask_carrier_basename,
                super::validate_profiled_carrier_basename,
            ] {
                let error = format!("{:#}", validate(name).unwrap_err());
                assert!(
                    error.contains(name) && error.contains(".binarypb"),
                    "{error}"
                );
            }
        }
    }

    #[test]
    fn profiled_folder_requires_mapping_lte_and_numbered_carriers() {
        let mut missing_all = MiniCorpus::new();
        missing_all.remove_profiled(|_| true);
        assert_prewrite_failure(missing_all, "exactly one ap_plmn_mapping.binarypb");

        let mut mapping_only = MiniCorpus::new();
        mapping_only.remove_profiled(|file| file.basename != "ap_plmn_mapping.binarypb");
        assert_prewrite_failure(mapping_only, "at least one lte_<id>.binarypb");

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

        let first_out = NamedTempFile::new_in(first.path()).unwrap();
        let second_out = NamedTempFile::new_in(second.path()).unwrap();
        assert_eq!(
            decompose(&first_bitmask, &first_profiled, Some(first_out.path())).unwrap(),
            Outcome::Clean
        );
        assert_eq!(
            decompose(&second_bitmask, &second_profiled, Some(second_out.path())).unwrap(),
            Outcome::Clean
        );
        assert_eq!(
            fs::read(first_out.path()).unwrap(),
            fs::read(second_out.path()).unwrap()
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

    /// The NR self-checks must be tied to the input folder, not re-derived from the source the
    /// generator reads. Both used to compare generated basenames against a list built from the
    /// same field `generate_*_files` iterates — sorted the same way — so they passed no matter
    /// what the folder held, and an `ingest_nr` that dropped or renamed a file would have gone
    /// unnoticed.
    ///
    /// Driving that through `decode_documents` is not possible: the sources are *derived* from
    /// the folder, so under normal operation the two always agree and only an ingest bug could
    /// separate them. The checks are therefore exercised directly, against a folder listing
    /// that disagrees with the sources in each direction. Under the old implementation both
    /// calls below returned `Ok`.
    #[test]
    fn nr_self_verification_is_tied_to_the_input_folder_not_re_derived() {
        let temp = tempdir().unwrap();
        let (bitmask, profiled) = MiniCorpus::new().write_to(temp.path(), false);
        let text = decode_documents(&bitmask, &profiled).unwrap();
        let sources = parse_sources(&text).unwrap();

        // A folder that held nothing: everything the generator produces is unaccounted for.
        let empty = BTreeMap::new();
        let error = format!(
            "{:#}",
            super::verify_legacy_nr_target(&sources.nr, &empty).unwrap_err()
        );
        assert!(error.contains("bitmask folder contained"), "{error}");

        let error = format!(
            "{:#}",
            super::verify_profiled_nr_targets(&sources.nr, &empty).unwrap_err()
        );
        assert!(
            error.contains("which the profiled folder did not contain"),
            "{error}"
        );

        // The real profiled NR listing must of course pass.
        let mut actual: BTreeMap<String, Vec<u8>> = fs::read_dir(&profiled)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().into_string().unwrap())
            .filter(|name| {
                name.ends_with(".binarypb")
                    && !name.starts_with("lte_")
                    && name != "ap_plmn_mapping.binarypb"
            })
            .map(|name| (name, Vec::new()))
            .collect();
        super::verify_profiled_nr_targets(&sources.nr, &actual)
            .expect("the folder the sources were built from must verify");

        // The other direction: a folder holding a file no anchor regenerates.
        actual.insert("GHOST_12345.binarypb".to_string(), Vec::new());
        let error = format!(
            "{:#}",
            super::verify_profiled_nr_targets(&sources.nr, &actual).unwrap_err()
        );
        assert!(error.contains("GHOST_12345.binarypb"), "{error}");
    }

    #[test]
    fn decompose_writes_exactly_one_newline_terminated_idempotent_document() {
        let temp = tempdir().unwrap();
        let (bitmask, profiled) = MiniCorpus::new().write_to(temp.path(), false);
        let out = NamedTempFile::new_in(temp.path()).unwrap();

        assert_eq!(
            decompose(&bitmask, &profiled, Some(out.path())).unwrap(),
            Outcome::Clean
        );

        // Exactly one document: the two corpus folders and the output, and nothing else. This is
        // what catches a leftover `.tmpXXXX` sibling from `write_bytes_atomic`'s temp-then-rename,
        // which the reparse below would happily ignore. Compared against the output's own
        // basename, because nothing requires the source document to have a particular name.
        let mut names: Vec<String> = fs::read_dir(temp.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name().into_string().unwrap())
            .collect();
        names.sort_unstable();
        let mut expected = vec![
            "bitmask".to_owned(),
            "profiled".to_owned(),
            out.path()
                .file_name()
                .unwrap()
                .to_string_lossy()
                .into_owned(),
        ];
        expected.sort_unstable();
        assert_eq!(names, expected);

        let text = fs::read_to_string(out.path()).unwrap();
        assert!(text.ends_with('\n') && !text.ends_with("\n\n"));
        // Through the *validating* `to_kdl`, not `ValidatedSources::to_kdl`: the assertion is
        // that a second validate + canonicalize pass over the written document is a fixed point.
        assert_eq!(to_kdl(&source_from_kdl(&text).unwrap()).unwrap(), text);
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
        let out = NamedTempFile::new_in(temp.path()).unwrap();

        assert_eq!(
            decompose(&bitmask, &profiled, Some(out.path())).unwrap(),
            Outcome::Clean
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
