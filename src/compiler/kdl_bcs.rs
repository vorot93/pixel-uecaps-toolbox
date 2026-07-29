//! The BCS property value: a 3GPP `BIT STRING` spelled as `b` + an ascending index list.
//!
//! ```text
//! b0        bandwidth combination set 0 only          (was 2147483648)
//! b0,1      sets 0 and 1                              (was 3221225472)
//! b0,1,4    sets 0, 1 and 4                           (was 3355443200)
//! ""        the empty set — the field is not emitted on the air interface
//! ```
//!
//! The stored number is the raw ASN.1 `BIT STRING (SIZE (1..32))` from
//! `supportedBandwidthCombinationSet`, left-aligned in a 32-bit word: index *i* is bit
//! `1 << (31 - i)`. This is not an inference from bit patterns — decoded
//! `UECapabilityInformation` captures print both forms of the same field, and the decimal
//! matches ours exactly: `supportedBandwidthCombinationSet-r13 : '11000000 …'B(3221225472)`.
//! See DESIGN.md for the full mapping and provenance.
//!
//! Every value in the real corpus uses only indices 0..=5, but all 32 are representable.
//!
//! The `b` prefix earns its byte: KDL quotes any value beginning with a digit, so a bare
//! `bn=0,1,4` would be written `bn="0,1,4"`. One prefix character replaces two quotes.

use anyhow::{Context, Result, ensure};

/// Keeps the value a bare KDL identifier — see the module doc.
const PREFIX: char = 'b';

/// Render a 32-bit left-aligned BIT STRING as `b` + its ascending BCS index list. The empty
/// set renders as the empty string, which KDL writes as `""`.
pub(crate) fn format_bcs(bits: u32) -> String {
    if bits == 0 {
        return String::new();
    }
    let mut out = String::with_capacity(8);
    out.push(PREFIX);
    let mut first = true;
    for index in 0..32u32 {
        if bits & (1u32 << (31 - index)) != 0 {
            if !first {
                out.push(',');
            }
            first = false;
            out.push_str(&index.to_string());
        }
    }
    out
}

/// The inverse of [`format_bcs`]. `key` names the property in diagnostics.
///
/// Indices must ascend and not repeat. Normalising a descending list instead would give one
/// value two spellings, and the round trip would stop being byte-stable — the same rule
/// [`super::kdl_direction::parse_class_mimo`] keeps for its own encoding.
pub(crate) fn parse_bcs(raw: &str, key: &str) -> Result<u32> {
    if raw.is_empty() {
        return Ok(0);
    }
    let rest = raw.strip_prefix(PREFIX).with_context(|| {
        format!("property `{key}` value `{raw}` must begin with `{PREFIX}`, as in `b0,1`")
    })?;
    ensure!(
        !rest.is_empty(),
        "property `{key}` value `{raw}` has no BCS index"
    );
    let mut bits = 0u32;
    let mut previous: Option<u32> = None;
    for part in rest.split(',') {
        ensure!(
            !part.is_empty(),
            "property `{key}` value `{raw}` has an empty BCS index"
        );
        ensure!(
            part.bytes().all(|b| b.is_ascii_digit()),
            "property `{key}` BCS index `{part}` is not a decimal number"
        );
        // `b00` and `b0` must not both parse: one value, one spelling, or the round trip
        // stops being byte-stable. Index 0 is legitimately spelled `0`, so only PADDED
        // zeros are refused.
        ensure!(
            part == "0" || !part.starts_with('0'),
            "property `{key}` BCS index `{part}` has a leading zero; each index has exactly \
             one spelling, so write it without padding"
        );
        let index: u32 = part
            .parse()
            .with_context(|| format!("property `{key}` BCS index `{part}` is out of range"))?;
        ensure!(
            index < 32,
            "property `{key}` BCS index {index} exceeds 31; the bit string is 32 bits wide"
        );
        if let Some(previous) = previous {
            ensure!(
                index > previous,
                "property `{key}` value `{raw}` lists BCS index {index} after {previous}; \
                 indices must ascend without repeating, so that each value has one spelling"
            );
        }
        previous = Some(index);
        bits |= 1u32 << (31 - index);
    }
    Ok(bits)
}

#[cfg(test)]
mod tests {
    use super::{format_bcs, parse_bcs};

    /// Every value the real corpus contains, plus the empty set. The left-aligned layout is
    /// the point: index 0 is the MOST significant bit, so `{0}` is 2147483648, not 1.
    #[test]
    fn round_trips_every_observed_value() {
        for (bits, text) in [
            (0u32, ""),
            (2_147_483_648, "b0"),
            (3_221_225_472, "b0,1"),
            (3_758_096_384, "b0,1,2"),
            (4_026_531_840, "b0,1,2,3"),
            (4_160_749_568, "b0,1,2,3,4"),
            (4_227_858_432, "b0,1,2,3,4,5"),
            (2_281_701_376, "b0,4"),
            (3_355_443_200, "b0,1,4"),
            (3_892_314_112, "b0,1,2,4"),
            (134_217_728, "b4"),
        ] {
            assert_eq!(format_bcs(bits), text, "formatting {bits}");
            assert_eq!(parse_bcs(text, "bn").unwrap(), bits, "parsing `{text}`");
        }
    }

    /// The whole 32-bit range is representable, not just the 0..=5 the corpus uses.
    #[test]
    fn the_last_index_round_trips() {
        assert_eq!(format_bcs(1), "b31");
        assert_eq!(parse_bcs("b31", "bn").unwrap(), 1);
    }

    /// Two spellings of one value would break byte-stable round-tripping, so a descending or
    /// repeating list is refused rather than normalised.
    #[test]
    fn rejects_anything_with_a_second_spelling() {
        for (bad, expect) in [
            ("b1,0", "ascend"),
            ("b0,0", "ascend"),
            ("b4,1", "ascend"),
            ("b00", "leading zero"),
            ("b01", "leading zero"),
            ("b0,01", "leading zero"),
        ] {
            let error = parse_bcs(bad, "bn").unwrap_err().to_string();
            assert!(
                error.contains(expect),
                "`{bad}` should mention `{expect}`, got: {error}"
            );
        }
    }

    #[test]
    fn rejects_malformed_values() {
        for (bad, expect) in [
            ("0,1", "must begin with"),
            ("b", "no BCS index"),
            ("b0,", "empty BCS index"),
            ("b,0", "empty BCS index"),
            ("b0,,1", "empty BCS index"),
            ("bx", "decimal"),
            ("b32", "exceeds 31"),
            ("b99999999999", "out of range"),
        ] {
            let error = parse_bcs(bad, "bn").unwrap_err().to_string();
            assert!(
                error.contains(expect),
                "`{bad}` should mention `{expect}`, got: {error}"
            );
        }
    }

    /// `b0` is bandwidth combination set 0; `""` is the empty set. They are different values
    /// and must never collapse into one spelling.
    #[test]
    fn the_empty_set_and_set_zero_stay_distinct() {
        assert_ne!(format_bcs(0), format_bcs(2_147_483_648));
        assert_eq!(parse_bcs("", "bi").unwrap(), 0);
        assert_ne!(parse_bcs("", "bi").unwrap(), parse_bcs("b0", "bi").unwrap());
    }
}
