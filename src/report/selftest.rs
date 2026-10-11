//! Data-independent runtime sanity checks (`self-test`).

use super::Report;
use crate::{
    factor::factorize,
    model::{
        CapabilityLayout, Family, LTE_CONFIGS, PHONE_MODELS, Tier, decode_plmn, fingerprint_info,
        identify_profile, known_model_codes, profile,
    },
    outcome::Outcome,
};
use pixel_bands::PIXEL_BANDS;
use std::fmt;

enum CheckLine {
    Section(&'static str),
    Check {
        description: &'static str,
        passed: bool,
    },
}

#[derive(Default)]
struct Checks(Vec<CheckLine>);

impl Checks {
    fn section(&mut self, title: &'static str) {
        self.0.push(CheckLine::Section(title));
    }
    fn check(&mut self, description: &'static str, passed: bool) {
        self.0.push(CheckLine::Check {
            description,
            passed,
        });
    }
    fn has_failures(&self) -> bool {
        self.0
            .iter()
            .any(|line| matches!(line, CheckLine::Check { passed: false, .. }))
    }
    fn report(&self) -> Report {
        Report {
            body: self.to_string(),
            diagnostics: String::new(),
            outcome: Outcome::from_failed_checks(self.has_failures()),
        }
    }
}

impl fmt::Display for Checks {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for line in &self.0 {
            match line {
                CheckLine::Section(title) => writeln!(f, "{title}:")?,
                CheckLine::Check {
                    description,
                    passed,
                } => writeln!(
                    f,
                    "  [{}] {description}",
                    if *passed { "ok  " } else { "FAIL" }
                )?,
            }
        }
        writeln!(
            f,
            "\n{}",
            if self.has_failures() {
                "SOME TESTS FAILED"
            } else {
                "ALL TESTS PASSED"
            }
        )
    }
}

pub fn self_test() -> Report {
    let mut checks = Checks::default();

    checks.section("profile identification (VZW worked examples)");
    checks.check(
        "anchor 167",
        identify_profile(193_698_151_252_893).map(|p| p.anchor) == Some(167),
    );
    checks.check(
        "anchor 8969",
        identify_profile(251_107_217_711_255).map(|p| p.anchor) == Some(8969),
    );
    checks.check(
        "anchor 2912407",
        identify_profile(326_540_974_641_771).map(|p| p.anchor) == Some(2_912_407),
    );

    checks.section("factorisation");
    let f = factorize(1_492_116_125);
    checks.check(
        "1_1_DE signature = 5^3 · 43 · 277603",
        f.get(&5) == Some(&3)
            && f.get(&43) == Some(&1)
            && f.get(&277_603) == Some(&1)
            && f.len() == 3,
    );
    checks.check("is_prime(154921957)", crate::factor::is_prime(154_921_957));

    checks.section("PLMN decode");
    checks.check(
        "5566544 -> 450-05",
        decode_plmn(5_566_544) == Some(("450".into(), "05".into())),
    );
    checks.check(
        "1245572 -> 311-480",
        decode_plmn(1_245_572) == Some(("311".into(), "480".into())),
    );
    checks.check(
        "out-of-range PLMN rejected, not masked",
        decode_plmn(0x100_0000 | 1_245_572).is_none(),
    );

    checks.section("fingerprint tiers");
    checks.check(
        "874888686 -> main/A",
        fingerprint_info(874_888_686) == Some((Family::A, Tier::Main)),
    );
    checks.check(
        "627223094 -> alt/B",
        fingerprint_info(627_223_094) == Some((Family::B, Tier::Alt)),
    );

    checks.section("model registry");
    let known = known_model_codes();
    checks.check(
        "52 unique, lexically sorted model codes",
        known.len() == 52 && known.windows(2).all(|pair| pair[0] < pair[1]),
    );
    checks.check(
        "34 bitmask and 18 profiled models",
        PHONE_MODELS.iter().filter(|m| m.is_bitmask()).count() == 34
            && PHONE_MODELS.iter().filter(|m| !m.is_bitmask()).count() == 18,
    );
    checks.check(
        "all model selectors and band records resolve",
        PHONE_MODELS.iter().all(|m| {
            PIXEL_BANDS.get(m.code).is_some()
                && match m.layout {
                    CapabilityLayout::Bitmask => true,
                    CapabilityLayout::Profiled { nr_anchor, lte_id } => {
                        profile(nr_anchor).is_some() && LTE_CONFIGS.iter().any(|c| c.id == lte_id)
                    }
                }
        }),
    );

    checks.section("protobuf decode");
    // Strict decode, and no `unwrap`: a tag renumber must fail this check rather than abort
    // `self-test` for the user.
    let caps = crate::wire::decode_uecaps(&[0x08u8, 0xAC, 0x02, 0x48, 0x07], "self-test vector");
    checks.check(
        "decode fingerprint=300, stub",
        caps.as_ref()
            .is_ok_and(|c| c.version == 300 && c.unknown == 7 && c.combo_groups.is_empty()),
    );

    checks.report()
}

#[cfg(test)]
mod tests {
    #[test]
    fn runtime_self_test_passes() {
        assert_eq!(super::self_test().outcome, crate::outcome::Outcome::Clean);
    }

    #[test]
    fn rendering_propagates_formatter_failure() {
        struct Closed;
        impl std::fmt::Write for Closed {
            fn write_str(&mut self, _: &str) -> std::fmt::Result {
                Err(std::fmt::Error)
            }
        }
        use std::fmt::Write;
        let mut checks = super::Checks::default();
        checks.section("sample");
        checks.check("passing", true);
        assert!(write!(Closed, "{checks}").is_err());
    }

    #[test]
    fn failed_check_renders_findings_without_mutating_registry() {
        let mut checks = super::Checks::default();
        checks.section("sample");
        checks.check("passing", true);
        checks.check("failing", false);
        let report = checks.report();
        assert_eq!(report.outcome, crate::outcome::Outcome::Findings);
        assert_eq!(
            report.body,
            "sample:\n  [ok  ] passing\n  [FAIL] failing\n\nSOME TESTS FAILED\n"
        );
        assert!(report.diagnostics.is_empty());
    }
}
