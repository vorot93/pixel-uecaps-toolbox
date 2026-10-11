#[path = "support/report.rs"]
mod support;
use pixel_uecaps_toolbox::proto::*;
use std::{ffi::OsStr, fs};
use support::{carrier, compare_pair, text};

#[test]
fn every_modeled_feature_and_header_field_is_compared_and_rendered() {
    let a = carrier();
    type FieldMutation = (&'static str, fn(&mut UeCaps));
    let cases: &[FieldMutation] = &[
        ("power_class=7", |c| {
            c.combo_groups[0].combo_header.as_mut().unwrap().power_class = Some(7)
        }),
        ("bcs_nr=7", |c| {
            c.combo_groups[0].combo_header.as_mut().unwrap().bcs_nr = Some(7)
        }),
        ("bcs_intra_endc=7", |c| {
            c.combo_groups[0]
                .combo_header
                .as_mut()
                .unwrap()
                .bcs_intra_endc = Some(7)
        }),
        ("bcs_eutra=7", |c| {
            c.combo_groups[0].combo_header.as_mut().unwrap().bcs_eutra = Some(7)
        }),
        ("intra_band_en_dc_support=7", |c| {
            c.combo_groups[0]
                .combo_header
                .as_mut()
                .unwrap()
                .intra_band_en_dc_support = Some(7)
        }),
        ("max_scs=7", |c| {
            c.dl_feature_per_cc_list[0].max_scs = Some(7)
        }),
        ("max_mimo=7", |c| {
            c.dl_feature_per_cc_list[0].max_mimo = Some(7)
        }),
        ("max_bw=7", |c| c.dl_feature_per_cc_list[0].max_bw = Some(7)),
        ("max_mod_order=7", |c| {
            c.dl_feature_per_cc_list[0].max_mod_order = Some(7)
        }),
        ("bw_90mhz_supported=false", |c| {
            c.dl_feature_per_cc_list[0].bw_90mhz_supported = Some(false)
        }),
        ("max_scs=7", |c| {
            c.ul_feature_per_cc_list[0].max_scs = Some(7)
        }),
        ("max_mimo_cb=7", |c| {
            c.ul_feature_per_cc_list[0].max_mimo_cb = Some(7)
        }),
        ("max_bw=7", |c| c.ul_feature_per_cc_list[0].max_bw = Some(7)),
        ("max_mod_order=7", |c| {
            c.ul_feature_per_cc_list[0].max_mod_order = Some(7)
        }),
        ("bw_90mhz_supported=false", |c| {
            c.ul_feature_per_cc_list[0].bw_90mhz_supported = Some(false)
        }),
        ("max_mimo_non_cb=7", |c| {
            c.ul_feature_per_cc_list[0].max_mimo_non_cb = Some(7)
        }),
    ];
    for (token, mutate) in cases {
        let mut b = a.clone();
        mutate(&mut b);
        let body = changed(&a, &b);
        assert!(body.contains(token), "{token}: {body}");
    }
}

#[cfg(unix)]
#[test]
fn folders_disclose_supported_unreadable_inputs_and_legend_errors() {
    let dir = support::fixture();
    support::write_message(&dir.path().join("VALID_66813533.binarypb"), &carrier());
    std::os::unix::fs::symlink("missing", dir.path().join("lte_400907661.binarypb")).unwrap();
    fs::create_dir(dir.path().join("ALPHA_1.binarypb")).unwrap();
    support::write_message(
        &dir.path().join("ap_plmn_mapping.binarypb"),
        &PlmnMap {
            carriers: vec![Carrier {
                name: "".into(),
                index: 0,
                plmns: vec![1 << 24],
            }],
        },
    );
    for command in ["check", "matrix"] {
        let out = support::run([OsStr::new(command), dir.path().as_os_str()]);
        let body = text(&out);
        assert_eq!(out.status.code(), Some(2), "{body}");
        for token in [
            "cannot read",
            "lte_400907661.binarypb",
            "ALPHA_1.binarypb",
            "anchor",
            "16777216",
            "empty",
        ] {
            assert!(body.contains(token), "{token}: {body}");
        }
        if command == "matrix" {
            assert_eq!(rows(&out.stdout).len(), 1);
        }
    }
}

#[test]
fn full_comparison_explains_different_shapes_with_identical_labels() {
    let mut a = carrier();
    a.combo_groups[0].combo[0].sub_blocks[0].ul_bw_class = None;
    let mut b = a.clone();
    b.combo_groups[0].combo[0].sub_blocks[0].ul_bw_class = Some(0);
    let out = compare_pair(&a, &b, true);
    let body = text(&out);
    assert_eq!(out.status.code(), Some(0));
    for token in [
        "1 only in A",
        "1 only in B",
        "ul_bw_class=absent",
        "ul_bw_class=0",
    ] {
        assert!(body.contains(token), "{token}: {body}");
    }
}
#[test]
fn repeated_components_retain_multiplicity() {
    let a = carrier();
    let mut b = a.clone();
    let component = b.combo_groups[0].combo[0].sub_blocks[0].clone();
    b.combo_groups[0].combo[0].sub_blocks.push(component);
    let out = compare_pair(&a, &b, true);
    let body = text(&out);
    assert_eq!(out.status.code(), Some(0));
    assert!(
        body.contains("1 only in A") && body.contains("1 only in B"),
        "{body}"
    );
}

#[cfg(target_os = "linux")]
#[test]
fn failed_error_stream_returns_two_instead_of_panicking() {
    let dir = support::fixture();
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_pixel-uecaps-toolbox"))
        .args([
            OsStr::new("inspect"),
            dir.path().join("missing.binarypb").as_os_str(),
        ])
        .stderr(
            fs::OpenOptions::new()
                .write(true)
                .open("/dev/full")
                .unwrap(),
        )
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
}
#[cfg(target_os = "linux")]
#[test]
fn report_stdout_failures_return_two() {
    let dir = support::fixture();
    let path = dir.path().join("ALPHA_66813533.binarypb");
    support::write_message(&path, &carrier());
    for args in [
        vec![OsStr::new("inspect"), path.as_os_str()],
        vec![OsStr::new("check"), dir.path().as_os_str()],
        vec![OsStr::new("matrix"), dir.path().as_os_str()],
        vec![OsStr::new("compare"), path.as_os_str(), path.as_os_str()],
    ] {
        let out = std::process::Command::new(env!("CARGO_BIN_EXE_pixel-uecaps-toolbox"))
            .args(args)
            .stdout(
                fs::OpenOptions::new()
                    .write(true)
                    .open("/dev/full")
                    .unwrap(),
            )
            .output()
            .unwrap();
        assert_eq!(out.status.code(), Some(2), "{}", text(&out));
        assert!(!String::from_utf8_lossy(&out.stderr).contains("panicked"));
    }
}
#[test]
fn report_commands_share_zero_two_contract() {
    let dir = support::fixture();
    let path = dir.path().join("ALPHA_66813533.binarypb");
    for invalid in [false, true] {
        if invalid {
            fs::write(&path, [0x78, 1]).unwrap();
        } else {
            support::write_message(&path, &carrier());
        }
        for args in [
            vec![OsStr::new("inspect"), path.as_os_str()],
            vec![OsStr::new("check"), dir.path().as_os_str()],
            vec![OsStr::new("matrix"), dir.path().as_os_str()],
            vec![OsStr::new("compare"), path.as_os_str(), path.as_os_str()],
        ] {
            let out = support::run(args);
            assert_eq!(
                out.status.code(),
                Some(if invalid { 2 } else { 0 }),
                "{}",
                text(&out)
            );
        }
    }
}
#[test]
fn comparison_flags_do_not_change_identity_or_status() {
    let dir = support::fixture();
    let a = dir.path().join("a.binarypb");
    let b = dir.path().join("b.binarypb");
    let caps = carrier();
    support::write_message(&a, &caps);
    for equal in [true, false] {
        let mut right = caps.clone();
        if !equal {
            right.dl_feature_per_cc_list[0].max_bw = Some(50);
        }
        support::write_message(&b, &right);
        for flags in [
            vec![],
            vec!["--common"],
            vec!["--full"],
            vec!["--full", "--common"],
        ] {
            let mut args = vec![OsStr::new("compare"), a.as_os_str(), b.as_os_str()];
            args.extend(flags.iter().map(OsStr::new));
            let out = support::run(args);
            let body = text(&out);
            assert_eq!(out.status.code(), Some(0), "{body}");
            assert!(
                body.contains(if equal {
                    "no differences"
                } else {
                    "1 caps-changed"
                }),
                "{body}"
            );
            assert_eq!(body.contains("\ncommon ("), flags.contains(&"--common"));
        }
    }
}
#[test]
fn matrix_diagnostics_and_csv_are_repeatable_with_mixed_inputs() {
    let dir = support::fixture();
    support::write_message(&dir.path().join("ALPHA_66813533.binarypb"), &carrier());
    fs::write(dir.path().join("lte_400907661.binarypb"), [0x78, 1]).unwrap();
    fs::create_dir(dir.path().join("unsupported.binarypb")).unwrap();
    let a = matrix(dir.path());
    let b = matrix(dir.path());
    assert_eq!(a.status.code(), Some(2));
    assert_eq!(b.status.code(), Some(2));
    assert_eq!(a.stdout, b.stdout);
    assert_eq!(a.stderr, b.stderr);
    assert_eq!(rows(&a.stdout).len(), 1);
    let err = String::from_utf8_lossy(&a.stderr);
    assert!(
        err.contains("unsupported/not validated") && err.contains("field #15"),
        "{err}"
    );
}

fn matrix(dir: &std::path::Path) -> std::process::Output {
    support::run([OsStr::new("matrix"), dir.as_os_str()])
}
fn rows(bytes: &[u8]) -> Vec<csv::StringRecord> {
    let mut reader = csv::Reader::from_reader(bytes);
    assert_eq!(reader.headers().unwrap().get(0), Some("carrier"));
    reader.records().collect::<Result<_, _>>().unwrap()
}
#[test]
fn matrix_retains_valid_collision_partners_in_numeric_order() {
    let dir = support::fixture();
    support::write_message(&dir.path().join("ALPHA_66813533.binarypb"), &carrier());
    let partner = dir.path().join("ALPHA_133627066.binarypb");
    fs::write(&partner, [0x78, 1]).unwrap();
    fs::write(dir.path().join("lte_400907661.binarypb"), [0x78, 1]).unwrap();
    for expected in ["66813533", "66813533 | 133627066"] {
        let out = matrix(dir.path());
        let body = text(&out);
        assert_eq!(out.status.code(), Some(2), "{body}");
        let records = rows(&out.stdout);
        assert_eq!(records.len(), 1);
        assert_eq!(&records[0][0], "ALPHA");
        assert_eq!(
            records[0]
                .iter()
                .filter(|s| !s.is_empty())
                .collect::<Vec<_>>(),
            ["ALPHA", expected]
        );
        let err = String::from_utf8_lossy(&out.stderr);
        for token in [
            "ALPHA_133627066.binarypb",
            "ALPHA_66813533.binarypb",
            "lte_400907661.binarypb",
            "collision",
        ] {
            assert!(err.contains(token), "{err}");
        }
        assert!(!String::from_utf8_lossy(&out.stdout).contains("error:"));
        support::write_message(&partner, &carrier());
    }
}
#[test]
fn matrix_omits_each_invalid_candidate_and_keeps_valid_siblings() {
    for case in 0..5 {
        let dir = support::fixture();
        support::write_message(&dir.path().join("VALID_66813533.binarypb"), &carrier());
        let mut bad = carrier();
        let (name, reason) = match case {
            0 => ("ALPHA_1.binarypb", "anchor"),
            1 => ("ALPHA_558949.binarypb", "anchor"),
            2 => {
                bad.version = 123;
                ("ALPHA_66813533.binarypb", "unknown fingerprint")
            }
            3 => {
                bad.version = 874_888_686;
                ("ALPHA_66813533.binarypb", "fingerprint family")
            }
            _ => {
                bad.combo_groups[0].combo[0].sub_blocks[0].dl_feature_per_cc_ids =
                    Some(vec![1, 255]);
                ("ALPHA_66813533.binarypb", "DL[2]")
            }
        };
        support::write_message(&dir.path().join(name), &bad);
        let out = matrix(dir.path());
        assert_eq!(out.status.code(), Some(2), "{}", text(&out));
        let records = rows(&out.stdout);
        assert_eq!(records.len(), 1);
        assert_eq!(&records[0][0], "VALID");
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(err.contains(name) && err.contains(reason), "{err}");
    }
}
#[test]
fn matrix_raw_eutra_and_distinct_carrier_tiers_are_valid() {
    let dir = support::fixture();
    let mut caps = carrier();
    caps.combo_groups[0].combo[0].sub_blocks[0].band = 66;
    caps.combo_groups[0].combo[0].sub_blocks[0].dl_feature_per_cc_ids = Some(vec![0, 255]);
    support::write_message(&dir.path().join("ALPHA_66813533.binarypb"), &caps);
    caps.version = 627_223_094;
    support::write_message(&dir.path().join("BETA_66813533.binarypb"), &caps);
    let out = matrix(dir.path());
    assert_eq!(out.status.code(), Some(0), "{}", text(&out));
    assert_eq!(rows(&out.stdout).len(), 2);
    fs::rename(
        dir.path().join("BETA_66813533.binarypb"),
        dir.path().join("ALPHA_133627066.binarypb"),
    )
    .unwrap();
    let out = matrix(dir.path());
    assert_eq!(out.status.code(), Some(2));
    assert!(text(&out).contains("mixes tier"));
    assert!(
        rows(&out.stdout)[0]
            .iter()
            .any(|v| v == "66813533 | 133627066")
    );
}
#[test]
fn matrix_unsupported_only_is_header_and_information() {
    let dir = support::fixture();
    fs::create_dir(dir.path().join("mystery.binarypb")).unwrap();
    fs::write(dir.path().join(".binarypb"), [255]).unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink("missing", dir.path().join("unmodeled.binarypb")).unwrap();
    let out = matrix(dir.path());
    assert_eq!(out.status.code(), Some(0), "{}", text(&out));
    assert!(rows(&out.stdout).is_empty());
    assert!(String::from_utf8_lossy(&out.stderr).contains("unsupported/not validated"));
    assert!(!String::from_utf8_lossy(&out.stdout).contains("unsupported"));
}
#[test]
fn matrix_publishes_partial_file_but_preserves_destination_on_scan_failure() {
    let dir = support::fixture();
    support::write_message(&dir.path().join("VALID_66813533.binarypb"), &carrier());
    fs::write(dir.path().join("lte_400907661.binarypb"), [0x78, 1]).unwrap();
    let dest = tempfile::NamedTempFile::new_in(dir.path()).unwrap();
    fs::write(dest.path(), b"sentinel").unwrap();
    let publish = |input: &std::path::Path, output: &std::path::Path| {
        support::run([
            OsStr::new("matrix"),
            input.as_os_str(),
            OsStr::new("-o"),
            output.as_os_str(),
        ])
    };
    let out = publish(dir.path(), dest.path());
    assert_eq!(out.status.code(), Some(2), "{}", text(&out));
    assert!(out.stdout.is_empty());
    assert_eq!(rows(&fs::read(dest.path()).unwrap()).len(), 1);
    assert!(String::from_utf8_lossy(&out.stderr).contains("lte_400907661.binarypb"));
    fs::write(dest.path(), b"sentinel").unwrap();
    assert_eq!(
        publish(&dir.path().join("absent"), dest.path())
            .status
            .code(),
        Some(2)
    );
    assert_eq!(fs::read(dest.path()).unwrap(), b"sentinel");
    let absent = dir.path().join("absent/out.csv");
    assert_eq!(publish(dir.path(), &absent).status.code(), Some(2));
    assert!(!absent.exists());
}

#[test]
fn check_decodes_lte_and_reports_selector_collisions() {
    let dir = support::fixture();
    for name in ["ALPHA_66813533.binarypb", "ALPHA_133627066.binarypb"] {
        support::write_message(&dir.path().join(name), &carrier());
    }
    fs::write(dir.path().join("lte_400907661.binarypb"), [0x78, 0x01]).unwrap();
    let out = support::run([OsStr::new("check"), dir.path().as_os_str()]);
    let body = text(&out);
    assert_eq!(out.status.code(), Some(2), "{body}");
    for token in [
        "lte_400907661.binarypb",
        "15",
        "collision",
        "ALPHA_66813533.binarypb",
        "ALPHA_133627066.binarypb",
        "validated profile coverage",
        "1/16",
    ] {
        assert!(body.contains(token), "{token}: {body}");
    }
}

#[test]
fn check_accumulates_independent_errors_and_excludes_invalid_coverage() {
    let dir = support::fixture();
    fs::write(dir.path().join("ALPHA_558949.binarypb"), [0x78, 0x01]).unwrap();
    let mut caps = carrier();
    caps.version = 123;
    support::write_message(&dir.path().join("BETA_66813533.binarypb"), &caps);
    caps.version = 874_888_686;
    support::write_message(&dir.path().join("GAMMA_66813533.binarypb"), &caps);
    support::write_message(
        &dir.path().join("ap_plmn_mapping.binarypb"),
        &PlmnMap {
            carriers: vec![Carrier {
                name: "".into(),
                index: 1,
                plmns: vec![1 << 24],
            }],
        },
    );
    let out = support::run([OsStr::new("check"), dir.path().as_os_str()]);
    let body = text(&out);
    assert_eq!(out.status.code(), Some(2), "{body}");
    for token in [
        "anchor",
        "167",
        "3347",
        "field #15",
        "unknown fingerprint 123",
        "fingerprint family",
        "16777216",
        "empty",
        "unavailable",
        "0/16",
    ] {
        assert!(body.contains(token), "{token}: {body}");
    }
}

#[test]
fn check_unsupported_names_are_informational() {
    let dir = support::fixture();
    for name in [".binarypb", "mystery.binarypb"] {
        fs::write(dir.path().join(name), [0x78, 0x01]).unwrap();
    }
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStringExt;
        fs::write(
            dir.path()
                .join(std::ffi::OsString::from_vec(b"bad\xff.binarypb".to_vec())),
            [0xff],
        )
        .unwrap();
    }
    let out = support::run([OsStr::new("check"), dir.path().as_os_str()]);
    let body = text(&out);
    assert_eq!(out.status.code(), Some(0), "{body}");
    assert!(body.contains("unsupported/not validated"), "{body}");
    assert!(body.contains("mystery.binarypb") && body.contains(".binarypb"));
    #[cfg(unix)]
    assert!(body.contains("\\xFF"), "{body}");
}

fn inspect(path: &std::path::Path) -> std::process::Output {
    support::run([
        OsStr::new("inspect"),
        path.as_os_str(),
        OsStr::new("--full"),
    ])
}

#[test]
fn main_only_profiles_are_rejected_in_alt_inspection_coverage_and_matrix() {
    for anchor in [2_912_407, 3539] {
        let dir = support::fixture();
        let path = dir.path().join(format!("ALPHA_{anchor}.binarypb"));
        for (fingerprint, status, coverage) in [
            (707_802_847, 2, "0/14 profiles"),
            (627_223_094, 2, "0/14 profiles"),
            (874_888_686, 0, "1/16 profiles"),
        ] {
            support::write_message(
                &path,
                &UeCaps {
                    version: fingerprint,
                    ..Default::default()
                },
            );
            let inspected = inspect(&path);
            assert_eq!(
                inspected.status.code(),
                Some(status),
                "{}",
                text(&inspected)
            );
            let checked = support::run([OsStr::new("check"), dir.path().as_os_str()]);
            assert_eq!(checked.status.code(), Some(status), "{}", text(&checked));
            assert!(text(&checked).contains(coverage), "{}", text(&checked));
            let matrix = matrix(dir.path());
            assert_eq!(matrix.status.code(), Some(status), "{}", text(&matrix));
            let records = rows(&matrix.stdout);
            if status == 2 {
                assert!(records.is_empty());
                for output in [&inspected, &checked, &matrix] {
                    let body = text(output);
                    for token in [anchor.to_string(), "Main-only".into(), "Alt".into()] {
                        assert!(body.contains(&token), "{body}");
                    }
                    if fingerprint == 627_223_094 {
                        assert!(body.contains("fingerprint family"), "{body}");
                    }
                }
            } else {
                assert_eq!(records.len(), 1);
                assert!(records[0].iter().any(|value| value == anchor.to_string()));
            }
        }
    }
}

fn lte_caps() -> LteCaps {
    LteCaps {
        fingerprint: 862_505_271,
        bitmask: 1,
        combos: vec![LteCombo {
            components: vec![LteComponent {
                band: 1,
                dl_bw_class_mimo: 32768,
                ul_bw_class_mimo: Some(0),
            }],
            bcs: Some(0),
            unknown1: Some(0),
            unknown2: Some(0),
        }],
    }
}

#[test]
fn lte_full_inspection_renders_dl_and_ul_mimo() {
    let dir = support::fixture();
    let path = dir.path().join("lte_400907661.binarypb");
    for (dl, ul, expected) in [
        (32768, 32768, "DL A 2x2  UL A 2x2"),
        (32768, 32769, "DL A 2x2  UL A 4x4"),
        (32769, 32768, "DL A 4x4  UL A 2x2"),
    ] {
        let mut caps = lte_caps();
        caps.combos[0].components[0].dl_bw_class_mimo = dl;
        caps.combos[0].components[0].ul_bw_class_mimo = Some(ul);
        support::write_message(&path, &caps);
        let out = inspect(&path);
        let body = text(&out);
        assert_eq!(out.status.code(), Some(0), "{body}");
        assert!(body.contains(expected), "{expected}: {body}");
    }
}

#[test]
fn lte_full_inspection_preserves_each_optional_fields_presence_and_value() {
    let dir = support::fixture();
    let path = dir.path().join("lte_400907661.binarypb");
    type LteMutation = (&'static str, fn(&mut LteCaps));
    let cases: &[LteMutation] = &[
        ("ul_bw_class_mimo=absent", |c| {
            c.combos[0].components[0].ul_bw_class_mimo = None
        }),
        ("ul_bw_class_mimo=32769", |c| {
            c.combos[0].components[0].ul_bw_class_mimo = Some(32769)
        }),
        ("bcs=absent", |c| c.combos[0].bcs = None),
        ("bcs=2147483648", |c| c.combos[0].bcs = Some(2_147_483_648)),
        ("unknown1=absent", |c| c.combos[0].unknown1 = None),
        ("unknown1=15060300583598546945", |c| {
            c.combos[0].unknown1 = Some(15_060_300_583_598_546_945)
        }),
        ("unknown2=absent", |c| c.combos[0].unknown2 = None),
        ("unknown2=123", |c| c.combos[0].unknown2 = Some(123)),
    ];
    let baseline = lte_caps();
    support::write_message(&path, &baseline);
    let out = inspect(&path);
    let body = text(&out);
    assert_eq!(out.status.code(), Some(0), "{body}");
    for field in ["ul_bw_class_mimo=0", "bcs=0", "unknown1=0", "unknown2=0"] {
        assert!(body.contains(field), "{field}: {body}");
    }
    for (expected, mutate) in cases {
        let mut changed = baseline.clone();
        mutate(&mut changed);
        support::write_message(&path, &changed);
        let changed = inspect(&path);
        let body = text(&changed);
        assert_eq!(changed.status.code(), Some(0), "{body}");
        assert!(body.contains(expected), "{expected}: {body}");
        assert_ne!(out.stdout, changed.stdout, "{expected}");
    }
}

#[test]
fn inspect_corruption_and_missing_files_fail() {
    let dir = support::fixture();
    for name in [
        "ALPHA_66813533.binarypb",
        "lte_400907661.binarypb",
        "ap_plmn_mapping.binarypb",
    ] {
        let path = dir.path().join(name);
        fs::write(&path, [0x78, 0x01]).unwrap();
        let out = inspect(&path);
        let body = text(&out);
        assert_eq!(out.status.code(), Some(2), "{body}");
        assert!(body.contains("15"), "{body}");
        assert!(!body.contains("file not present"));
        fs::remove_file(&path).unwrap();
        assert_eq!(inspect(&path).status.code(), Some(2));
    }
}

#[test]
fn inspect_invalid_anchor_cardinality_returns_two() {
    let dir = support::fixture();
    for name in ["ALPHA_1.binarypb", "ALPHA_558949.binarypb"] {
        let path = dir.path().join(name);
        support::write_message(&path, &carrier());
        let out = inspect(&path);
        let body = text(&out);
        assert_eq!(out.status.code(), Some(2), "{body}");
        assert!(body.contains("anchor"));
        if name.contains("558949") {
            assert!(body.contains("167") && body.contains("3347"));
        }
    }
}

#[test]
fn legend_errors_accumulate_before_lookup() {
    let dir = support::fixture();
    let path = dir.path().join("ALPHA_66813533.binarypb");
    support::write_message(&path, &carrier());
    let legend = dir.path().join("ap_plmn_mapping.binarypb");
    let map = PlmnMap {
        carriers: vec![
            Carrier {
                name: String::new(),
                index: 1,
                plmns: vec![1 << 24],
            },
            Carrier {
                name: "ALPHA".into(),
                index: 1,
                plmns: vec![2_291_967],
            },
            Carrier {
                name: "ALPHA".into(),
                index: 3,
                plmns: vec![5_435_408],
            },
        ],
    };
    support::write_message(&legend, &map);
    for file in [&path, &legend] {
        let out = inspect(file);
        let body = text(&out);
        assert_eq!(out.status.code(), Some(2), "{body}");
        for token in ["empty", "duplicate", "index", "16777216", "ALPHA"] {
            assert!(body.contains(token), "{token}: {body}");
        }
    }
    let body = text(&inspect(&path));
    assert!(body.contains("DL[2]"), "{body}");
}

#[test]
fn valid_empty_and_optional_legends_are_distinct_from_missing_requested() {
    let dir = support::fixture();
    let path = dir.path().join("ALPHA_66813533.binarypb");
    support::write_message(&path, &carrier());
    assert_eq!(inspect(&path).status.code(), Some(0));
    let legend = dir.path().join("ap_plmn_mapping.binarypb");
    assert_eq!(inspect(&legend).status.code(), Some(2));
    support::write_message(&legend, &PlmnMap::default());
    assert_eq!(inspect(&legend).status.code(), Some(0));
    support::write_message(
        &legend,
        &PlmnMap {
            carriers: vec![Carrier {
                name: "ALPHA".into(),
                index: 1,
                plmns: vec![2_291_967, 2_291_967],
            }],
        },
    );
    assert_eq!(inspect(&path).status.code(), Some(0));
}

#[cfg(unix)]
#[test]
fn dangling_optional_legend_is_an_error() {
    let dir = support::fixture();
    let path = dir.path().join("ALPHA_66813533.binarypb");
    support::write_message(&path, &carrier());
    std::os::unix::fs::symlink("missing", dir.path().join("ap_plmn_mapping.binarypb")).unwrap();
    let out = inspect(&path);
    assert_eq!(out.status.code(), Some(2), "{}", text(&out));
    assert!(text(&out).contains("cannot read"));
}

fn changed(a: &UeCaps, b: &UeCaps) -> String {
    let out = compare_pair(a, b, true);
    let body = text(&out);
    assert_eq!(out.status.code(), Some(0), "{body}");
    assert!(body.contains("1 caps-changed"), "{body}");
    body
}

#[test]
fn later_dl_and_ul_changes_are_visible_and_successful() {
    for dl in [true, false] {
        let a = carrier();
        let mut b = a.clone();
        let cc = &mut b.combo_groups[0].combo[0].sub_blocks[0];
        if dl {
            cc.dl_feature_per_cc_ids = Some(vec![1, 2]);
        } else {
            cc.ul_feature_per_cc_ids = Some(vec![1, 2]);
        }
        let full = changed(&a, &b);
        assert!(full.contains(if dl { "DL[2]" } else { "UL[2]" }), "{full}");
        assert!(full.contains("100") && full.contains("50"));
        let summary = compare_pair(&a, &b, false);
        assert_eq!(summary.status.code(), Some(0));
        assert!(text(&summary).contains("1 caps-changed"));
    }
}

#[test]
fn optional_presence_and_observed_indexes_are_different() {
    let a = carrier();
    for field in 0..7 {
        let mut b = a.clone();
        match field {
            0 => b.combo_groups[0].combo_header = None,
            1 => b.combo_groups[0].combo[0].bitmask = None,
            2 => b.dl_feature_per_cc_list[0].bw_90mhz_supported = Some(false),
            3 => b.combo_groups[0].combo[0].sub_blocks[0].dl_feature_index = Some(7),
            4 => b.combo_groups[0].combo[0].sub_blocks[0].ul_feature_index = None,
            5 => b.combo_groups[0].combo[0].sub_blocks[0].srstxswitch = Some(0),
            _ => b.ul_feature_per_cc_list[0].max_mimo_non_cb = Some(0),
        }
        let body = changed(&a, &b);
        if field == 2 {
            assert!(body.contains("bw_90mhz_supported=absent"), "{body}");
            assert!(body.contains("bw_90mhz_supported=false"), "{body}");
        }
    }
}

#[test]
fn eutra_selectors_are_raw_and_do_not_reference_nr_catalogs() {
    let mut a = carrier();
    a.combo_groups[0].combo[0].sub_blocks[0].band = 66;
    a.dl_feature_per_cc_list[1] = a.dl_feature_per_cc_list[0];
    for ids in [
        Some(vec![1, 2]),
        Some(vec![0, 7]),
        Some(vec![255]),
        None,
        Some(vec![]),
    ] {
        let mut b = a.clone();
        b.combo_groups[0].combo[0].sub_blocks[0].dl_feature_per_cc_ids = ids;
        changed(&a, &b);
    }
    let mut b = a.clone();
    b.combo_groups[0].combo[0].sub_blocks[0].dl_feature_index = Some(2);
    changed(&a, &b);
}

#[test]
fn selector_presence_placeholders_and_empty_records_remain_distinct() {
    let states = [
        None,
        Some(vec![]),
        Some(vec![0]),
        Some(vec![0, 0]),
        Some(vec![1]),
    ];
    for (i, left) in states.iter().enumerate() {
        for right in &states[i + 1..] {
            let mut a = carrier();
            a.dl_feature_per_cc_list[0] = Default::default();
            a.combo_groups[0].combo[0].sub_blocks[0].dl_feature_per_cc_ids = left.clone();
            let mut b = a.clone();
            b.combo_groups[0].combo[0].sub_blocks[0].dl_feature_per_cc_ids = right.clone();
            changed(&a, &b);
        }
    }
}

#[test]
fn invalid_nr_references_report_all_source_coordinates() {
    let a = carrier();
    let mut b = a.clone();
    let original = b.combo_groups[0].combo[0].sub_blocks[0].clone();
    b.combo_groups[0].combo[0].sub_blocks.push(original);
    b.combo_groups[0].combo[0].sub_blocks[1].dl_feature_per_cc_ids = Some(vec![254, 255]);
    b.combo_groups[0].combo[0].sub_blocks[1].ul_feature_per_cc_ids = Some(vec![0, 1]);
    let out = compare_pair(&a, &b, true);
    let body = text(&out);
    assert_eq!(out.status.code(), Some(2), "{body}");
    for token in [
        "g1",
        "c1",
        "component 2",
        "DL[1]",
        "DL[2]",
        "UL[1]",
        "254",
        "255",
    ] {
        assert!(body.contains(token), "{token}: {body}");
    }
    assert!(!body.contains("no differences"));
}

#[test]
fn catalog_numbering_group_packing_and_component_order_do_not_change_sets() {
    let mut a = carrier();
    let mut cc = a.combo_groups[0].combo[0].sub_blocks[0].clone();
    cc.band += 1;
    a.combo_groups[0].combo[0].sub_blocks.push(cc);
    let mut b = a.clone();
    b.combo_groups[0].combo[0].sub_blocks.reverse();
    b.dl_feature_per_cc_list.swap(0, 1);
    b.ul_feature_per_cc_list.swap(0, 1);
    for cc in &mut b.combo_groups[0].combo[0].sub_blocks {
        cc.dl_feature_per_cc_ids = Some(vec![2, 2]);
        cc.ul_feature_per_cc_ids = Some(vec![2, 2]);
    }
    b.combo_groups.push(b.combo_groups[0].clone());
    b.dl_feature_per_cc_list.push(Default::default());
    let out = compare_pair(&a, &b, true);
    assert_eq!(out.status.code(), Some(0));
    assert!(text(&out).contains("no differences"));
    let mut ordered = carrier();
    ordered.combo_groups[0].combo[0].sub_blocks[0].dl_feature_per_cc_ids = Some(vec![1, 2]);
    let mut reverse = ordered.clone();
    reverse.combo_groups[0].combo[0].sub_blocks[0].dl_feature_per_cc_ids = Some(vec![2, 1]);
    changed(&ordered, &reverse);
}

#[test]
fn empty_groups_compare_but_true_stubs_do_not() {
    let mut empty = carrier();
    empty.combo_groups[0].combo.clear();
    let out = compare_pair(&empty, &empty, true);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out));
    assert!(text(&out).contains("0 common · no differences"));
    let out = compare_pair(&empty, &carrier(), true);
    assert_eq!(out.status.code(), Some(0));
    assert!(text(&out).contains("1 only in B"));
    let mut stub = empty.clone();
    stub.combo_groups.clear();
    assert_eq!(compare_pair(&stub, &empty, true).status.code(), Some(2));
}

#[test]
fn full_output_retains_all_variants() {
    let a = carrier();
    let mut b = a.clone();
    let seed = b.combo_groups[0].combo[0].clone();
    b.combo_groups[0].combo = [0, 7, 9]
        .map(|mask| {
            let mut c = seed.clone();
            c.bitmask = Some(mask);
            c
        })
        .to_vec();
    let body = changed(&a, &b);
    for marker in [
        "B[1]",
        "B[2]",
        "B[3]",
        "bitmask=0",
        "bitmask=7",
        "bitmask=9",
    ] {
        assert!(body.contains(marker), "{body}");
    }
}
