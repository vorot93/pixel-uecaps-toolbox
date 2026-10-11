use crate::{
    compiler::kdl_keys::{carrier, plmn as keys},
    kdl_support::NodeReader,
    mapping::Plmn,
};
use anyhow::{Result, anyhow, bail};
use kdl::{KdlEntry, KdlNode};

fn decimal_field(s: &str) -> Option<u32> {
    if !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()) {
        s.parse().ok()
    } else {
        None
    }
}

/// KDL represents decimal MCC/MNC values and the wildcard MNC, a strict subset of raw BCD.
/// Preserve a leading-zero three-digit MNC with an explicit width marker.
pub(super) fn write_plmn(plmn: Plmn) -> Result<KdlNode> {
    let text = plmn.to_string();
    let (mcc, mnc) = text
        .split_once('-')
        .expect("Plmn Display always contains a hyphen");
    let mcc_val =
        decimal_field(mcc).ok_or_else(|| anyhow!("PLMN `{plmn}` has a non-decimal MCC"))?;
    let mut node = KdlNode::new(carrier::PLMN);
    node.push(KdlEntry::new_prop(keys::MCC, i128::from(mcc_val)));
    if mnc != "ff" {
        let mnc_val =
            decimal_field(mnc).ok_or_else(|| anyhow!("PLMN `{plmn}` has a non-decimal MNC"))?;
        node.push(KdlEntry::new_prop(keys::MNC, i128::from(mnc_val)));
        if mnc.len() == 3 && mnc_val < 100 {
            node.push(KdlEntry::new_prop(keys::MNC_DIGITS, 3i128));
        }
    }
    Ok(node)
}

fn mnc_width(value: u32, digits: Option<u32>) -> Result<usize> {
    match digits {
        None => Ok(if value >= 100 { 3 } else { 2 }),
        Some(3) => Ok(3),
        Some(other) => bail!("`plmn` `mnc-digits` must be 3, got {other}"),
    }
}

pub(super) fn read_plmn(node: &KdlNode) -> Result<Plmn> {
    let mut r = NodeReader::new(node);
    let mcc: u32 = r.req_int(keys::MCC)?;
    let mnc: Option<u32> = r.opt_int(keys::MNC)?;
    let digits: Option<u32> = r.opt_int(keys::MNC_DIGITS)?;
    r.finish()?;
    let text = match mnc {
        None => {
            if digits.is_some() {
                bail!("`plmn` has `mnc-digits` without `mnc`");
            }
            format!("{mcc:03}-ff")
        }
        Some(value) => {
            let width = mnc_width(value, digits)?;
            format!("{mcc:03}-{value:0width$}")
        }
    };
    text.parse()
        .map_err(|e| anyhow!("`plmn` reconstructs to invalid PLMN `{text}`: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plmn_forms_round_trip() {
        for text in [
            "311-480", "310-260", "202-01", "310-04", "310-004", "334-030", "228-ff",
        ] {
            let value = text.parse().unwrap();
            assert_eq!(
                read_plmn(&write_plmn(value).unwrap()).unwrap(),
                value,
                "{text}"
            );
        }
    }

    #[test]
    fn wildcard_omits_mnc() {
        let node = write_plmn("228-ff".parse().unwrap()).unwrap();
        assert!(node.get("mnc").is_none());
        assert_eq!(node.get("mcc").unwrap().as_integer(), Some(228));
    }

    #[test]
    fn leading_zero_three_digit_gets_marker() {
        let node = write_plmn("310-004".parse().unwrap()).unwrap();
        assert_eq!(node.get("mnc").unwrap().as_integer(), Some(4));
        assert_eq!(node.get("mnc-digits").unwrap().as_integer(), Some(3));
    }

    #[test]
    fn two_digit_and_big_three_digit_omit_marker() {
        for text in ["310-04", "302-220"] {
            assert!(
                write_plmn(text.parse().unwrap())
                    .unwrap()
                    .get("mnc-digits")
                    .is_none()
            );
        }
    }

    #[test]
    fn ordered_plmns_preserve_duplicates_and_mnc_width() {
        let values: Vec<Plmn> = ["310-04", "310-004", "228-ff", "310-04"]
            .map(|text| text.parse().unwrap())
            .into();
        assert_ne!(values[0], values[1]);
        assert_ne!(values[0], values[2]);
        assert_ne!(values[1], values[2]);
        let decoded: Vec<_> = values
            .iter()
            .map(|&plmn| {
                let text = write_plmn(plmn).unwrap().to_string();
                read_plmn(&text.parse::<KdlNode>().unwrap()).unwrap()
            })
            .collect();
        assert_eq!(decoded, values);
    }

    #[test]
    fn source_writer_rejects_nondecimal_nibbles_except_wildcard() {
        for (text, diagnostic) in [("31a-04", "non-decimal MCC"), ("310-a4", "non-decimal MNC")] {
            let plmn = text.parse::<Plmn>().unwrap();
            assert!(
                write_plmn(plmn)
                    .unwrap_err()
                    .to_string()
                    .contains(diagnostic)
            );
        }
    }

    #[test]
    fn mnc_digits_requires_mnc_and_only_accepts_three() {
        for text in [
            "p mcc=310 mnc-digits=3",
            "p mcc=310 mnc=4 mnc-digits=2",
            "p mcc=310 mnc=4 mnc-digits=4",
        ] {
            assert!(
                read_plmn(&text.parse::<KdlNode>().unwrap()).is_err(),
                "{text}"
            );
        }
    }
}
