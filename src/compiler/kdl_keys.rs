//! Every KDL node name and property key the compiler's two source documents use, in one place.
//!
//! The reader and the writer both consume these. They used to spell all 121 keys independently
//! (`KdlNode::new("combo")` in the writer, `r.children("combo")` in the reader), which is a
//! drift hazard: a mismatched pair only fails if a test happens to exercise that key.
//!
//! **Grouped by scope, and the grouping is load-bearing.** Two keys may share a spelling only if
//! they are never siblings. [`tests::collisions_are_only_across_scopes`] enforces that, so a key
//! added to the wrong group fails the suite instead of silently shadowing one.

/// Document level in `nr.kdl`. Every name here is a sibling of every other.
pub(crate) mod nr_doc {
    /// Deliberately NOT abbreviated. A version mismatch must be diagnosable, and it cannot be if
    /// the marker announcing the version is itself renamed by the version change — the reader
    /// would reject the document as having an unknown top-level node before ever reaching the
    /// version check.
    pub(crate) const VERSION: &str = "version";
    pub(crate) const BITMASK_CARRIERS: &str = "bc";
    pub(crate) const BITMASK_FINGERPRINT: &str = "bf";
    pub(crate) const CARRIER: &str = "cr";
    pub(crate) const DL_FEATURE: &str = "df";
    pub(crate) const UL_FEATURE: &str = "uf";
    pub(crate) const COMBO: &str = "c";
}

/// Children of a `bitmask-fingerprint`.
pub(crate) mod fingerprint {
    pub(crate) const CARRIERS: &str = "c";
}

/// Properties and children of a `carrier`.
pub(crate) mod carrier {
    pub(crate) const BITMASK_ID: &str = "bi";
    pub(crate) const PROFILED_ID: &str = "pi";
    pub(crate) const MAPPING_ID: &str = "mi";
    pub(crate) const SIGNATURE: &str = "sg";
    pub(crate) const TIER: &str = "t";
    pub(crate) const PLMN: &str = "p";
    pub(crate) const PLMNS: &str = "ps";
    pub(crate) const PROFILE: &str = "pf";
}

/// Properties of a `plmn`. Deliberately never abbreviated — 444 occurrences in the whole real
/// corpus, already minimal, and standard 3GPP terms.
///
/// The node itself is [`carrier::PLMN`]; these are its properties. Consumed by the PLMN codec in
/// `kdl_support`, which already depends on `raw_nr::SubBlockKind` and so is not a
/// vocabulary-free toolkit.
pub(crate) mod plmn {
    pub(crate) const MCC: &str = "mcc";
    pub(crate) const MNC: &str = "mnc";
    /// Pins a 3-digit MNC that would otherwise print as 2 — the only legal override.
    pub(crate) const MNC_DIGITS: &str = "mnc-digits";
}

/// Properties of a `profile`.
pub(crate) mod profile {
    pub(crate) const MULTIPLIER: &str = "x";
    pub(crate) const UNKNOWN: &str = "u";
}

/// Properties and children of an `nr.kdl` `combo`.
pub(crate) mod combo {
    pub(crate) const POWER_CLASS: &str = "pc";
    pub(crate) const BCS_NR: &str = "bn";
    pub(crate) const BCS_INTRA_ENDC: &str = "bi";
    pub(crate) const BCS_EUTRA: &str = "be";
    pub(crate) const INTRA_BAND_EN_DC_SUPPORT: &str = "ie";
    pub(crate) const SELECTION: &str = "s";
    /// Sub-block node-name prefix for an NR component. The band is appended to it.
    pub(crate) const NR_PREFIX: &str = "n";
    /// Sub-block node-name prefix for an E-UTRA component.
    pub(crate) const LTE_PREFIX: &str = "B";
}

/// Children of a `selection`.
pub(crate) mod selection {
    pub(crate) const CARRIERS: &str = "c";
    pub(crate) const SKUS: &str = "m";
}

/// Properties of an `nr.kdl` sub-block. `DL`/`UL` each carry the bandwidth-class letter and the
/// per-CC index list in one value — see `compiler::kdl_direction`.
pub(crate) mod sub_block {
    pub(crate) const DL: &str = "d";
    pub(crate) const UL: &str = "u";
    pub(crate) const SRS_TX_SWITCH: &str = "st";
}

/// Properties of a `dl-feature` catalog node.
pub(crate) mod dl_catalog {
    pub(crate) const MAX_SCS: &str = "s";
    pub(crate) const MAX_MIMO: &str = "m";
    pub(crate) const MAX_BW: &str = "b";
    pub(crate) const MAX_MOD_ORDER: &str = "o";
    pub(crate) const BW_90MHZ_SUPPORTED: &str = "w";
}

/// Properties of a `ul-feature` catalog node.
pub(crate) mod ul_catalog {
    pub(crate) const MAX_SCS: &str = "s";
    pub(crate) const MAX_MIMO_CB: &str = "m";
    pub(crate) const MAX_BW: &str = "b";
    pub(crate) const MAX_MOD_ORDER: &str = "o";
    pub(crate) const BW_90MHZ_SUPPORTED: &str = "w";
    pub(crate) const MAX_MIMO_NON_CB: &str = "nc";
}

/// Document level in `lte.kdl`.
pub(crate) mod lte_doc {
    /// Not abbreviated, for the same reason as [`super::nr_doc::VERSION`].
    pub(crate) const VERSION: &str = "version";
    pub(crate) const FILE: &str = "f";
    pub(crate) const COMBO: &str = "c";
}

/// Properties of an `lte.kdl` `file`.
pub(crate) mod lte_file {
    pub(crate) const FINGERPRINT: &str = "fp";
    pub(crate) const BITMASK: &str = "bm";
}

/// Properties and children of an `lte.kdl` `combo`.
pub(crate) mod lte_combo {
    pub(crate) const BCS: &str = "b";
    pub(crate) const UNKNOWN1: &str = "u1";
    pub(crate) const UNKNOWN2: &str = "u2";
    pub(crate) const SELECTION: &str = "s";
    /// Sub-block node-name prefix. The band is appended to it.
    pub(crate) const SUB_BLOCK_PREFIX: &str = "B";
}

/// Properties of an `lte.kdl` sub-block: the class+MIMO bitfield as `<letter><mimo>`.
///
/// Spelled `d`/`u` like the `nr.kdl` sub-block's, because the document already fixes which
/// encoding applies — the same rule that lets a sub-block node name carry no radio-kind tag.
/// The Rust constants keep `_MIMO` precisely *because* the KDL spelling no longer distinguishes
/// them: `B66 d=C2` is a per-CC feature index in `nr.kdl` and a class+MIMO bitfield here.
pub(crate) mod lte_sub_block {
    pub(crate) const DL_MIMO: &str = "d";
    pub(crate) const UL_MIMO: &str = "u";
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    /// One scope's keys. Two keys may share a spelling across scopes but never within one.
    fn scopes() -> Vec<(&'static str, Vec<&'static str>)> {
        use super::{
            carrier, combo, dl_catalog, fingerprint, lte_combo, lte_doc, lte_file, lte_sub_block,
            nr_doc, plmn, profile, selection, sub_block, ul_catalog,
        };
        vec![
            (
                "nr_doc",
                vec![
                    nr_doc::VERSION,
                    nr_doc::BITMASK_CARRIERS,
                    nr_doc::BITMASK_FINGERPRINT,
                    nr_doc::CARRIER,
                    nr_doc::DL_FEATURE,
                    nr_doc::UL_FEATURE,
                    nr_doc::COMBO,
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
                "combo",
                vec![
                    combo::POWER_CLASS,
                    combo::BCS_NR,
                    combo::BCS_INTRA_ENDC,
                    combo::BCS_EUTRA,
                    combo::INTRA_BAND_EN_DC_SUPPORT,
                    combo::SELECTION,
                    combo::NR_PREFIX,
                    combo::LTE_PREFIX,
                ],
            ),
            ("selection", vec![selection::CARRIERS, selection::SKUS]),
            (
                "sub_block",
                vec![sub_block::DL, sub_block::UL, sub_block::SRS_TX_SWITCH],
            ),
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
            (
                "lte_doc",
                vec![lte_doc::VERSION, lte_doc::FILE, lte_doc::COMBO],
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
            (
                "lte_sub_block",
                vec![lte_sub_block::DL_MIMO, lte_sub_block::UL_MIMO],
            ),
        ]
    }

    /// Keys within one scope are siblings, so a shared spelling would make one shadow the other.
    /// Across scopes, sharing is deliberate and legal (`combo` and `carriers` both abbreviate to
    /// `c`, at different nesting depths).
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
