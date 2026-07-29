//! UE-capability protobuf message types, hand-written with `#[derive(prost::Message)]`.
//!
//! Each field's `#[prost(...)]` attribute states its wire behavior directly: a bare scalar
//! uses proto3 default-skip (a zero/empty value is not emitted); an `optional` field carries
//! explicit presence, so `Some(0)` re-encodes as a present field instead of being dropped;
//! `packed = "false"` keeps a repeated scalar unpacked. Modeling that proto2-origin wire
//! format by hand is why these types are written out rather than generated from a `.proto`.

/// Full per-carrier UE-capability schema for <CARRIER>_<NUMBER>.binarypb, merged
/// from the reverse-engineered definition. Fields 1 (version/fingerprint) and 9
/// (unknown/reference) are typed uint64 — the tool reads and prints them as plain
/// numbers, so the wider type avoids any truncation while staying value-identical.
///
/// This schema reconstructs a proto2-origin wire format: repeated scalars are unpacked
/// (see `packed = "false"` on `plmns`) and some scalars carry explicit presence for their
/// default value. Such fields are `optional` so prost preserves the explicit value on
/// re-encode — a bare proto3 scalar would silently drop a default (e.g. a zero). Verified
/// against real Pixel dumps (mustang, cheetah).
#[derive(Clone, PartialEq, ::prost::Message)]
pub struct UeCaps {
    /// capability fingerprint -> family/tier
    #[prost(uint64, tag = "1")]
    pub version: u64,
    /// carrier ID
    #[prost(int32, optional, tag = "2")]
    pub id: Option<i32>,
    /// band-combination payload; empty => reference stub
    #[prost(message, repeated, tag = "3")]
    pub combo_groups: Vec<ComboGroup>,
    /// NR DL per-CC feature sets
    #[prost(message, repeated, tag = "6")]
    pub dl_feature_per_cc_list: Vec<ShannonFeatureSetDlPerCcNr>,
    /// NR UL per-CC feature sets
    #[prost(message, repeated, tag = "7")]
    pub ul_feature_per_cc_list: Vec<ShannonFeatureSetUlPerCcNr>,
    /// stub delegation reference
    #[prost(uint64, tag = "9")]
    pub unknown: u64,
}

#[derive(Clone, PartialEq, ::prost::Message)]
pub struct ComboGroup {
    #[prost(message, optional, tag = "1")]
    pub combo_header: Option<ComboHeader>,
    #[prost(message, repeated, tag = "2")]
    pub combo: Vec<Combo>,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, ::prost::Message)]
pub struct ComboHeader {
    #[prost(uint32, optional, tag = "1")]
    pub bcs_nr: Option<u32>,
    #[prost(uint32, optional, tag = "2")]
    pub bcs_intra_endc: Option<u32>,
    #[prost(uint32, optional, tag = "3")]
    pub bcs_eutra: Option<u32>,
    #[prost(int32, optional, tag = "4")]
    pub power_class: Option<i32>,
    #[prost(int32, optional, tag = "5")]
    pub intra_band_en_dc_support: Option<i32>,
}

#[derive(Clone, PartialEq, ::prost::Message)]
pub struct Combo {
    #[prost(message, repeated, tag = "1")]
    pub sub_blocks: Vec<SubBlock>,
    /// Explicit presence: real files carry an explicit bitmask=0 that a bare proto3 scalar
    /// would drop on re-encode, so it is `optional`. Unsigned: a bitmask is never
    /// negative (verified: max 32575 across real dumps, no value sets bit 31).
    #[prost(uint32, optional, tag = "2")]
    pub bitmask: Option<u32>,
}

/// One band + CA-bandwidth-class entry — NOT one component carrier. It physically
/// contains cc_count(bw_class) component carriers (a Samsung Shannon bw_class ->
/// CC-count table, kind-specific for NR vs E-UTRA; see src/raw_nr.rs's cc_count/
/// NR_CC_COUNTS/LTE_CC_COUNTS), e.g. band 78 class C = 2 CCs. dl_feature_per_cc_ids/
/// ul_feature_per_cc_ids each carry one selector byte per CC, in CC order, indexing
/// UeCaps.dl_feature_per_cc_list/ul_feature_per_cc_list (1-based; 0 = none).
#[derive(Clone, PartialEq, Eq, Hash, ::prost::Message)]
pub struct SubBlock {
    #[prost(int32, tag = "1")]
    pub band: i32,
    #[prost(int32, optional, tag = "2")]
    pub dl_bw_class: Option<i32>,
    #[prost(int32, optional, tag = "3")]
    pub ul_bw_class: Option<i32>,
    #[prost(int32, optional, tag = "4")]
    pub dl_feature_index: Option<i32>,
    #[prost(int32, optional, tag = "5")]
    pub ul_feature_index: Option<i32>,
    #[prost(bytes = "vec", optional, tag = "6")]
    pub dl_feature_per_cc_ids: Option<Vec<u8>>,
    #[prost(bytes = "vec", optional, tag = "7")]
    pub ul_feature_per_cc_ids: Option<Vec<u8>>,
    #[prost(int32, optional, tag = "8")]
    pub srstxswitch: Option<i32>,
}

/// **Field order is load-bearing.** The derived `Ord` on this type and its UL sibling fixes the
/// canonical order of `nr.kdl`'s feature catalogs, which fixes the 1-based selector bytes that
/// reference them, which fixes the generated `.binarypb` bytes. Reordering these fields changes
/// generated output for every carrier — do not reorder to match a doc, a spec, or taste.
///
/// The `Ord` derive is also why three hand-written `Ord`-only mirrors of these messages
/// (`DlFeatureSource`/`UlFeatureSource`, the `DlFeatureKey`/`UlFeatureKey` tuples, and
/// `LteSourceComponent`) no longer exist: they were needed only because the types used to be
/// generated by `prost-build`, which would not add derives, and each had to be kept
/// field-order-identical to this file by hand.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, ::prost::Message)]
pub struct ShannonFeatureSetDlPerCcNr {
    #[prost(int32, optional, tag = "1")]
    pub max_scs: Option<i32>,
    #[prost(int32, optional, tag = "2")]
    pub max_mimo: Option<i32>,
    #[prost(int32, optional, tag = "3")]
    pub max_bw: Option<i32>,
    #[prost(int32, optional, tag = "4")]
    pub max_mod_order: Option<i32>,
    #[prost(bool, optional, tag = "5")]
    pub bw_90mhz_supported: Option<bool>,
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, ::prost::Message)]
pub struct ShannonFeatureSetUlPerCcNr {
    #[prost(int32, optional, tag = "1")]
    pub max_scs: Option<i32>,
    #[prost(int32, optional, tag = "2")]
    pub max_mimo_cb: Option<i32>,
    #[prost(int32, optional, tag = "3")]
    pub max_bw: Option<i32>,
    #[prost(int32, optional, tag = "4")]
    pub max_mod_order: Option<i32>,
    #[prost(bool, optional, tag = "5")]
    pub bw_90mhz_supported: Option<bool>,
    #[prost(int32, optional, tag = "6")]
    pub max_mimo_non_cb: Option<i32>,
}

/// LTE-only fallback files (lte_*.binarypb): a Shannon-format LTE UE-capability blob.
/// Field 1 = fingerprint/version, field 2 = repeated CA combinations, field 3 = a bitmask.
/// Schema + class/MIMO semantics per the Shannon LTE editor reference; the opaque `unknown1`/
/// `unknown2` fields are widened to uint64 (real files carry 64-bit values in unknown1). `bcs`
/// is NOT opaque — see DESIGN.md's "BCS: a 3GPP bit string, not an opaque number" section. The
/// four `optional` fields carry explicit presence because real lte_*.binarypb files encode
/// them as explicit zeros; bare (non-`optional`) scalars would drop zeros on re-encode,
/// producing a 4 KB-smaller file.
///
/// The NON-optional scalars in the bit-identity messages — LteCaps.fingerprint/bitmask,
/// LteComponent.band and dl_bw_class_mimo, and Carrier.index below — deliberately assume no
/// observed file encodes an explicit zero for them (fingerprints are large; bands are >= 1;
/// DL is always an active class; bitmasks/indices are nonzero). The opt-in corpus
/// byte-identity checks would fail if one did, so this is verified, not assumed. A
/// foreign/future file that DID carry an explicit zero (e.g. dl_bw_class_mimo = 0
/// "DL disabled") would need `optional` here to round-trip bit-for-bit — do NOT add it on
/// spec alone; confirm against a real file and update the corpus test together, since the
/// opt-in corpus byte-identity checks are what would catch the regression.
#[derive(Clone, PartialEq, ::prost::Message)]
pub struct LteCaps {
    /// = version
    #[prost(uint64, tag = "1")]
    pub fingerprint: u64,
    #[prost(message, repeated, tag = "2")]
    pub combos: Vec<LteCombo>,
    #[prost(uint64, tag = "3")]
    pub bitmask: u64,
}

#[derive(Clone, PartialEq, ::prost::Message)]
pub struct LteCombo {
    #[prost(message, repeated, tag = "1")]
    pub components: Vec<LteComponent>,
    /// bandwidth combination set
    #[prost(uint64, optional, tag = "2")]
    pub bcs: Option<u64>,
    /// opaque (64-bit on real data)
    #[prost(uint64, optional, tag = "3")]
    pub unknown1: Option<u64>,
    /// opaque
    #[prost(uint64, optional, tag = "4")]
    pub unknown2: Option<u64>,
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, ::prost::Message)]
pub struct LteComponent {
    #[prost(int32, tag = "1")]
    pub band: i32,
    /// Shannon class+MIMO encoding; 0 = DL disabled
    #[prost(int32, tag = "2")]
    pub dl_bw_class_mimo: i32,
    /// 0 = UL disabled (explicit-presence: preserved on re-encode)
    #[prost(int32, optional, tag = "3")]
    pub ul_bw_class_mimo: Option<i32>,
}

/// ap_plmn_mapping.binarypb: the PLMN -> carrier legend.
#[derive(Clone, PartialEq, ::prost::Message)]
pub struct PlmnMap {
    #[prost(message, repeated, tag = "1")]
    pub carriers: Vec<Carrier>,
}

#[derive(Clone, PartialEq, Eq, Hash, ::prost::Message)]
pub struct Carrier {
    /// unpacked: required for bit-for-bit identity
    #[prost(uint64, repeated, packed = "false", tag = "1")]
    pub plmns: Vec<u64>,
    /// internal index (non-optional: assumes no explicit zero — see LteCaps note)
    #[prost(uint64, tag = "2")]
    pub index: u64,
    /// carrier-config name (== filename prefix)
    #[prost(string, tag = "3")]
    pub name: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use prost::Message;

    // Hand-encoded bytes verify our field numbers match the real wire layout.
    // field1=varint 300 (08 AC 02), field3=empty ComboGroup (1A 00), field9=varint 7 (48 07)
    #[test]
    fn decodes_with_payload() {
        let caps = UeCaps::decode(&[0x08, 0xAC, 0x02, 0x1A, 0x00, 0x48, 0x07][..]).unwrap();
        assert_eq!(caps.version, 300);
        assert_eq!(caps.unknown, 7);
        assert_eq!(caps.combo_groups.len(), 1); // field 3 present => not a stub
    }

    // Same but with no field 3 => a reference stub.
    #[test]
    fn decodes_stub() {
        let caps = UeCaps::decode(&[0x08, 0xAC, 0x02, 0x48, 0x07][..]).unwrap();
        assert_eq!(caps.version, 300);
        assert_eq!(caps.unknown, 7);
        assert!(caps.combo_groups.is_empty()); // stub
    }

    // A Carrier with index=0 and empty name encodes to ONLY its PLMNs.
    // Unpacked => each PLMN is a separate field-1 varint (tag 0x08).
    // Packed (proto3 default) would emit a single length-delimited field 1 (tag 0x0a).
    #[test]
    fn plmns_encode_unpacked() {
        let c = Carrier {
            plmns: vec![5],
            index: 0,
            name: String::new(),
        };
        assert_eq!(c.encode_to_vec(), vec![0x08, 0x05]);
    }

    // A Combo with an explicit bitmask=0 must SERIALIZE the field (proto2-style
    // presence). Real files carry an explicit zero; plain proto3 drops it. Field 2,
    // varint wire type => tag 0x10; value 0 => 0x00. sub_blocks is empty so encodes nothing.
    #[test]
    fn combo_encodes_explicit_zero_bitmask() {
        let n = Combo {
            sub_blocks: vec![],
            bitmask: Some(0),
        };
        assert_eq!(n.encode_to_vec(), vec![0x10, 0x00]);
    }
}
