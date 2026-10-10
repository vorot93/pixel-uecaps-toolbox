//! Reports facade: `inspect`, `compare`, folder-wide `check`/`matrix`, the runtime
//! `self-test`, and the shared `Detail`/`Common` presentation types.

mod check;
pub(crate) mod combos;
mod compare;
mod detail;
mod identity;
mod input;
mod inspect;
mod lte;
mod matrix;
mod selftest;
mod validation;

pub use check::check_folder;
pub use compare::compare;
pub use detail::{Common, Detail};
pub use inspect::inspect;
pub use matrix::matrix;
pub use selftest::self_test;

use crate::proto::UeCaps;
use anyhow::Context;
use std::{io::Write, path::Path};

fn write_report(body: &str, diagnostics: &str) -> anyhow::Result<()> {
    let mut stderr = std::io::stderr().lock();
    stderr.write_all(diagnostics.as_bytes())?;
    stderr.flush()?;
    let mut stdout = std::io::stdout().lock();
    stdout.write_all(body.as_bytes())?;
    stdout.flush()?;
    Ok(())
}

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
