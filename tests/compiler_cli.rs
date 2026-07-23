use std::{
    fs,
    io::{Cursor, Read},
    path::{Path, PathBuf},
    process::{Command, Output},
};

use pixel_uecaps_toolbox::proto::{
    Carrier, ComboGroup, LteCaps, LteCombo, LteComponent, PlmnMap, ShannonFeatureSetDlPerCcNr,
    UeCaps,
    combo_group::{Combo, ComboHeader, combo::SubBlock},
};
use prost::Message;
use tempfile::TempDir;
use zip::ZipArchive;

const MODEL: &str = "G2YBB";
const NR_ANCHOR: u64 = 66_813_533;
const LTE_ID: u64 = 400_907_661;

struct Fixture {
    _temp: TempDir,
    bitmask: PathBuf,
    profiled: PathBuf,
    source: PathBuf,
    module: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let bitmask = temp.path().join("bitmask");
        let profiled = temp.path().join("profiled");
        let source = temp.path().join("source");
        let module = temp.path().join("module.zip");
        fs::create_dir(&bitmask).unwrap();
        fs::create_dir(&profiled).unwrap();

        write_message(
            &bitmask.join("ALPHA.binarypb"),
            &nr_caps(715_188_856, Some(7), 0, 10_041, Some(29)),
        );
        let mut profiled_caps = nr_caps(862_505_271, None, 11, 10_078, Some(0));
        profiled_caps.dl_feature_per_cc_list = vec![
            ShannonFeatureSetDlPerCcNr {
                max_scs: Some(1),
                ..Default::default()
            },
            ShannonFeatureSetDlPerCcNr {
                max_scs: Some(3),
                max_bw: Some(100),
                ..Default::default()
            },
        ];
        // A single in-range byte: all-or-nothing resolution needs every byte in range to
        // resolve (an out-of-range trailing byte, e.g. `[2, 99]`, now stays raw instead
        // of resolving on the in-range prefix).
        profiled_caps.combo_groups[0].combo[0].sub_blocks[0].dl_feature_per_cc_ids = Some(vec![2]);
        write_message(
            &profiled.join(format!("ALPHA_{NR_ANCHOR}.binarypb")),
            &profiled_caps,
        );
        write_message(
            &profiled.join("ap_plmn_mapping.binarypb"),
            &PlmnMap {
                carriers: vec![Carrier {
                    plmns: vec![5_435_408],
                    index: 7,
                    name: "ALPHA".into(),
                }],
            },
        );
        write_message(
            &profiled.join(format!("lte_{LTE_ID}.binarypb")),
            &LteCaps {
                fingerprint: 123,
                combos: vec![LteCombo {
                    components: vec![LteComponent {
                        band: 1,
                        dl_bw_class_mimo: 32_768,
                        ul_bw_class_mimo: Some(0),
                    }],
                    bcs: Some(0),
                    unknown1: Some(0),
                    unknown2: Some(0),
                }],
                bitmask: 456,
            },
        );

        Self {
            _temp: temp,
            bitmask,
            profiled,
            source,
            module,
        }
    }

    fn decompose(&self) -> Output {
        command()
            .args(["decompose", "--bitmask"])
            .arg(&self.bitmask)
            .arg("--profiled")
            .arg(&self.profiled)
            .arg("-o")
            .arg(&self.source)
            .output()
            .unwrap()
    }

    fn provision(&self, model: &str) -> Output {
        command()
            .arg("provision")
            .arg(model)
            .arg(&self.source)
            .arg("-o")
            .arg(&self.module)
            .output()
            .unwrap()
    }
}

fn command() -> Command {
    Command::new(env!("CARGO_BIN_EXE_pixel-uecaps-toolbox"))
}

fn write_message(path: &Path, message: &impl Message) {
    fs::write(path, message.encode_to_vec()).unwrap();
}

fn nr_caps(
    fingerprint: u64,
    id: Option<i32>,
    unknown: u64,
    band: i32,
    bitmask: Option<u32>,
) -> UeCaps {
    UeCaps {
        version: fingerprint,
        id,
        combo_groups: vec![ComboGroup {
            // The four corpus-verified always-`Some` header fields (all but
            // `bcs_intra_endc`) — the strict decode boundary (`raw_nr::from_proto_combo`)
            // fails closed on a missing one (Task 8).
            combo_header: Some(ComboHeader {
                power_class: Some(0),
                bcs_nr: Some(0),
                bcs_intra_endc: None,
                bcs_eutra: Some(0),
                intra_band_en_dc_support: Some(0),
            }),
            combo: vec![Combo {
                sub_blocks: vec![SubBlock {
                    band,
                    dl_bw_class: Some(1),
                    ul_bw_class: Some(1),
                    ..Default::default()
                }],
                bitmask,
            }],
        }],
        unknown,
        ..Default::default()
    }
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

#[test]
fn decompose_then_provision_runs_the_real_compiler_pipeline() {
    let fixture = Fixture::new();

    let decoded = fixture.decompose();
    assert!(decoded.status.success(), "{}", stderr(&decoded));
    let mut source_names = fs::read_dir(&fixture.source)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect::<Vec<_>>();
    source_names.sort();
    assert_eq!(source_names, ["lte.kdl", "nr.kdl"]);
    let nr_source = fs::read_to_string(fixture.source.join("nr.kdl")).unwrap();
    assert!(nr_source.contains("mapping-id=7"), "{nr_source}");
    assert!(!nr_source.contains("profiled-id="), "{nr_source}");
    assert!(nr_source.contains("dl-feature"), "{nr_source}");
    assert!(nr_source.contains("max-scs=3"), "{nr_source}");
    assert!(!nr_source.contains("max-scs=1"), "{nr_source}");
    assert!(nr_source.contains("dl-feature=1"), "{nr_source}");

    let provisioned = fixture.provision(MODEL);
    assert!(provisioned.status.success(), "{}", stderr(&provisioned));
    let zip = fs::read(&fixture.module).unwrap();
    let mut archive = ZipArchive::new(Cursor::new(zip)).unwrap();
    assert!(
        archive
            .by_name("system/vendor/firmware/uecapconfig/.replace")
            .is_ok()
    );
    assert!(
        archive
            .by_name(&format!(
                "system/vendor/firmware/uecapconfig/ALPHA_{NR_ANCHOR}.binarypb"
            ))
            .is_ok()
    );
    assert!(
        archive
            .by_name(&format!(
                "system/vendor/firmware/uecapconfig/lte_{LTE_ID}.binarypb"
            ))
            .is_ok()
    );
    let mut carrier = archive
        .by_name(&format!(
            "system/vendor/firmware/uecapconfig/ALPHA_{NR_ANCHOR}.binarypb"
        ))
        .unwrap();
    let mut carrier_bytes = Vec::new();
    carrier.read_to_end(&mut carrier_bytes).unwrap();
    let caps = UeCaps::decode(carrier_bytes.as_slice()).unwrap();
    assert_eq!(caps.id, None);
    assert_eq!(caps.dl_feature_per_cc_list.len(), 1);
    assert_eq!(caps.dl_feature_per_cc_list[0].max_scs, Some(3));
    assert_eq!(
        caps.combo_groups[0].combo[0].sub_blocks[0].dl_feature_per_cc_ids,
        Some(vec![1])
    );
}

#[test]
fn provision_unknown_model_is_a_hard_error_that_lists_registered_models() {
    let fixture = Fixture::new();
    let decoded = fixture.decompose();
    assert!(decoded.status.success(), "{}", stderr(&decoded));

    let provisioned = fixture.provision("NOT-A-MODEL");
    assert_eq!(provisioned.status.code(), Some(2));
    let stderr = stderr(&provisioned);
    assert!(stderr.contains("unknown model"), "{stderr}");
    assert!(stderr.contains(MODEL), "{stderr}");
    assert!(!fixture.module.exists());
}
