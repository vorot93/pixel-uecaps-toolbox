#[path = "support/report.rs"]
mod support;
use pixel_uecaps_toolbox::proto::*;
use std::{ffi::OsStr, fs};
use support::{carrier, compare_pair, text};

fn inspect(path: &std::path::Path) -> std::process::Output {
    support::run([
        OsStr::new("inspect"),
        path.as_os_str(),
        OsStr::new("--full"),
    ])
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
