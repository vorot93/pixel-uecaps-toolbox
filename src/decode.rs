//! `decode`: one `.binarypb` file to its KDL. The kind comes from the filename
//! unless `--kind` overrides it — the escape hatch for a renamed or backed-up file.
//!
//! A capability file yields a write-only slice of `nr.kdl`/`lte.kdl`; the PLMN
//! legend yields the editable document `mapping encode` re-encodes bit-for-bit.
//! The two differ in strictness on purpose: the legend must fail closed on anything
//! it cannot reproduce, while a slice is lenient because nothing consumes it.

use crate::{
    compiler::{lte_slice, nr_slice},
    mapping::decode_bytes,
    model::{Parsed, parse_name},
    output::write_out,
};
use anyhow::Context;
use std::path::Path;

/// Which document a file decodes to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
pub enum Kind {
    /// A `<CARRIER>_<NUMBER>.binarypb` carrier file — a slice of `nr.kdl`.
    Nr,
    /// An `lte_*.binarypb` fallback — a slice of `lte.kdl`.
    Lte,
    /// `ap_plmn_mapping.binarypb` — the editable PLMN legend.
    Mapping,
}

/// `decode <FILE> [--kind KIND]`: write the file's KDL to stdout, return the exit code.
pub fn run(file: &Path, kind: Option<Kind>) -> anyhow::Result<i32> {
    let (bytes, code) = render(file, kind)?;
    write_out(&bytes, None, "")?;
    Ok(code)
}

/// The document `run` writes, plus its exit code. Split out so tests can assert on
/// the bytes without capturing stdout.
fn render(file: &Path, kind: Option<Kind>) -> anyhow::Result<(Vec<u8>, i32)> {
    let label = file.file_name().and_then(|s| s.to_str()).unwrap_or("?");
    let kind = match kind {
        Some(k) => k,
        None => kind_from_name(label)?,
    };
    // Read once and decode per kind: `report::read_ue_caps` is private to `report`,
    // returns `Option<UeCaps>` rather than bytes, and is wrong for the legend branch,
    // which hands raw bytes to `mapping::decode_bytes`.
    let bytes = std::fs::read(file).with_context(|| format!("reading {label}"))?;
    Ok(match kind {
        Kind::Nr => {
            let (text, code) = nr_slice(&bytes)?;
            (text.into_bytes(), code)
        }
        Kind::Lte => {
            let (text, code) = lte_slice(&bytes);
            (text.into_bytes(), code)
        }
        Kind::Mapping => (decode_bytes(&bytes)?, 0),
    })
}

/// The kind a filename implies, or an error pointing at `--kind`.
fn kind_from_name(label: &str) -> anyhow::Result<Kind> {
    match parse_name(label) {
        Parsed::Mapping => Ok(Kind::Mapping),
        Parsed::Lte(_) => Ok(Kind::Lte),
        Parsed::Carrier { .. } => Ok(Kind::Nr),
        Parsed::Other => anyhow::bail!(
            "{label} is not a recognised uecaps filename; pass --kind <nr|lte|mapping>"
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::{Kind, render};
    use crate::proto::{
        Carrier, ComboGroup, LteCaps, LteCombo, LteComponent, PlmnMap, UeCaps, combo_group,
        combo_group::combo::SubBlock,
    };
    use prost::Message;
    use std::path::{Path, PathBuf};

    /// A temp dir unique to this test binary + the given tag, removed by `cleanup`.
    fn scratch(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("uecaps-decode-{tag}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write(dir: &Path, name: &str, bytes: &[u8]) -> PathBuf {
        let path = dir.join(name);
        std::fs::write(&path, bytes).unwrap();
        path
    }

    fn carrier_bytes() -> Vec<u8> {
        UeCaps {
            version: 874_888_686,
            combo_groups: vec![ComboGroup {
                combo_header: None,
                combo: vec![combo_group::Combo {
                    bitmask: Some(0),
                    sub_blocks: vec![SubBlock {
                        band: 10078,
                        dl_bw_class: Some(1),
                        ul_bw_class: Some(1),
                        ..Default::default()
                    }],
                }],
            }],
            ..Default::default()
        }
        .encode_to_vec()
    }

    fn lte_bytes() -> Vec<u8> {
        LteCaps {
            fingerprint: 862_505_271,
            bitmask: 0,
            combos: vec![LteCombo {
                components: vec![LteComponent {
                    band: 1,
                    dl_bw_class_mimo: 32768,
                    ul_bw_class_mimo: None,
                }],
                bcs: None,
                unknown1: None,
                unknown2: None,
            }],
        }
        .encode_to_vec()
    }

    fn legend_bytes() -> Vec<u8> {
        PlmnMap {
            carriers: vec![Carrier {
                plmns: vec![5_435_408, 197_154],
                index: 1,
                name: "VZW".into(),
            }],
        }
        .encode_to_vec()
    }

    #[test]
    fn carrier_filename_yields_an_nr_slice() {
        let dir = scratch("carrier");
        let path = write(&dir, "VZW_3616442437.binarypb", &carrier_bytes());
        let (bytes, code) = render(&path, None).unwrap();
        std::fs::remove_dir_all(&dir).ok();

        let text = String::from_utf8(bytes).unwrap();
        assert_eq!(code, 0);
        assert!(text.starts_with("version 1"), "{text}");
        assert!(text.contains("nr 78"), "{text}");
    }

    #[test]
    fn lte_filename_yields_an_lte_slice() {
        let dir = scratch("lte");
        let path = write(&dir, "lte_2160127815.binarypb", &lte_bytes());
        let (bytes, code) = render(&path, None).unwrap();
        std::fs::remove_dir_all(&dir).ok();

        let text = String::from_utf8(bytes).unwrap();
        assert_eq!(code, 0);
        assert!(text.contains("subblock 1 dl-bw-class-mimo=32768"), "{text}");
    }

    #[test]
    fn legend_filename_yields_kdl_that_re_encodes_bit_for_bit() {
        let dir = scratch("legend");
        let original = legend_bytes();
        let path = write(&dir, "ap_plmn_mapping.binarypb", &original);
        let (kdl, code) = render(&path, None).unwrap();
        std::fs::remove_dir_all(&dir).ok();

        assert_eq!(code, 0);
        assert_eq!(
            crate::mapping::encode_bytes(&kdl).unwrap(),
            original,
            "decode -> mapping encode must be bit-identical"
        );
    }

    #[test]
    fn legend_with_an_unmodeled_field_fails_closed() {
        // The legend must re-encode bit-for-bit, so a field outside the modeled
        // schema has to fail here rather than be silently dropped. `[0x10, 0x05]` is
        // PlmnMap field #2 (varint), which is not modeled; prost decodes past it, so
        // only the strict wire validator catches it. This is the strictness half of
        // the asymmetry -- an undecodable *capability* file is lenient (exit 1).
        let dir = scratch("strict");
        let mut bytes = legend_bytes();
        bytes.extend_from_slice(&[0x10, 0x05]);
        let path = write(&dir, "ap_plmn_mapping.binarypb", &bytes);
        let result = render(&path, None);
        std::fs::remove_dir_all(&dir).ok();

        assert!(result.is_err(), "legend decoding must fail closed");
    }

    #[test]
    fn kind_overrides_a_non_canonical_filename() {
        let dir = scratch("override");
        let canonical = write(&dir, "ap_plmn_mapping.binarypb", &legend_bytes());
        let renamed = write(&dir, "legend.bak", &legend_bytes());

        let (by_name, _) = render(&canonical, None).unwrap();
        let (by_flag, _) = render(&renamed, Some(Kind::Mapping)).unwrap();
        std::fs::remove_dir_all(&dir).ok();

        assert_eq!(by_flag, by_name, "--kind must reproduce filename dispatch");
    }

    #[test]
    fn unrecognised_filename_without_kind_errors_and_names_the_flag() {
        let dir = scratch("unknown");
        let path = write(&dir, "whatever.bin", &carrier_bytes());
        let err = render(&path, None).unwrap_err();
        std::fs::remove_dir_all(&dir).ok();

        let message = err.to_string();
        assert!(
            message.contains("--kind"),
            "the error must point at the escape hatch: {message}"
        );
    }

    #[test]
    fn unreadable_carrier_yields_a_version_only_document_and_code_one() {
        let dir = scratch("bad");
        // Truncated field 3 -- UeCaps::decode fails.
        let path = write(&dir, "VZW_3616442437.binarypb", &[0x1a, 0x05, 0x01]);
        let (bytes, code) = render(&path, None).unwrap();
        std::fs::remove_dir_all(&dir).ok();

        let text = String::from_utf8(bytes).unwrap();
        assert_eq!(code, 1, "an unreadable file must exit 1");
        assert!(
            text.starts_with("version 1") && !text.contains("combo"),
            "{text}"
        );
    }

    #[test]
    fn ambiguous_number_still_exits_zero() {
        // 3347 * 3539 is divisible by two anchor primes. Under the slice shape,
        // ambiguity is not a `decode`-level exit condition -- the diagnostic lives in
        // `inspect`'s text report.
        let dir = scratch("ambiguous");
        let number = 3347u64 * 3539;
        let path = write(&dir, &format!("VZW_{number}.binarypb"), &carrier_bytes());
        let (_bytes, code) = render(&path, None).unwrap();
        std::fs::remove_dir_all(&dir).ok();

        assert_eq!(code, 0);
    }
}
