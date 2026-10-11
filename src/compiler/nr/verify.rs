use super::{InputLayout, ingest::canonical_payloads};
use crate::{
    proto::{SubBlock as ProtoSubBlock, UeCaps},
    raw_nr::{Direction, RawNrPayload, RawNrPayloadKey, SubBlockKind, cc_count, feature_index},
};
use anyhow::{Context, ensure};
use prost::Message;
use std::collections::BTreeSet;
/// `entries` yields `(raw_band, bw_class, selector)` per component — `dl_bw_class`/DL or
/// `ul_bw_class`/UL depending on `direction`. A selector counts as a resolved catalog
/// reference (checked against `cc_count`, and its bytes fold into the coverage set) only
/// when its FIRST byte resolves against `records`. Unlike report resolution, this checks
/// compiler-generated selectors. By construction (`LocalFeaturePlan::reconstruct_sub_block`), a
/// resolved reference's bytes are ALL catalog indices, never a mix with raw selector-only
/// data, so it is enough to gate on the first byte and then trust every byte.
fn verify_compact_feature_list<'a, T>(
    records: &[T],
    entries: impl IntoIterator<Item = (i32, Option<i32>, Option<&'a [u8]>)>,
    direction: Direction,
    basename: &str,
) -> anyhow::Result<()> {
    let mut referenced = BTreeSet::new();
    for (raw_band, bw_class, ids) in entries {
        let Some(ids) = ids else { continue };
        if feature_index(Some(ids), records.len()).is_none() {
            continue;
        }
        let (kind, _) = SubBlockKind::split_raw_band(raw_band);
        let bw_class = bw_class.with_context(|| {
            format!(
                "generated {basename} has a resolved {direction} selector with no bw_class to derive its CC count"
            )
        })?;
        // `kind` is dead-in-practice as `Lte` here: an LTE per-CC selector is always the
        // all-zero placeholder, which never passes the `feature_index` gate above (per-CC
        // selectors that resolve against a compact list are NR-only, corpus-verified — see
        // DESIGN.md), so this branch only ever runs for `Nr` on real data.
        let cc = u8::try_from(bw_class).with_context(|| {
            format!("generated {basename} {direction} bw_class {bw_class} is out of range")
        })?;
        // `cc_count`'s contract excludes 0 ("callers gate on it first"), and this caller did
        // not — a class-0 direction reached it and produced a bare "unknown Nr bw_class 0"
        // naming neither the file nor the band. Reaching here with 0 also means the generated
        // file broke the selector-presence biconditional `RawSubBlock::validate` enforces, so
        // say that rather than bottoming out in a CC-count lookup.
        ensure!(
            cc >= 1,
            "generated {basename} band {raw_band} carries a resolved {direction} selector but \
             its bw_class is 0; per-CC data and a live bandwidth class imply each other"
        );
        let expected = cc_count(kind, cc)?;
        ensure!(
            ids.len() == expected,
            "generated {basename} resolved {direction} selector has {} byte(s), expected {expected} for {kind:?} bw_class {bw_class}",
            ids.len()
        );
        for &b in ids {
            let k = b as usize;
            ensure!(
                (1..=records.len()).contains(&k),
                "generated {basename} resolved {direction} selector byte {b} is out of the compact {direction} list bounds 1..={}",
                records.len()
            );
            referenced.insert(k - 1);
        }
    }
    ensure!(
        referenced.len() == records.len() && referenced.iter().copied().eq(0..records.len()),
        "generated {basename} has an unused or missing {direction} feature record"
    );
    Ok(())
}

/// Confirms every sub-block's resolved per-CC selector fully references the file's compact
/// DL/UL feature catalogs exactly once each (no unused or missing record) — both directions,
/// since a per-CC selector references only its own direction's catalog.
fn verify_compact_feature_lists(
    decoded: &UeCaps,
    components: &[&ProtoSubBlock],
    basename: &str,
) -> anyhow::Result<()> {
    verify_compact_feature_list(
        &decoded.dl_feature_per_cc_list,
        components.iter().map(|component| {
            (
                component.band,
                component.dl_bw_class,
                component.dl_feature_per_cc_ids.as_deref(),
            )
        }),
        Direction::Dl,
        basename,
    )?;
    verify_compact_feature_list(
        &decoded.ul_feature_per_cc_list,
        components.iter().map(|component| {
            (
                component.band,
                component.ul_bw_class,
                component.ul_feature_per_cc_ids.as_deref(),
            )
        }),
        Direction::Ul,
        basename,
    )?;
    Ok(())
}

/// Confirms the generated file's canonical payload set exactly matches what generation
/// intended, order-independent (both sides are sorted before comparison) — the last line of
/// defense against a generation bug that emits the wrong SET of payloads even when every
/// individual payload otherwise self-verifies.
fn verify_canonical_payloads(
    decoded: &UeCaps,
    layout: InputLayout,
    basename: &str,
    expected_payloads: &[&RawNrPayload],
) -> anyhow::Result<()> {
    let actual = canonical_payloads(decoded, layout, basename)?;
    let actual: Vec<_> = actual.iter().map(RawNrPayloadKey::from).collect();
    let mut expected: Vec<_> = expected_payloads
        .iter()
        .map(|payload| RawNrPayloadKey::from(*payload))
        .collect();
    expected.sort_unstable();
    ensure!(
        actual == expected,
        "generated NR canonical payload self-check failed for {basename}"
    );
    Ok(())
}

pub(super) fn verify_generated_file(
    basename: &str,
    bytes: &[u8],
    expected_caps: &UeCaps,
    expected_payloads: &[&RawNrPayload],
    layout: InputLayout,
) -> anyhow::Result<()> {
    let expected_bitmask = layout.bitmask();
    let decoded = UeCaps::decode(bytes)
        .with_context(|| format!("self-verifying generated NR file {basename}"))?;
    ensure!(
        decoded.version == expected_caps.version
            && decoded.id == expected_caps.id
            && decoded.unknown == expected_caps.unknown,
        "generated NR identity self-check failed for {basename}"
    );
    // Read the bitmask straight off the decoded message. This used to call
    // `build_combos_with_bitmasks` and use only the tuple's second element, allocating and
    // discarding the entire report display tree — a `Vec<SubBlock>` per combo plus a band
    // `CompactString`, two `resolve_all` vectors and a `render_component` `String` per
    // component, then a joined `bands` string — on the order of 7-8M throwaway allocations per
    // `decompose`, to check one integer per combo.
    ensure!(
        decoded
            .combo_groups
            .iter()
            .flat_map(|group| &group.combo)
            .all(|combo| combo.bitmask == Some(expected_bitmask)),
        "generated NR bitmask self-check failed for {basename}"
    );
    ensure!(
        decoded.dl_feature_per_cc_list == expected_caps.dl_feature_per_cc_list,
        "generated NR DL feature-list self-check failed for {basename}"
    );
    ensure!(
        decoded.ul_feature_per_cc_list == expected_caps.ul_feature_per_cc_list,
        "generated NR UL feature-list self-check failed for {basename}"
    );
    let components = decoded
        .combo_groups
        .iter()
        .flat_map(|group| &group.combo)
        .flat_map(|combo| &combo.sub_blocks)
        .collect::<Vec<_>>();
    verify_compact_feature_lists(&decoded, &components, basename)?;
    verify_canonical_payloads(&decoded, layout, basename, expected_payloads)?;
    Ok(())
}
