//! Shared validations retain independent failures; no compiler normalization here.
use super::{
    identity::project_combinations,
    input::{InputError, LoadedInput, Payload},
};
use crate::{
    mapping::Plmn,
    model::{family_desc, fp_info, matching_anchors},
    outcome::Outcome,
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
pub(super) fn diagnostics(issues: &[Issue]) -> String {
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
pub(super) fn completion(has_errors: bool) -> Outcome {
    if has_errors {
        Outcome::Rejected
    } else {
        Outcome::Clean
    }
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
pub(super) fn payload_issues(input: &LoadedInput) -> Vec<Issue> {
    match &input.payload {
        Payload::Carrier(caps) => project_combinations(caps)
            .err()
            .unwrap_or_default()
            .into_iter()
            .map(|message| Issue::new(IssueKind::Reference, &input.path, message))
            .collect(),
        Payload::Legend(map) => legend_issues(&input.path, map),
        Payload::Lte(_) => Vec::new(),
    }
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
        match fp_info(caps.version) {
            None => issues.push(Issue::new(
                IssueKind::Fingerprint,
                path,
                format!("unknown fingerprint {}", caps.version),
            )),
            Some((family, _)) if anchors.len() == 1 && family != anchors[0].family => {
                issues.push(Issue::new(
                    IssueKind::Fingerprint,
                    path,
                    format!(
                        "fingerprint family {} != profile {} family {}",
                        family_desc(family),
                        anchors[0].anchor,
                        family_desc(anchors[0].family)
                    ),
                ))
            }
            _ => (),
        }
    }
    issues
}
