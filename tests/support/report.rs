use pixel_uecaps_toolbox::{NR_BAND_OFFSET, proto::*};
use prost::Message;
use std::{
    ffi::OsStr,
    fs,
    path::{Path, PathBuf},
    process::{Command, Output},
};
use tempfile::TempDir;

pub fn fixture() -> TempDir {
    let root = PathBuf::from(std::env::var_os("HOME").expect("HOME"))
        .join(".superpowers/pixel-uecaps-toolbox/report-correctness-runtime");
    fs::create_dir_all(&root).unwrap();
    tempfile::tempdir_in(root).unwrap()
}
pub fn write_message(path: &Path, message: &impl Message) {
    fs::write(path, message.encode_to_vec()).unwrap();
}
pub fn run<I, S>(args: I) -> Output
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    Command::new(env!("CARGO_BIN_EXE_pixel-uecaps-toolbox"))
        .args(args)
        .output()
        .unwrap()
}
pub fn text(out: &Output) -> String {
    format!(
        "{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}
pub fn carrier() -> UeCaps {
    UeCaps {
        version: 862_505_271,
        combo_groups: vec![ComboGroup {
            combo_header: Some(ComboHeader::default()),
            combo: vec![Combo {
                sub_blocks: vec![SubBlock {
                    band: NR_BAND_OFFSET + 78,
                    dl_bw_class: Some(3),
                    ul_bw_class: Some(3),
                    dl_feature_index: Some(1),
                    ul_feature_index: Some(1),
                    dl_feature_per_cc_ids: Some(vec![1, 1]),
                    ul_feature_per_cc_ids: Some(vec![1, 1]),
                    ..Default::default()
                }],
                bitmask: Some(0),
            }],
        }],
        dl_feature_per_cc_list: [100, 50]
            .map(|bw| ShannonFeatureSetDlPerCcNr {
                max_scs: Some(1),
                max_bw: Some(bw),
                ..Default::default()
            })
            .to_vec(),
        ul_feature_per_cc_list: [100, 50]
            .map(|bw| ShannonFeatureSetUlPerCcNr {
                max_scs: Some(1),
                max_bw: Some(bw),
                ..Default::default()
            })
            .to_vec(),
        ..Default::default()
    }
}
pub fn compare_pair(a: &UeCaps, b: &UeCaps, full: bool) -> Output {
    let dir = fixture();
    let a_path = dir.path().join("a.binarypb");
    let b_path = dir.path().join("b.binarypb");
    write_message(&a_path, a);
    write_message(&b_path, b);
    let mut args = vec![
        OsStr::new("compare"),
        a_path.as_os_str(),
        b_path.as_os_str(),
    ];
    if full {
        args.push(OsStr::new("--full"));
    }
    run(args)
}
