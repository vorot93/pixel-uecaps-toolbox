//! `magisk` — package UE-capability files into a flashable Magisk module (.zip).

use anyhow::{Context, bail};
use std::{
    collections::BTreeSet,
    fs,
    io::{Cursor, Write},
    path::{Path, PathBuf},
};
use zip::{CompressionMethod, DateTime, ZipWriter, write::SimpleFileOptions};

const UPDATE_BINARY: &str = include_str!("assets/update-binary");
const UPDATER_SCRIPT: &str = "#MAGISK\n";
const REPLACEMENT_DEST: &str = "/vendor/firmware/uecapconfig";
const COMPRESSION_LEVEL: i64 = 9;

/// One Magisk-module input: its on-device basename and bytes.
pub(crate) type ModuleEntry = (String, Vec<u8>);

/// Shared zip entry options: deflate-compressed, with the given unix mode.
fn opts(mode: u32) -> SimpleFileOptions {
    SimpleFileOptions::default()
        .compression_method(CompressionMethod::Deflated)
        .compression_level(Some(COMPRESSION_LEVEL))
        .last_modified_time(DateTime::default())
        .unix_permissions(mode)
}

/// Assemble the module `.zip` in memory from already-read inputs (basename -> bytes).
pub(crate) fn build_module(
    inputs: &[ModuleEntry],
    dest: &str,
    name: &str,
) -> anyhow::Result<Vec<u8>> {
    build_archive(inputs, dest, name, false)
}

/// Assemble a complete deterministic uecapconfig replacement module in memory.
pub(crate) fn build_replacement_module(
    inputs: &[ModuleEntry],
    name: &str,
) -> anyhow::Result<Vec<u8>> {
    build_archive(inputs, REPLACEMENT_DEST, name, true)
}

/// Deterministic archive writer shared by ordinary overlays and complete replacements.
fn build_archive(
    inputs: &[ModuleEntry],
    dest: &str,
    name: &str,
    replacement: bool,
) -> anyhow::Result<Vec<u8>> {
    let prefix = dest_prefix(dest)?;
    validate_module_name(name)?;
    let inputs = sorted_inputs(inputs, replacement)?;
    let basenames: Vec<String> = inputs.iter().map(|(n, _)| n.clone()).collect();

    let mut zip = ZipWriter::new(Cursor::new(Vec::new()));

    zip.start_file("module.prop", opts(0o644))?;
    zip.write_all(module_prop(dest, name, &basenames).as_bytes())?;

    zip.start_file("META-INF/com/google/android/update-binary", opts(0o755))?;
    zip.write_all(UPDATE_BINARY.as_bytes())?;

    zip.start_file("META-INF/com/google/android/updater-script", opts(0o644))?;
    zip.write_all(UPDATER_SCRIPT.as_bytes())?;

    if replacement {
        zip.start_file(module_path(&prefix, ".replace"), opts(0o644))?;
        zip.write_all(&[])?;
    }

    for (basename, data) in &inputs {
        zip.start_file(module_path(&prefix, basename), opts(0o644))?;
        zip.write_all(data)?;
    }

    Ok(zip.finish()?.into_inner())
}

fn sorted_inputs(inputs: &[ModuleEntry], replacement: bool) -> anyhow::Result<Vec<ModuleEntry>> {
    let mut inputs = inputs.to_vec();
    inputs.sort_by(|left, right| left.0.cmp(&right.0));

    let mut previous: Option<&str> = None;
    for (basename, _) in &inputs {
        validate_module_basename(basename)?;
        if replacement && basename == ".replace" {
            bail!("module input basename `.replace` is the reserved replacement marker");
        }
        if previous == Some(basename) {
            bail!("duplicate module input basename {basename:?}");
        }
        previous = Some(basename);
    }
    Ok(inputs)
}

/// Reject names that could change the archive path or inject lines into `module.prop`.
pub(crate) fn validate_module_basename(basename: &str) -> anyhow::Result<()> {
    if basename.is_empty() || basename == "." || basename == ".." {
        bail!("module input {basename:?} must be a usable basename");
    }
    if basename.contains(['/', '\\']) {
        bail!("module input {basename:?} must be a basename without path separators");
    }
    if basename.chars().any(is_control_or_line_separator) {
        bail!("module input basename must not contain control or line-separator characters");
    }
    Ok(())
}

fn is_control_or_line_separator(character: char) -> bool {
    character.is_control() || matches!(character, '\u{2028}' | '\u{2029}')
}

/// Read the inputs, assemble the module, and write the `.zip` to `out` (or stdout).
pub fn package(
    files: &[PathBuf],
    dest: &str,
    out: Option<&Path>,
    name: &str,
) -> anyhow::Result<i32> {
    let mut inputs: Vec<ModuleEntry> = Vec::with_capacity(files.len());
    let mut seen = BTreeSet::new();
    for path in files {
        let basename = path
            .file_name()
            .and_then(|s| s.to_str())
            .with_context(|| format!("input has no valid file name: {}", path.display()))?
            .to_string();
        if !seen.insert(basename.clone()) {
            bail!("duplicate input file name {basename:?}; each must be unique within the module");
        }
        let data = fs::read(path).with_context(|| format!("reading input {}", path.display()))?;
        inputs.push((basename, data));
    }

    let zip = build_module(&inputs, dest, name)?;

    crate::output::write_out(&zip, out, "module")?;
    Ok(0)
}

/// Validate an absolute on-device directory and return it without its leading `/`
/// (and without a trailing `/`). `/vendor/firmware/uecapconfig` -> `vendor/firmware/uecapconfig`.
fn dest_prefix(dest: &str) -> anyhow::Result<String> {
    let trimmed = dest
        .strip_prefix('/')
        .with_context(|| format!("--dest must be an absolute path, got {dest:?}"))?
        .trim_end_matches('/');
    if trimmed.is_empty() {
        bail!("--dest must name a directory, not the filesystem root");
    }
    // `dest` becomes archive path segments and is interpolated into module.prop, so
    // reject anything that could escape the module tree on extraction or inject a
    // module.prop line (R11).
    if trimmed.chars().any(is_control_or_line_separator) {
        bail!("--dest must not contain control or line-separator characters");
    }
    for component in trimmed.split('/') {
        if component.is_empty() || component == "." || component == ".." {
            bail!("--dest must not contain empty, `.`, or `..` path components (got {dest:?})");
        }
    }
    Ok(trimmed.to_string())
}

/// Reject a module name that could inject extra lines into `module.prop` — it is
/// interpolated as `name=<name>`, so a control or line-separator character would add a
/// second `id=`/`name=` line and change module identity (R11).
fn validate_module_name(name: &str) -> anyhow::Result<()> {
    if name.is_empty() {
        bail!("module --name must not be empty");
    }
    if name.chars().any(is_control_or_line_separator) {
        bail!("module --name must not contain control or line-separator characters");
    }
    Ok(())
}

/// Map a slash-trimmed dest prefix and a file basename to its path inside the
/// module's `system/` overlay tree.
fn module_path(prefix: &str, basename: &str) -> String {
    format!("system/{prefix}/{basename}")
}

/// Render `module.prop` for the given on-device dest, module name, and input basenames.
fn module_prop(dest: &str, name: &str, basenames: &[String]) -> String {
    format!(
        "id=pixel_uecaps_override\n\
         name={name}\n\
         version=v1.0\n\
         versionCode=1\n\
         author=pixel-uecaps-toolbox\n\
         description=Overlays {n} file(s) onto {dest}: {list}\n",
        n = basenames.len(),
        list = basenames.join(", "),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dest_prefix_strips_slashes() {
        assert_eq!(
            dest_prefix("/vendor/firmware/uecapconfig").unwrap(),
            "vendor/firmware/uecapconfig"
        );
        assert_eq!(dest_prefix("/system/etc/foo/").unwrap(), "system/etc/foo");
    }

    #[test]
    fn non_absolute_dest_errors() {
        assert!(dest_prefix("vendor/firmware/uecapconfig").is_err());
    }

    #[test]
    fn root_dest_errors() {
        assert!(dest_prefix("/").is_err());
    }

    #[test]
    fn module_path_joins_under_system() {
        assert_eq!(
            module_path("vendor/firmware/uecapconfig", "x.binarypb"),
            "system/vendor/firmware/uecapconfig/x.binarypb"
        );
    }

    #[test]
    fn module_prop_has_fields_and_name_override() {
        let p = module_prop(
            "/vendor/firmware/uecapconfig",
            "My Mod",
            &["a.binarypb".to_string(), "b.binarypb".to_string()],
        );
        assert!(p.contains("id=pixel_uecaps_override\n"));
        assert!(p.contains("name=My Mod\n"));
        assert!(p.contains("author=pixel-uecaps-toolbox\n"));
        assert!(p.contains(
            "description=Overlays 2 file(s) onto /vendor/firmware/uecapconfig: a.binarypb, b.binarypb\n"
        ));
    }

    use std::io::Read;
    use zip::{DateTime, ZipArchive};

    const REPLACEMENT_DEST: &str = "/vendor/firmware/uecapconfig";

    fn replacement_module(inputs: &[(String, Vec<u8>)], name: &str) -> anyhow::Result<Vec<u8>> {
        build_replacement_module(inputs, name)
    }

    /// Read a produced zip back into a name -> bytes map (hermetic; no system `unzip`).
    fn entries(zip: &[u8]) -> std::collections::BTreeMap<String, Vec<u8>> {
        let mut archive = ZipArchive::new(Cursor::new(zip.to_vec())).unwrap();
        let mut out = std::collections::BTreeMap::new();
        for i in 0..archive.len() {
            let mut f = archive.by_index(i).unwrap();
            let name = f.name().to_string();
            let mut buf = Vec::new();
            f.read_to_end(&mut buf).unwrap();
            out.insert(name, buf);
        }
        out
    }

    #[test]
    fn builds_expected_entries() {
        let inputs = vec![
            ("VZW_1.binarypb".to_string(), vec![1u8, 2, 3]),
            ("ap_plmn_mapping.binarypb".to_string(), vec![9u8]),
        ];
        let zip = build_module(
            &inputs,
            "/vendor/firmware/uecapconfig",
            "Pixel UE-caps override",
        )
        .unwrap();
        let e = entries(&zip);
        assert!(e.contains_key("module.prop"));
        assert!(e.contains_key("META-INF/com/google/android/update-binary"));
        assert_eq!(
            e.get("META-INF/com/google/android/updater-script").unwrap(),
            b"#MAGISK\n"
        );
        assert_eq!(
            e.get("system/vendor/firmware/uecapconfig/VZW_1.binarypb")
                .unwrap(),
            &vec![1u8, 2, 3]
        );
        assert_eq!(
            e.get("system/vendor/firmware/uecapconfig/ap_plmn_mapping.binarypb")
                .unwrap(),
            &vec![9u8]
        );
    }

    #[test]
    fn replacement_archive_has_exact_order_bytes_and_metadata() {
        let inputs = vec![
            ("ZED.binarypb".to_string(), vec![9u8, 8]),
            ("ALPHA.binarypb".to_string(), vec![1u8, 2, 3]),
        ];
        let zip = replacement_module(&inputs, "Replacement").unwrap();
        let mut archive = ZipArchive::new(Cursor::new(zip)).unwrap();
        let expected = [
            ("module.prop", 0o644, None),
            (
                "META-INF/com/google/android/update-binary",
                0o755,
                Some(UPDATE_BINARY.as_bytes()),
            ),
            (
                "META-INF/com/google/android/updater-script",
                0o644,
                Some(UPDATER_SCRIPT.as_bytes()),
            ),
            (
                "system/vendor/firmware/uecapconfig/.replace",
                0o644,
                Some(&[][..]),
            ),
            (
                "system/vendor/firmware/uecapconfig/ALPHA.binarypb",
                0o644,
                Some(&[1u8, 2, 3][..]),
            ),
            (
                "system/vendor/firmware/uecapconfig/ZED.binarypb",
                0o644,
                Some(&[9u8, 8][..]),
            ),
        ];

        assert_eq!(archive.len(), expected.len());
        for (index, (name, mode, expected_bytes)) in expected.into_iter().enumerate() {
            let mut entry = archive.by_index(index).unwrap();
            assert_eq!(entry.name(), name, "entry {index}");
            assert_eq!(entry.last_modified(), Some(DateTime::default()), "{name}");
            assert_eq!(
                entry.unix_mode().map(|value| value & 0o777),
                Some(mode),
                "{name}"
            );
            assert_eq!(entry.compression(), CompressionMethod::Deflated, "{name}");

            let mut bytes = Vec::new();
            entry.read_to_end(&mut bytes).unwrap();
            if let Some(expected_bytes) = expected_bytes {
                assert_eq!(bytes, expected_bytes, "{name}");
            } else {
                let text = String::from_utf8(bytes).unwrap();
                assert!(text.contains("name=Replacement\n"), "{text}");
                assert!(text.contains("ALPHA.binarypb, ZED.binarypb\n"), "{text}");
            }
        }
    }

    #[test]
    fn replacement_archive_is_reproducible_across_input_orders() {
        let forward = vec![
            ("A.binarypb".to_string(), vec![1u8]),
            ("B.binarypb".to_string(), vec![2u8]),
        ];
        let reverse = vec![forward[1].clone(), forward[0].clone()];

        assert_eq!(
            replacement_module(&forward, "n").unwrap(),
            replacement_module(&reverse, "n").unwrap()
        );
    }

    #[test]
    fn replacement_rejects_duplicate_basenames() {
        let inputs = vec![
            ("A.binarypb".to_string(), vec![1u8]),
            ("A.binarypb".to_string(), vec![2u8]),
        ];
        let error = replacement_module(&inputs, "n").unwrap_err().to_string();
        assert!(error.contains("duplicate"), "{error}");
    }

    #[test]
    fn replacement_rejects_path_separators_in_basenames() {
        for basename in ["dir/A.binarypb", r"dir\A.binarypb"] {
            let error = replacement_module(&[(basename.into(), vec![1u8])], "n")
                .unwrap_err()
                .to_string();
            assert!(error.contains("basename"), "{error}");
        }
    }

    #[test]
    fn replacement_rejects_control_and_unicode_line_separators_in_basenames() {
        for character in ['\0', '\n', '\r', '\u{2028}', '\u{2029}'] {
            let basename = format!("BAD{character}NAME.binarypb");
            let error = replacement_module(&[(basename, vec![1u8])], "n")
                .unwrap_err()
                .to_string();
            assert!(error.contains("control or line-separator"), "{error:?}");
        }
    }

    #[test]
    fn replacement_rejects_reserved_marker_basename_but_ordinary_module_allows_it() {
        let input = [(".replace".to_string(), Vec::new())];
        let error = replacement_module(&input, "replacement")
            .unwrap_err()
            .to_string();
        assert!(error.contains("reserved replacement marker"), "{error}");

        let zip = build_module(&input, REPLACEMENT_DEST, "ordinary").unwrap();
        assert_eq!(
            entries(&zip)
                .get("system/vendor/firmware/uecapconfig/.replace")
                .unwrap(),
            &Vec::<u8>::new()
        );
    }

    #[test]
    fn ordinary_module_omits_replacement_marker() {
        let zip = build_module(
            &[("A.binarypb".to_string(), vec![1u8])],
            REPLACEMENT_DEST,
            "ordinary",
        )
        .unwrap();
        assert!(!entries(&zip).contains_key("system/vendor/firmware/uecapconfig/.replace"));
    }

    #[test]
    fn dest_override_changes_prefix() {
        let inputs = vec![("x.binarypb".to_string(), vec![0u8])];
        let zip = build_module(&inputs, "/system/etc/foo/", "n").unwrap();
        // leading slash stripped, trailing slash trimmed, `system/` prefixed (hence system/system).
        assert!(entries(&zip).contains_key("system/system/etc/foo/x.binarypb"));
    }

    #[test]
    fn update_binary_is_well_formed() {
        assert!(UPDATE_BINARY.starts_with("#!"));
        assert!(UPDATE_BINARY.contains("util_functions.sh"));
        assert!(UPDATE_BINARY.contains("install_module"));
    }

    #[test]
    fn package_writes_zip_to_out_file() {
        let dir = std::env::temp_dir().join(format!("uecaps-magisk-out-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let inp = dir.join("VZW_1.binarypb");
        fs::write(&inp, [7u8, 8, 9]).unwrap();
        let outp = dir.join("mod.zip");

        let code = package(
            &[inp],
            "/vendor/firmware/uecapconfig",
            Some(&outp),
            "Pixel UE-caps override",
        )
        .unwrap();
        let zip = fs::read(&outp).unwrap();
        fs::remove_dir_all(&dir).ok();

        assert_eq!(code, 0);
        let e = entries(&zip);
        assert!(e.contains_key("module.prop"));
        assert_eq!(
            e.get("system/vendor/firmware/uecapconfig/VZW_1.binarypb")
                .unwrap(),
            &vec![7u8, 8, 9]
        );
    }

    #[test]
    fn package_rejects_duplicate_basenames() {
        let dir = std::env::temp_dir().join(format!("uecaps-magisk-dup-{}", std::process::id()));
        let a = dir.join("a");
        let b = dir.join("b");
        fs::create_dir_all(&a).unwrap();
        fs::create_dir_all(&b).unwrap();
        fs::write(a.join("x.binarypb"), [1u8]).unwrap();
        fs::write(b.join("x.binarypb"), [2u8]).unwrap();

        let res = package(
            &[a.join("x.binarypb"), b.join("x.binarypb")],
            "/vendor/firmware/uecapconfig",
            None,
            "n",
        );
        fs::remove_dir_all(&dir).ok();

        assert!(res.is_err());
    }

    #[test]
    fn package_errors_on_missing_input() {
        let res = package(
            &[PathBuf::from("/no/such/file.binarypb")],
            "/vendor/firmware/uecapconfig",
            None,
            "n",
        );
        assert!(res.is_err());
    }

    #[test]
    fn dest_with_parent_traversal_is_rejected() {
        // R11: a --dest with `..` would emit archive entries that escape the module tree
        // on extraction (zip does no path sanitization). Reject it, don't build a
        // traversal path.
        let inputs = vec![("x.binarypb".to_string(), vec![0u8])];
        let err = build_module(&inputs, "/vendor/../../overlay", "n")
            .unwrap_err()
            .to_string();
        assert!(err.contains(".."), "{err}");
        // sanity: the same dest without traversal is still accepted
        assert!(build_module(&inputs, "/vendor/overlay", "n").is_ok());
    }

    #[test]
    fn dest_with_newline_is_rejected() {
        // R11: --dest is interpolated into the module.prop description line.
        let inputs = vec![("x.binarypb".to_string(), vec![0u8])];
        assert!(build_module(&inputs, "/vendor/x\nid=evil", "n").is_err());
    }

    #[test]
    fn name_with_newline_is_rejected() {
        // R11: --name is interpolated as `name=<name>`; a newline injects a second
        // `id=`/`name=` line and changes module identity.
        let inputs = vec![("x.binarypb".to_string(), vec![0u8])];
        let err = build_module(&inputs, "/vendor/firmware/uecapconfig", "X\nid=evil")
            .unwrap_err()
            .to_string();
        assert!(err.contains("control or line-separator"), "{err}");
    }
}
