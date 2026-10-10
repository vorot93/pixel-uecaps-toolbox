//! Carrier × profile matrix as CSV (`matrix`).

use super::{
    folder::{FolderReport, scan_folder},
    validation::{completion, diagnostics},
};
use crate::{
    model::{PROFILES, Profile},
    outcome::Outcome,
};
use anyhow::Context;
use std::{collections::BTreeMap, fmt::Write, path::Path};

/// The column header for a profile: its known Pixel model, else the anchor prime.
fn header_for(p: &Profile) -> String {
    p.model.map_or_else(|| p.anchor.to_string(), String::from)
}

/// `(header, anchor)` for all 16 profiles, sorted by header text (anchor breaks ties).
fn sorted_columns() -> Vec<(String, u64)> {
    let mut cols: Vec<(String, u64)> = PROFILES.iter().map(|p| (header_for(p), p.anchor)).collect();
    cols.sort_unstable();
    cols
}

/// Render the matrix: a `carrier` header row, then one alphabetical row per carrier;
/// an absent profile is an empty cell. Columns are emitted in the given order.
fn build_csv(
    columns: &[(String, u64)],
    cells: &BTreeMap<String, BTreeMap<u64, Vec<u64>>>,
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
            // A cell with more than one number is a collision (two files for the same
            // carrier/anchor); render both joined rather than silently dropping one.
            let cell = row.get(anchor).map_or_else(String::new, |nums| {
                nums.iter()
                    .map(u64::to_string)
                    .collect::<Vec<_>>()
                    .join(" | ")
            });
            rec.push(cell);
        }
        wtr.write_record(&rec)?;
    }

    let bytes = wtr.into_inner().map_err(csv::IntoInnerError::into_error)?;
    Ok(String::from_utf8(bytes)?)
}

/// `matrix [DIR] [-o FILE]`: scan `dir` and emit the carrier × profile CSV to
/// `out` (a file) or stdout.
pub fn matrix(dir: &Path, out: Option<&Path>) -> anyhow::Result<Outcome> {
    finish_matrix(scan_folder(dir), out)
}
fn finish_matrix(
    report: anyhow::Result<FolderReport>,
    out: Option<&Path>,
) -> anyhow::Result<Outcome> {
    publish_matrix(&report?, out)
}
fn publish_matrix(report: &FolderReport, out: Option<&Path>) -> anyhow::Result<Outcome> {
    let mut cells: BTreeMap<String, BTreeMap<u64, Vec<u64>>> = BTreeMap::new();
    for c in &report.candidates {
        cells
            .entry(c.carrier.clone())
            .or_default()
            .entry(c.anchor)
            .or_default()
            .push(c.number);
    }
    let mut errors = diagnostics(report.errors());
    for path in &report.unsupported {
        writeln!(errors, "info: {path:?}: unsupported/not validated")?;
    }
    let csv = build_csv(&sorted_columns(), &cells)?;
    match out {
        // Through the crate's atomic writer like every other file output, so a failure part-way
        // cannot leave a truncated CSV in place of a previously good one, and replacing an
        // existing file keeps its mode.
        Some(path) => {
            super::write_report("", &errors)?;
            crate::atomic::write_bytes_atomic(path, csv.as_bytes())
                .with_context(|| format!("cannot write {}", path.display()))?;
        }
        None => super::write_report(&csv, &errors)?,
    }
    Ok(completion(report.has_errors()))
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
        let mut cells: BTreeMap<String, BTreeMap<u64, Vec<u64>>> = BTreeMap::new();
        cells.entry("ZED".into()).or_default().insert(11, vec![100]);
        cells.entry("ZED".into()).or_default().insert(22, vec![200]);
        cells.entry("ZED".into()).or_default().insert(33, vec![300]);
        cells.entry("ABE".into()).or_default().insert(11, vec![1]);
        cells.entry("ABE".into()).or_default().insert(33, vec![3]); // missing anchor 22

        let csv = build_csv(&columns, &cells).unwrap();
        assert_eq!(csv, "carrier,99,Alpha,Bravo\nABE,3,,1\nZED,300,200,100\n");
    }

    #[test]
    fn matrix_flags_same_anchor_collision() {
        // Two files for the same carrier both divisible by anchor 3347 (3347 and
        // 6694 = 2*3347) must not silently overwrite — both must appear and the run
        // must exit 2. Valid reference stubs remain eligible candidates.
        let dir = tempfile::tempdir().unwrap();
        use prost::Message;
        let bytes = crate::proto::UeCaps {
            version: 874_888_686,
            ..Default::default()
        }
        .encode_to_vec();
        std::fs::write(dir.path().join("ALPHA_3347.binarypb"), &bytes).unwrap();
        std::fs::write(dir.path().join("ALPHA_6694.binarypb"), &bytes).unwrap();
        let out = dir.path().join("m.csv");
        let code = matrix(dir.path(), Some(&out)).unwrap();
        let csv = std::fs::read_to_string(&out).unwrap();
        assert_eq!(
            code,
            Outcome::Rejected,
            "a (carrier, anchor) collision must exit 2:\n{csv}"
        );
        assert!(
            csv.contains("3347 | 6694"),
            "both colliding numbers must appear in the cell:\n{csv}"
        );
    }

    #[test]
    fn build_csv_quotes_fields_with_commas() {
        let columns = vec![("Pixel, Comma".to_string(), 7u64)];
        let mut cells: BTreeMap<String, BTreeMap<u64, Vec<u64>>> = BTreeMap::new();
        cells.entry("C".into()).or_default().insert(7, vec![42]);

        let csv = build_csv(&columns, &cells).unwrap();
        assert_eq!(csv, "carrier,\"Pixel, Comma\"\nC,42\n");
    }

    #[test]
    fn enumeration_failure_preserves_destination() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("out.csv");
        std::fs::write(&path, b"sentinel").unwrap();
        let error = finish_matrix(
            Err(anyhow::anyhow!("injected iteration failure")),
            Some(&path),
        )
        .unwrap_err();
        assert!(error.to_string().contains("injected iteration failure"));
        assert_eq!(std::fs::read(path).unwrap(), b"sentinel");
    }
}
