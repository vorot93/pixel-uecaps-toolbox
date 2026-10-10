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
#[derive(Debug)]
pub(super) enum Payload {
    Carrier(UeCaps),
    Lte(LteCaps),
    Legend(PlmnMap),
}
#[derive(Debug)]
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

#[cfg(test)]
mod tests {
    use super::*;
    use prost::Message;

    #[test]
    fn rejects_a_file_carrying_an_unmodeled_field() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("VZW_311480.binarypb");
        let mut bytes = UeCaps {
            version: 874_888_686,
            ..Default::default()
        }
        .encode_to_vec();
        bytes.extend([0x78, 0x01]);
        fs::write(&path, &bytes).unwrap();
        assert!(UeCaps::decode(&bytes[..]).is_ok());
        let error = read_input(&path, DecodeKind::Carrier).unwrap_err();
        assert!(matches!(error, InputError::Wire { .. }));
        assert!(error.to_string().contains("field #15"));
        assert!(error.to_string().contains("VZW_311480.binarypb"));
    }
    #[test]
    fn distinguishes_an_io_failure_from_a_decode_failure() {
        let dir = tempfile::tempdir().unwrap();
        let err = read_input(&dir.path().join("absent.binarypb"), DecodeKind::Carrier).unwrap_err();
        assert!(matches!(err, InputError::Read { .. }));
        assert!(err.to_string().contains("cannot read"));
        assert!(!err.to_string().contains("field #"));
        assert!(read_optional_legend(dir.path()).unwrap().is_none());
    }
    #[test]
    fn accepts_a_well_formed_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("VZW_311480.binarypb");
        let caps = UeCaps {
            version: 874_888_686,
            ..Default::default()
        };
        fs::write(&path, caps.encode_to_vec()).unwrap();
        let input = read_input(&path, DecodeKind::Carrier).unwrap();
        let Payload::Carrier(decoded) = input.payload else {
            panic!("wrong payload")
        };
        assert_eq!(decoded, caps);
    }
    #[test]
    fn legend_preserves_entries_and_wire_failure() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ap_plmn_mapping.binarypb");
        let map = PlmnMap {
            carriers: vec![crate::proto::Carrier {
                name: "TEST".into(),
                index: 63,
                plmns: vec![5_566_544],
            }],
        };
        fs::write(&path, map.encode_to_vec()).unwrap();
        let input = read_optional_legend(dir.path()).unwrap().unwrap();
        let Payload::Legend(decoded) = input.payload else {
            panic!("wrong payload")
        };
        assert_eq!(decoded, map);
        fs::write(&path, [0x78, 0x01]).unwrap();
        let error = read_optional_legend(dir.path()).unwrap_err();
        assert!(error.to_string().contains("field #15"));
    }
}
