//! Shared "write to a file or stdout" helper for the CLI output boundaries.

use anyhow::Context;
use std::{io::Write, path::Path};

/// Write `bytes` to `out` (a file path) or, when `out` is `None`, to stdout.
///
/// `what` names the payload for error messages: `"module"` → `writing module <path>` /
/// `writing module to stdout`; `""` → `writing <path>` / `writing to stdout`. Consolidates
/// the byte/text output tails of `patch create`/`apply`, `patch filter`, `magisk`, and
/// `provision`, which previously copy-pasted this match (C-write). Text callers pass
/// `text.as_bytes()`.
pub(crate) fn write_out(bytes: &[u8], out: Option<&Path>, what: &str) -> anyhow::Result<()> {
    let label = if what.is_empty() {
        String::new()
    } else {
        format!("{what} ")
    };
    match out {
        Some(path) => std::fs::write(path, bytes)
            .with_context(|| format!("writing {label}{}", path.display())),
        None => {
            let mut handle = std::io::stdout().lock();
            handle
                .write_all(bytes)
                .with_context(|| format!("writing {label}to stdout"))?;
            handle.flush().context("flushing stdout")
        }
    }
}
