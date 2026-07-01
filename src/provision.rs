//! `provision` — build a flashable Magisk package for one phone.

use crate::{
    model::{PHONE_MODELS, PROFILES, Parsed, PhoneModel, fp_info, parse_name, phone_model},
    patch::{
        self,
        format::{self, Patch},
    },
    proto::{LteCaps, UeCaps},
    report::combos::{Cc, NR_BAND_OFFSET},
};
use anyhow::Context;
use pixel_bands::{Bands, PIXEL_BANDS};
use prost::Message;
use std::{io::Write, path::Path};

/// Result of choosing which on-device files to pull: the basenames to fetch, plus
/// any human-readable reasons selection could not complete. `to_pull` is empty when
/// `errors` is non-empty.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Selection {
    pub to_pull: Vec<String>,
    pub errors: Vec<String>,
}

/// Given a model code, a target carrier, and the basenames present in the device's
/// uecapconfig dir, return the LTE file (`lte_<id>.binarypb`) and the carrier's NR
/// file (`<carrier>_<n>.binarypb` with `n` divisible by the model's `nr_anchor`).
pub fn select_files(code: &str, carrier: &str, available: &[String]) -> Selection {
    let mut errors = Vec::new();

    let Some(m) = phone_model(code) else {
        errors.push(format!("unknown model {code:?}"));
        return Selection {
            to_pull: Vec::new(),
            errors,
        };
    };

    // LTE file: deterministic name, must be present.
    let lte_name = format!("lte_{}.binarypb", m.lte_id);
    if !available.contains(&lte_name) {
        errors.push(format!("LTE file {lte_name} not found on device"));
    }

    // NR file: the carrier file whose number is divisible by the model's anchor prime.
    let mut nr_matches: Vec<String> = available
        .iter()
        .filter(|n| {
            matches!(parse_name(n), Parsed::Carrier { carrier: c, number }
                if c == carrier && number.is_multiple_of(m.nr_anchor))
        })
        .cloned()
        .collect();
    nr_matches.sort();

    let nr_name = match nr_matches.as_slice() {
        [] => {
            errors.push(format!(
                "no {carrier} file for this model (anchor {})",
                m.nr_anchor
            ));
            None
        }
        [one] => Some(one.clone()),
        many => {
            errors.push(format!("ambiguous {carrier} files: {}", many.join(", ")));
            None
        }
    };

    match nr_name {
        Some(nr) if errors.is_empty() => Selection {
            to_pull: vec![lte_name, nr],
            errors,
        },
        _ => Selection {
            to_pull: Vec::new(),
            errors,
        },
    }
}

/// One Magisk-module input: its on-device basename and its bytes.
type ModuleEntry = (String, Vec<u8>);

/// Build a Magisk module for `model` from files in `dir`, including only the pieces whose
/// modifier flag is present. Returns the process exit code (0 ok / 1 patch-skipped).
#[allow(clippy::too_many_arguments)]
pub fn run(
    model: &str,
    dir: &Path,
    carrier: Option<&str>,
    lte_patch: Option<&Path>,
    nr_patch: Option<&Path>,
    add_plmn: &[String],
    out: Option<&Path>,
    dest: &str,
    name: Option<&str>,
    strict: bool,
) -> anyhow::Result<i32> {
    // 1. Validate the flag combination (clap also enforces ≥1 modifier and modifier⟹carrier).
    if lte_patch.is_none() && nr_patch.is_none() && add_plmn.is_empty() {
        anyhow::bail!("nothing to build; pass at least one of --lte-patch/--nr-patch/--add-plmn");
    }
    if (nr_patch.is_some() || !add_plmn.is_empty()) && carrier.is_none() {
        anyhow::bail!("--add-plmn/--nr-patch require --carrier");
    }
    if carrier.is_some() && nr_patch.is_none() && add_plmn.is_empty() {
        anyhow::bail!("--carrier has no effect without --add-plmn or --nr-patch");
    }

    // 2. Resolve the model.
    let m = phone_model(model)
        .ok_or_else(|| anyhow::anyhow!("unknown model {model:?}; known: {}", known_codes()))?;

    let bands = PIXEL_BANDS
        .get(m.code)
        .ok_or_else(|| anyhow::anyhow!("no band data for model {}", m.code))?;

    // Precondition: a named carrier must have files in the dump.
    if let Some(c) = carrier {
        carrier_exists(dir, c)?;
    }

    // 3. Assemble inputs (each gated on its modifier).
    let mut inputs: Vec<ModuleEntry> = Vec::new();
    let mut skipped_total = 0usize;

    if let Some(p) = lte_patch {
        let name = format!("lte_{}.binarypb", m.lte_id);
        let path = dir.join(&name);
        let bytes =
            std::fs::read(&path).with_context(|| format!("reading LTE file {}", path.display()))?;
        let text =
            std::fs::read_to_string(p).with_context(|| format!("reading patch {}", p.display()))?;
        let (entry, warnings, skipped) =
            lte_entry(name, &bytes, m, &text, "--lte-patch", strict, bands)?;
        for w in &warnings {
            eprintln!("warning: {w}");
        }
        skipped_total += skipped;
        inputs.push(entry);
    }
    if let Some(p) = nr_patch {
        let c = carrier.expect("validated: --nr-patch implies --carrier");
        // Keep the ORIGINAL dir-scan selection so the CLI's error messages are byte-for-byte
        // unchanged (existing tests assert on them). Do NOT use select_files here — that is the
        // web API's selector with different wording.
        let mut matches: Vec<(String, std::path::PathBuf)> = Vec::new();
        for entry in std::fs::read_dir(dir).with_context(|| format!("reading {}", dir.display()))? {
            let entry = entry?;
            let fname = entry.file_name();
            let Some(fname) = fname.to_str() else {
                continue;
            };
            if let Parsed::Carrier {
                carrier: cc,
                number,
            } = parse_name(fname)
                && cc == c
                && number.is_multiple_of(m.nr_anchor)
            {
                matches.push((fname.to_string(), entry.path()));
            }
        }
        let (name, path) = match matches.as_slice() {
            [] => anyhow::bail!(
                "no NR file for carrier {c} at profile {} in {}",
                m.nr_anchor,
                dir.display()
            ),
            [one] => one.clone(),
            many => {
                let names: Vec<&str> = many.iter().map(|(n, _)| n.as_str()).collect();
                anyhow::bail!(
                    "ambiguous NR files for carrier {c} at profile {}: {}",
                    m.nr_anchor,
                    names.join(", ")
                );
            }
        };
        let bytes =
            std::fs::read(&path).with_context(|| format!("reading NR file {}", path.display()))?;
        let text =
            std::fs::read_to_string(p).with_context(|| format!("reading patch {}", p.display()))?;
        let (entry, warnings, skipped) =
            nr_entry(name, &bytes, m, &text, "--nr-patch", strict, bands)?;
        for w in &warnings {
            eprintln!("warning: {w}");
        }
        skipped_total += skipped;
        inputs.push(entry);
    }
    if !add_plmn.is_empty() {
        let c = carrier.expect("validated: --add-plmn implies --carrier");
        inputs.push(legend_input(dir, c, add_plmn)?);
    }

    // 4. Package and write.
    let module_name = name.map_or_else(|| default_name(m, carrier), str::to_string);
    let zip = crate::magisk::build_module(&inputs, dest, &module_name)?;
    write_out(&zip, out)?;
    eprintln!("provisioned {} -> {} file(s)", m.display, inputs.len());
    Ok(i32::from(skipped_total > 0))
}

/// Comma-separated list of valid model codes (for the unknown-model error).
fn known_codes() -> String {
    PHONE_MODELS
        .iter()
        .map(|m| m.code)
        .collect::<Vec<_>>()
        .join(", ")
}

/// Default Magisk module name when `--name` is omitted.
fn default_name(m: &PhoneModel, carrier: Option<&str>) -> String {
    match carrier {
        Some(c) => format!("Pixel UE-caps: {}/{}", m.code, c),
        None => format!("Pixel UE-caps: {}", m.code),
    }
}

/// Apply the LTE patch to in-memory base bytes; return the module entry, warnings, and skip count.
/// `skipped` = band-drop count + apply-skip count; advisory-only messages are in `warnings` only.
fn lte_entry(
    name: String,
    bytes: &[u8],
    m: &PhoneModel,
    patch_text: &str,
    source_label: &str,
    strict: bool,
    bands: &Bands,
) -> anyhow::Result<(ModuleEntry, Vec<String>, usize)> {
    let Patch::Lte(mut lp) = format::from_toml(patch_text)? else {
        anyhow::bail!("{source_label} expects a kind=\"lte\" patch");
    };
    let band_warns = retain_lte_compatible(&mut lp.set, bands, m);
    let caps = LteCaps::decode(bytes).with_context(|| format!("decoding {name}"))?;
    let (result, outcome) = patch::lte::apply_lte_patch(&caps, &lp, strict)?;
    let skipped = band_warns.len() + outcome.skipped.len();
    let mut warnings = band_warns;
    warnings.extend(outcome.skipped);
    Ok(((name, result.encode_to_vec()), warnings, skipped))
}

/// Error unless at least one `<CARRIER>_<NUMBER>.binarypb` for `carrier` exists in `dir`.
fn carrier_exists(dir: &Path, carrier: &str) -> anyhow::Result<()> {
    for entry in std::fs::read_dir(dir).with_context(|| format!("reading {}", dir.display()))? {
        let entry = entry?;
        let fname = entry.file_name();
        let Some(fname) = fname.to_str() else {
            continue;
        };
        if matches!(parse_name(fname), Parsed::Carrier { carrier: c, .. } if c == carrier) {
            return Ok(());
        }
    }
    anyhow::bail!("carrier {carrier} has no files in {}", dir.display());
}

/// Apply the NR patch to in-memory base bytes; return the module entry, warnings, and skip count.
/// `skipped` = band-drop count + apply-skip count. The soft fingerprint advisory is in `warnings`
/// only and is never counted as a skip.
fn nr_entry(
    name: String,
    bytes: &[u8],
    m: &PhoneModel,
    patch_text: &str,
    source_label: &str,
    strict: bool,
    bands: &Bands,
) -> anyhow::Result<(ModuleEntry, Vec<String>, usize)> {
    // Soft fingerprint check (warn-only; never blocks packaging, never counted as a skip).
    let mut warnings = Vec::new();
    if let Ok(caps) = UeCaps::decode(bytes)
        && let Some((fam, _)) = fp_info(caps.version)
        && let Some(prof) = PROFILES.iter().find(|p| p.anchor == m.nr_anchor)
        && fam != prof.family
    {
        warnings.push(format!(
            "{name} fingerprint family {fam:?} != expected {:?}",
            prof.family
        ));
    }
    let Patch::Nr(mut np) = format::from_toml(patch_text)? else {
        anyhow::bail!("{source_label} expects a kind=\"nr\" patch");
    };
    let band_warns = retain_nr_compatible(&mut np.set, bands, m);
    let caps = patch::build::decode_base(bytes)?;
    let (result, outcome) = patch::build::apply_patch(&caps, &np, strict)?;
    let skipped = band_warns.len() + outcome.skipped.len();
    warnings.extend(band_warns);
    warnings.extend(outcome.skipped);
    Ok(((name, result.encode_to_vec()), warnings, skipped))
}

/// Read the legend, strictly add `plmns` to `carrier`, and return the module entry.
fn legend_input(dir: &Path, carrier: &str, plmns: &[String]) -> anyhow::Result<(String, Vec<u8>)> {
    let name = "ap_plmn_mapping.binarypb".to_string();
    let path = dir.join(&name);
    let bytes =
        std::fs::read(&path).with_context(|| format!("reading legend {}", path.display()))?;
    let out = crate::mapping::add_plmns_strict(&bytes, carrier, plmns)?;
    Ok((name, out))
}

/// `Some(label)` if this carrier/NR component's band is not supported by the model.
/// An NR component (`band >= 10000`) is checked against `bands.nr`; otherwise an E-UTRA
/// (LTE) component checked against `bands.lte`. Labels render as `n78` / `B66`.
fn nr_cc_unsupported(cc: &Cc, bands: &Bands) -> Option<String> {
    if cc.band >= NR_BAND_OFFSET {
        let n = (cc.band - NR_BAND_OFFSET) as u16;
        (!bands.nr.contains(&n)).then(|| format!("n{n}"))
    } else {
        let b = cc.band as u16;
        (!bands.lte.contains(&b)).then(|| format!("B{b}"))
    }
}

/// The pinned "dropped entry" warning:
/// `skipping <key>: band(s) <list> not supported by <code> (<display>)`.
fn drop_warning(key: &str, bad: &[String], m: &PhoneModel) -> String {
    format!(
        "skipping {key:?}: band(s) {} not supported by {} ({})",
        bad.join(", "),
        m.code,
        m.display
    )
}

/// Drop NR/carrier `set` entries that reference any band the model lacks.
/// Returns one warning string per dropped entry.
fn retain_nr_compatible(
    set: &mut Vec<format::SetEntry>,
    bands: &Bands,
    m: &PhoneModel,
) -> Vec<String> {
    let mut warnings = Vec::new();
    set.retain(|e| {
        let mut bad: Vec<String> = e
            .combo
            .iter()
            .flat_map(|c| c.cc.iter())
            .filter_map(|cc| nr_cc_unsupported(cc, bands))
            .collect();
        bad.sort();
        bad.dedup();
        if bad.is_empty() {
            return true;
        }
        warnings.push(drop_warning(&e.key, &bad, m));
        false
    });
    warnings
}

/// Drop LTE `set` entries that reference any band the model lacks.
/// Returns one warning string per dropped entry.
fn retain_lte_compatible(
    set: &mut Vec<format::LteSetEntry>,
    bands: &Bands,
    m: &PhoneModel,
) -> Vec<String> {
    let mut warnings = Vec::new();
    set.retain(|e| {
        let mut bad: Vec<String> = e
            .combo
            .iter()
            .flat_map(|c| c.components.iter())
            .filter(|comp| !bands.lte.contains(&(comp.band as u16)))
            .map(|comp| format!("B{}", comp.band))
            .collect();
        bad.sort();
        bad.dedup();
        if bad.is_empty() {
            return true;
        }
        warnings.push(drop_warning(&e.key, &bad, m));
        false
    });
    warnings
}

/// Write the assembled module to `out` (a file) or stdout.
fn write_out(zip: &[u8], out: Option<&Path>) -> anyhow::Result<()> {
    match out {
        Some(path) => {
            std::fs::write(path, zip).with_context(|| format!("writing module {}", path.display()))
        }
        None => {
            let mut handle = std::io::stdout().lock();
            handle.write_all(zip).context("writing module to stdout")?;
            handle.flush().context("flushing stdout")
        }
    }
}

const UECAP_DEST: &str = "/vendor/firmware/uecapconfig";

/// Outcome of an in-memory provision: the Magisk module zip, the basenames packaged,
/// human-readable warnings, and the count of skipped/dropped patch entries.
#[derive(Debug, Clone)]
pub struct ProvisionResult {
    pub zip: Vec<u8>,
    pub included: Vec<String>,
    pub warnings: Vec<String>,
    pub skipped: usize,
}

/// Build a flashable Magisk module from pulled base files + the NR & LTE patch texts,
/// entirely in memory. Best-effort: band-incompatible or non-applying entries are
/// dropped and reported, never fatal. `files` holds the pulled `(basename, bytes)`;
/// both `lte_<id>.binarypb` and the carrier's NR file must be present.
pub fn provision_in_memory(
    code: &str,
    carrier: &str,
    files: &[(String, Vec<u8>)],
    nr_patch: &str,
    lte_patch: &str,
) -> anyhow::Result<ProvisionResult> {
    let m = phone_model(code).ok_or_else(|| anyhow::anyhow!("unknown model {code:?}"))?;
    let bands = PIXEL_BANDS
        .get(m.code)
        .ok_or_else(|| anyhow::anyhow!("no band data for model {}", m.code))?;

    let names: Vec<String> = files.iter().map(|(n, _)| n.clone()).collect();
    let sel = select_files(code, carrier, &names);
    if !sel.errors.is_empty() {
        anyhow::bail!("cannot provision: {}", sel.errors.join("; "));
    }
    let lookup = |name: &str| -> &[u8] {
        files
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, b)| b.as_slice())
            .expect("present per select_files")
    };
    // select_files guarantees to_pull == [lte_name, nr_name].
    let lte_name = sel.to_pull[0].clone();
    let nr_name = sel.to_pull[1].clone();

    let mut warnings = Vec::new();
    let mut skipped_total = 0usize;
    let mut inputs: Vec<ModuleEntry> = Vec::new();

    let lte_bytes = lookup(&lte_name);
    let (lte, lte_warn, lte_skip) =
        lte_entry(lte_name, lte_bytes, m, lte_patch, "lte patch", false, bands)?;
    warnings.extend(lte_warn);
    skipped_total += lte_skip;
    inputs.push(lte);

    let nr_bytes = lookup(&nr_name);
    let (nr, nr_warn, nr_skip) =
        nr_entry(nr_name, nr_bytes, m, nr_patch, "nr patch", false, bands)?;
    warnings.extend(nr_warn);
    skipped_total += nr_skip;
    inputs.push(nr);

    let included: Vec<String> = inputs.iter().map(|(n, _)| n.clone()).collect();
    let name = default_name(m, Some(carrier));
    let zip = crate::magisk::build_module(&inputs, UECAP_DEST, &name)?;

    Ok(ProvisionResult {
        zip,
        included,
        skipped: skipped_total,
        warnings,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proto::{
        Carrier, ComboGroup, LteCombo, LteComponent, PlmnMap, combo_group,
        combo_group::nested2::ComboFeatures,
    };
    use std::{
        collections::{BTreeMap, BTreeSet},
        io::{Cursor, Read},
    };
    use zip::ZipArchive;

    const UECAP_DEST: &str = "/vendor/firmware/uecapconfig";

    fn lte_caps(bands: &[i32]) -> LteCaps {
        LteCaps {
            fingerprint: 874_888_686,
            combos: bands
                .iter()
                .map(|&b| LteCombo {
                    components: vec![LteComponent {
                        band: b,
                        bw_class_mimo_dl: 32768,
                        bw_class_mimo_ul: Some(0),
                    }],
                    bcs: Some(0),
                    unknown1: Some(0),
                    unknown2: Some(0),
                })
                .collect(),
            bitmask: 42,
        }
    }

    fn zip_entries(zip: &[u8]) -> BTreeMap<String, Vec<u8>> {
        let mut a = ZipArchive::new(Cursor::new(zip)).unwrap();
        let mut out = BTreeMap::new();
        for i in 0..a.len() {
            let mut f = a.by_index(i).unwrap();
            let n = f.name().to_string();
            let mut b = Vec::new();
            f.read_to_end(&mut b).unwrap();
            out.insert(n, b);
        }
        out
    }

    fn nr_caps(bands: &[i32]) -> UeCaps {
        UeCaps {
            version: 862_505_271, // family B / main — matches G2YBB (anchor 66813533)
            combo_groups: vec![ComboGroup {
                combo_header: None,
                combo: bands
                    .iter()
                    .map(|&b| combo_group::Nested2 {
                        bitmask: Some(0),
                        cc: vec![ComboFeatures {
                            band: NR_BAND_OFFSET + b,
                            bw_class_dl: Some(1),
                            bw_class_ul: Some(1),
                            ..Default::default()
                        }],
                    })
                    .collect(),
            }],
            ..Default::default()
        }
    }

    fn legend_bytes(carrier: &str, plmns: Vec<u64>) -> Vec<u8> {
        PlmnMap {
            carriers: vec![Carrier {
                plmns,
                index: 1,
                name: carrier.into(),
            }],
        }
        .encode_to_vec()
    }

    #[test]
    fn no_modifier_errors() {
        let e = run(
            "G2YBB",
            Path::new("/nope"),
            None,
            None,
            None,
            &[],
            None,
            UECAP_DEST,
            None,
            false,
        )
        .unwrap_err();
        assert!(e.to_string().contains("nothing to build"), "{e}");
    }

    #[test]
    fn nr_patch_without_carrier_errors() {
        let e = run(
            "G2YBB",
            Path::new("/nope"),
            None,
            None,
            Some(Path::new("n.toml")),
            &[],
            None,
            UECAP_DEST,
            None,
            false,
        )
        .unwrap_err();
        assert!(e.to_string().contains("require --carrier"), "{e}");
    }

    #[test]
    fn carrier_with_only_lte_patch_errors() {
        let e = run(
            "G2YBB",
            Path::new("/nope"),
            Some("VZW"),
            Some(Path::new("l.toml")),
            None,
            &[],
            None,
            UECAP_DEST,
            None,
            false,
        )
        .unwrap_err();
        assert!(e.to_string().contains("--carrier has no effect"), "{e}");
    }

    #[test]
    fn unknown_model_errors() {
        let e = run(
            "p99-zz",
            Path::new("/nope"),
            None,
            Some(Path::new("l.toml")),
            None,
            &[],
            None,
            UECAP_DEST,
            None,
            false,
        )
        .unwrap_err();
        assert!(e.to_string().contains("unknown model"), "{e}");
    }

    #[test]
    fn lte_patch_packages_patched_lte() {
        use crate::report::lte::lte_combo_key;

        let dir = std::env::temp_dir().join(format!("uecaps-prov-lte-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();

        // G2YBB lte_id = 400907661; provision reads lte_400907661.binarypb (A).
        let a = lte_caps(&[1, 5]); // B1A, B5A
        let b = lte_caps(&[1, 7]); // B1A, B7A
        let a_path = dir.join("lte_400907661.binarypb");
        let b_path = dir.join("lte_2160127815.binarypb");
        std::fs::write(&a_path, a.encode_to_vec()).unwrap();
        std::fs::write(&b_path, b.encode_to_vec()).unwrap();

        let patch_path = dir.join("p.toml");
        crate::patch::create(&a_path, &b_path, Some(&patch_path)).unwrap();
        let out_path = dir.join("out.zip");

        let code = run(
            "G2YBB",
            &dir,
            None,
            Some(&patch_path),
            None,
            &[],
            Some(&out_path),
            UECAP_DEST,
            None,
            false,
        )
        .unwrap();
        assert_eq!(code, 0);

        let entries = zip_entries(&std::fs::read(&out_path).unwrap());
        std::fs::remove_dir_all(&dir).ok();

        let key = "system/vendor/firmware/uecapconfig/lte_400907661.binarypb";
        assert!(entries.contains_key(key), "missing {key}");
        assert!(
            !entries.contains_key("system/vendor/firmware/uecapconfig/ap_plmn_mapping.binarypb")
        );
        let packaged = LteCaps::decode(&entries[key][..]).unwrap();
        let got: BTreeSet<String> = packaged.combos.iter().map(lte_combo_key).collect();
        let want: BTreeSet<String> = b.combos.iter().map(lte_combo_key).collect();
        assert_eq!(got, want);
        assert_eq!(packaged.fingerprint, 874_888_686);
    }

    #[test]
    fn lte_patch_rejects_nr_kind() {
        let dir = std::env::temp_dir().join(format!("uecaps-prov-kind-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("lte_400907661.binarypb"),
            lte_caps(&[1]).encode_to_vec(),
        )
        .unwrap();
        let patch_path = dir.join("nr.toml");
        std::fs::write(&patch_path, "kind = \"nr\"\nversion = 1\n").unwrap();

        let e = run(
            "G2YBB",
            &dir,
            None,
            Some(&patch_path),
            None,
            &[],
            None,
            UECAP_DEST,
            None,
            false,
        )
        .unwrap_err();
        std::fs::remove_dir_all(&dir).ok();
        assert!(
            e.to_string()
                .contains("--lte-patch expects a kind=\"lte\" patch"),
            "{e}"
        );
    }

    #[test]
    fn lte_missing_file_errors() {
        let dir = std::env::temp_dir().join(format!("uecaps-prov-missing-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let patch_path = dir.join("l.toml");
        std::fs::write(&patch_path, "kind = \"lte\"\nversion = 1\n").unwrap();

        let e = run(
            "G2YBB",
            &dir,
            None,
            Some(&patch_path),
            None,
            &[],
            None,
            UECAP_DEST,
            None,
            false,
        )
        .unwrap_err();
        std::fs::remove_dir_all(&dir).ok();
        assert!(e.to_string().contains("reading LTE file"), "{e}");
    }

    #[test]
    fn carrier_not_in_dir_errors() {
        let dir = std::env::temp_dir().join(format!("uecaps-prov-noc-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("ATT_1234.binarypb"), b"x").unwrap(); // decoy carrier
        std::fs::write(
            dir.join("ap_plmn_mapping.binarypb"),
            legend_bytes("VZW", vec![]),
        )
        .unwrap();

        let e = run(
            "G2YBB",
            &dir,
            Some("VZW"),
            None,
            None,
            &["250-99".to_string()],
            None,
            UECAP_DEST,
            None,
            false,
        )
        .unwrap_err();
        std::fs::remove_dir_all(&dir).ok();
        assert!(e.to_string().contains("has no files"), "{e}");
    }

    #[test]
    fn nr_patch_packages_patched_nr() {
        use crate::patch::build::present_keys;

        let dir = std::env::temp_dir().join(format!("uecaps-prov-nr-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let a = nr_caps(&[78]); // n78A
        let b = nr_caps(&[2]); // n2A
        let a_path = dir.join("VZW_66813533.binarypb"); // 66813533 % 66813533 == 0
        let b_path = dir.join("BBB_2.binarypb");
        std::fs::write(&a_path, a.encode_to_vec()).unwrap();
        std::fs::write(&b_path, b.encode_to_vec()).unwrap();
        let patch_path = dir.join("nr.toml");
        crate::patch::create(&a_path, &b_path, Some(&patch_path)).unwrap();
        let out_path = dir.join("out.zip");

        let code = run(
            "G2YBB",
            &dir,
            Some("VZW"),
            None,
            Some(&patch_path),
            &[],
            Some(&out_path),
            UECAP_DEST,
            None,
            false,
        )
        .unwrap();
        assert_eq!(code, 0);

        let entries = zip_entries(&std::fs::read(&out_path).unwrap());
        std::fs::remove_dir_all(&dir).ok();
        let key = "system/vendor/firmware/uecapconfig/VZW_66813533.binarypb";
        assert!(entries.contains_key(key), "missing {key}");
        let packaged = UeCaps::decode(&entries[key][..]).unwrap();
        assert_eq!(present_keys(&packaged), present_keys(&b));
        assert_eq!(packaged.version, 862_505_271);
    }

    #[test]
    fn nr_ambiguous_errors() {
        let dir = std::env::temp_dir().join(format!("uecaps-prov-amb-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        // Both numbers divisible by the G2YBB anchor (66813533 and 2×66813533).
        std::fs::write(
            dir.join("VZW_66813533.binarypb"),
            nr_caps(&[78]).encode_to_vec(),
        )
        .unwrap();
        std::fs::write(
            dir.join("VZW_133627066.binarypb"),
            nr_caps(&[2]).encode_to_vec(),
        )
        .unwrap();
        let patch_path = dir.join("nr.toml");
        std::fs::write(&patch_path, "kind = \"nr\"\nversion = 1\n").unwrap();

        let e = run(
            "G2YBB",
            &dir,
            Some("VZW"),
            None,
            Some(&patch_path),
            &[],
            None,
            UECAP_DEST,
            None,
            false,
        )
        .unwrap_err();
        std::fs::remove_dir_all(&dir).ok();
        assert!(e.to_string().contains("ambiguous"), "{e}");
    }

    #[test]
    fn add_plmn_packages_legend() {
        let dir = std::env::temp_dir().join(format!("uecaps-prov-plmn-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("VZW_66813533.binarypb"), b"x").unwrap(); // name-only; not decoded
        std::fs::write(
            dir.join("ap_plmn_mapping.binarypb"),
            legend_bytes("VZW", vec![5_435_408]),
        )
        .unwrap();
        let out_path = dir.join("out.zip");

        let code = run(
            "G2YBB",
            &dir,
            Some("VZW"),
            None,
            None,
            &["302-220".to_string()],
            Some(&out_path),
            UECAP_DEST,
            None,
            false,
        )
        .unwrap();
        assert_eq!(code, 0);

        let entries = zip_entries(&std::fs::read(&out_path).unwrap());
        std::fs::remove_dir_all(&dir).ok();
        let key = "system/vendor/firmware/uecapconfig/ap_plmn_mapping.binarypb";
        let map = PlmnMap::decode(&entries[key][..]).unwrap();
        assert_eq!(map.carriers[0].plmns, vec![5_435_408, 197_154]); // 302-220 -> 197154
    }

    #[test]
    fn add_plmn_rejects_existing_plmn() {
        let dir = std::env::temp_dir().join(format!("uecaps-prov-dup-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("VZW_66813533.binarypb"), b"x").unwrap();
        std::fs::write(
            dir.join("ap_plmn_mapping.binarypb"),
            legend_bytes("VZW", vec![5_435_408]),
        )
        .unwrap();
        let out_path = dir.join("out.zip");

        // 250-01 -> 5435408, already present.
        let e = run(
            "G2YBB",
            &dir,
            Some("VZW"),
            None,
            None,
            &["250-01".to_string()],
            Some(&out_path),
            UECAP_DEST,
            None,
            false,
        )
        .unwrap_err();
        let wrote = out_path.exists();
        std::fs::remove_dir_all(&dir).ok();
        assert!(e.to_string().contains("already mapped"), "{e}");
        assert!(!wrote, "no zip on error");
    }

    #[test]
    fn all_three_modifiers_package_three_entries() {
        let dir = std::env::temp_dir().join(format!("uecaps-prov-all3-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();

        std::fs::write(
            dir.join("lte_400907661.binarypb"),
            lte_caps(&[1, 5]).encode_to_vec(),
        )
        .unwrap();
        std::fs::write(
            dir.join("lte_2160127815.binarypb"),
            lte_caps(&[1, 7]).encode_to_vec(),
        )
        .unwrap();
        let lte_patch = dir.join("lte.toml");
        crate::patch::create(
            &dir.join("lte_400907661.binarypb"),
            &dir.join("lte_2160127815.binarypb"),
            Some(&lte_patch),
        )
        .unwrap();

        std::fs::write(
            dir.join("VZW_66813533.binarypb"),
            nr_caps(&[78]).encode_to_vec(),
        )
        .unwrap();
        std::fs::write(dir.join("BBB_2.binarypb"), nr_caps(&[2]).encode_to_vec()).unwrap();
        let nr_patch = dir.join("nr.toml");
        crate::patch::create(
            &dir.join("VZW_66813533.binarypb"),
            &dir.join("BBB_2.binarypb"),
            Some(&nr_patch),
        )
        .unwrap();

        std::fs::write(
            dir.join("ap_plmn_mapping.binarypb"),
            legend_bytes("VZW", vec![]),
        )
        .unwrap();
        let out_path = dir.join("out.zip");

        let code = run(
            "G2YBB",
            &dir,
            Some("VZW"),
            Some(&lte_patch),
            Some(&nr_patch),
            &["302-220".to_string()],
            Some(&out_path),
            UECAP_DEST,
            None,
            false,
        )
        .unwrap();
        assert_eq!(code, 0);

        let entries = zip_entries(&std::fs::read(&out_path).unwrap());
        std::fs::remove_dir_all(&dir).ok();
        let p = "system/vendor/firmware/uecapconfig/";
        assert!(entries.contains_key(&format!("{p}lte_400907661.binarypb")));
        assert!(entries.contains_key(&format!("{p}VZW_66813533.binarypb")));
        assert!(entries.contains_key(&format!("{p}ap_plmn_mapping.binarypb")));
    }

    #[test]
    fn nr_missing_file_errors() {
        let dir = std::env::temp_dir().join(format!("uecaps-prov-nrmiss-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        // VZW exists (carrier_exists passes) but no file divisible by the G2YBB anchor (3 isn't).
        std::fs::write(dir.join("VZW_3.binarypb"), nr_caps(&[78]).encode_to_vec()).unwrap();
        let patch_path = dir.join("nr.toml");
        std::fs::write(&patch_path, "kind = \"nr\"\nversion = 1\n").unwrap();

        let e = run(
            "G2YBB",
            &dir,
            Some("VZW"),
            None,
            Some(&patch_path),
            &[],
            None,
            UECAP_DEST,
            None,
            false,
        )
        .unwrap_err();
        std::fs::remove_dir_all(&dir).ok();
        assert!(e.to_string().contains("no NR file"), "{e}");
    }

    #[test]
    fn legend_missing_file_errors() {
        let dir = std::env::temp_dir().join(format!("uecaps-prov-legmiss-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("VZW_66813533.binarypb"), b"x").unwrap(); // carrier_exists passes
        // no ap_plmn_mapping.binarypb present
        let e = run(
            "G2YBB",
            &dir,
            Some("VZW"),
            None,
            None,
            &["302-220".to_string()],
            None,
            UECAP_DEST,
            None,
            false,
        )
        .unwrap_err();
        std::fs::remove_dir_all(&dir).ok();
        assert!(e.to_string().contains("reading legend"), "{e}");
    }

    #[test]
    fn nr_cc_unsupported_flags_missing_bands() {
        let bands = PIXEL_BANDS.get("GUL82").unwrap();
        let cc = |band| Cc {
            band,
            ..Default::default()
        };
        assert_eq!(nr_cc_unsupported(&cc(10078), bands), None); // n78 supported
        assert_eq!(nr_cc_unsupported(&cc(10079), bands), Some("n79".into())); // n79 not
        assert_eq!(nr_cc_unsupported(&cc(66), bands), None); // B66 supported
        assert_eq!(nr_cc_unsupported(&cc(32), bands), Some("B32".into())); // B32 not
    }

    #[test]
    fn nr_band_filter_drops_unsupported_combo() {
        use crate::patch::build::present_keys;
        let dir = std::env::temp_dir().join(format!("uecaps-prov-bandnr-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        // G2YBB (Pixel 9, mmWave US): nr_anchor 66813533; supports n78, not n79.
        std::fs::write(
            dir.join("VZW_66813533.binarypb"),
            nr_caps(&[2]).encode_to_vec(),
        )
        .unwrap();
        std::fs::write(
            dir.join("BBB_2.binarypb"),
            nr_caps(&[2, 78, 79]).encode_to_vec(),
        )
        .unwrap();
        let patch_path = dir.join("nr.toml");
        crate::patch::create(
            &dir.join("VZW_66813533.binarypb"),
            &dir.join("BBB_2.binarypb"),
            Some(&patch_path),
        )
        .unwrap();
        let out_path = dir.join("out.zip");

        let code = run(
            "G2YBB",
            &dir,
            Some("VZW"),
            None,
            Some(&patch_path),
            &[],
            Some(&out_path),
            UECAP_DEST,
            None,
            false,
        )
        .unwrap();
        assert_eq!(code, 1); // n79 dropped -> skipped tally -> exit 1

        let entries = zip_entries(&std::fs::read(&out_path).unwrap());
        std::fs::remove_dir_all(&dir).ok();
        let packaged = UeCaps::decode(
            &entries["system/vendor/firmware/uecapconfig/VZW_66813533.binarypb"][..],
        )
        .unwrap();
        let keys = present_keys(&packaged);
        assert!(keys.contains("n78A"), "n78 (supported) should be applied");
        assert!(
            !keys.contains("n79A"),
            "n79 (unsupported) should be dropped"
        );
    }

    #[test]
    fn lte_band_filter_drops_unsupported_combo() {
        let dir = std::env::temp_dir().join(format!("uecaps-prov-bandlte-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        // GUL82 (Pixel 10 Pro XL): lte_id 1254026417; supports B66, not B32.
        std::fs::write(
            dir.join("lte_1254026417.binarypb"),
            lte_caps(&[66]).encode_to_vec(),
        )
        .unwrap();
        std::fs::write(
            dir.join("lte_2160127815.binarypb"),
            lte_caps(&[66, 32]).encode_to_vec(),
        )
        .unwrap();
        let patch_path = dir.join("lte.toml");
        crate::patch::create(
            &dir.join("lte_1254026417.binarypb"),
            &dir.join("lte_2160127815.binarypb"),
            Some(&patch_path),
        )
        .unwrap();
        let out_path = dir.join("out.zip");

        let code = run(
            "GUL82",
            &dir,
            None,
            Some(&patch_path),
            None,
            &[],
            Some(&out_path),
            UECAP_DEST,
            None,
            false,
        )
        .unwrap();
        assert_eq!(code, 1);

        let entries = zip_entries(&std::fs::read(&out_path).unwrap());
        std::fs::remove_dir_all(&dir).ok();
        let packaged = LteCaps::decode(
            &entries["system/vendor/firmware/uecapconfig/lte_1254026417.binarypb"][..],
        )
        .unwrap();
        let used: Vec<i32> = packaged
            .combos
            .iter()
            .flat_map(|c| c.components.iter().map(|x| x.band))
            .collect();
        assert!(used.contains(&66), "B66 (supported) should be applied");
        assert!(!used.contains(&32), "B32 (unsupported) should be dropped");
    }

    #[test]
    fn select_files_picks_lte_and_carrier_nr() {
        // GUL82: lte_id 1254026417, nr_anchor 3616442437.
        let available = vec![
            "lte_1254026417.binarypb".to_string(),
            "APAC_COMMON_3616442437.binarypb".to_string(), // 3616442437 % 3616442437 == 0
            "VZW_66813533.binarypb".to_string(),           // other carrier — ignored
            "ap_plmn_mapping.binarypb".to_string(),
        ];
        let sel = select_files("GUL82", "APAC_COMMON", &available);
        assert!(sel.errors.is_empty(), "unexpected errors: {:?}", sel.errors);
        assert_eq!(
            sel.to_pull,
            vec![
                "lte_1254026417.binarypb".to_string(),
                "APAC_COMMON_3616442437.binarypb".to_string(),
            ]
        );
    }

    #[test]
    fn select_files_reports_missing_nr() {
        let available = vec!["lte_1254026417.binarypb".to_string()];
        let sel = select_files("GUL82", "APAC_COMMON", &available);
        assert!(sel.to_pull.is_empty());
        assert!(
            sel.errors.iter().any(|e| e.contains("no APAC_COMMON file")),
            "{:?}",
            sel.errors
        );
    }

    #[test]
    fn select_files_reports_missing_lte() {
        let available = vec!["APAC_COMMON_3616442437.binarypb".to_string()];
        let sel = select_files("GUL82", "APAC_COMMON", &available);
        assert!(sel.to_pull.is_empty());
        assert!(
            sel.errors
                .iter()
                .any(|e| e.contains("lte_1254026417.binarypb")),
            "{:?}",
            sel.errors
        );
    }

    #[test]
    fn select_files_reports_ambiguous_nr() {
        let available = vec![
            "lte_1254026417.binarypb".to_string(),
            "APAC_COMMON_3616442437.binarypb".to_string(),
            "APAC_COMMON_7232884874.binarypb".to_string(), // 2 × anchor — also divisible
        ];
        let sel = select_files("GUL82", "APAC_COMMON", &available);
        assert!(
            sel.errors.iter().any(|e| e.contains("ambiguous")),
            "{:?}",
            sel.errors
        );
    }

    #[test]
    fn select_files_reports_unknown_model() {
        let sel = select_files("ZZ999", "APAC_COMMON", &[]);
        assert!(
            sel.errors.iter().any(|e| e.contains("unknown model")),
            "{:?}",
            sel.errors
        );
    }

    #[test]
    fn nr_patch_rejects_lte_kind() {
        let dir = std::env::temp_dir().join(format!("uecaps-prov-nrkind-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("VZW_66813533.binarypb"),
            nr_caps(&[78]).encode_to_vec(),
        )
        .unwrap();
        let patch_path = dir.join("lte.toml");
        std::fs::write(&patch_path, "kind = \"lte\"\nversion = 1\n").unwrap();

        let e = run(
            "G2YBB",
            &dir,
            Some("VZW"),
            None,
            Some(&patch_path),
            &[],
            None,
            UECAP_DEST,
            None,
            false,
        )
        .unwrap_err();
        std::fs::remove_dir_all(&dir).ok();
        assert!(
            e.to_string()
                .contains("--nr-patch expects a kind=\"nr\" patch"),
            "{e}"
        );
    }

    #[test]
    fn nonstrict_skip_returns_exit_1() {
        let dir = std::env::temp_dir().join(format!("uecaps-prov-skip-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("VZW_66813533.binarypb"),
            nr_caps(&[78]).encode_to_vec(),
        )
        .unwrap();
        // Delete a key that isn't present -> best-effort skip.
        let patch_path = dir.join("nr.toml");
        std::fs::write(
            &patch_path,
            "kind = \"nr\"\nversion = 1\ndelete = [\"n99A\"]\n",
        )
        .unwrap();
        let out_path = dir.join("out.zip");

        let code = run(
            "G2YBB",
            &dir,
            Some("VZW"),
            None,
            Some(&patch_path),
            &[],
            Some(&out_path),
            UECAP_DEST,
            None,
            false,
        )
        .unwrap();
        std::fs::remove_dir_all(&dir).ok();
        assert_eq!(code, 1); // built, but an entry was skipped
    }

    #[test]
    fn strict_skip_returns_error() {
        let dir = std::env::temp_dir().join(format!("uecaps-prov-strict-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("VZW_66813533.binarypb"),
            nr_caps(&[78]).encode_to_vec(),
        )
        .unwrap();
        let patch_path = dir.join("nr.toml");
        std::fs::write(
            &patch_path,
            "kind = \"nr\"\nversion = 1\ndelete = [\"n99A\"]\n",
        )
        .unwrap();

        let res = run(
            "G2YBB",
            &dir,
            Some("VZW"),
            None,
            Some(&patch_path),
            &[],
            None,
            UECAP_DEST,
            None,
            true,
        );
        std::fs::remove_dir_all(&dir).ok();
        assert!(res.is_err()); // strict: a non-applying entry aborts (exit 2 via main)
    }

    #[test]
    fn in_memory_matches_filesystem_and_reports() {
        use crate::patch::build::present_keys;

        // G2YBB: lte_id 400907661, nr_anchor 66813533.
        let dir = std::env::temp_dir().join(format!("uecaps-parity-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("lte_400907661.binarypb"),
            lte_caps(&[1, 5]).encode_to_vec(),
        )
        .unwrap();
        std::fs::write(
            dir.join("lte_2160127815.binarypb"),
            lte_caps(&[1, 7]).encode_to_vec(),
        )
        .unwrap();
        let lte_patch_path = dir.join("lte.toml");
        crate::patch::create(
            &dir.join("lte_400907661.binarypb"),
            &dir.join("lte_2160127815.binarypb"),
            Some(&lte_patch_path),
        )
        .unwrap();

        std::fs::write(
            dir.join("APAC_COMMON_66813533.binarypb"),
            nr_caps(&[78]).encode_to_vec(),
        )
        .unwrap();
        std::fs::write(dir.join("BBB_2.binarypb"), nr_caps(&[2]).encode_to_vec()).unwrap();
        let nr_patch_path = dir.join("nr.toml");
        crate::patch::create(
            &dir.join("APAC_COMMON_66813533.binarypb"),
            &dir.join("BBB_2.binarypb"),
            Some(&nr_patch_path),
        )
        .unwrap();

        // Filesystem path (CLI).
        let fs_out = dir.join("fs.zip");
        let code = run(
            "G2YBB",
            &dir,
            Some("APAC_COMMON"),
            Some(&lte_patch_path),
            Some(&nr_patch_path),
            &[],
            Some(&fs_out),
            UECAP_DEST,
            None,
            false,
        )
        .unwrap();
        assert_eq!(code, 0);
        let fs_entries = zip_entries(&std::fs::read(&fs_out).unwrap());

        // In-memory path (web).
        let files = vec![
            (
                "lte_400907661.binarypb".to_string(),
                std::fs::read(dir.join("lte_400907661.binarypb")).unwrap(),
            ),
            (
                "APAC_COMMON_66813533.binarypb".to_string(),
                std::fs::read(dir.join("APAC_COMMON_66813533.binarypb")).unwrap(),
            ),
        ];
        let nr_text = std::fs::read_to_string(&nr_patch_path).unwrap();
        let lte_text = std::fs::read_to_string(&lte_patch_path).unwrap();
        let res = provision_in_memory("G2YBB", "APAC_COMMON", &files, &nr_text, &lte_text).unwrap();
        let mem_entries = zip_entries(&res.zip);

        std::fs::remove_dir_all(&dir).ok();

        // Identical packaged overlay files (the binarypb payloads).
        let p = "system/vendor/firmware/uecapconfig/";
        for key in [
            format!("{p}lte_400907661.binarypb"),
            format!("{p}APAC_COMMON_66813533.binarypb"),
        ] {
            assert_eq!(
                mem_entries.get(&key),
                fs_entries.get(&key),
                "mismatch for {key}"
            );
        }
        // Structured result is populated and consistent.
        assert_eq!(res.included.len(), 2);
        assert_eq!(res.skipped, 0);
        assert!(
            res.warnings.is_empty(),
            "expected no warnings, got {:?}",
            res.warnings
        );
        let nr =
            UeCaps::decode(&mem_entries[&format!("{p}APAC_COMMON_66813533.binarypb")][..]).unwrap();
        assert_eq!(present_keys(&nr), present_keys(&nr_caps(&[2])));
    }

    /// Regression test for the soft fingerprint advisory not being counted as a skip.
    ///
    /// GUL82 has `nr_anchor` 3616442437 → PROFILES entry with `family = Family::A`.
    /// `version` 862505271 → `fp_info` returns `(Family::B, Main)`, which mismatches
    /// the profile's `Family::A`. This fires the advisory warning branch in `nr_entry`.
    ///
    /// n78 is supported by GUL82 (no band drops). The no-op patch has no set entries,
    /// so `apply_patch` produces 0 apply-skips. `skipped` must therefore be 0 even
    /// though `warnings` is non-empty.
    #[test]
    fn nr_soft_fingerprint_mismatch_warns_but_does_not_count_as_skip() {
        let m = phone_model("GUL82").unwrap(); // profile family A (anchor 3616442437)
        let bands = PIXEL_BANDS.get("GUL82").unwrap();

        let base = UeCaps {
            version: 862_505_271, // fp_info -> (Family::B, Main) — mismatches profile Family::A
            combo_groups: vec![ComboGroup {
                combo_header: None,
                combo: vec![combo_group::Nested2 {
                    bitmask: Some(0),
                    cc: vec![ComboFeatures {
                        band: 10078, // n78: supported by GUL82, so no band drop
                        bw_class_dl: Some(1),
                        bw_class_ul: Some(1),
                        ..Default::default()
                    }],
                }],
            }],
            ..Default::default()
        };
        let base_bytes = base.encode_to_vec();

        // No-op NR patch: no add/delete operations → 0 apply-skips.
        let patch_text = "kind = \"nr\"\nversion = 1\n";

        let (_entry, warnings, skipped) = nr_entry(
            "APAC_COMMON_3616442437.binarypb".to_string(),
            &base_bytes,
            m,
            patch_text,
            "nr patch",
            false,
            bands,
        )
        .unwrap();

        assert!(
            warnings.iter().any(|w| w.contains("fingerprint")),
            "expected a fingerprint advisory in warnings, got {warnings:?}"
        );
        assert_eq!(skipped, 0, "fingerprint advisory must not count as a skip");
    }
}
