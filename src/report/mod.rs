//! Reports facade: `inspect`, `compare`, folder-wide `check`/`matrix`, the runtime
//! `self-test`, and the shared `Detail`/`Common` presentation types.

mod check;
pub(crate) mod combos;
mod compare;
mod detail;
mod inspect;
mod lte;
mod matrix;
mod selftest;

pub use check::check_folder;
pub use compare::compare;
pub use detail::{Common, Detail};
pub use inspect::inspect;
pub use matrix::matrix;
pub use selftest::self_test;

use crate::{
    model::{Parsed, parse_name},
    proto::UeCaps,
};
use anyhow::Context;
use combos::{Combo, build_combos};
use std::path::Path;

/// Read and **strictly** validate one capability file.
///
/// Two things this deliberately does not do, both of which it used to. It does not collapse an
/// I/O failure into the same value as a decode failure — a mode-0000 file, one owned by another
/// user, or a dangling symlink was previously indistinguishable from a corrupt one, so `check`
/// blamed the contents for what was an access problem. And it does not decode leniently: every
/// audit surface now goes through `wire`'s fail-closed scanner, so the commands whose whole
/// purpose is finding anomalies no longer accept exactly the corruption that layer exists to
/// reject.
fn read_ue_caps(path: &Path) -> anyhow::Result<UeCaps> {
    let label = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("<unnamed>");
    let data = std::fs::read(path).with_context(|| format!("cannot read {}", path.display()))?;
    crate::wire::decode_uecaps(&data, label)
}

/// Sorted names of every `*.binarypb` file directly in `dir` — the shared first
/// step of the folder-scanning commands (`check`, `matrix`).
fn binarypb_names(dir: &Path) -> anyhow::Result<Vec<String>> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .with_context(|| format!("cannot read {}", dir.display()))?
        .flatten()
        .filter_map(|e| e.file_name().to_str().map(str::to_string))
        .filter(|n| n.ends_with(".binarypb"))
        .collect();
    names.sort_unstable();
    Ok(names)
}

#[derive(Debug)]
struct CarrierCombos {
    combos: Vec<Combo>,
    number: Option<u64>,
    version: u64,
    filename: String,
}

/// Validate a filename as a `<CARRIER>_<NUMBER>` capability file, decode it, and
/// build its combos. Errors on the legend / `lte_*` names, undecodable content, and
/// reference stubs (no band combinations). The name checks run before any file read.
///
/// Decoding is fail-closed, via [`read_ue_caps`]. This module was once deliberately lenient
/// here on the grounds that `compare` is read-only, but that left `check` — whose entire job is
/// auditing for anomalies — accepting unmodeled fields, wrong wire types and packed PLMN lists
/// that the compiler hard-errors on. An audit that is more permissive than the writer reports
/// corrupt input as clean, so the leniency is gone.
fn load_carrier_combos(path: &Path) -> anyhow::Result<CarrierCombos> {
    let filename = path
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("?")
        .to_string();
    let number = match parse_name(&filename) {
        Parsed::Mapping => {
            anyhow::bail!("{filename} is the PLMN legend, not a <CARRIER>_<NUMBER> capability file")
        }
        Parsed::Lte(_) => {
            anyhow::bail!("{filename} is an LTE fallback, not a <CARRIER>_<NUMBER> capability file")
        }
        Parsed::Carrier { number, .. } => Some(number),
        Parsed::Other => None,
    };
    let caps = read_ue_caps(path)
        .with_context(|| format!("cannot read {filename} as a UE-capability file"))?;
    let combos = build_combos(&caps);
    if combos.is_empty() {
        anyhow::bail!("{filename} has no band combinations (reference stub)");
    }
    Ok(CarrierCombos {
        combos,
        number,
        version: caps.version,
        filename,
    })
}

#[cfg(test)]
mod tests {
    use super::read_ue_caps;
    use crate::proto::UeCaps;
    use prost::Message;

    /// The audit surfaces used to decode with bare prost, so a capability file carrying an
    /// unmodeled field was reported as clean by the very commands meant to find that.
    #[test]
    fn rejects_a_file_carrying_an_unmodeled_field() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("VZW_311480.binarypb");
        let mut bytes = UeCaps {
            version: 874_888_686,
            ..Default::default()
        }
        .encode_to_vec();
        bytes.extend([0x78, 0x01]); // field 15, not modeled
        std::fs::write(&path, &bytes).unwrap();
        // Bare prost accepts it, which is exactly why the strict layer is needed here.
        assert!(UeCaps::decode(&bytes[..]).is_ok());

        let error = format!("{:#}", read_ue_caps(&path).unwrap_err());

        assert!(error.contains("field #15"), "{error}");
        assert!(error.contains("VZW_311480.binarypb"), "{error}");
    }

    /// An I/O failure and a decode failure used to collapse into the same `None`, so `check`
    /// blamed a file's contents for what was really an access problem.
    #[test]
    fn distinguishes_an_io_failure_from_a_decode_failure() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("absent.binarypb");

        let error = format!("{:#}", read_ue_caps(&missing).unwrap_err());

        assert!(error.contains("cannot read"), "{error}");
        assert!(!error.contains("field #"), "{error}");
    }

    #[test]
    fn accepts_a_well_formed_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("VZW_311480.binarypb");
        let caps = UeCaps {
            version: 874_888_686,
            ..Default::default()
        };
        std::fs::write(&path, caps.encode_to_vec()).unwrap();

        assert_eq!(read_ue_caps(&path).unwrap(), caps);
    }
}
