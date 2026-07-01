//! Reports: single-file `inspect`, folder-wide `check`, and the runtime `self-test`.

mod check;
pub(crate) mod combos;
mod compare;
mod inspect;
pub(crate) mod lte;
mod matrix;
mod selftest;

pub use check::check_folder;
pub use compare::compare;
pub use inspect::inspect;
pub use matrix::matrix;
pub use selftest::self_test;

use crate::{
    model::{Parsed, parse_name},
    proto::UeCaps,
};
use anyhow::Context;
use combos::{Combo, build_combos};
use prost::Message;
use std::path::Path;

fn read_ue_caps(path: &Path) -> Option<UeCaps> {
    let data = std::fs::read(path).ok()?;
    UeCaps::decode(&data[..]).ok()
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
    names.sort();
    Ok(names)
}

#[derive(Debug)]
pub(crate) struct CarrierCombos {
    pub(crate) combos: Vec<Combo>,
    pub(crate) number: Option<u64>,
    pub(crate) version: u64,
    pub(crate) filename: String,
}

/// Validate a filename as a `<CARRIER>_<NUMBER>` capability file, decode it, and
/// build its combos. Errors on the legend / `lte_*` names, undecodable content, and
/// reference stubs (no band combinations). The name checks run before any file read.
pub(crate) fn load_carrier_combos(path: &Path) -> anyhow::Result<CarrierCombos> {
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
        .ok_or_else(|| anyhow::anyhow!("cannot decode {filename} as a UE-capability file"))?;
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
