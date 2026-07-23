//! Fail-closed protobuf decoders for every modeled on-disk message kind.

use crate::proto::{LteCaps, PlmnMap, UeCaps};
use anyhow::{Context, ensure};
use prost::Message;

/// Top-level protobuf message encoded by a capability or mapping file.
#[derive(Clone, Copy, Debug)]
pub(crate) enum RootMessage {
    UeCaps,
    LteCaps,
    PlmnMap,
}

/// Every message reachable from a supported root.
#[derive(Clone, Copy, Debug)]
enum ModeledMessage {
    UeCaps,
    ComboGroup,
    Header,
    Combo,
    Cc,
    DlFeature,
    UlFeature,
    LteCaps,
    LteCombo,
    LteComponent,
    PlmnMap,
    Carrier,
}

impl RootMessage {
    const fn modeled(self) -> ModeledMessage {
        match self {
            Self::UeCaps => ModeledMessage::UeCaps,
            Self::LteCaps => ModeledMessage::LteCaps,
            Self::PlmnMap => ModeledMessage::PlmnMap,
        }
    }
}

impl ModeledMessage {
    const fn name(self) -> &'static str {
        match self {
            Self::UeCaps => "UeCaps",
            Self::ComboGroup => "ComboGroup",
            Self::Header => "ComboGroup.ComboHeader",
            Self::Combo => "ComboGroup.Combo",
            Self::Cc => "ComboGroup.Combo.SubBlock",
            Self::DlFeature => "ShannonFeatureSetDlPerCCNr",
            Self::UlFeature => "ShannonFeatureSetUlPerCCNr",
            Self::LteCaps => "LteCaps",
            Self::LteCombo => "LteCombo",
            Self::LteComponent => "LteComponent",
            Self::PlmnMap => "PlmnMap",
            Self::Carrier => "Carrier",
        }
    }
}

/// The expected payload for a modeled field.
#[derive(Clone, Copy, Debug)]
enum ModeledField {
    Varint,
    Bytes,
    Message(ModeledMessage),
}

impl ModeledField {
    const fn wire_type(self) -> u64 {
        match self {
            Self::Varint => 0,
            Self::Bytes | Self::Message(_) => 2,
        }
    }
}

/// Return the exact modeled payload type for one field. Keeping scalar, bytes, and
/// nested-message fields distinct makes the scanner enforce wire types before prost
/// can accept compatible-but-unfaithful encodings (notably packed PLMN varints).
const fn modeled_field(message: ModeledMessage, field: u64) -> Option<ModeledField> {
    use ModeledField::{Bytes, Message, Varint};
    use ModeledMessage::{
        Carrier, Cc, Combo, ComboGroup, DlFeature, Header, LteCaps, LteCombo, LteComponent,
        PlmnMap, UeCaps, UlFeature,
    };

    Some(match (message, field) {
        (UeCaps, 1 | 2 | 9) => Varint,
        (UeCaps, 3) => Message(ComboGroup),
        (UeCaps, 6) => Message(DlFeature),
        (UeCaps, 7) => Message(UlFeature),
        (ComboGroup, 1) => Message(Header),
        (ComboGroup, 2) => Message(Combo),
        (Header, 1..=5) => Varint,
        (Combo, 1) => Message(Cc),
        (Combo, 2) => Varint,
        (Cc, 1..=5 | 8) => Varint,
        (Cc, 6 | 7) => Bytes,
        (DlFeature, 1..=5) => Varint,
        (UlFeature, 1..=6) => Varint,
        (LteCaps, 1 | 3) => Varint,
        (LteCaps, 2) => Message(LteCombo),
        (LteCombo, 1) => Message(LteComponent),
        (LteCombo, 2..=4) => Varint,
        (LteComponent, 1..=3) => Varint,
        (PlmnMap, 1) => Message(Carrier),
        // `plmns` is intentionally unpacked. Length-delimited packed varints are not
        // accepted even though a protobuf decoder may treat them as wire-compatible.
        (Carrier, 1 | 2) => Varint,
        (Carrier, 3) => Bytes,
        _ => return None,
    })
}

fn read_varint(bytes: &[u8], offset: &mut usize) -> anyhow::Result<u64> {
    let mut shift = 0u32;
    let mut value = 0u64;
    loop {
        let byte = *bytes.get(*offset).context("truncated varint")?;
        *offset += 1;
        ensure!(shift < 63 || byte & 0x7f <= 1, "varint overflows u64");
        value |= u64::from(byte & 0x7f) << shift;
        if byte & 0x80 == 0 {
            return Ok(value);
        }
        shift += 7;
        ensure!(shift < 64, "varint too long");
    }
}

const fn wire_name(wire_type: u64) -> &'static str {
    match wire_type {
        0 => "varint",
        1 => "64-bit",
        2 => "length-delimited",
        3 => "start-group",
        4 => "end-group",
        5 => "32-bit",
        _ => "invalid",
    }
}

/// Consume one length-delimited field (a `Bytes` or nested `Message` field): read its
/// length prefix, slice out the payload, advance `offset` past it, and recurse into
/// [`scan`] when `modeled` names a nested message.
fn scan_length_delimited(
    bytes: &[u8],
    offset: &mut usize,
    message: ModeledMessage,
    field_number: u64,
    modeled: ModeledField,
) -> anyhow::Result<()> {
    let len = read_varint(bytes, offset).with_context(|| {
        format!(
            "reading the length of {} field #{field_number}",
            message.name()
        )
    })?;
    let len = usize::try_from(len)
        .with_context(|| format!("{} field #{field_number} is too large", message.name()))?;
    let end = offset
        .checked_add(len)
        .with_context(|| format!("{} field #{field_number} length overflows", message.name()))?;
    let payload = bytes
        .get(*offset..end)
        .with_context(|| format!("truncated {} field #{field_number}", message.name()))?;
    *offset = end;
    if let ModeledField::Message(child) = modeled {
        scan(payload, child)?;
    }
    Ok(())
}

/// Recursively walk one modeled message, rejecting unknown fields and incorrect wire
/// types before decoding can silently normalize or discard them.
fn scan(bytes: &[u8], message: ModeledMessage) -> anyhow::Result<()> {
    let mut offset = 0usize;
    while offset < bytes.len() {
        let key = read_varint(bytes, &mut offset)
            .with_context(|| format!("reading a field key in {}", message.name()))?;
        let field_number = key >> 3;
        let actual_wire = key & 7;
        let modeled = modeled_field(message, field_number).with_context(|| {
            format!(
                "{} field #{field_number} is not modeled; cannot guarantee a \
                 value-preserving round-trip",
                message.name()
            )
        })?;
        let expected_wire = modeled.wire_type();
        ensure!(
            actual_wire == expected_wire,
            "{} field #{field_number} expects {} (wire type {expected_wire}), found {} (wire \
             type {actual_wire})",
            message.name(),
            wire_name(expected_wire),
            wire_name(actual_wire),
        );

        match modeled {
            ModeledField::Varint => {
                read_varint(bytes, &mut offset)
                    .with_context(|| format!("reading {} field #{field_number}", message.name()))?;
            }
            ModeledField::Bytes | ModeledField::Message(_) => {
                scan_length_delimited(bytes, &mut offset, message, field_number, modeled)?;
            }
        }
    }
    Ok(())
}

/// Reject any field that the selected root schema cannot preserve exactly at value
/// level, including unknown fields nested inside modeled messages.
pub(crate) fn ensure_modeled(bytes: &[u8], root: RootMessage) -> anyhow::Result<()> {
    scan(bytes, root.modeled())
}

/// Strictly validate and decode an NR carrier capability message.
pub(crate) fn decode_uecaps(bytes: &[u8], label: &str) -> anyhow::Result<UeCaps> {
    ensure_modeled(bytes, RootMessage::UeCaps).with_context(|| format!("validating {label}"))?;
    UeCaps::decode(bytes).with_context(|| format!("decoding {label}"))
}

/// Strictly validate and decode an LTE fallback capability message.
pub(crate) fn decode_lte_caps(bytes: &[u8], label: &str) -> anyhow::Result<LteCaps> {
    ensure_modeled(bytes, RootMessage::LteCaps).with_context(|| format!("validating {label}"))?;
    LteCaps::decode(bytes).with_context(|| format!("decoding {label}"))
}

/// Strictly validate and decode a PLMN mapping message.
pub(crate) fn decode_plmn_map(bytes: &[u8], label: &str) -> anyhow::Result<PlmnMap> {
    ensure_modeled(bytes, RootMessage::PlmnMap).with_context(|| format!("validating {label}"))?;
    PlmnMap::decode(bytes).with_context(|| format!("decoding {label}"))
}

#[cfg(test)]
mod tests {
    use super::{RootMessage, decode_lte_caps, decode_plmn_map, decode_uecaps, ensure_modeled};
    use crate::proto::{
        Carrier, ComboGroup, LteCaps, LteCombo, LteComponent, PlmnMap, ShannonFeatureSetDlPerCcNr,
        ShannonFeatureSetUlPerCcNr, UeCaps,
        combo_group::{Combo, ComboHeader, combo::SubBlock},
    };
    use prost::Message;

    #[derive(Clone, Copy, Debug)]
    enum TestMessage {
        UeCaps,
        ComboGroup,
        Header,
        Combo,
        Cc,
        DlFeature,
        UlFeature,
        LteCaps,
        LteCombo,
        LteComponent,
        PlmnMap,
        Carrier,
    }

    impl TestMessage {
        const fn name(self) -> &'static str {
            match self {
                Self::UeCaps => "UeCaps",
                Self::ComboGroup => "ComboGroup",
                Self::Header => "ComboGroup.ComboHeader",
                Self::Combo => "ComboGroup.Combo",
                Self::Cc => "ComboGroup.Combo.SubBlock",
                Self::DlFeature => "ShannonFeatureSetDlPerCCNr",
                Self::UlFeature => "ShannonFeatureSetUlPerCCNr",
                Self::LteCaps => "LteCaps",
                Self::LteCombo => "LteCombo",
                Self::LteComponent => "LteComponent",
                Self::PlmnMap => "PlmnMap",
                Self::Carrier => "Carrier",
            }
        }

        fn valid_bytes(self) -> Vec<u8> {
            match self {
                Self::UeCaps => UeCaps::default().encode_to_vec(),
                Self::ComboGroup => ComboGroup::default().encode_to_vec(),
                Self::Header => ComboHeader::default().encode_to_vec(),
                Self::Combo => Combo::default().encode_to_vec(),
                Self::Cc => SubBlock::default().encode_to_vec(),
                Self::DlFeature => ShannonFeatureSetDlPerCcNr::default().encode_to_vec(),
                Self::UlFeature => ShannonFeatureSetUlPerCcNr::default().encode_to_vec(),
                Self::LteCaps => LteCaps::default().encode_to_vec(),
                Self::LteCombo => LteCombo::default().encode_to_vec(),
                Self::LteComponent => LteComponent::default().encode_to_vec(),
                Self::PlmnMap => PlmnMap::default().encode_to_vec(),
                Self::Carrier => Carrier::default().encode_to_vec(),
            }
        }

        fn wrap_in_root(self, payload: Vec<u8>) -> (RootMessage, Vec<u8>) {
            match self {
                Self::UeCaps => (RootMessage::UeCaps, payload),
                Self::ComboGroup => (RootMessage::UeCaps, length_delimited(3, payload)),
                Self::Header => (
                    RootMessage::UeCaps,
                    length_delimited(3, length_delimited(1, payload)),
                ),
                Self::Combo => (
                    RootMessage::UeCaps,
                    length_delimited(3, length_delimited(2, payload)),
                ),
                Self::Cc => (
                    RootMessage::UeCaps,
                    length_delimited(3, length_delimited(2, length_delimited(1, payload))),
                ),
                Self::DlFeature => (RootMessage::UeCaps, length_delimited(6, payload)),
                Self::UlFeature => (RootMessage::UeCaps, length_delimited(7, payload)),
                Self::LteCaps => (RootMessage::LteCaps, payload),
                Self::LteCombo => (RootMessage::LteCaps, length_delimited(2, payload)),
                Self::LteComponent => (
                    RootMessage::LteCaps,
                    length_delimited(2, length_delimited(1, payload)),
                ),
                Self::PlmnMap => (RootMessage::PlmnMap, payload),
                Self::Carrier => (RootMessage::PlmnMap, length_delimited(1, payload)),
            }
        }
    }

    const MESSAGES: &[TestMessage] = &[
        TestMessage::UeCaps,
        TestMessage::ComboGroup,
        TestMessage::Header,
        TestMessage::Combo,
        TestMessage::Cc,
        TestMessage::DlFeature,
        TestMessage::UlFeature,
        TestMessage::LteCaps,
        TestMessage::LteCombo,
        TestMessage::LteComponent,
        TestMessage::PlmnMap,
        TestMessage::Carrier,
    ];

    const MODELED_FIELDS: &[(TestMessage, &[(u64, u8)])] = &[
        (
            TestMessage::UeCaps,
            &[(1, 0), (2, 0), (3, 2), (6, 2), (7, 2), (9, 0)],
        ),
        (TestMessage::ComboGroup, &[(1, 2), (2, 2)]),
        (
            TestMessage::Header,
            &[(1, 0), (2, 0), (3, 0), (4, 0), (5, 0)],
        ),
        (TestMessage::Combo, &[(1, 2), (2, 0)]),
        (
            TestMessage::Cc,
            &[
                (1, 0),
                (2, 0),
                (3, 0),
                (4, 0),
                (5, 0),
                (6, 2),
                (7, 2),
                (8, 0),
            ],
        ),
        (
            TestMessage::DlFeature,
            &[(1, 0), (2, 0), (3, 0), (4, 0), (5, 0)],
        ),
        (
            TestMessage::UlFeature,
            &[(1, 0), (2, 0), (3, 0), (4, 0), (5, 0), (6, 0)],
        ),
        (TestMessage::LteCaps, &[(1, 0), (2, 2), (3, 0)]),
        (TestMessage::LteCombo, &[(1, 2), (2, 0), (3, 0), (4, 0)]),
        (TestMessage::LteComponent, &[(1, 0), (2, 0), (3, 0)]),
        (TestMessage::PlmnMap, &[(1, 2)]),
        (TestMessage::Carrier, &[(1, 0), (2, 0), (3, 2)]),
    ];

    fn push_varint(mut value: u64, out: &mut Vec<u8>) {
        while value >= 0x80 {
            out.push((value as u8 & 0x7f) | 0x80);
            value >>= 7;
        }
        out.push(value as u8);
    }

    fn encoded_varint(value: u64) -> Vec<u8> {
        let mut out = Vec::new();
        push_varint(value, &mut out);
        out
    }

    fn varint(field: u64, value: u64) -> Vec<u8> {
        let mut out = Vec::new();
        push_varint(field << 3, &mut out);
        out.extend(encoded_varint(value));
        out
    }

    fn length_delimited(field: u64, payload: Vec<u8>) -> Vec<u8> {
        let mut out = Vec::new();
        push_varint((field << 3) | 2, &mut out);
        push_varint(payload.len() as u64, &mut out);
        out.extend(payload);
        out
    }

    #[test]
    fn rejects_unknown_fields_at_every_message_depth() {
        for &message in MESSAGES {
            let mut tampered = message.valid_bytes();
            tampered.extend(varint(15, 1));
            let (root, bytes) = message.wrap_in_root(tampered);

            let error = ensure_modeled(&bytes, root).unwrap_err();
            let diagnostic = format!("{error:#}");
            assert!(
                diagnostic.contains(message.name()),
                "diagnostic for {message:?} did not name its message: {diagnostic}"
            );
            assert!(
                diagnostic.contains("field #15"),
                "diagnostic for {message:?} did not name its field: {diagnostic}"
            );
        }
    }

    #[test]
    fn rejects_the_wrong_wire_type_for_every_modeled_field() {
        for &(message, fields) in MODELED_FIELDS {
            for &(field, expected_wire) in fields {
                let (wrong_wire, invalid) = if expected_wire == 0 {
                    (2, length_delimited(field, Vec::new()))
                } else {
                    (0, varint(field, 0))
                };
                let (root, bytes) = message.wrap_in_root(invalid);

                let error = ensure_modeled(&bytes, root).unwrap_err();
                let diagnostic = format!("{error:#}");
                assert!(
                    diagnostic.contains(message.name()),
                    "diagnostic for {message:?} field #{field} did not name its message: \
                     {diagnostic}"
                );
                assert!(
                    diagnostic.contains(&format!("field #{field}")),
                    "diagnostic for {message:?} did not name field #{field}: {diagnostic}"
                );
                assert!(
                    diagnostic.contains(&format!("wire type {expected_wire}"))
                        && diagnostic.contains(&format!("wire type {wrong_wire}")),
                    "diagnostic for {message:?} field #{field} did not name expected/found wire \
                     types: {diagnostic}"
                );
            }
        }
    }

    #[test]
    fn carrier_plmns_rejects_packed_varints() {
        let packed_plmns = length_delimited(1, encoded_varint(302_220));
        let (root, bytes) = TestMessage::Carrier.wrap_in_root(packed_plmns);

        let error = ensure_modeled(&bytes, root).unwrap_err();
        let diagnostic = format!("{error:#}");
        assert!(diagnostic.contains("Carrier"), "{diagnostic}");
        assert!(diagnostic.contains("field #1"), "{diagnostic}");
        assert!(diagnostic.contains("wire type 0"), "{diagnostic}");
        assert!(diagnostic.contains("wire type 2"), "{diagnostic}");
    }

    #[test]
    fn typed_decoders_accept_valid_modeled_messages() {
        let uecaps = UeCaps {
            version: 874_888_686,
            ..Default::default()
        };
        let lte = LteCaps {
            fingerprint: 874_888_686,
            ..Default::default()
        };
        let mapping = PlmnMap {
            carriers: vec![Carrier {
                plmns: vec![197_154],
                index: 1,
                name: "TEST".to_string(),
            }],
        };

        assert_eq!(
            decode_uecaps(&uecaps.encode_to_vec(), "test NR file").unwrap(),
            uecaps
        );
        assert_eq!(
            decode_lte_caps(&lte.encode_to_vec(), "test LTE file").unwrap(),
            lte
        );
        assert_eq!(
            decode_plmn_map(&mapping.encode_to_vec(), "test mapping").unwrap(),
            mapping
        );
    }

    #[test]
    fn typed_decoder_validation_errors_include_the_input_label() {
        let mut bytes = UeCaps::default().encode_to_vec();
        bytes.extend(varint(15, 1));

        let error = decode_uecaps(&bytes, "carrier fixture").unwrap_err();
        assert!(format!("{error:#}").contains("carrier fixture"));
    }
}
