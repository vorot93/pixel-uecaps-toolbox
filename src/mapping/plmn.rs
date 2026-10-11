use super::Error;
use std::{fmt, str::FromStr};

/// A 24-bit packed-BCD PLMN (MCC-MNC). Bijective with its `MCC-MNC` string form.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Plmn(u32);

impl Plmn {
    /// Wrap a raw encoded value, validating it fits in 24 bits.
    pub(crate) const fn from_encoded(value: u64) -> Result<Self, Error> {
        if value > 0xFF_FFFF {
            return Err(Error::PlmnOutOfRange(value));
        }
        Ok(Self(value as u32))
    }

    /// The raw encoded value (always within 24 bits).
    pub(crate) const fn to_encoded(self) -> u64 {
        self.0 as u64
    }

    /// Packed-BCD nibbles: `([mcc1, mcc2, mcc3], [mnc1, mnc2], mnc3)`, each `0x0..=0xF`.
    /// `mnc3 == 0xF` is the filler nibble (2-digit MNC; third digit absent).
    pub(crate) const fn nibbles(self) -> ([u8; 3], [u8; 2], u8) {
        let [_, b0, b1, b2] = self.0.to_be_bytes();
        ([b0 & 0xf, b0 >> 4, b1 & 0xf], [b2 & 0xf, b2 >> 4], b1 >> 4)
    }
}

impl fmt::Display for Plmn {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let (mcc, mnc, mnc3) = self.nibbles();
        write!(
            f,
            "{:x}{:x}{:x}-{:x}{:x}",
            mcc[0], mcc[1], mcc[2], mnc[0], mnc[1]
        )?;
        if mnc3 != 0xf {
            write!(f, "{mnc3:x}")?;
        }
        Ok(())
    }
}

impl FromStr for Plmn {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let bad = || Error::PlmnFormat(s.to_string());
        let (mcc, mnc) = s.split_once('-').ok_or_else(bad)?;
        let mcc = mcc.as_bytes();
        let mnc = mnc.as_bytes();
        if mcc.len() != 3 || (mnc.len() != 2 && mnc.len() != 3) {
            return Err(bad());
        }
        if !mcc.iter().chain(mnc.iter()).all(u8::is_ascii_hexdigit) {
            return Err(bad());
        }
        // A 3-digit MNC may not end in the filler nibble F (use the 2-digit form).
        if mnc.len() == 3 && mnc[2].eq_ignore_ascii_case(&b'f') {
            return Err(bad());
        }
        let nibble = |b: u8| {
            u32::from(if b.is_ascii_digit() {
                b - b'0'
            } else {
                b.to_ascii_lowercase() - b'a' + 10
            })
        };
        let n3 = if mnc.len() == 3 { nibble(mnc[2]) } else { 15 };
        // wire nibble layout: M2 M1 N3 M3 N2 N1
        let value = [
            nibble(mcc[1]),
            nibble(mcc[0]),
            n3,
            nibble(mcc[2]),
            nibble(mnc[1]),
            nibble(mnc[0]),
        ]
        .into_iter()
        .fold(0, |packed, n| (packed << 4) | n);
        Ok(Self(value))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const VECTORS: &[(u64, &str)] = &[
        (197_154, "302-220"),   // 3-digit MNC
        (10_090_905, "999-99"), // 2-digit MNC
        (5_435_408, "250-01"),  // 2-digit MNC
        (2_291_967, "228-ff"),  // wildcard MNC
    ];

    #[test]
    fn known_vectors_round_trip() {
        for &(val, s) in VECTORS {
            assert_eq!(
                Plmn::from_encoded(val).unwrap().to_string(),
                s,
                "decode {val}"
            );
            assert_eq!(s.parse::<Plmn>().unwrap().to_encoded(), val, "encode {s}");
        }
    }

    #[test]
    fn parse_is_case_insensitive() {
        assert_eq!(
            "228-FF".parse::<Plmn>().unwrap(),
            "228-ff".parse::<Plmn>().unwrap()
        );
    }

    #[test]
    fn rejects_out_of_range() {
        assert!(matches!(
            Plmn::from_encoded(0x100_0000),
            Err(Error::PlmnOutOfRange(_))
        ));
    }

    #[test]
    fn rejects_bad_strings() {
        for s in [
            "302", "30-220", "3022-20", "302-2", "302-2222", "30g-220", "302-2g0", "302-22f",
        ] {
            assert!(s.parse::<Plmn>().is_err(), "should reject {s}");
        }
    }

    #[test]
    fn bijection_full_sweep() {
        for v in 0u32..=0xFF_FFFF {
            let p = Plmn::from_encoded(v as u64).unwrap();
            assert_eq!(p.to_string().parse::<Plmn>().unwrap(), p, "{v:#08x}");
        }
    }
}
