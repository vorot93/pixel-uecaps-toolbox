//! `magisk` — package UE-capability files into a flashable Magisk module (.zip).

use anyhow::{Context, bail};
use std::{
    collections::BTreeSet,
    fs,
    io::{Cursor, Write},
    path::{Path, PathBuf},
};
use zip::{CompressionMethod, ZipWriter, write::SimpleFileOptions};

const UPDATE_BINARY: &str = include_str!("assets/update-binary");
const UPDATER_SCRIPT: &str = "#MAGISK\n";

/// Shared zip entry options: deflate-compressed, with the given unix mode.
fn opts(mode: u32) -> SimpleFileOptions {
    SimpleFileOptions::default()
        .compression_method(CompressionMethod::Deflated)
        .unix_permissions(mode)
}

/// Assemble the module `.zip` in memory from already-read inputs (basename -> bytes).
pub(crate) fn build_module(
    inputs: &[(String, Vec<u8>)],
    dest: &str,
    name: &str,
) -> anyhow::Result<Vec<u8>> {
    let prefix = dest_prefix(dest)?;
    let basenames: Vec<String> = inputs.iter().map(|(n, _)| n.clone()).collect();

    let mut zip = ZipWriter::new(Cursor::new(Vec::new()));

    zip.start_file("module.prop", opts(0o644))?;
    zip.write_all(module_prop(dest, name, &basenames).as_bytes())?;

    zip.start_file("META-INF/com/google/android/update-binary", opts(0o755))?;
    zip.write_all(UPDATE_BINARY.as_bytes())?;

    zip.start_file("META-INF/com/google/android/updater-script", opts(0o644))?;
    zip.write_all(UPDATER_SCRIPT.as_bytes())?;

    for (basename, data) in inputs {
        zip.start_file(module_path(&prefix, basename), opts(0o644))?;
        zip.write_all(data)?;
    }

    Ok(zip.finish()?.into_inner())
}

/// Read the inputs, assemble the module, and write the `.zip` to `out` (or stdout).
pub fn package(
    files: &[PathBuf],
    dest: &str,
    out: Option<&Path>,
    name: &str,
) -> anyhow::Result<i32> {
    let mut inputs: Vec<(String, Vec<u8>)> = Vec::with_capacity(files.len());
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

    match out {
        Some(path) => {
            fs::write(path, &zip).with_context(|| format!("writing module {}", path.display()))?
        }
        None => {
            let mut handle = std::io::stdout().lock();
            handle.write_all(&zip).context("writing module to stdout")?;
            handle.flush().context("flushing stdout")?;
        }
    }
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
    Ok(trimmed.to_string())
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
    use zip::ZipArchive;

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
}
