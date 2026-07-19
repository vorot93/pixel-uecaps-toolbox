use std::{
    io::Write,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};
use tempfile::NamedTempFile;

/// A fully written, flushed, and synchronized temporary sibling whose final path has not yet
/// been replaced.
pub(crate) struct PreparedSiblingAtomic {
    temporary: NamedTempFile,
    path: PathBuf,
}

impl PreparedSiblingAtomic {
    /// Atomically replace the prepared final path. Dropping without persisting removes only the
    /// temporary sibling and leaves the final path unchanged.
    pub(crate) fn persist(self) -> Result<()> {
        let path = self.path;
        let persisted = self
            .temporary
            .persist(&path)
            .map_err(|error| error.error)
            .with_context(|| format!("persist temporary file as output {}", path.display()))?;
        drop(persisted);
        Ok(())
    }
}

/// Prepare a uniquely named temporary sibling without replacing `path`. The returned object is
/// ready to persist only after the writer has succeeded and the file is flushed and synchronized.
pub(crate) fn prepare_sibling_atomic(
    path: &Path,
    write: impl FnOnce(&mut dyn Write) -> Result<()>,
) -> Result<PreparedSiblingAtomic> {
    let parent = match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent,
        _ => Path::new("."),
    };
    let mut temporary = NamedTempFile::new_in(parent).with_context(|| {
        format!(
            "create sibling temporary file for output {}",
            path.display()
        )
    })?;

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
pub(crate) fn write_sibling_atomic(
    path: &Path,
    write: impl FnOnce(&mut dyn Write) -> Result<()>,
) -> Result<()> {
    prepare_sibling_atomic(path, write)?.persist()
}

pub(crate) fn write_bytes_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    write_sibling_atomic(path, |writer| {
        writer.write_all(bytes)?;
        Ok(())
    })
}

#[cfg(test)]
mod tests {
    use std::fs;

    use anyhow::bail;

    use super::{prepare_sibling_atomic, write_bytes_atomic, write_sibling_atomic};

    #[test]
    fn preparation_does_not_replace_output_until_explicit_persist() {
        let dir = tempfile::tempdir().unwrap();
        let output = dir.path().join("nr.kdl");
        fs::write(&output, b"original").unwrap();

        let prepared = prepare_sibling_atomic(&output, |writer| {
            writer.write_all(b"replacement")?;
            Ok(())
        })
        .unwrap();

        assert_eq!(fs::read(&output).unwrap(), b"original");
        prepared.persist().unwrap();
        assert_eq!(fs::read(&output).unwrap(), b"replacement");
    }

    #[test]
    fn writes_bytes_through_a_sibling_temporary_file() {
        let dir = tempfile::tempdir().unwrap();
        let output = dir.path().join("nr.kdl");

        write_bytes_atomic(&output, b"version 1\n").unwrap();

        assert_eq!(fs::read(&output).unwrap(), b"version 1\n");
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 1);
    }

    #[test]
    fn reports_a_missing_parent_directory() {
        let dir = tempfile::tempdir().unwrap();
        let output = dir.path().join("missing").join("nr.kdl");

        let error = write_bytes_atomic(&output, b"version 1\n").unwrap_err();

        assert!(
            error.to_string().contains("create sibling temporary file"),
            "unexpected error: {error:#}"
        );
        assert!(!output.exists());
    }

    #[test]
    fn removes_the_temporary_file_when_the_writer_fails() {
        let dir = tempfile::tempdir().unwrap();
        let output = dir.path().join("nr.kdl");

        let error = write_sibling_atomic(&output, |writer| {
            writer.write_all(b"partial")?;
            bail!("encode failed")
        })
        .unwrap_err();

        assert!(error.to_string().contains("write temporary file"));
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 0);
    }

    #[test]
    fn preserves_existing_output_when_byte_production_fails() {
        let dir = tempfile::tempdir().unwrap();
        let output = dir.path().join("nr.kdl");
        fs::write(&output, b"original").unwrap();

        let error = write_sibling_atomic(&output, |writer| {
            writer.write_all(b"replacement")?;
            bail!("encode failed")
        })
        .unwrap_err();

        assert!(error.to_string().contains("write temporary file"));
        assert_eq!(fs::read(&output).unwrap(), b"original");
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 1);
    }
}
