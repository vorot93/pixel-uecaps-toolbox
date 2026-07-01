//! Carrier × profile matrix as CSV (`matrix`).

use super::binarypb_names;
use crate::model::{PROFILES, Parsed, Profile, identify_profile, parse_name};
use anyhow::Context;
use std::{collections::BTreeMap, fs, path::Path};

/// The column header for a profile: its known Pixel model, else the anchor prime.
fn header_for(p: &Profile) -> String {
    p.model.map_or_else(|| p.anchor.to_string(), String::from)
}

/// `(header, anchor)` for all 16 profiles, sorted by header text (anchor breaks ties).
fn sorted_columns() -> Vec<(String, u64)> {
    let mut cols: Vec<(String, u64)> = PROFILES.iter().map(|p| (header_for(p), p.anchor)).collect();
    cols.sort();
    cols
}

/// Render the matrix: a `carrier` header row, then one alphabetical row per carrier;
/// an absent profile is an empty cell. Columns are emitted in the given order.
fn build_csv(
    columns: &[(String, u64)],
    cells: &BTreeMap<String, BTreeMap<u64, u64>>,
) -> anyhow::Result<String> {
    let mut wtr = csv::WriterBuilder::new()
        .terminator(csv::Terminator::Any(b'\n'))
        .from_writer(Vec::new());

    let mut header: Vec<&str> = Vec::with_capacity(columns.len() + 1);
    header.push("carrier");
    header.extend(columns.iter().map(|(h, _)| h.as_str()));
    wtr.write_record(&header)?;

    for (carrier, row) in cells {
        let mut rec: Vec<String> = Vec::with_capacity(columns.len() + 1);
        rec.push(carrier.clone());
        for (_, anchor) in columns {
            rec.push(row.get(anchor).map(u64::to_string).unwrap_or_default());
        }
        wtr.write_record(&rec)?;
    }

    let bytes = wtr.into_inner().map_err(csv::IntoInnerError::into_error)?;
    Ok(String::from_utf8(bytes)?)
}

/// `matrix [DIR] [-o FILE]`: scan `dir` and emit the carrier × profile CSV to
/// `out` (a file) or stdout.
pub fn matrix(dir: &Path, out: Option<&Path>) -> anyhow::Result<i32> {
    let names = binarypb_names(dir)?;

    let mut cells: BTreeMap<String, BTreeMap<u64, u64>> = BTreeMap::new();
    for name in &names {
        if let Parsed::Carrier { carrier, number } = parse_name(name)
            && let Some(profile) = identify_profile(number)
        {
            cells
                .entry(carrier)
                .or_default()
                .insert(profile.anchor, number);
        }
    }

    let csv = build_csv(&sorted_columns(), &cells)?;
    match out {
        Some(path) => {
            fs::write(path, csv).with_context(|| format!("cannot write {}", path.display()))?
        }
        None => print!("{csv}"),
    }
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_uses_model_then_anchor() {
        let with_model = PROFILES.iter().find(|p| p.anchor == 3_616_442_437).unwrap();
        assert_eq!(header_for(with_model), "Pixel 10 Pro XL");
        let no_model = PROFILES.iter().find(|p| p.anchor == 8969).unwrap();
        assert_eq!(header_for(no_model), "8969");
    }

    #[test]
    fn columns_sorted_by_header_anchors_before_names() {
        let headers: Vec<String> = sorted_columns().into_iter().map(|(h, _)| h).collect();
        assert_eq!(
            headers,
            vec![
                "1002739",
                "196911437",
                "2912407",
                "3347",
                "3539",
                "688679",
                "8969",
                "Pixel 10 Pro Fold",
                "Pixel 10 Pro XL",
                "Pixel 9 (5G Sub-6 GHz)",
                "Pixel 9 (5G mmWave + Sub 6 GHz)",
                "Pixel 9 Pro (5G Sub-6 GHz)",
                "Pixel 9 Pro (5G mmWave + Sub 6 GHz)",
                "Pixel 9 Pro Fold",
                "Pixel 9 Pro XL (5G Sub 6 GHz)",
                "Pixel 9 Pro XL (5G mmWave + Sub 6 GHz)",
            ]
        );
    }

    #[test]
    fn build_csv_renders_columns_rows_and_empty_cells() {
        // already in sorted-header order: "99" < "Alpha" < "Bravo"
        let columns = vec![
            ("99".to_string(), 33u64),
            ("Alpha".to_string(), 22u64),
            ("Bravo".to_string(), 11u64),
        ];
        let mut cells: BTreeMap<String, BTreeMap<u64, u64>> = BTreeMap::new();
        cells.entry("ZED".into()).or_default().insert(11, 100);
        cells.entry("ZED".into()).or_default().insert(22, 200);
        cells.entry("ZED".into()).or_default().insert(33, 300);
        cells.entry("ABE".into()).or_default().insert(11, 1);
        cells.entry("ABE".into()).or_default().insert(33, 3); // missing anchor 22

        let csv = build_csv(&columns, &cells).unwrap();
        assert_eq!(csv, "carrier,99,Alpha,Bravo\nABE,3,,1\nZED,300,200,100\n");
    }

    #[test]
    fn build_csv_quotes_fields_with_commas() {
        let columns = vec![("Pixel, Comma".to_string(), 7u64)];
        let mut cells: BTreeMap<String, BTreeMap<u64, u64>> = BTreeMap::new();
        cells.entry("C".into()).or_default().insert(7, 42);

        let csv = build_csv(&columns, &cells).unwrap();
        assert_eq!(csv, "carrier,\"Pixel, Comma\"\nC,42\n");
    }
}
