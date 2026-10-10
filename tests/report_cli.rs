#[path = "support/report.rs"]
mod support;
use pixel_uecaps_toolbox::proto::*;
use support::{carrier, compare_pair, text};

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
