//! `magisk` — package UE-capability files into a flashable Magisk module (.zip).

use anyhow::bail;
use std::io::{Cursor, Write};
use zip::{CompressionMethod, DateTime, ZipWriter, write::SimpleFileOptions};

const UPDATE_BINARY: &str = include_str!("assets/update-binary");
const UPDATER_SCRIPT: &str = "#MAGISK\n";

/// The module's fixed on-device destination, without its leading `/` — the compiler is the
/// only caller and always writes a complete `/vendor/firmware/uecapconfig` replacement, so
/// this is a literal rather than a validated `--dest`.
const REPLACEMENT_PREFIX: &str = "vendor/firmware/uecapconfig";

/// The same path with its leading `/`, for the `module.prop` description line.
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

/// Assemble a complete deterministic uecapconfig replacement module in memory. Always writes
/// the `.replace` marker: this crate builds nothing but complete replacements.
pub(crate) fn replacement_module(inputs: &[ModuleEntry], name: &str) -> anyhow::Result<Vec<u8>> {
    validate_module_name(name)?;
    let inputs = sorted_inputs(inputs)?;
    let basenames: Vec<String> = inputs.iter().map(|(n, _)| n.clone()).collect();

    let mut zip = ZipWriter::new(Cursor::new(Vec::new()));

    zip.start_file("module.prop", opts(0o644))?;
    zip.write_all(module_prop(REPLACEMENT_DEST, name, &basenames).as_bytes())?;

    zip.start_file("META-INF/com/google/android/update-binary", opts(0o755))?;
    zip.write_all(UPDATE_BINARY.as_bytes())?;

    zip.start_file("META-INF/com/google/android/updater-script", opts(0o644))?;
    zip.write_all(UPDATER_SCRIPT.as_bytes())?;

    zip.start_file(module_path(REPLACEMENT_PREFIX, ".replace"), opts(0o644))?;
    zip.write_all(&[])?;

    for (basename, data) in &inputs {
        zip.start_file(module_path(REPLACEMENT_PREFIX, basename), opts(0o644))?;
        zip.write_all(data)?;
    }

    Ok(zip.finish()?.into_inner())
}

fn sorted_inputs(inputs: &[ModuleEntry]) -> anyhow::Result<Vec<ModuleEntry>> {
    let mut inputs = inputs.to_vec();
    inputs.sort_by(|left, right| left.0.cmp(&right.0));

    let mut previous: Option<&str> = None;
    for (basename, _) in &inputs {
        validate_module_basename(basename)?;
        if basename == ".replace" {
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

/// Reject a module name that could inject extra lines into `module.prop` — it is
/// interpolated as `name=<name>`, so a control or line-separator character would add a
/// second `id=`/`name=` line and change module identity.
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
        let zip = replacement_module(&inputs, "Pixel UE-caps override").unwrap();
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
    fn replacement_rejects_reserved_marker_basename() {
        let input = [(".replace".to_string(), Vec::new())];
        let error = replacement_module(&input, "replacement")
            .unwrap_err()
            .to_string();
        assert!(error.contains("reserved replacement marker"), "{error}");
    }

    #[test]
    fn update_binary_is_well_formed() {
        assert!(UPDATE_BINARY.starts_with("#!"));
        assert!(UPDATE_BINARY.contains("util_functions.sh"));
        assert!(UPDATE_BINARY.contains("install_module"));
    }

    #[test]
    fn name_with_newline_is_rejected() {
        // R11: --name is interpolated as `name=<name>`; a newline injects a second
        // `id=`/`name=` line and changes module identity.
        let inputs = vec![("x.binarypb".to_string(), vec![0u8])];
        let err = replacement_module(&inputs, "X\nid=evil")
            .unwrap_err()
            .to_string();
        assert!(err.contains("control or line-separator"), "{err}");
    }
}
