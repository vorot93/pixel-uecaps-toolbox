//! Deterministic evidence collection shared by folder reports.
use super::{
    input::{FileEvidence, load_validated},
    validation::{Issue, IssueKind},
};
use crate::model::{
    Tier, classify_filename, fingerprint_info, matching_anchors, tier_profile_count,
};
use anyhow::Context;
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
};

pub(super) struct FolderFile {
    pub evidence: FileEvidence,
    pub anchor: Option<u64>,
}
pub(super) struct Candidate {
    pub path: PathBuf,
    pub carrier: String,
    pub number: u64,
    pub anchor: u64,
}
pub(super) struct CarrierCoverage {
    pub carrier: String,
    pub seen: usize,
    pub expected: usize,
    pub tier: Option<Tier>,
}
#[derive(Default)]
pub(super) struct FolderReport {
    pub files: Vec<FolderFile>,
    pub relational_errors: Vec<Issue>,
    pub unsupported: Vec<PathBuf>,
    pub candidates: Vec<Candidate>,
    pub coverage: Vec<CarrierCoverage>,
    pub stubs: Vec<PathBuf>,
    // None means an invalid legend makes membership unavailable.
    pub legend_gaps: Option<Vec<String>>,
}
impl FolderReport {
    pub(super) fn errors(&self) -> impl Iterator<Item = &Issue> {
        self.files
            .iter()
            .flat_map(|f| f.evidence.issues())
            .chain(&self.relational_errors)
    }
    pub(super) fn has_errors(&self) -> bool {
        self.errors().next().is_some()
    }
}

fn collect_binarypb(
    entries: impl IntoIterator<Item = std::io::Result<PathBuf>>,
) -> anyhow::Result<Vec<PathBuf>> {
    let mut paths = Vec::new();
    for entry in entries {
        let path = entry?;
        if path
            .file_name()
            .is_some_and(|n| n.as_encoded_bytes().ends_with(b".binarypb"))
        {
            paths.push(path);
        }
    }
    paths.sort();
    Ok(paths)
}
pub(super) fn enumerate_binarypb(dir: &Path) -> anyhow::Result<Vec<PathBuf>> {
    let entries =
        std::fs::read_dir(dir).with_context(|| format!("cannot enumerate {}", dir.display()))?;
    collect_binarypb(entries.map(|e| e.map(|e| e.path())))
        .with_context(|| format!("cannot finish enumerating {}", dir.display()))
}
pub(super) fn scan_folder(dir: &Path) -> anyhow::Result<FolderReport> {
    let paths = enumerate_binarypb(dir)?;
    let mut report = FolderReport::default();
    for path in paths {
        let Some(evidence) = load_validated(&path, classify_filename(&path)) else {
            report.unsupported.push(path);
            continue;
        };
        let anchor = evidence.carrier().and_then(|(_, number, _)| {
            match matching_anchors(number).as_slice() {
                [profile] => Some(profile.anchor),
                _ => None,
            }
        });
        report.files.push(FolderFile { evidence, anchor });
    }
    report.relational_errors =
        collect_candidates_and_collisions(&report.files, &mut report.candidates, &mut report.stubs);
    report.relational_errors.extend(collect_coverage_and_tiers(
        &report.files,
        &report.candidates,
        &mut report.coverage,
    ));
    report.legend_gaps = collect_legend_membership(&report.files);
    Ok(report)
}
fn collect_candidates_and_collisions(
    files: &[FolderFile],
    candidates: &mut Vec<Candidate>,
    stubs: &mut Vec<PathBuf>,
) -> Vec<Issue> {
    let mut errors = Vec::new();
    let mut cells: BTreeMap<(&str, u64), Vec<PathBuf>> = BTreeMap::new();
    for file in files {
        let Some((carrier, number, observation)) = file.evidence.carrier() else {
            continue;
        };
        if let Some(anchor) = file.anchor {
            cells
                .entry((carrier, anchor))
                .or_default()
                .push(observation.path.clone());
            if observation.issues.is_empty() {
                candidates.push(Candidate {
                    path: observation.path.clone(),
                    carrier: carrier.into(),
                    number,
                    anchor,
                });
                if let Some(caps) = &observation.value
                    && caps.combo_groups.is_empty()
                {
                    stubs.push(observation.path.clone());
                }
            }
        }
    }
    for ((carrier, anchor), paths) in cells {
        if paths.len() > 1 {
            errors.push(Issue {
                kind: IssueKind::Collision,
                message: format!(
                    "selector collision: {carrier} has {} files for anchor {anchor}",
                    paths.len()
                ),
                paths,
            });
        }
    }
    candidates.sort_by(|a, b| {
        (&a.carrier, a.anchor, a.number, &a.path).cmp(&(&b.carrier, b.anchor, b.number, &b.path))
    });
    errors
}

fn collect_coverage_and_tiers(
    files: &[FolderFile],
    candidates: &[Candidate],
    coverage: &mut Vec<CarrierCoverage>,
) -> Vec<Issue> {
    let mut errors = Vec::new();
    let mut carriers: BTreeMap<&str, Vec<&FolderFile>> = BTreeMap::new();
    for file in files {
        if let Some((carrier, _, _)) = file.evidence.carrier() {
            carriers.entry(carrier).or_default().push(file);
        }
    }
    for (carrier, files) in &carriers {
        let (mut main, mut alt) = (0, 0);
        for file in files {
            if let Some((_, _, observation)) = file.evidence.carrier()
                && let Some(caps) = &observation.value
                && let Some((_, tier)) = fingerprint_info(caps.version)
            {
                match tier {
                    Tier::Main => main += 1,
                    Tier::Alt => alt += 1,
                }
            }
        }
        if main > 0 && alt > 0 {
            errors.push(Issue {
                kind: IssueKind::MixedTier,
                paths: files.iter().map(|f| f.evidence.path().to_owned()).collect(),
                message: format!("{carrier} mixes tier fingerprints (alt={alt}, main={main})"),
            });
        }
        let tier = match (main, alt) {
            (0, 0) => None,
            (m, a) if a > m => Some(Tier::Alt),
            _ => Some(Tier::Main),
        };
        let seen = candidates
            .iter()
            .filter(|c| c.carrier == *carrier)
            .map(|c| c.anchor)
            .collect::<BTreeSet<_>>()
            .len();
        coverage.push(CarrierCoverage {
            carrier: (*carrier).into(),
            seen,
            expected: tier_profile_count(tier.unwrap_or(Tier::Main)),
            tier,
        });
    }
    errors
}

fn collect_legend_membership(files: &[FolderFile]) -> Option<Vec<String>> {
    let legend = files.iter().find_map(|f| f.evidence.legend());
    let names = match legend {
        Some(observation) if !observation.issues.is_empty() => return None,
        Some(observation) => observation
            .value
            .as_ref()?
            .carriers
            .iter()
            .map(|c| c.name.as_str())
            .collect(),
        _ => BTreeSet::new(),
    };
    Some(
        files
            .iter()
            .filter_map(|f| f.evidence.carrier().map(|(carrier, _, _)| carrier))
            .filter(|carrier| !names.contains(carrier))
            .collect::<BTreeSet<_>>()
            .into_iter()
            .map(str::to_owned)
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proto::UeCaps;
    use prost::Message;

    #[test]
    fn enumeration_error_is_fatal() {
        let err = collect_binarypb([
            Ok(PathBuf::from("A_167.binarypb")),
            Err(std::io::Error::other("injected iteration failure")),
        ])
        .unwrap_err();
        assert!(err.to_string().contains("injected iteration failure"));
        assert_eq!(
            collect_binarypb([
                Ok("z.binarypb".into()),
                Ok("notes.txt".into()),
                Ok(".binarypb".into())
            ])
            .unwrap(),
            vec![PathBuf::from(".binarypb"), PathBuf::from("z.binarypb")]
        );
    }
    fn write(dir: &Path, name: &str, version: u64) {
        std::fs::write(
            dir.join(name),
            UeCaps {
                version,
                ..Default::default()
            }
            .encode_to_vec(),
        )
        .unwrap();
    }
    #[test]
    fn invalid_collision_partner_does_not_hide_valid_candidate() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "ALPHA_66813533.binarypb", 862_505_271);
        std::fs::write(dir.path().join("ALPHA_133627066.binarypb"), [0x78, 1]).unwrap();
        let report = scan_folder(dir.path()).unwrap();
        assert_eq!(report.candidates.len(), 1);
        assert_eq!(report.candidates[0].anchor, 66_813_533);
        assert_eq!(report.coverage[0].seen, 1);
        assert_eq!(report.relational_errors[0].paths.len(), 2);
        assert_eq!(report.relational_errors[0].kind, IssueKind::Collision);
        assert!(report.errors().any(|e| e.kind == IssueKind::Wire));
    }
    #[test]
    fn tier_consistency_is_per_carrier_and_tie_uses_main() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "ALPHA_66813533.binarypb", 862_505_271);
        write(dir.path(), "BETA_66813533.binarypb", 627_223_094);
        let report = scan_folder(dir.path()).unwrap();
        assert!(!report.has_errors());
        assert_eq!(report.coverage[0].expected, 16);
        assert_eq!(report.coverage[1].expected, 14);
        std::fs::rename(
            dir.path().join("BETA_66813533.binarypb"),
            dir.path().join("ALPHA_133627066.binarypb"),
        )
        .unwrap();
        let report = scan_folder(dir.path()).unwrap();
        assert!(report.errors().any(|e| e.kind == IssueKind::MixedTier));
        assert_eq!(report.candidates.len(), 2);
        assert_eq!(report.coverage[0].seen, 1);
        assert_eq!(report.coverage[0].expected, 16);
    }
}
