use anyhow::{Context, Result, ensure};
use pixel_uecaps_toolbox::{
    model::CapabilityLayout,
    proto::{SubBlock, UeCaps},
};
use prost::Message;
use std::{
    env,
    ffi::OsString,
    fs,
    path::{Path, PathBuf},
    process::Command,
};

pub(super) struct Corpora {
    pub bitmask: PathBuf,
    pub profiled: PathBuf,
}

pub(super) fn corpora() -> Option<Corpora> {
    let (Some(bitmask), Some(profiled)) = (
        env::var_os("UECAPS_BITMASK_CORPUS"),
        env::var_os("UECAPS_PROFILED_CORPUS"),
    ) else {
        eprintln!("skipping optional corpus test: set both corpus variables");
        return None;
    };
    Some(Corpora {
        bitmask: bitmask.into(),
        profiled: profiled.into(),
    })
}

pub(super) fn output_root() -> Result<PathBuf> {
    resolve_output_root(env::var_os("UECAPS_CORPUS_OUTPUT_ROOT"))
}

fn resolve_output_root(value: Option<OsString>) -> Result<PathBuf> {
    let root = PathBuf::from(
        value.context("UECAPS_CORPUS_OUTPUT_ROOT is required for emitting corpus tests")?,
    )
    .canonicalize()
    .context("resolving corpus output root")?;
    validate_output_root(&root)?;
    Ok(root)
}

fn git(root: &Path, args: &[&str]) -> Result<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .output()
        .context("running corpus checkout validation")?;
    ensure!(
        out.status.success(),
        "git {args:?} in {} failed: {}",
        root.display(),
        String::from_utf8_lossy(&out.stderr)
    );
    Ok(String::from_utf8(out.stdout)?.trim().to_owned())
}

pub(super) fn validate_output_root(root: &Path) -> Result<()> {
    let root = root.canonicalize()?;
    ensure!(
        root.file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.ends_with("-decomposed")),
        "corpus output root must be a *-decomposed product checkout"
    );
    let toplevel = PathBuf::from(git(&root, &["rev-parse", "--show-toplevel"])?).canonicalize()?;
    ensure!(
        root == toplevel,
        "corpus output root must be the repository root"
    );
    let local = PathBuf::from(git(&root, &["rev-parse", "--absolute-git-dir"])?).canonicalize()?;
    let common = PathBuf::from(git(
        &root,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    )?)
    .canonicalize()?;
    ensure!(
        local == common,
        "corpus output root must be the primary checkout, not a linked worktree"
    );
    let branch = git(&root, &["symbolic-ref", "--short", "HEAD"])?;
    ensure!(
        branch != "master",
        "select a new corpus branch instead of master before emitting"
    );
    Ok(())
}

pub(super) fn is_uecaps_basename(layout: CapabilityLayout, name: &str) -> bool {
    let profiled_lte = name
        .strip_prefix("lte_")
        .and_then(|name| name.strip_suffix(".binarypb"))
        .is_some_and(|decimal| {
            !decimal.is_empty()
                && decimal.bytes().all(|byte| byte.is_ascii_digit())
                && decimal
                    .parse::<u64>()
                    .is_ok_and(|id| id.to_string() == decimal)
        });
    name.ends_with(".binarypb")
        && name != "ap_plmn_mapping.binarypb"
        && (matches!(layout, CapabilityLayout::Bitmask) || !profiled_lte)
}

pub(super) fn raw_carriers(
    bitmask: &Path,
    profiled: &Path,
) -> impl Iterator<Item = (PathBuf, UeCaps)> {
    let mut paths = Vec::new();
    for (directory, layout) in [
        (bitmask, CapabilityLayout::Bitmask),
        (
            profiled,
            CapabilityLayout::Profiled {
                nr_anchor: 0,
                lte_id: 0,
            },
        ),
    ] {
        for entry in fs::read_dir(directory).expect("reading raw corpus directory") {
            let entry = entry.expect("reading raw corpus entry");
            if entry
                .file_name()
                .to_str()
                .is_some_and(|name| is_uecaps_basename(layout, name))
            {
                paths.push(entry.path());
            }
        }
    }
    paths.sort();
    paths.into_iter().map(|path| {
        let bytes =
            fs::read(&path).unwrap_or_else(|error| panic!("reading {}: {error}", path.display()));
        let caps = UeCaps::decode(bytes.as_slice())
            .unwrap_or_else(|error| panic!("decoding raw carrier {}: {error}", path.display()));
        (path, caps)
    })
}

pub(super) fn components(caps: &UeCaps) -> impl Iterator<Item = &SubBlock> {
    caps.combo_groups
        .iter()
        .flat_map(|group| &group.combo)
        .flat_map(|combo| &combo.sub_blocks)
}

/// Independent all-or-nothing oracle, retaining every CC in selector order.
pub(super) fn resolve_selector<'a, T>(ids: Option<&[u8]>, list: &'a [T]) -> Option<Vec<&'a T>> {
    let ids = ids.filter(|ids| !ids.is_empty())?;
    ids.iter()
        .map(|&byte| {
            usize::from(byte)
                .checked_sub(1)
                .and_then(|index| list.get(index))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{fs, path::Path};

    fn repository(root: &Path) {
        fs::create_dir_all(root).unwrap();
        git(root, &["init", "-b", "master"]).unwrap();
        git(
            root,
            &[
                "-c",
                "user.name=Fixture",
                "-c",
                "user.email=fixture@example.invalid",
                "commit",
                "--allow-empty",
                "-m",
                "fixture",
            ],
        )
        .unwrap();
    }

    #[test]
    fn emitting_root_requires_primary_feature_branch_and_product_root() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("fixture-decomposed");
        repository(&root);
        assert!(
            validate_output_root(&root)
                .unwrap_err()
                .to_string()
                .contains("master")
        );
        git(&root, &["switch", "-c", "acceptance"]).unwrap();
        validate_output_root(&root).unwrap();
        let nested = root.join("nested-decomposed");
        fs::create_dir(&nested).unwrap();
        assert!(
            validate_output_root(&nested)
                .unwrap_err()
                .to_string()
                .contains("repository root")
        );
        let linked = temp.path().join("linked-decomposed");
        git(
            &root,
            &["worktree", "add", "-b", "linked", linked.to_str().unwrap()],
        )
        .unwrap();
        assert!(
            validate_output_root(&linked)
                .unwrap_err()
                .to_string()
                .contains("primary checkout")
        );
        git(&root, &["checkout", "--detach"]).unwrap();
        assert!(validate_output_root(&root).is_err());
        let unrelated = temp.path().join("unrelated");
        repository(&unrelated);
        git(&unrelated, &["switch", "-c", "acceptance"]).unwrap();
        assert!(
            validate_output_root(&unrelated)
                .unwrap_err()
                .to_string()
                .contains("-decomposed")
        );
    }

    #[test]
    fn failed_git_is_an_error_not_empty_success() {
        let temp = tempfile::tempdir().unwrap();
        assert!(git(temp.path(), &["rev-parse", "--show-toplevel"]).is_err());
    }

    #[test]
    fn missing_output_root_names_required_variable() {
        let error = resolve_output_root(None).unwrap_err();
        assert!(
            error.to_string().contains("UECAPS_CORPUS_OUTPUT_ROOT"),
            "{error}"
        );
    }

    #[test]
    fn selector_requires_every_byte_and_preserves_order() {
        let rows = [10, 20];
        assert_eq!(
            resolve_selector(Some(&[2, 1, 2]), &rows),
            Some(vec![&20, &10, &20])
        );
        for selector in [None, Some(&[][..]), Some(&[1, 0][..]), Some(&[1, 99][..])] {
            assert_eq!(resolve_selector(selector, &rows), None);
        }
    }
}
