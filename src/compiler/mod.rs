mod decompose;
pub(crate) mod features;
pub(crate) mod kdl_keys;
mod kdl_source;
pub(crate) mod lte;
pub(crate) mod nr;
mod provision;
pub(crate) mod schema;
pub(crate) mod selection;

#[cfg(test)]
pub(crate) mod test_support;

pub use decompose::decompose;
pub(crate) use kdl_source::{lte_from_kdl, lte_to_kdl, nr_from_kdl, nr_to_kdl};
pub use provision::{load_sources, provision, provision_from_sources};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct GeneratedFile {
    pub(crate) basename: String,
    pub(crate) bytes: Vec<u8>,
}

/// Parse a **shortest-decimal** `u64`: `Some(n)` iff `s` is exactly `n`'s canonical decimal
/// text (no leading zeros, no sign, in range). The one source for the compiler's
/// shortest-decimal contract; callers map `None` to their own serde/anyhow error.
/// (`decompose::parse_filename_number` deliberately keeps its own two-step form — it reports a
/// distinct "does not fit u64" vs "must be shortest decimal" message, one of which is tested.)
pub(crate) fn parse_shortest_u64(s: &str) -> Option<u64> {
    s.parse::<u64>().ok().filter(|n| n.to_string() == s)
}
