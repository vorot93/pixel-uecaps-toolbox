use super::{Error, Plmn, schema};
use crate::proto::PlmnMap;
use anyhow::Context;
use prost::Message;
use std::io::{Read, Write};

fn read_stdin() -> anyhow::Result<Vec<u8>> {
    let mut buf = Vec::new();
    std::io::stdin()
        .lock()
        .read_to_end(&mut buf)
        .context("reading stdin")?;
    Ok(buf)
}

fn write_stdout(data: &[u8]) -> anyhow::Result<()> {
    std::io::stdout()
        .lock()
        .write_all(data)
        .context("writing stdout")
}

/// binarypb bytes -> editable TOML bytes.
pub fn decode_bytes(input: &[u8]) -> anyhow::Result<Vec<u8>> {
    let map = PlmnMap::decode(input).map_err(Error::Decode)?;
    let root = schema::map_to_root(&map)?;
    let text = toml::to_string_pretty(&root).context("serializing mapping TOML")?;
    Ok(text.into_bytes())
}

/// editable TOML bytes -> binarypb bytes.
pub fn encode_bytes(input: &[u8]) -> anyhow::Result<Vec<u8>> {
    let text = std::str::from_utf8(input).context("input is not UTF-8")?;
    let root: schema::Root = toml::from_str(text).context("parsing mapping TOML")?;
    let map = schema::root_to_map(&root)?;
    Ok(map.encode_to_vec())
}

/// `mapping decode`: stdin (binarypb) -> stdout (TOML).
pub fn decode() -> anyhow::Result<i32> {
    let out = decode_bytes(&read_stdin()?)?;
    write_stdout(&out)?;
    Ok(0)
}

/// `mapping encode`: stdin (TOML) -> stdout (binarypb).
pub fn encode() -> anyhow::Result<i32> {
    let out = encode_bytes(&read_stdin()?)?;
    write_stdout(&out)?;
    Ok(0)
}

/// Append one or more PLMNs (MCC-MNC) to the carrier named `carrier`. Each PLMN is
/// added only if not already present; the file is otherwise unchanged. On any error
/// (unknown carrier or malformed PLMN) it returns before emitting any bytes, so the
/// output is never partially written.
pub fn inject_bytes(input: &[u8], carrier: &str, plmns: &[String]) -> anyhow::Result<Vec<u8>> {
    let mut map = PlmnMap::decode(input).map_err(Error::Decode)?;
    let entry = map
        .carriers
        .iter_mut()
        .find(|c| c.name == carrier)
        .ok_or_else(|| Error::MappingNotFound(carrier.to_string()))?;
    for s in plmns {
        let value = s.parse::<Plmn>()?.to_encoded();
        if !entry.plmns.contains(&value) {
            entry.plmns.push(value);
        }
    }
    Ok(map.encode_to_vec())
}

/// `mapping inject-plmn <carrier> <plmn>...`: stdin (binarypb) -> stdout (binarypb).
pub fn inject(carrier: &str, plmns: &[String]) -> anyhow::Result<i32> {
    let out = inject_bytes(&read_stdin()?, carrier, plmns)?;
    write_stdout(&out)?;
    Ok(0)
}

/// Append `plmns` (MCC-MNC) to the EXISTING carrier `carrier`, strictly. The input is
/// de-duplicated (a PLMN listed twice is added once); it errors if `carrier` is absent, a
/// PLMN is malformed, or a PLMN is already mapped under ANY carrier (naming the owner). On
/// any error it returns before emitting bytes, so the output is never partially written.
pub fn add_plmns_strict(input: &[u8], carrier: &str, plmns: &[String]) -> anyhow::Result<Vec<u8>> {
    let mut map = PlmnMap::decode(input).map_err(Error::Decode)?;

    // Parse + de-duplicate the requested PLMNs, keeping the original strings for messages.
    let mut wanted: Vec<(String, u64)> = Vec::new();
    for s in plmns {
        let value = s.parse::<Plmn>()?.to_encoded();
        if !wanted.iter().any(|(_, v)| *v == value) {
            wanted.push((s.clone(), value));
        }
    }

    // Reject any PLMN already mapped under any carrier.
    for (s, value) in &wanted {
        if let Some(owner) = map.carriers.iter().find(|c| c.plmns.contains(value)) {
            anyhow::bail!("PLMN {s} is already mapped to carrier {}", owner.name);
        }
    }

    let entry = map
        .carriers
        .iter_mut()
        .find(|c| c.name == carrier)
        .ok_or_else(|| Error::MappingNotFound(carrier.to_string()))?;
    entry
        .plmns
        .extend(wanted.into_iter().map(|(_, value)| value));
    Ok(map.encode_to_vec())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proto::{Carrier, PlmnMap};
    use prost::Message;

    #[test]
    fn round_trip_is_bit_identical() {
        // synthetic legend: multiple carriers; multiple, duplicate, and wildcard PLMNs
        let map = PlmnMap {
            carriers: vec![
                Carrier {
                    plmns: vec![5_435_408, 197_154, 197_154],
                    index: 1,
                    name: "AAA".into(),
                },
                Carrier {
                    plmns: vec![2_291_967],
                    index: 2,
                    name: "BBB".into(),
                },
            ],
        };
        let original = map.encode_to_vec();
        let toml = decode_bytes(&original).unwrap();
        let reencoded = encode_bytes(&toml).unwrap();
        assert_eq!(original, reencoded, "decode->encode must be bit-identical");
    }

    #[test]
    fn real_file_round_trip_is_bit_identical() {
        // Opt-in: set UECAPS_PLMN_FIXTURE=/path/to/ap_plmn_mapping.binarypb
        let Ok(path) = std::env::var("UECAPS_PLMN_FIXTURE") else {
            return;
        };
        let original = std::fs::read(&path).expect("reading fixture");
        let toml = decode_bytes(&original).unwrap();
        let reencoded = encode_bytes(&toml).unwrap();
        assert_eq!(
            original, reencoded,
            "real-file round-trip must be bit-identical"
        );
    }

    #[test]
    fn inject_appends_new_plmn() {
        let map = PlmnMap {
            carriers: vec![Carrier {
                plmns: vec![5_435_408],
                index: 1,
                name: "VZW".into(),
            }],
        };
        let out = inject_bytes(&map.encode_to_vec(), "VZW", &["302-220".to_string()]).unwrap();
        let back = PlmnMap::decode(&out[..]).unwrap();
        assert_eq!(back.carriers[0].plmns, vec![5_435_408, 197_154]);
    }

    #[test]
    fn inject_skips_duplicate() {
        let map = PlmnMap {
            carriers: vec![Carrier {
                plmns: vec![5_435_408],
                index: 1,
                name: "VZW".into(),
            }],
        };
        // "250-01" encodes to 5435408, already present
        let out = inject_bytes(&map.encode_to_vec(), "VZW", &["250-01".to_string()]).unwrap();
        let back = PlmnMap::decode(&out[..]).unwrap();
        assert_eq!(back.carriers[0].plmns, vec![5_435_408]);
    }

    #[test]
    fn inject_unknown_carrier_errors() {
        let map = PlmnMap {
            carriers: vec![Carrier {
                plmns: vec![5_435_408],
                index: 1,
                name: "VZW".into(),
            }],
        };
        assert!(inject_bytes(&map.encode_to_vec(), "NOPE", &["250-01".to_string()]).is_err());
    }

    #[test]
    fn inject_bad_plmn_errors() {
        let map = PlmnMap {
            carriers: vec![Carrier {
                plmns: vec![5_435_408],
                index: 1,
                name: "VZW".into(),
            }],
        };
        assert!(inject_bytes(&map.encode_to_vec(), "VZW", &["not-a-plmn".to_string()]).is_err());
    }

    #[test]
    fn add_plmns_strict_appends_new() {
        let map = PlmnMap {
            carriers: vec![Carrier {
                plmns: vec![5_435_408],
                index: 1,
                name: "VZW".into(),
            }],
        };
        let out = add_plmns_strict(&map.encode_to_vec(), "VZW", &["302-220".to_string()]).unwrap();
        let back = PlmnMap::decode(&out[..]).unwrap();
        assert_eq!(back.carriers[0].plmns, vec![5_435_408, 197_154]); // 302-220 -> 197154
    }

    #[test]
    fn add_plmns_strict_dedups_input() {
        let map = PlmnMap {
            carriers: vec![Carrier {
                plmns: vec![],
                index: 1,
                name: "VZW".into(),
            }],
        };
        let out = add_plmns_strict(
            &map.encode_to_vec(),
            "VZW",
            &["302-220".to_string(), "302-220".to_string()],
        )
        .unwrap();
        let back = PlmnMap::decode(&out[..]).unwrap();
        assert_eq!(back.carriers[0].plmns, vec![197_154]); // added once
    }

    #[test]
    fn add_plmns_strict_rejects_existing_same_carrier() {
        let map = PlmnMap {
            carriers: vec![Carrier {
                plmns: vec![5_435_408],
                index: 1,
                name: "VZW".into(),
            }],
        };
        // 250-01 encodes to 5435408, already on VZW
        assert!(add_plmns_strict(&map.encode_to_vec(), "VZW", &["250-01".to_string()]).is_err());
    }

    #[test]
    fn add_plmns_strict_rejects_existing_other_carrier() {
        let map = PlmnMap {
            carriers: vec![
                Carrier {
                    plmns: vec![5_435_408],
                    index: 1,
                    name: "AAA".into(),
                },
                Carrier {
                    plmns: vec![],
                    index: 2,
                    name: "VZW".into(),
                },
            ],
        };
        // 250-01 already on AAA; adding to VZW must error and name the owner
        let err =
            add_plmns_strict(&map.encode_to_vec(), "VZW", &["250-01".to_string()]).unwrap_err();
        assert!(
            err.to_string().contains("AAA"),
            "error should name the owning carrier: {err}"
        );
    }

    #[test]
    fn add_plmns_strict_rejects_absent_carrier() {
        let map = PlmnMap {
            carriers: vec![Carrier {
                plmns: vec![5_435_408],
                index: 1,
                name: "VZW".into(),
            }],
        };
        assert!(add_plmns_strict(&map.encode_to_vec(), "NOPE", &["302-220".to_string()]).is_err());
    }

    #[test]
    fn add_plmns_strict_rejects_malformed() {
        let map = PlmnMap {
            carriers: vec![Carrier {
                plmns: vec![],
                index: 1,
                name: "VZW".into(),
            }],
        };
        assert!(
            add_plmns_strict(&map.encode_to_vec(), "VZW", &["not-a-plmn".to_string()]).is_err()
        );
    }
}
