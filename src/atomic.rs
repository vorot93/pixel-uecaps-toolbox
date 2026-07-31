use std::{
    io::Write,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};
use tempfile::NamedTempFile;

/// The mode `File::create` yields here, i.e. `0o666 & !umask` — what `fs::write` would have
/// produced for a brand-new output.
///
/// Measured once, in a private temp dir that is removed immediately, rather than read from
/// `umask(2)`: that syscall would mean a `libc` dependency for a single number, and the whole
/// point is to match what the standard library already does. Falls back to `0o644` if the probe
/// fails for any reason, which is the conventional result under the usual `022` umask.
#[cfg(unix)]
fn default_file_mode() -> u32 {
    use std::{os::unix::fs::PermissionsExt, sync::OnceLock};

    static MODE: OnceLock<u32> = OnceLock::new();
    *MODE.get_or_init(|| {
        let probe = || -> Option<u32> {
            let dir = tempfile::tempdir().ok()?;
            let file = std::fs::File::create(dir.path().join("probe")).ok()?;
            Some(file.metadata().ok()?.permissions().mode() & 0o777)
        };
        probe().unwrap_or(0o644)
    })
}

/// Give the not-yet-persisted temporary the mode its destination should end up with.
///
/// `NamedTempFile` hardcodes 0600 (correct for a temp file), and `persist` is a bare rename, so
/// without this every atomic write silently narrowed the permissions of whatever it replaced —
/// `provision -o module.zip` over a world-readable ZIP made it owner-only, and `decompose`'s
/// source document ignored the umask entirely.
#[cfg(unix)]
fn adopt_destination_mode(temporary: &NamedTempFile, path: &Path) -> Result<()> {
    use std::{fs::Permissions, os::unix::fs::PermissionsExt};

    let mode = match std::fs::metadata(path) {
        // Replacing an existing output: keep exactly the mode it already had.
        Ok(metadata) => metadata.permissions().mode() & 0o777,
        Err(_) => default_file_mode(),
    };
    temporary
        .as_file()
        .set_permissions(Permissions::from_mode(mode))
        .with_context(|| format!("set permissions for output {}", path.display()))
}

#[cfg(not(unix))]
fn adopt_destination_mode(_temporary: &NamedTempFile, _path: &Path) -> Result<()> {
    Ok(())
}

/// Flush the rename itself to disk. Without this the directory entry can be lost on a crash even
/// though the file's contents were already `sync_all`ed — the durability
/// [`PreparedSiblingAtomic`] documents was stronger than what it delivered.
#[cfg(unix)]
fn sync_parent_dir(path: &Path) -> Result<()> {
    let parent = parent_dir(path);
    std::fs::File::open(parent)
        .and_then(|dir| dir.sync_all())
        .with_context(|| format!("sync parent directory of output {}", path.display()))
}

#[cfg(not(unix))]
fn sync_parent_dir(_path: &Path) -> Result<()> {
    Ok(())
}

/// The directory an output lives in, treating a bare filename as the current directory.
fn parent_dir(path: &Path) -> &Path {
    match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent,
        _ => Path::new("."),
    }
}

/// The module's only entry point. The two-phase API below it exists so a writer can be fully
/// flushed and synced before anything replaces the destination; it stopped having an outside
/// caller when `decompose` went from writing a pair of files to writing one.
pub(crate) fn write_bytes_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    write_sibling_atomic(path, |writer| {
        writer.write_all(bytes)?;
        Ok(())
    })
}

/// A fully written, flushed, and synchronized temporary sibling whose final path has not yet
/// been replaced.
struct PreparedSiblingAtomic {
    temporary: NamedTempFile,
    path: PathBuf,
}

impl PreparedSiblingAtomic {
    /// Atomically replace the prepared final path. Dropping without persisting removes only the
    /// temporary sibling and leaves the final path unchanged.
    fn persist(self) -> Result<()> {
        let path = self.path;
        let persisted = self
            .temporary
            .persist(&path)
            .map_err(|error| error.error)
            .with_context(|| format!("persist temporary file as output {}", path.display()))?;
        drop(persisted);
        sync_parent_dir(&path)
    }
}

/// Prepare a uniquely named temporary sibling without replacing `path`. The returned object is
/// ready to persist only after the writer has succeeded and the file is flushed and synchronized.
fn prepare_sibling_atomic(
    path: &Path,
    write: impl FnOnce(&mut dyn Write) -> Result<()>,
) -> Result<PreparedSiblingAtomic> {
    let mut temporary = NamedTempFile::new_in(parent_dir(path)).with_context(|| {
        format!(
            "create sibling temporary file for output {}",
            path.display()
        )
    })?;
    adopt_destination_mode(&temporary, path)?;

    write(&mut temporary)
        .with_context(|| format!("write temporary file for output {}", path.display()))?;
    temporary
        .flush()
        .with_context(|| format!("flush temporary file for output {}", path.display()))?;
    temporary
        .as_file()
        .sync_all()
        .with_context(|| format!("sync temporary file for output {}", path.display()))?;

    Ok(PreparedSiblingAtomic {
        temporary,
        path: path.to_owned(),
    })
}

/// Write a file through a uniquely named temporary sibling, replacing `path` only after the
/// writer has succeeded and the temporary file has been flushed and synchronized.
fn write_sibling_atomic(
    path: &Path,
    write: impl FnOnce(&mut dyn Write) -> Result<()>,
) -> Result<()> {
    prepare_sibling_atomic(path, write)?.persist()
}

#[cfg(test)]
mod tests {
    use std::fs;

    use anyhow::bail;

    use super::{prepare_sibling_atomic, write_bytes_atomic, write_sibling_atomic};

    #[test]
    fn preparation_does_not_replace_output_until_explicit_persist() {
        let dir = tempfile::tempdir().unwrap();
        let output = tempfile::NamedTempFile::new_in(dir.path()).unwrap();
        fs::write(output.path(), b"original").unwrap();

        let prepared = prepare_sibling_atomic(output.path(), |writer| {
            writer.write_all(b"replacement")?;
            Ok(())
        })
        .unwrap();

        assert_eq!(fs::read(output.path()).unwrap(), b"original");
        prepared.persist().unwrap();
        assert_eq!(fs::read(output.path()).unwrap(), b"replacement");
    }

    #[test]
    fn writes_bytes_through_a_sibling_temporary_file() {
        let dir = tempfile::tempdir().unwrap();
        let output = tempfile::NamedTempFile::new_in(dir.path()).unwrap();

        write_bytes_atomic(output.path(), b"version 1\n").unwrap();

        assert_eq!(fs::read(output.path()).unwrap(), b"version 1\n");
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 1);
    }

    /// `NamedTempFile` creates with mode 0600 for security, and `persist` is a bare rename, so
    /// those bits landed on the destination — silently making a world-readable output
    /// owner-only. `provision -o module.zip` over an existing 0644 ZIP broke any second
    /// account, container user, or CI step that reads the artifact.
    #[test]
    fn preserves_the_replaced_files_permissions() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let output = dir.path().join("module.zip");
        fs::write(&output, b"original").unwrap();
        fs::set_permissions(&output, fs::Permissions::from_mode(0o644)).unwrap();

        write_bytes_atomic(&output, b"replacement").unwrap();

        let mode = fs::metadata(&output).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o644, "replacing a file must not narrow its mode");
    }

    #[test]
    fn preserves_existing_output_when_byte_production_fails() {
        let dir = tempfile::tempdir().unwrap();
        let output = tempfile::NamedTempFile::new_in(dir.path()).unwrap();
        fs::write(output.path(), b"original").unwrap();

        let error = write_sibling_atomic(output.path(), |writer| {
            writer.write_all(b"replacement")?;
            bail!("encode failed")
        })
        .unwrap_err();

        assert!(error.to_string().contains("write temporary file"));
        assert_eq!(fs::read(output.path()).unwrap(), b"original");
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 1);
    }
}
