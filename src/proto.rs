//! prost-generated message types (built from `proto/ue_caps.proto` by build.rs).
#![allow(clippy::all)]
include!(concat!(env!("OUT_DIR"), "/uecaps.rs"));

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

    // A Nested2 with an explicit bitmask=0 must SERIALIZE the field (proto2-style
    // presence). Real files carry an explicit zero; plain proto3 drops it. Field 2,
    // varint wire type => tag 0x10; value 0 => 0x00. cc is empty so encodes nothing.
    #[test]
    fn nested2_encodes_explicit_zero_bitmask() {
        let n = combo_group::Nested2 {
            cc: vec![],
            bitmask: Some(0),
        };
        assert_eq!(n.encode_to_vec(), vec![0x10, 0x00]);
    }
}
