//! Strict, single-read inputs. A missing optional legend is not a failed requested read.
use crate::{
    proto::{LteCaps, PlmnMap, UeCaps},
    wire,
};
use std::{
    fs,
    path::{Path, PathBuf},
};

#[derive(Clone, Copy)]
pub(super) enum DecodeKind {
    Carrier,
    Lte,
    Legend,
}
pub(super) enum Payload {
    Carrier(UeCaps),
    Lte(LteCaps),
    Legend(PlmnMap),
}
pub(super) struct LoadedInput {
    pub path: PathBuf,
    pub payload: Payload,
}
#[derive(Debug, thiserror::Error)]
pub(super) enum InputError {
    #[error("cannot read {path}: {source}")]
    Read {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("{path}: {source:#}")]
    Wire {
        path: PathBuf,
        source: anyhow::Error,
    },
}
pub(super) fn read_input(path: &Path, kind: DecodeKind) -> Result<LoadedInput, InputError> {
    let data = fs::read(path).map_err(|source| InputError::Read {
        path: path.into(),
        source,
    })?;
    let name = path.to_string_lossy();
    let payload = match kind {
        DecodeKind::Carrier => wire::decode_uecaps(&data, &name).map(Payload::Carrier),
        DecodeKind::Lte => wire::decode_lte_caps(&data, &name).map(Payload::Lte),
        DecodeKind::Legend => wire::decode_plmn_map(&data, &name).map(Payload::Legend),
    }
    .map_err(|source| InputError::Wire {
        path: path.into(),
        source,
    })?;
    Ok(LoadedInput {
        path: path.into(),
        payload,
    })
}
pub(super) fn read_optional_legend(dir: &Path) -> Result<Option<LoadedInput>, InputError> {
    let path = dir.join("ap_plmn_mapping.binarypb");
    match fs::symlink_metadata(&path) {
        Ok(_) => read_input(&path, DecodeKind::Legend).map(Some),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(source) => Err(InputError::Read { path, source }),
    }
}
