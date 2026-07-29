//! A sub-block's positional direction value: a 3GPP CA bandwidth-class letter followed by an optional
//! comma-separated per-CC index list.
//!
//! ```text
//! A            class A, no per-CC list (the all-zero placeholder, re-derived on read)
//! A3           class A, one per-CC reference
//! H30,30,30    class H (FR2, 3 CC), three per-CC references
//! ```
//!
//! The letter is the plain index into the 3GPP letters — `'A' + class - 1`. The corpus confirms
//! it: E-UTRA classes 1..=5 map to A..=E with CC counts 1, 2, 2, 3, 4, matching 3GPP 36.101
//! including the distinctive detail that B and C are *both* 2 CC. NR classes 7..=13 (G..=M) are
//! FR2-only — every occurrence is on n257/n258/n260/n261 at 120 kHz with a 100 MHz channel.
//!
//! Parsing here deliberately does NOT go through `NodeReader`'s typed readers, so it owes the
//! same strictness by hand: every rejection below has its own message and its own test. The 464
//! non-uniform per-CC lists in the corpus are exactly the rows an earlier CC0-only projection
//! dropped silently, so a parser that collapses a list is the failure mode to guard against.
//!
//! **The same text means different things in the two documents.** `B66 C2` is a bandwidth
//! class plus a per-CC feature index in `nr.kdl` and a class+MIMO bitfield in `lte.kdl`. That
//! is a deliberate trade, not an oversight: the *document* fixes the interpretation, the same
//! way a sub-block node name carries no radio-kind tag. `format_direction`/`parse_direction`
//! serve the first, `format_class_mimo`/`parse_class_mimo` the second, and
//! `identical_d_equals_text_means_different_things_in_each_document` in `kdl_source.rs` is
//! where the trade is meant to be learned before anyone "unifies" the two codecs.

use anyhow::{Context, Result, bail, ensure};

/// A parsed DL or UL positional direction argument.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Direction {
    pub(crate) bw_class: u8,
    /// One 1-based reference per CC. Empty means the all-zero placeholder, which the source
    /// omits and `resolve` re-materialises.
    pub(crate) indices: Vec<u16>,
}

/// Render a class plus its per-CC list.
///
/// Fails closed on a class with no letter rather than emitting a value that cannot be read
/// back — the same stance `cc_count` takes on an unobserved bandwidth class.
pub(crate) fn format_direction(bw_class: u8, indices: &[u16]) -> Result<String> {
    ensure!(
        (1..=26).contains(&bw_class),
        "bandwidth class {bw_class} has no 3GPP letter (expected 1..=26)"
    );
    let mut out = String::with_capacity(1 + indices.len() * 3);
    out.push((b'A' + bw_class - 1) as char);
    for (position, index) in indices.iter().enumerate() {
        if position > 0 {
            out.push(',');
        }
        out.push_str(&index.to_string());
    }
    Ok(out)
}

/// Parse a class plus its per-CC list. `label` names the positional direction argument
/// (`DL`/`UL`) in diagnostics.
pub(crate) fn parse_direction(raw: &str, label: &str) -> Result<Direction> {
    let mut chars = raw.chars();
    let letter = chars
        .next()
        .with_context(|| format!("{label} positional argument is empty"))?;
    ensure!(
        letter.is_ascii_uppercase(),
        "{label} positional argument value `{raw}` must begin with an uppercase bandwidth-class \
         letter"
    );
    let bw_class = (letter as u8) - b'A' + 1;

    let rest: String = chars.collect();
    if rest.is_empty() {
        return Ok(Direction {
            bw_class,
            indices: Vec::new(),
        });
    }

    let mut indices = Vec::new();
    for part in rest.split(',') {
        ensure!(
            !part.is_empty(),
            "{label} positional argument value `{raw}` has an empty index"
        );
        ensure!(
            part.bytes().all(|b| b.is_ascii_digit()),
            "{label} positional argument index `{part}` is not a decimal number"
        );
        let index: u16 = part.parse().with_context(|| {
            format!("{label} positional argument index `{part}` is out of range")
        })?;
        // NOT rejected here: index 0. On an NR sub-block a catalog reference is 1-based and 0
        // is invalid, but on an E-UTRA sub-block the index is a `parseLteFeatureIndex` MIMO
        // code for which 0 is a legitimate value. That distinction is kind semantics, and the
        // reader owns it — this codec only parses the syntax.
        indices.push(index);
    }
    Ok(Direction { bw_class, indices })
}

/// `lte.kdl`'s class+MIMO bitfield, highest bit first: 32768→A … 1024→F, low bit selects 4x4.
/// Corpus-verified: those six bases with either MIMO width are the only values observed.
///
/// Note this runs the OPPOSITE way from the sub-block bandwidth class above, where the class is
/// a small ascending integer. Two encodings of the same 3GPP concept, in one toolbox.
const CLASS_MIMO_BASES: [(i32, char); 6] = [
    (32768, 'A'),
    (16384, 'B'),
    (8192, 'C'),
    (4096, 'D'),
    (2048, 'E'),
    (1024, 'F'),
];

/// Render the class+MIMO bitfield as `<letter><mimo>`, e.g. 32769 → `A4`.
///
/// Fails closed on an unobserved value rather than inventing a letter — including 0, which is
/// UL-disabled. `lte.kdl` spells that by *omitting* the argument, so 0 has no rendering here and
/// no value has two spellings. A 0 reaching this function is a disabled downlink, which
/// `validate_lte_combos` rejects first with a message naming the combo and band.
pub(crate) fn format_class_mimo(value: i32) -> Result<String> {
    let base = value & !1;
    let (_, letter) = CLASS_MIMO_BASES
        .iter()
        .find(|(candidate, _)| *candidate == base)
        .with_context(|| format!("class+MIMO value {value} has no known bandwidth-class letter"))?;
    let mimo = if value & 1 == 1 { '4' } else { '2' };
    Ok(format!("{letter}{mimo}"))
}

/// The inverse of [`format_class_mimo`]. `label` names the positional direction argument
/// (`DL`/`UL`) in diagnostics.
///
/// Never returns 0: every result is `base + low` with `base >= 1024`. The only route to a zero UL
/// class is an omitted argument, which the reader defaults — so the source format has exactly one
/// spelling per value and the round trip stays byte-stable without a uniqueness check.
pub(crate) fn parse_class_mimo(raw: &str, label: &str) -> Result<i32> {
    let mut chars = raw.chars();
    let letter = chars
        .next()
        .with_context(|| format!("{label} positional argument is empty"))?;
    let mimo = chars
        .next()
        .with_context(|| format!("{label} positional argument value `{raw}` has no MIMO width"))?;
    ensure!(
        chars.next().is_none(),
        "{label} positional argument value `{raw}` has trailing characters"
    );
    let (base, _) = CLASS_MIMO_BASES
        .iter()
        .find(|(_, candidate)| *candidate == letter)
        .with_context(|| {
            format!(
                "{label} positional argument bandwidth-class letter `{letter}` is not one of A..F"
            )
        })?;
    let low = match mimo {
        '2' => 0,
        '4' => 1,
        other => bail!("{label} positional argument MIMO width `{other}` must be 2 or 4"),
    };
    Ok(base + low)
}

#[cfg(test)]
mod tests {
    use super::{format_class_mimo, format_direction, parse_class_mimo, parse_direction};

    #[test]
    fn round_trips_every_shape() {
        for (class, indices, text) in [
            (1u8, vec![], "A"),
            (1, vec![3u16], "A3"),
            (8, vec![30, 30, 30], "H30,30,30"),
            (7, vec![22, 23], "G22,23"),
            (13, vec![1, 2, 3, 4, 5, 6, 7, 8], "M1,2,3,4,5,6,7,8"),
        ] {
            assert_eq!(format_direction(class, &indices).unwrap(), text);
            let parsed = parse_direction(text, "DL").unwrap();
            assert_eq!(parsed.bw_class, class);
            assert_eq!(parsed.indices, indices);
        }
    }

    /// A two-digit index must not read as two one-digit indices — the comma is the separator,
    /// and its absence means one number.
    #[test]
    fn a_multi_digit_index_is_one_index() {
        let parsed = parse_direction("A12", "DL").unwrap();
        assert_eq!(parsed.indices, vec![12]);
    }

    /// The non-uniform case is the one that matters: a parser that collapsed a list would still
    /// satisfy a length check on a uniform one.
    #[test]
    fn distinct_per_cc_indices_survive() {
        let parsed = parse_direction("G22,23", "DL").unwrap();
        assert_eq!(parsed.indices, vec![22, 23]);
        assert_ne!(parsed.indices[0], parsed.indices[1]);
    }

    #[test]
    fn rejects_malformed_values() {
        for (bad, expect) in [
            ("", "empty"),
            ("3", "letter"),
            ("a3", "letter"),
            ("A3,", "empty index"),
            ("A,3", "empty index"),
            ("A3,x", "decimal"),
            ("A99999999", "out of range"),
        ] {
            let error = parse_direction(bad, "DL").unwrap_err().to_string();
            assert!(
                error.contains(expect),
                "`{bad}` should mention `{expect}`, got: {error}"
            );
        }
    }

    /// Index 0 parses. It is invalid as an NR catalog reference and legitimate as an E-UTRA
    /// `parseLteFeatureIndex` value, so only the reader — which knows the kind — can judge it.
    #[test]
    fn index_zero_parses_and_is_left_to_the_reader() {
        assert_eq!(parse_direction("A0", "DL").unwrap().indices, vec![0]);
    }

    #[test]
    fn a_class_with_no_letter_fails_closed() {
        assert!(format_direction(27, &[]).is_err());
        assert!(format_direction(0, &[]).is_err());
    }

    /// The E-UTRA class+MIMO bitfield runs the OTHER way from the sub-block class: 32768 is A,
    /// 1024 is F, and the low bit selects 4x4 over 2x2.
    #[test]
    fn class_mimo_round_trips_every_observed_value() {
        for (value, text) in [
            (32768i32, "A2"),
            (32769, "A4"),
            (16384, "B2"),
            (16385, "B4"),
            (8192, "C2"),
            (8193, "C4"),
            (4096, "D2"),
            (4097, "D4"),
            (2048, "E2"),
            (2049, "E4"),
            (1024, "F2"),
            (1025, "F4"),
        ] {
            assert_eq!(format_class_mimo(value).unwrap(), text);
            assert_eq!(parse_class_mimo(text, "DL").unwrap(), value);
        }
    }

    #[test]
    fn class_mimo_fails_closed_on_an_unknown_bitfield() {
        assert!(format_class_mimo(999).is_err());
        // 0 is UL-disabled. It has no letter and no spelling: `lte.kdl` omits the argument.
        assert!(format_class_mimo(0).is_err());
        assert!(parse_class_mimo("Z2", "DL").is_err());
        assert!(parse_class_mimo("A3", "DL").is_err());
        assert!(parse_class_mimo("A", "DL").is_err());
        // The superseded `off` spelling is not accepted back.
        assert!(parse_class_mimo("off", "UL").is_err());
    }
}
