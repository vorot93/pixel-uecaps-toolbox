mod additional_features;
mod decompose;
mod feature_palette;
mod features;
mod kdl_bcs;
mod kdl_direction;
mod kdl_keys;
mod kdl_source;
mod lte;
mod nr;
mod provision;
mod schema;
mod selection;

#[cfg(test)]
mod test_support;

pub use decompose::decompose;
pub(in crate::compiler) use kdl_source::{read_sources, write_sources};
pub use provision::{load_sources, provision, provision_from_sources};
pub use schema::ValidatedSources;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::compiler) struct GeneratedFile {
    pub(in crate::compiler) basename: String,
    pub(in crate::compiler) bytes: Vec<u8>,
    pub(in crate::compiler) role: GeneratedRole,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::compiler) enum GeneratedRole {
    LegacyNr,
    ProfiledNr,
    Mapping,
    Lte { id: u64 },
}

/// Parse a **shortest-decimal** `u64`: `Some(n)` iff `s` is exactly `n`'s canonical decimal
/// text (no leading zeros, no sign, in range). The one source for the compiler's
/// shortest-decimal contract; callers map `None` to their own serde/anyhow error.
/// (`decompose::parse_filename_number` deliberately keeps its own two-step form — it reports a
/// distinct "does not fit u64" vs "must be shortest decimal" message, one of which is tested.)
pub(in crate::compiler) fn parse_shortest_u64(s: &str) -> Option<u64> {
    s.parse::<u64>().ok().filter(|n| n.to_string() == s)
}

/// Source traversal uses decimal-text order, even though identities are numeric.
pub(in crate::compiler) fn decimal_key_entries<T>(
    map: &std::collections::BTreeMap<u64, T>,
) -> Vec<(&u64, &T)> {
    let mut entries: Vec<_> = map.iter().collect();
    entries.sort_by_cached_key(|(id, _)| id.to_string());
    entries
}
