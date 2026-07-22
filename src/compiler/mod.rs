mod build;
mod decompose;
pub(crate) mod features;
mod kdl_source;
pub(crate) mod lte;
pub(crate) mod nr;
pub(crate) mod schema;
pub(crate) mod selection;

#[cfg(test)]
pub(crate) mod test_support;

pub use build::{build, build_from_sources, load_sources};
pub use decompose::decompose;
pub(crate) use kdl_source::{
    emit_dl_feature, emit_lte_combo, emit_nr_combo, emit_ul_feature, lte_from_kdl, lte_to_kdl,
    nr_from_kdl, nr_to_kdl,
};
pub(crate) use lte::lte_source_from_one_file;
pub(crate) use nr::nr_source_from_one_file;
pub use schema::ValidatedSources;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct GeneratedFile {
    pub(crate) basename: String,
    pub(crate) bytes: Vec<u8>,
}

/// Parse a **shortest-decimal** `u64`: `Some(n)` iff `s` is exactly `n`'s canonical decimal
/// text (no leading zeros, no sign, in range). The one source for the compiler's
/// shortest-decimal contract; callers map `None` to their own serde/anyhow error (C-dec).
/// (`decompose::parse_filename_number` deliberately keeps its own two-step form — it reports a
/// distinct "does not fit u64" vs "must be shortest decimal" message, one of which is tested.)
pub(crate) fn parse_shortest_u64(s: &str) -> Option<u64> {
    s.parse::<u64>().ok().filter(|n| n.to_string() == s)
}
