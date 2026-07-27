//! Fail-closed protobuf decoders for every modeled on-disk message kind.

use crate::proto::{LteCaps, PlmnMap, UeCaps};
use anyhow::{Context, ensure};

/// Top-level protobuf message encoded by a capability or mapping file.
#[derive(Clone, Copy, Debug)]
pub(crate) enum RootMessage {
    UeCaps,
    LteCaps,
    PlmnMap,
}

/// Every message reachable from a supported root.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ModeledMessage {
    UeCaps,
    ComboGroup,
    Header,
    Combo,
    SubBlock,
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
    /// The `src/proto.rs` type name, so a diagnostic can be grepped straight to the struct.
    const fn name(self) -> &'static str {
        match self {
            Self::UeCaps => "UeCaps",
            Self::ComboGroup => "ComboGroup",
            Self::Header => "ComboHeader",
            Self::Combo => "Combo",
            Self::SubBlock => "SubBlock",
            Self::DlFeature => "ShannonFeatureSetDlPerCcNr",
            Self::UlFeature => "ShannonFeatureSetUlPerCcNr",
            Self::LteCaps => "LteCaps",
            Self::LteCombo => "LteCombo",
            Self::LteComponent => "LteComponent",
            Self::PlmnMap => "PlmnMap",
            Self::Carrier => "Carrier",
        }
    }
}

/// The declared integer width of a varint field, as written in `src/proto.rs`. prost decodes
/// a varint into the declared Rust type and re-encodes from it, so a wire value the type
/// cannot hold comes back as a *different number* — the width is what makes that detectable.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum IntWidth {
    U32,
    I32,
    U64,
    Bool,
}

impl IntWidth {
    const fn name(self) -> &'static str {
        match self {
            Self::U32 => "uint32",
            Self::I32 => "int32",
            Self::U64 => "uint64",
            Self::Bool => "bool",
        }
    }

    /// Whether `value` survives a decode/re-encode through this width byte-for-byte.
    const fn round_trips(self, value: u64) -> bool {
        match self {
            Self::U32 => value <= u32::MAX as u64,
            // An `int32` legitimately carries a negative as a 10-byte sign-extended varint,
            // so the test is not a magnitude bound: re-truncate and re-widen, and require the
            // original bytes back. This accepts `-1` as `u64::MAX` while rejecting a bare
            // `0xFFFF_FFFF`, which prost would read as `-1` and re-emit sign-extended.
            Self::I32 => (value as u32 as i32 as i64) as u64 == value,
            Self::U64 => true,
            // Any nonzero decodes to `true` and re-encodes as exactly 1.
            Self::Bool => value <= 1,
        }
    }
}

/// The expected payload for a modeled field.
#[derive(Clone, Copy, Debug)]
enum ModeledField {
    Varint(IntWidth),
    Bytes,
    Message(ModeledMessage),
}

impl ModeledField {
    const fn wire_type(self) -> u64 {
        match self {
            Self::Varint(_) => 0,
            Self::Bytes | Self::Message(_) => 2,
        }
    }
}

/// Return the exact modeled payload type for one field. Keeping scalar, bytes, and
/// nested-message fields distinct makes the scanner enforce wire types before prost
/// can accept compatible-but-unfaithful encodings (notably packed PLMN varints); the
/// [`IntWidth`] on each varint additionally pins the declared integer type, so a value too
/// wide for it cannot slip through and be re-encoded as a different number.
///
/// Every arm mirrors a `#[prost(...)]` attribute in `src/proto.rs` — the hand-written oracle
/// in this module's tests is what keeps the two in step.
const fn modeled_field(message: ModeledMessage, field: u64) -> Option<ModeledField> {
    use IntWidth::{Bool, I32, U32, U64};
    use ModeledField::{Bytes, Message, Varint};
    use ModeledMessage::{
        Carrier, Combo, ComboGroup, DlFeature, Header, LteCaps, LteCombo, LteComponent, PlmnMap,
        SubBlock, UeCaps, UlFeature,
    };

    Some(match (message, field) {
        (UeCaps, 1 | 9) => Varint(U64),
        (UeCaps, 2) => Varint(I32),
        (UeCaps, 3) => Message(ComboGroup),
        (UeCaps, 6) => Message(DlFeature),
        (UeCaps, 7) => Message(UlFeature),
        (ComboGroup, 1) => Message(Header),
        (ComboGroup, 2) => Message(Combo),
        (Header, 1..=3) => Varint(U32),
        (Header, 4..=5) => Varint(I32),
        (Combo, 1) => Message(SubBlock),
        (Combo, 2) => Varint(U32),
        (SubBlock, 1..=5 | 8) => Varint(I32),
        (SubBlock, 6 | 7) => Bytes,
        (DlFeature, 1..=4) => Varint(I32),
        (DlFeature, 5) => Varint(Bool),
        (UlFeature, 1..=4 | 6) => Varint(I32),
        (UlFeature, 5) => Varint(Bool),
        (LteCaps, 1 | 3) => Varint(U64),
        (LteCaps, 2) => Message(LteCombo),
        (LteCombo, 1) => Message(LteComponent),
        (LteCombo, 2..=4) => Varint(U64),
        (LteComponent, 1..=3) => Varint(I32),
        (PlmnMap, 1) => Message(Carrier),
        // `plmns` is intentionally unpacked. Length-delimited packed varints are not
        // accepted even though a protobuf decoder may treat them as wire-compatible.
        (Carrier, 1 | 2) => Varint(U64),
        (Carrier, 3) => Bytes,
        _ => return None,
    })
}

/// Whether a field may legitimately appear more than once in one message instance. prost keeps
/// only the last occurrence of a singular field and discards the rest, so this is what lets
/// [`scan`] tell a repeated field from a lossy duplicate.
const fn repeated_field(message: ModeledMessage, field: u64) -> bool {
    use ModeledMessage::{Carrier, Combo, ComboGroup, LteCaps, LteCombo, PlmnMap, UeCaps};

    matches!(
        (message, field),
        (UeCaps, 3 | 6 | 7)
            | (ComboGroup, 2)
            | (Combo, 1)
            | (LteCaps, 2)
            | (LteCombo, 1)
            | (PlmnMap, 1)
            | (Carrier, 1)
    )
}

/// Read one varint, also reporting whether it was encoded in more bytes than its value needs.
/// prost always emits the minimal form, so an overlong encoding decodes to the right number but
/// re-encodes to different bytes — indistinguishable from corruption at value level, and fatal
/// to byte-identity.
fn read_varint(bytes: &[u8], offset: &mut usize) -> anyhow::Result<(u64, bool)> {
    let start = *offset;
    let mut shift = 0u32;
    let mut value = 0u64;
    loop {
        let byte = *bytes.get(*offset).context("truncated varint")?;
        *offset += 1;
        ensure!(shift < 63 || byte & 0x7f <= 1, "varint overflows u64");
        value |= u64::from(byte & 0x7f) << shift;
        if byte & 0x80 == 0 {
            let minimal = (u64::BITS - value.leading_zeros()).div_ceil(7).max(1) as usize;
            return Ok((value, *offset - start > minimal));
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
    let (len, overlong) = read_varint(bytes, offset).with_context(|| {
        format!(
            "reading the length of {} field #{field_number}",
            message.name()
        )
    })?;
    ensure!(
        !overlong,
        "{} field #{field_number} has a non-minimally encoded length prefix",
        message.name(),
    );
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

/// Recursively walk one modeled message, rejecting anything decoding could silently normalize
/// or discard: unknown fields, incorrect wire types, out-of-range varints, repeats of a
/// singular field, descending tag order, and non-minimal varint encodings.
///
/// The state below is deliberately per-invocation, i.e. per *message instance* — a repeated
/// nested message gets a fresh `seen`/`last_field` on each recursion.
fn scan(bytes: &[u8], message: ModeledMessage) -> anyhow::Result<()> {
    let mut offset = 0usize;
    let mut seen = std::collections::BTreeSet::<u64>::new();
    let mut last_field: Option<u64> = None;
    while offset < bytes.len() {
        let (key, overlong) = read_varint(bytes, &mut offset)
            .with_context(|| format!("reading a field key in {}", message.name()))?;
        ensure!(
            !overlong,
            "{} has a non-minimally encoded field key",
            message.name(),
        );
        let field_number = key >> 3;
        let actual_wire = key & 7;
        let modeled = modeled_field(message, field_number).with_context(|| {
            format!(
                "{} field #{field_number} is not modeled; cannot guarantee a \
                 value-preserving round-trip",
                message.name()
            )
        })?;

        if let Some(previous) = last_field {
            ensure!(
                field_number >= previous,
                "{} field #{field_number} appears after field #{previous}; encoding emits fields \
                 in ascending tag order, so this order cannot be reproduced",
                message.name(),
            );
        }
        last_field = Some(field_number);

        if !repeated_field(message, field_number) {
            ensure!(
                seen.insert(field_number),
                "{} field #{field_number} appears more than once but is singular; decoding keeps \
                 only the last value and discards the rest",
                message.name(),
            );
        }

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
            ModeledField::Varint(width) => {
                let (value, overlong) = read_varint(bytes, &mut offset)
                    .with_context(|| format!("reading {} field #{field_number}", message.name()))?;
                ensure!(
                    !overlong,
                    "{} field #{field_number} is not minimally encoded",
                    message.name(),
                );
                ensure!(
                    width.round_trips(value),
                    "{} field #{field_number} value {value} does not fit its modeled {}; \
                     decoding would alter it",
                    message.name(),
                    width.name(),
                );
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

/// Strictly validate against `root`, then decode. The shared body behind the typed helpers below.
fn decode_checked<T: prost::Message + Default>(
    bytes: &[u8],
    label: &str,
    root: RootMessage,
) -> anyhow::Result<T> {
    ensure_modeled(bytes, root).with_context(|| format!("validating {label}"))?;
    T::decode(bytes).with_context(|| format!("decoding {label}"))
}

/// Strictly validate and decode an NR carrier capability message.
pub(crate) fn decode_uecaps(bytes: &[u8], label: &str) -> anyhow::Result<UeCaps> {
    decode_checked(bytes, label, RootMessage::UeCaps)
}

/// Strictly validate and decode an LTE fallback capability message.
pub(crate) fn decode_lte_caps(bytes: &[u8], label: &str) -> anyhow::Result<LteCaps> {
    decode_checked(bytes, label, RootMessage::LteCaps)
}

/// Strictly validate and decode a PLMN mapping message.
pub(crate) fn decode_plmn_map(bytes: &[u8], label: &str) -> anyhow::Result<PlmnMap> {
    decode_checked(bytes, label, RootMessage::PlmnMap)
}

#[cfg(test)]
mod tests {
    use super::{
        ModeledField, RootMessage, decode_lte_caps, decode_plmn_map, decode_uecaps, ensure_modeled,
        modeled_field,
    };
    use crate::proto::{
        Carrier, Combo, ComboGroup, ComboHeader, LteCaps, LteCombo, LteComponent, PlmnMap,
        ShannonFeatureSetDlPerCcNr, ShannonFeatureSetUlPerCcNr, SubBlock as ProtoSubBlock, UeCaps,
    };
    use prost::Message;

    /// The test fixtures hang off the production enum rather than a copy of it, so a message
    /// added to [`ModeledMessage`] is a compile error here until it is covered, instead of
    /// silently dropping out of the sweeps below.
    use super::ModeledMessage as TestMessage;

    impl TestMessage {
        fn valid_bytes(self) -> Vec<u8> {
            match self {
                Self::UeCaps => UeCaps::default().encode_to_vec(),
                Self::ComboGroup => ComboGroup::default().encode_to_vec(),
                Self::Header => ComboHeader::default().encode_to_vec(),
                Self::Combo => Combo::default().encode_to_vec(),
                Self::SubBlock => ProtoSubBlock::default().encode_to_vec(),
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
                Self::SubBlock => (
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
        TestMessage::SubBlock,
        TestMessage::DlFeature,
        TestMessage::UlFeature,
        TestMessage::LteCaps,
        TestMessage::LteCombo,
        TestMessage::LteComponent,
        TestMessage::PlmnMap,
        TestMessage::Carrier,
    ];

    /// The schema as written in `src/proto.rs`, hand-transcribed: `(message, field, wire type,
    /// declared type)`. This is the oracle for [`modeled_field`] and MUST NOT be derived from
    /// it — deriving the expectation from the function under test is what made the previous
    /// version of this suite unable to catch a wrong wire type, since flipping an arm in
    /// `modeled_field` flipped the expectation in lockstep.
    ///
    /// Transcribe from the `#[prost(...)]` attributes, not from `modeled_field`.
    const SCHEMA: &[(TestMessage, u64, u8, &str)] = &[
        (TestMessage::UeCaps, 1, 0, "uint64"),
        (TestMessage::UeCaps, 2, 0, "int32"),
        (TestMessage::UeCaps, 3, 2, "message"),
        (TestMessage::UeCaps, 6, 2, "message"),
        (TestMessage::UeCaps, 7, 2, "message"),
        (TestMessage::UeCaps, 9, 0, "uint64"),
        (TestMessage::ComboGroup, 1, 2, "message"),
        (TestMessage::ComboGroup, 2, 2, "message"),
        (TestMessage::Header, 1, 0, "uint32"),
        (TestMessage::Header, 2, 0, "uint32"),
        (TestMessage::Header, 3, 0, "uint32"),
        (TestMessage::Header, 4, 0, "int32"),
        (TestMessage::Header, 5, 0, "int32"),
        (TestMessage::Combo, 1, 2, "message"),
        (TestMessage::Combo, 2, 0, "uint32"),
        (TestMessage::SubBlock, 1, 0, "int32"),
        (TestMessage::SubBlock, 2, 0, "int32"),
        (TestMessage::SubBlock, 3, 0, "int32"),
        (TestMessage::SubBlock, 4, 0, "int32"),
        (TestMessage::SubBlock, 5, 0, "int32"),
        // The per-CC selectors are `bytes`, NOT varints — the case the self-deriving
        // oracle could never have caught.
        (TestMessage::SubBlock, 6, 2, "bytes"),
        (TestMessage::SubBlock, 7, 2, "bytes"),
        (TestMessage::SubBlock, 8, 0, "int32"),
        (TestMessage::DlFeature, 1, 0, "int32"),
        (TestMessage::DlFeature, 2, 0, "int32"),
        (TestMessage::DlFeature, 3, 0, "int32"),
        (TestMessage::DlFeature, 4, 0, "int32"),
        (TestMessage::DlFeature, 5, 0, "bool"),
        (TestMessage::UlFeature, 1, 0, "int32"),
        (TestMessage::UlFeature, 2, 0, "int32"),
        (TestMessage::UlFeature, 3, 0, "int32"),
        (TestMessage::UlFeature, 4, 0, "int32"),
        (TestMessage::UlFeature, 5, 0, "bool"),
        (TestMessage::UlFeature, 6, 0, "int32"),
        (TestMessage::LteCaps, 1, 0, "uint64"),
        (TestMessage::LteCaps, 2, 2, "message"),
        (TestMessage::LteCaps, 3, 0, "uint64"),
        (TestMessage::LteCombo, 1, 2, "message"),
        (TestMessage::LteCombo, 2, 0, "uint64"),
        (TestMessage::LteCombo, 3, 0, "uint64"),
        (TestMessage::LteCombo, 4, 0, "uint64"),
        (TestMessage::LteComponent, 1, 0, "int32"),
        (TestMessage::LteComponent, 2, 0, "int32"),
        (TestMessage::LteComponent, 3, 0, "int32"),
        (TestMessage::PlmnMap, 1, 2, "message"),
        (TestMessage::Carrier, 1, 0, "uint64"),
        (TestMessage::Carrier, 2, 0, "uint64"),
        (TestMessage::Carrier, 3, 2, "bytes"),
    ];

    /// Every `(field, expected wire type)` for `message`, taken from the hand-written
    /// [`SCHEMA`] above rather than from the function under test.
    fn modeled_fields(message: TestMessage) -> Vec<(u64, u8)> {
        SCHEMA
            .iter()
            .filter(|(m, ..)| *m == message)
            .map(|&(_, field, wire, _)| (field, wire))
            .collect()
    }

    /// `modeled_field` agrees with the hand-written schema exactly — same field set, same
    /// wire type, same declared integer type — in both directions, so neither a missing arm
    /// nor an extra one survives.
    #[test]
    fn modeled_field_matches_the_hand_written_schema() {
        for &(message, field, wire, declared) in SCHEMA {
            let modeled = modeled_field(message, field)
                .unwrap_or_else(|| panic!("{message:?} field #{field} is not modeled"));
            assert_eq!(
                modeled.wire_type(),
                u64::from(wire),
                "{message:?} field #{field} wire type"
            );
            let actual = match modeled {
                ModeledField::Varint(width) => width.name(),
                ModeledField::Bytes => "bytes",
                ModeledField::Message(_) => "message",
            };
            assert_eq!(actual, declared, "{message:?} field #{field} declared type");
        }

        for &message in MESSAGES {
            for field in 1..=15u64 {
                let expected = SCHEMA.iter().any(|&(m, f, ..)| m == message && f == field);
                assert_eq!(
                    modeled_field(message, field).is_some(),
                    expected,
                    "{message:?} field #{field}: modeled/schema disagree"
                );
            }
        }
    }

    /// The repeated-field table must match `src/proto.rs`'s `repeated` labels, since it is what
    /// decides whether a second occurrence is a legitimate repeat or silent data loss.
    #[test]
    fn repeated_fields_match_the_proto_labels() {
        const REPEATED: &[(TestMessage, u64)] = &[
            (TestMessage::UeCaps, 3),
            (TestMessage::UeCaps, 6),
            (TestMessage::UeCaps, 7),
            (TestMessage::ComboGroup, 2),
            (TestMessage::Combo, 1),
            (TestMessage::LteCaps, 2),
            (TestMessage::LteCombo, 1),
            (TestMessage::PlmnMap, 1),
            (TestMessage::Carrier, 1),
        ];
        for &(message, field, ..) in SCHEMA {
            let expected = REPEATED.iter().any(|&(m, f)| m == message && f == field);
            assert_eq!(
                super::repeated_field(message, field),
                expected,
                "{message:?} field #{field} repeated-ness"
            );
        }
    }

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
        for &message in MESSAGES {
            let fields = modeled_fields(message);
            assert!(!fields.is_empty(), "{message:?} models no fields");
            for (field, expected_wire) in fields {
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

    /// prost keeps the *last* occurrence of a singular field and silently discards the
    /// earlier ones, so accepting a repeat breaks the value-preserving round-trip the
    /// scanner's own error text promises.
    #[test]
    fn rejects_a_duplicate_singular_field() {
        let bytes = [0x08, 0x01, 0x08, 0x02];
        assert_eq!(UeCaps::decode(&bytes[..]).unwrap().version, 2);

        let error = ensure_modeled(&bytes, RootMessage::UeCaps).unwrap_err();
        let diagnostic = format!("{error:#}");
        assert!(diagnostic.contains("field #1"), "{diagnostic}");
        assert!(diagnostic.contains("more than once"), "{diagnostic}");
    }

    /// The duplicate check must not fire on a genuinely repeated field.
    #[test]
    fn accepts_a_repeated_field_more_than_once() {
        let mut two_groups = length_delimited(3, ComboGroup::default().encode_to_vec());
        two_groups.extend(length_delimited(3, ComboGroup::default().encode_to_vec()));
        ensure_modeled(&two_groups, RootMessage::UeCaps).expect("combo_groups is repeated");

        let mut two_plmns = varint(1, 302_220);
        two_plmns.extend(varint(1, 302_221));
        let (root, bytes) = TestMessage::Carrier.wrap_in_root(two_plmns);
        ensure_modeled(&bytes, root).expect("plmns is repeated");
    }

    /// A varint wider than the modeled `uint32` is truncated by prost and re-encoded as a
    /// different number, which the scanner must reject rather than wave through.
    #[test]
    fn rejects_an_out_of_range_uint32() {
        let too_wide = varint(2, 0x1_0000_0007);
        let (root, bytes) = TestMessage::Combo.wrap_in_root(too_wide);

        // The truncation being guarded against: prost reads the low 32 bits and would
        // re-encode a two-byte `bitmask = 7`, bypassing the profiled-layout guard in
        // `compiler::nr` that requires the bitmask to be absent or zero.
        let combo = Combo::decode(&varint(2, 0x1_0000_0007)[..]).unwrap();
        assert_eq!(combo.bitmask, Some(7));

        let error = ensure_modeled(&bytes, root).unwrap_err();
        let diagnostic = format!("{error:#}");
        assert!(diagnostic.contains("field #2"), "{diagnostic}");
        assert!(diagnostic.contains("4294967303"), "{diagnostic}");
        assert!(diagnostic.contains("uint32"), "{diagnostic}");
    }

    /// A `bool` field re-encodes any nonzero value as 1, so only 0 and 1 round-trip.
    #[test]
    fn rejects_an_out_of_range_bool() {
        let (root, bytes) = TestMessage::DlFeature.wrap_in_root(varint(5, 2));

        let error = ensure_modeled(&bytes, root).unwrap_err();
        assert!(format!("{error:#}").contains("field #5"), "{error:#}");
    }

    /// An `int32` legitimately carries a negative value as a 10-byte sign-extended varint,
    /// so the range check must accept that while still rejecting a value that only prost's
    /// truncation would turn into an i32.
    #[test]
    fn accepts_a_sign_extended_negative_int32_but_not_a_truncating_one() {
        let negative = varint(1, u64::MAX); // -1, sign-extended
        let (root, bytes) = TestMessage::SubBlock.wrap_in_root(negative);
        ensure_modeled(&bytes, root).expect("a sign-extended -1 round-trips byte-for-byte");

        let unextended = varint(1, u64::from(u32::MAX)); // decodes to -1, re-encodes wider
        let (root, bytes) = TestMessage::SubBlock.wrap_in_root(unextended);
        assert!(ensure_modeled(&bytes, root).is_err());
    }

    /// prost normalizes an overlong varint on re-encode, changing the bytes.
    #[test]
    fn rejects_an_overlong_varint() {
        let error = ensure_modeled(&[0x08, 0x80, 0x00], RootMessage::UeCaps).unwrap_err();
        assert!(format!("{error:#}").contains("field #1"), "{error:#}");
    }

    /// prost emits fields in ascending tag order, so a descending pair cannot round-trip.
    #[test]
    fn rejects_a_descending_tag_order() {
        let mut bytes = varint(9, 7);
        bytes.extend(varint(1, 300));

        let error = ensure_modeled(&bytes, RootMessage::UeCaps).unwrap_err();
        let diagnostic = format!("{error:#}");
        assert!(diagnostic.contains("field #1"), "{diagnostic}");
        assert!(diagnostic.contains("order"), "{diagnostic}");
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
