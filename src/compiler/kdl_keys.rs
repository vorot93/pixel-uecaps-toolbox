//! Every KDL node name and property key the compiler's source document uses, in one place.
//!
//! The reader and writer share these keys so vocabulary cannot drift between the two codecs.
//!
//! **Grouped by scope, and the grouping is load-bearing.** Two keys may share a spelling only if
//! they are never siblings. `tests::collisions_are_only_across_scopes` enforces that, so a key
//! added to the wrong group fails the suite instead of silently shadowing one.

/// Document level. Every name here is a sibling of every other: both radio kinds' nodes live in
/// one document, so this is one scope, and `tests::collisions_are_only_across_scopes` is what
/// keeps it that way.
pub(in crate::compiler) mod doc {
    /// Deliberately NOT abbreviated. A version mismatch must be diagnosable, and it cannot be if
    /// the marker announcing the version is itself renamed by the version change — the reader
    /// would reject the document as having an unknown top-level node before ever reaching the
    /// version check.
    pub(in crate::compiler) const VERSION: &str = "version";
    pub(in crate::compiler) const BITMASK_CARRIERS: &str = "bc";
    pub(in crate::compiler) const BITMASK_FINGERPRINT: &str = "bf";
    /// Carrier. `c` at every level: here, inside a `bf` group, and inside an `s` selection.
    pub(in crate::compiler) const CARRIER: &str = "c";
    pub(in crate::compiler) const LTE_FILE: &str = "f";
    pub(in crate::compiler) const DL_FEATURE: &str = "df";
    pub(in crate::compiler) const UL_FEATURE: &str = "uf";
    /// NR / EN-DC combo. Mirrors the `n<band>` sub-block prefix.
    pub(in crate::compiler) const NR_COMBO: &str = "n";
    /// LTE-fallback combo. Deliberately not `b`, which would make the header line read
    /// `b b=""` against the combo's own `bcs` property.
    pub(in crate::compiler) const LTE_COMBO: &str = "l";
}

/// Children of a `bitmask-fingerprint`.
pub(in crate::compiler) mod fingerprint {
    pub(in crate::compiler) const CARRIERS: &str = "c";
}

/// Properties and children of a `carrier`.
pub(in crate::compiler) mod carrier {
    pub(in crate::compiler) const BITMASK_ID: &str = "bi";
    pub(in crate::compiler) const PROFILED_ID: &str = "pi";
    pub(in crate::compiler) const MAPPING_ID: &str = "mi";
    pub(in crate::compiler) const SIGNATURE: &str = "sg";
    pub(in crate::compiler) const TIER: &str = "t";
    pub(in crate::compiler) const PLMN: &str = "p";
    pub(in crate::compiler) const PLMNS: &str = "ps";
    pub(in crate::compiler) const PROFILE: &str = "pf";
}

/// Properties of a `plmn`. Deliberately never abbreviated — 444 occurrences in the whole real
/// corpus, already minimal, and standard 3GPP terms.
///
/// The node itself is [`carrier::PLMN`]; these are its properties. Consumed by the PLMN codec in
/// `kdl_support`, which already depends on `raw_nr::SubBlockKind` and so is not a
/// vocabulary-free toolkit.
pub(in crate::compiler) mod plmn {
    pub(in crate::compiler) const MCC: &str = "mcc";
    pub(in crate::compiler) const MNC: &str = "mnc";
    /// Pins a 3-digit MNC that would otherwise print as 2 — the only legal override.
    pub(in crate::compiler) const MNC_DIGITS: &str = "mnc-digits";
}

/// Properties of a `profile`.
pub(in crate::compiler) mod profile {
    pub(in crate::compiler) const MULTIPLIER: &str = "x";
    pub(in crate::compiler) const UNKNOWN: &str = "u";
}

/// Properties and children of an NR combo. Named for its radio kind, paired with [`lte_combo`].
pub(in crate::compiler) mod nr_combo {
    pub(in crate::compiler) const POWER_CLASS: &str = "pc";
    pub(in crate::compiler) const BCS_NR: &str = "bn";
    pub(in crate::compiler) const BCS_INTRA_ENDC: &str = "bi";
    pub(in crate::compiler) const BCS_EUTRA: &str = "be";
    pub(in crate::compiler) const INTRA_BAND_EN_DC_SUPPORT: &str = "ie";
    pub(in crate::compiler) const SELECTION: &str = "s";
    /// Sub-block node-name prefix for an NR component. The band is appended to it.
    pub(in crate::compiler) const NR_PREFIX: &str = "n";
    /// Sub-block node-name prefix for an E-UTRA component.
    pub(in crate::compiler) const LTE_PREFIX: &str = "B";
}

/// Children of a `selection`.
pub(in crate::compiler) mod selection {
    pub(in crate::compiler) const CARRIERS: &str = "c";
    pub(in crate::compiler) const SKUS: &str = "m";
}

/// Properties of a sub-block inside an `n` combo. The two directions are **positional
/// arguments**, not properties — DL first, UL second — so `srs-tx-switch` is all that is left
/// here. See
/// `compiler::kdl_direction` for the value format.
pub(in crate::compiler) mod sub_block {
    pub(in crate::compiler) const SRS_TX_SWITCH: &str = "st";
}

/// Properties of a `df` catalog node.
pub(in crate::compiler) mod dl_catalog {
    pub(in crate::compiler) const MAX_SCS: &str = "s";
    pub(in crate::compiler) const MAX_MIMO: &str = "m";
    pub(in crate::compiler) const MAX_BW: &str = "b";
    pub(in crate::compiler) const MAX_MOD_ORDER: &str = "o";
    pub(in crate::compiler) const BW_90MHZ_SUPPORTED: &str = "w";
}

/// Properties of a `uf` catalog node.
pub(in crate::compiler) mod ul_catalog {
    pub(in crate::compiler) const MAX_SCS: &str = "s";
    pub(in crate::compiler) const MAX_MIMO_CB: &str = "m";
    pub(in crate::compiler) const MAX_BW: &str = "b";
    pub(in crate::compiler) const MAX_MOD_ORDER: &str = "o";
    pub(in crate::compiler) const BW_90MHZ_SUPPORTED: &str = "w";
    pub(in crate::compiler) const MAX_MIMO_NON_CB: &str = "nc";
}

/// Properties of an LTE file whitelist entry.
pub(in crate::compiler) mod lte_file {
    pub(in crate::compiler) const FINGERPRINT: &str = "fp";
    pub(in crate::compiler) const BITMASK: &str = "bm";
}

/// Properties and children of an LTE-fallback combo.
pub(in crate::compiler) mod lte_combo {
    pub(in crate::compiler) const BCS: &str = "b";
    pub(in crate::compiler) const UNKNOWN1: &str = "u1";
    pub(in crate::compiler) const UNKNOWN2: &str = "u2";
    pub(in crate::compiler) const SELECTION: &str = "s";
    /// Sub-block node-name prefix. The band is appended to it.
    pub(in crate::compiler) const SUB_BLOCK_PREFIX: &str = "B";
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    /// One scope's keys. Two keys may share a spelling across scopes but never within one.
    fn scopes() -> Vec<(&'static str, Vec<&'static str>)> {
        use super::{
            carrier, dl_catalog, doc, fingerprint, lte_combo, lte_file, nr_combo, plmn, profile,
            selection, sub_block, ul_catalog,
        };
        vec![
            (
                "doc",
                vec![
                    doc::VERSION,
                    doc::BITMASK_CARRIERS,
                    doc::BITMASK_FINGERPRINT,
                    doc::CARRIER,
                    doc::LTE_FILE,
                    doc::DL_FEATURE,
                    doc::UL_FEATURE,
                    doc::NR_COMBO,
                    doc::LTE_COMBO,
                ],
            ),
            ("fingerprint", vec![fingerprint::CARRIERS]),
            (
                "carrier",
                vec![
                    carrier::BITMASK_ID,
                    carrier::PROFILED_ID,
                    carrier::MAPPING_ID,
                    carrier::SIGNATURE,
                    carrier::TIER,
                    carrier::PLMN,
                    carrier::PLMNS,
                    carrier::PROFILE,
                ],
            ),
            ("plmn", vec![plmn::MCC, plmn::MNC, plmn::MNC_DIGITS]),
            ("profile", vec![profile::MULTIPLIER, profile::UNKNOWN]),
            (
                "nr_combo",
                vec![
                    nr_combo::POWER_CLASS,
                    nr_combo::BCS_NR,
                    nr_combo::BCS_INTRA_ENDC,
                    nr_combo::BCS_EUTRA,
                    nr_combo::INTRA_BAND_EN_DC_SUPPORT,
                    nr_combo::SELECTION,
                    nr_combo::NR_PREFIX,
                    nr_combo::LTE_PREFIX,
                ],
            ),
            ("selection", vec![selection::CARRIERS, selection::SKUS]),
            ("sub_block", vec![sub_block::SRS_TX_SWITCH]),
            (
                "dl_catalog",
                vec![
                    dl_catalog::MAX_SCS,
                    dl_catalog::MAX_MIMO,
                    dl_catalog::MAX_BW,
                    dl_catalog::MAX_MOD_ORDER,
                    dl_catalog::BW_90MHZ_SUPPORTED,
                ],
            ),
            (
                "ul_catalog",
                vec![
                    ul_catalog::MAX_SCS,
                    ul_catalog::MAX_MIMO_CB,
                    ul_catalog::MAX_BW,
                    ul_catalog::MAX_MOD_ORDER,
                    ul_catalog::BW_90MHZ_SUPPORTED,
                    ul_catalog::MAX_MIMO_NON_CB,
                ],
            ),
            ("lte_file", vec![lte_file::FINGERPRINT, lte_file::BITMASK]),
            (
                "lte_combo",
                vec![
                    lte_combo::BCS,
                    lte_combo::UNKNOWN1,
                    lte_combo::UNKNOWN2,
                    lte_combo::SELECTION,
                    lte_combo::SUB_BLOCK_PREFIX,
                ],
            ),
        ]
    }

    /// Keys within one scope are siblings, so a shared spelling would make one shadow the other.
    /// Across scopes, sharing is deliberate and legal (the top-level `carrier` node and a `bf`
    /// group's nested carriers list both spell `c`, at different nesting depths).
    #[test]
    fn collisions_are_only_across_scopes() {
        for (scope, keys) in scopes() {
            let unique: BTreeSet<&str> = keys.iter().copied().collect();
            assert_eq!(
                unique.len(),
                keys.len(),
                "scope `{scope}` has two keys with the same spelling: {keys:?}"
            );
        }
    }

    /// Every key must be a legal KDL bare identifier, or the writer would have to quote it and
    /// the round trip would not be byte-stable.
    #[test]
    fn every_key_is_a_bare_identifier() {
        for (scope, keys) in scopes() {
            for key in keys {
                assert!(!key.is_empty(), "scope `{scope}` has an empty key");
                assert!(
                    key.bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_'),
                    "scope `{scope}` key `{key}` is not a bare identifier"
                );
                assert!(
                    !key.as_bytes()[0].is_ascii_digit(),
                    "scope `{scope}` key `{key}` starts with a digit"
                );
            }
        }
    }
}
