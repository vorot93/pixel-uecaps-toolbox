//! Shared validations retain independent failures; no compiler normalization here.
use super::{identity::project_combinations, input::InputError};
use crate::{
    mapping::Plmn,
    model::{MAIN_ONLY_ANCHORS, Tier, fingerprint_info, matching_anchors},
    proto::{PlmnMap, UeCaps},
};
use std::{
    collections::BTreeMap,
    fmt::Write,
    path::{Path, PathBuf},
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum IssueKind {
    Read,
    Wire,
    Reference,
    Anchor,
    Fingerprint,
    Legend,
    Collision,
    MixedTier,
}
#[derive(Debug, Clone)]
pub(super) struct Issue {
    pub kind: IssueKind,
    pub paths: Vec<PathBuf>,
    pub message: String,
}
impl Issue {
    pub fn new(kind: IssueKind, path: &Path, message: impl Into<String>) -> Self {
        Self {
            kind,
            paths: vec![path.into()],
            message: message.into(),
        }
    }
    pub fn input(path: &Path, error: InputError) -> Self {
        let kind = match error {
            InputError::Read { .. } => IssueKind::Read,
            InputError::Wire { .. } => IssueKind::Wire,
        };
        Self::new(kind, path, error.to_string())
    }
}
pub(super) fn diagnostics<'a>(issues: impl IntoIterator<Item = &'a Issue>) -> String {
    let mut out = String::new();
    for issue in issues {
        let paths = issue
            .paths
            .iter()
            .map(|p| p.display().to_string())
            .collect::<Vec<_>>()
            .join(", ");
        let _ = writeln!(out, "error: {paths} [{:?}]: {}", issue.kind, issue.message);
    }
    out
}
pub(super) fn legend_issues(path: &Path, map: &PlmnMap) -> Vec<Issue> {
    let mut names = BTreeMap::new();
    let mut indexes = BTreeMap::new();
    for c in &map.carriers {
        *names.entry(&c.name).or_insert(0) += 1;
        *indexes.entry(c.index).or_insert(0) += 1;
    }
    let mut issues = Vec::new();
    for (i, c) in map.carriers.iter().enumerate() {
        let context = format!("entry {} carrier {:?} index {}", i + 1, c.name, c.index);
        let mut add = |why: String| {
            issues.push(Issue::new(
                IssueKind::Legend,
                path,
                format!("{context}: {why}"),
            ))
        };
        if c.name.is_empty() {
            add("empty carrier name".into());
        }
        if names[&c.name] > 1 {
            add("duplicate carrier name".into());
        }
        if indexes[&c.index] > 1 {
            add("duplicate carrier index".into());
        }
        for (j, &value) in c.plmns.iter().enumerate() {
            if let Err(error) = Plmn::from_encoded(value) {
                add(format!("PLMN {} value {value}: {error}", j + 1));
            }
        }
    }
    issues
}
pub(super) fn carrier_reference_issues(path: &Path, caps: &UeCaps) -> Vec<Issue> {
    project_combinations(caps)
        .err()
        .unwrap_or_default()
        .into_iter()
        .map(|error| Issue::new(IssueKind::Reference, path, error.to_string()))
        .collect()
}
pub(super) fn carrier_context_issues(
    path: &Path,
    number: u64,
    caps: Option<&UeCaps>,
) -> Vec<Issue> {
    let anchors = matching_anchors(number);
    let mut issues = Vec::new();
    if anchors.len() != 1 {
        issues.push(Issue::new(
            IssueKind::Anchor,
            path,
            format!(
                "expected one anchor, found {}: {:?}",
                anchors.len(),
                anchors.iter().map(|p| p.anchor).collect::<Vec<_>>()
            ),
        ));
    }
    if let Some(caps) = caps {
        match fingerprint_info(caps.version) {
            None => issues.push(Issue::new(
                IssueKind::Fingerprint,
                path,
                format!("unknown fingerprint {}", caps.version),
            )),
            Some((family, tier)) if anchors.len() == 1 => {
                let profile = anchors[0];
                if family != profile.family {
                    issues.push(Issue::new(
                        IssueKind::Fingerprint,
                        path,
                        format!(
                            "fingerprint family {} != profile {} family {}",
                            family.description(),
                            profile.anchor,
                            profile.family.description()
                        ),
                    ));
                }
                if tier == Tier::Alt && MAIN_ONLY_ANCHORS.contains(&profile.anchor) {
                    issues.push(Issue::new(
                        IssueKind::Fingerprint,
                        path,
                        format!(
                            "Alt fingerprint {} is not eligible for Main-only profile anchor {}",
                            caps.version, profile.anchor
                        ),
                    ));
                }
            }
            _ => (),
        }
    }
    issues
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proto::Carrier;
    #[test]
    fn duplicate_indices_survive_original_entries() {
        let mut map = PlmnMap {
            carriers: vec![
                Carrier {
                    name: "VZW".into(),
                    index: 311_480,
                    plmns: vec![1_245_572],
                },
                Carrier {
                    name: "OTHER".into(),
                    index: 311_480,
                    plmns: vec![197_154],
                },
            ],
        };
        let issues = legend_issues(Path::new("legend"), &map);
        assert_eq!(issues.len(), 2);
        assert!(
            issues
                .iter()
                .all(|e| e.message.contains("duplicate carrier index"))
        );
        map.carriers[1].index = 2;
        assert!(legend_issues(Path::new("legend"), &map).is_empty());
    }
}
