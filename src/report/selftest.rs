//! Data-independent runtime sanity checks (`self-test`).

use crate::{factor::factorize, model::*, proto::UeCaps};
use prost::Message;

pub fn self_test() -> anyhow::Result<i32> {
    let mut ok = true;
    let mut check = |desc: &str, pass: bool| {
        println!("  [{}] {desc}", if pass { "ok  " } else { "FAIL" });
        ok &= pass;
    };

    println!("profile identification (VZW worked examples):");
    check(
        "anchor 167",
        identify_profile(193_698_151_252_893).map(|p| p.anchor) == Some(167),
    );
    check(
        "anchor 8969",
        identify_profile(251_107_217_711_255).map(|p| p.anchor) == Some(8969),
    );
    check(
        "anchor 2912407",
        identify_profile(326_540_974_641_771).map(|p| p.anchor) == Some(2_912_407),
    );

    println!("factorisation:");
    let f = factorize(1_492_116_125);
    check(
        "1_1_DE signature = 5^3 · 43 · 277603",
        f.get(&5) == Some(&3)
            && f.get(&43) == Some(&1)
            && f.get(&277_603) == Some(&1)
            && f.len() == 3,
    );
    check("is_prime(154921957)", crate::factor::is_prime(154_921_957));

    println!("PLMN decode:");
    check(
        "5566544 -> 450-05",
        decode_plmn(5_566_544) == ("450".into(), "05".into()),
    );
    check(
        "1245572 -> 311-480",
        decode_plmn(1_245_572) == ("311".into(), "480".into()),
    );

    println!("fingerprint tiers:");
    check(
        "874888686 -> A/main",
        fp_info(874_888_686) == Some((Family::A, Tier::Main)),
    );
    check(
        "627223094 -> B/alt",
        fp_info(627_223_094) == Some((Family::B, Tier::Alt)),
    );

    println!("protobuf decode:");
    let caps = UeCaps::decode(&[0x08u8, 0xAC, 0x02, 0x48, 0x07][..]).unwrap();
    check(
        "decode fingerprint=300, stub",
        caps.version == 300 && caps.unknown == 7 && caps.combo_groups.is_empty(),
    );

    println!(
        "\n{}",
        if ok {
            "ALL TESTS PASSED"
        } else {
            "SOME TESTS FAILED"
        }
    );
    Ok(i32::from(!ok))
}
