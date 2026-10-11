//! Strict, single-read inputs. A missing optional legend is not a failed requested read.
use super::validation::{Issue, carrier_context_issues, carrier_reference_issues, legend_issues};
use crate::{
    model::{FileKind, MAPPING_BASENAME},
    proto::{LteCaps, PlmnMap, UeCaps},
    wire,
};
use std::{
    fs,
    path::{Path, PathBuf},
};

#[derive(Debug)]
pub(super) enum FileEvidence {
    Carrier {
        carrier: String,
        number: u64,
        observation: Observation<UeCaps>,
    },
    Lte {
        id: u64,
        observation: Observation<LteCaps>,
    },
    Legend(Observation<PlmnMap>),
}
#[derive(Debug)]
pub(super) struct Observation<T> {
    pub path: PathBuf,
    pub value: Option<T>,
    pub issues: Vec<Issue>,
}

impl<T> Observation<T> {
    fn from_result(path: &Path, result: Result<T, InputError>) -> Self {
        let (value, issues) = match result {
            Ok(value) => (Some(value), Vec::new()),
            Err(error) => (None, vec![Issue::input(error)]),
        };
        Self {
            path: path.into(),
            value,
            issues,
        }
    }
}

impl FileEvidence {
    pub(super) fn path(&self) -> &Path {
        match self {
            Self::Carrier { observation, .. } => &observation.path,
            Self::Lte { observation, .. } => &observation.path,
            Self::Legend(observation) => &observation.path,
        }
    }
    pub(super) fn issues(&self) -> &[Issue] {
        match self {
            Self::Carrier { observation, .. } => &observation.issues,
            Self::Lte { observation, .. } => &observation.issues,
            Self::Legend(observation) => &observation.issues,
        }
    }
    pub(super) fn carrier(&self) -> Option<(&str, u64, &Observation<UeCaps>)> {
        match self {
            Self::Carrier {
                carrier,
                number,
                observation,
            } => Some((carrier, *number, observation)),
            _ => None,
        }
    }
    pub(super) fn legend(&self) -> Option<&Observation<PlmnMap>> {
        match self {
            Self::Legend(observation) => Some(observation),
            _ => None,
        }
    }
}

pub(super) fn load_validated(path: &Path, kind: FileKind<'_>) -> Option<FileEvidence> {
    Some(match kind {
        FileKind::Carrier { carrier, number } => {
            let mut observation = Observation::from_result(path, read_carrier(path));
            if let Some(caps) = &observation.value {
                observation
                    .issues
                    .extend(carrier_reference_issues(path, caps));
            }
            observation.issues.extend(carrier_context_issues(
                path,
                number,
                observation.value.as_ref(),
            ));
            FileEvidence::Carrier {
                carrier: carrier.into(),
                number,
                observation,
            }
        }
        FileKind::Lte(id) => FileEvidence::Lte {
            id,
            observation: Observation::from_result(path, read_lte(path)),
        },
        FileKind::Mapping => {
            let mut observation = Observation::from_result(path, read_legend(path));
            if let Some(map) = &observation.value {
                observation.issues.extend(legend_issues(path, map));
            }
            FileEvidence::Legend(observation)
        }
        FileKind::Other => return None,
    })
}
#[derive(Debug, thiserror::Error)]
pub(super) enum InputError {
    #[error("cannot read {path}")]
    Read {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("invalid input {path}")]
    Wire {
        path: PathBuf,
        source: anyhow::Error,
    },
}
fn read_bytes(path: &Path) -> Result<Vec<u8>, InputError> {
    fs::read(path).map_err(|source| InputError::Read {
        path: path.into(),
        source,
    })
}
pub(super) fn read_carrier(path: &Path) -> Result<UeCaps, InputError> {
    wire::decode_uecaps(&read_bytes(path)?, "UeCaps").map_err(|source| InputError::Wire {
        path: path.into(),
        source,
    })
}
pub(super) fn read_lte(path: &Path) -> Result<LteCaps, InputError> {
    wire::decode_lte_caps(&read_bytes(path)?, "LteCaps").map_err(|source| InputError::Wire {
        path: path.into(),
        source,
    })
}
pub(super) fn read_legend(path: &Path) -> Result<PlmnMap, InputError> {
    wire::decode_plmn_map(&read_bytes(path)?, "PlmnMap").map_err(|source| InputError::Wire {
        path: path.into(),
        source,
    })
}
pub(super) fn read_optional_legend(dir: &Path) -> Result<Option<PlmnMap>, InputError> {
    let path = dir.join(MAPPING_BASENAME);
    match fs::symlink_metadata(&path) {
        Ok(_) => read_legend(&path).map(Some),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(source) => Err(InputError::Read { path, source }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use prost::Message;

    #[test]
    fn error_chains_and_issue_diagnostics_include_path_and_cause_once() {
        let dir = tempfile::tempdir().unwrap();
        for (name, corrupt) in [("missing.binarypb", false), ("bad.binarypb", true)] {
            let path = dir.path().join(name);
            if corrupt {
                fs::write(&path, [0x78, 1]).unwrap();
            }
            let error = read_carrier(&path).unwrap_err();
            let cause = match &error {
                InputError::Read { source, .. } => source.to_string(),
                InputError::Wire { source, .. } => source.root_cause().to_string(),
            };
            let chain = format!("{:#}", anyhow::Error::from(error));
            assert_eq!(chain.matches(path.to_str().unwrap()).count(), 1, "{chain}");
            assert_eq!(chain.matches(&cause).count(), 1, "{chain}");
            let issue = Issue::input(read_carrier(&path).unwrap_err());
            let diagnostic = super::super::validation::diagnostics([&issue]);
            assert_eq!(
                diagnostic.matches(path.to_str().unwrap()).count(),
                1,
                "{diagnostic}"
            );
            assert_eq!(diagnostic.matches(&cause).count(), 1, "{diagnostic}");
        }
    }

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
        let error = read_carrier(&path).unwrap_err();
        assert!(matches!(error, InputError::Wire { .. }));
        assert!(
            format!(
                "{:#}",
                anyhow::Error::from(read_carrier(&path).unwrap_err())
            )
            .contains("field #15")
        );
        assert!(error.to_string().contains("VZW_311480.binarypb"));
    }
    #[test]
    fn distinguishes_an_io_failure_from_a_decode_failure() {
        let dir = tempfile::tempdir().unwrap();
        let err = read_carrier(&dir.path().join("absent.binarypb")).unwrap_err();
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
        let decoded = read_carrier(&path).unwrap();
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
        let decoded = read_optional_legend(dir.path()).unwrap().unwrap();
        assert_eq!(decoded, map);
        fs::write(&path, [0x78, 0x01]).unwrap();
        let error = read_optional_legend(dir.path()).unwrap_err();
        assert!(format!("{:#}", anyhow::Error::from(error)).contains("field #15"));
    }
}
