//! KDL (de)serialization boundary for the folder-compiler source documents.
//! Hand-mapped over the `kdl` crate (KDL v2); replaces the former TOML/serde path.

use std::collections::BTreeMap;

use anyhow::{Context, Result, anyhow, bail, ensure};
use kdl::{KdlDocument, KdlEntry, KdlNode};

use crate::{
    compiler::{
        features::{NrSourceSubBlock, SourceLteSubBlock, SourceNrSubBlock},
        kdl_direction::{format_class_mimo, format_direction, parse_class_mimo, parse_direction},
        kdl_keys::{
            carrier, combo, dl_catalog, fingerprint, lte_combo, lte_doc, lte_file, lte_sub_block,
            nr_doc, profile, selection, sub_block, ul_catalog,
        },
        schema::{
            BitmaskFingerprint, CarrierSource, CarrierTier, DecimalU64, LteDocument, LteFileSource,
            LteSourceCombo, NrDocument, NrSourceCombo, ProfileSource,
        },
        selection::SelectionRect,
    },
    kdl_support::{
        NodeReader, finish_doc, opt_bool_prop, opt_int_prop, opt_str_prop, plmn_to_node, read_plmn,
        read_str_list, str_list_node,
    },
    proto::{LteComponent, ShannonFeatureSetDlPerCcNr, ShannonFeatureSetUlPerCcNr},
    raw_nr::{SubBlockKind, cc_count},
};

// ---- NR document mapping ----
fn tier_to_str(t: CarrierTier) -> &'static str {
    match t {
        CarrierTier::Main => "main",
        CarrierTier::Alt => "alt",
    }
}
fn str_to_tier(s: &str) -> Result<CarrierTier> {
    match s {
        "main" => Ok(CarrierTier::Main),
        "alt" => Ok(CarrierTier::Alt),
        other => bail!("u tier `{other}` (expected `main` or `alt`)"),
    }
}

/// A sub-block's node name: its kind prefix followed by the plain band, e.g. `nr257`, `lte66`.
///
/// This is the tool's own band-label convention (`SubBlockKind::band_label`) used as the node
/// name, so a source line reads as the 3GPP band designation it describes.
fn sub_block_node_name(prefix: &str, band: u16) -> String {
    format!("{prefix}{band}")
}

/// The inverse of [`sub_block_node_name`]. `None` when `name` does not start with `prefix`, or
/// the remainder is not a nonempty run of ASCII digits that fits `u16` — so `nr`, `nr257x`,
/// `nrfoo` and `nr99999999` all fail rather than silently yielding a band.
fn parse_sub_block_name(name: &str, prefix: &str) -> Option<u16> {
    let digits = name.strip_prefix(prefix)?;
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    digits.parse().ok()
}

/// A `selection { carriers …; skus … }` block (shared by NR and LTE combos).
fn selection_to_node(rect: &SelectionRect) -> KdlNode {
    let mut node = KdlNode::new(combo::SELECTION);
    if rect.carriers.is_some() || rect.skus.is_some() {
        let kids = node.ensure_children();
        if let Some(carriers) = &rect.carriers {
            kids.nodes_mut()
                .push(str_list_node(selection::CARRIERS, carriers));
        }
        if let Some(skus) = &rect.skus {
            kids.nodes_mut().push(str_list_node(selection::SKUS, skus));
        }
    }
    node
}

/// Emit one `nr.kdl` sub-block node. Property order is load-bearing for byte-identity:
/// `band` positional, then `dl-bw-class`, `dl-feature`, `ul-bw-class`, `ul-feature`,
/// `srs-tx-switch` — direction-grouped, so DL and UL each read as a contiguous run.
///
/// The two node kinds spell proto 4/5 and 6/7 differently, which is why the source model is a
/// sum type and this matches on it once:
///   * `nr`: the proto-4/5 index is NOT surfaced — NR derives it from its feature set on
///     provision. The per-CC catalog list becomes repeated `dl-feature=`/`ul-feature=`. An
///     unresolved NR selector is only ever the all-zero placeholder (corpus: 0 of 1.74M
///     non-zero), omitted here and re-derived by the reader.
///   * `lte`: the index becomes a single scalar `dl-feature`/`ul-feature` (the LTE MIMO ×
///     CC-count value). LTE never carries a per-CC list, so the un-suffixed name is free.
///     `ul-feature` is always-`Some` on LTE with `Some(0)` ⟺ no UL, so its zero is omitted
///     (Task 8 omit-when-0) and the reader re-defaults it. LTE has no `srs-tx-switch`.
fn cc_to_node(cc: &NrSourceSubBlock) -> Result<KdlNode> {
    let prefix = match cc.kind() {
        SubBlockKind::Nr => combo::NR_PREFIX,
        SubBlockKind::Lte => combo::LTE_PREFIX,
    };
    // The band is the node NAME's suffix, not a positional argument.
    let mut node = KdlNode::new(sub_block_node_name(prefix, cc.band()));

    let (dl_features, ul_features, srs_tx_switch): (Vec<u16>, Vec<u16>, Option<i32>) = match cc {
        NrSourceSubBlock::Lte(cc) => (
            cc.dl_feature.into_iter().collect(),
            cc.ul_feature.filter(|&v| v != 0).into_iter().collect(),
            None,
        ),
        NrSourceSubBlock::Nr(cc) => (
            cc.dl_feature.iter().map(|&v| v as u16).collect(),
            cc.ul_feature.iter().map(|&v| v as u16).collect(),
            cc.srs_tx_switch,
        ),
    };

    // Class and per-CC list are one value per direction: `dl=G30,30`. An empty list is the
    // all-zero placeholder, which the source omits and `resolve` re-materialises.
    if let Some(class) = cc.dl_bw_class() {
        node.push(KdlEntry::new_prop(
            sub_block::DL,
            format_direction(class, &dl_features)?.as_str(),
        ));
    }
    // `ul-bw-class` is corpus-verified always `Some` on a real sub-block, so `Some(0)` is
    // omitted here and re-defaulted to `Some(0)` by `read_sub_block` (omit-when-0) — a
    // value-faithful round trip, not a lossy one. Absent `ul` therefore means class 0.
    if let Some(class) = cc.ul_bw_class().filter(|&v| v != 0) {
        node.push(KdlEntry::new_prop(
            sub_block::UL,
            format_direction(class, &ul_features)?.as_str(),
        ));
    }
    opt_int_prop(&mut node, sub_block::SRS_TX_SWITCH, srs_tx_switch);
    Ok(node)
}

fn lte_cc_to_node(comp: &LteComponent) -> Result<KdlNode> {
    let band = u16::try_from(comp.band).with_context(|| {
        format!(
            "LTE component band {} does not fit the source format",
            comp.band
        )
    })?;
    let mut node = KdlNode::new(sub_block_node_name(lte_combo::SUB_BLOCK_PREFIX, band));
    // The bitfield becomes `<letter><mimo>`: 32769 -> `A4`. The number told you nothing without
    // the table in `report::lte::lte_class`.
    node.push(KdlEntry::new_prop(
        lte_sub_block::DL_MIMO,
        format_class_mimo(comp.dl_bw_class_mimo)?.as_str(),
    ));
    // Local guard. The `.filter` below maps `None` and `Some(0)` to the same output, so this
    // function alone cannot tell them apart. Every production path already passes
    // `validate_lte_combos`, which rejects a `None`; this keeps the writer honest if it ever
    // gains a second caller.
    ensure!(
        comp.ul_bw_class_mimo.is_some(),
        "LTE component band {} omits ul_bw_class_mimo; the source format cannot represent an \
         absent uplink class",
        comp.band
    );
    // UL 0 is the majority value — 8 281 of 12 159 corpus sub-blocks — and carries no
    // information, so it is omitted and re-defaulted by `read_lte_cc`. Same omit-when-0 rule the
    // NR sub-block uses for its UL bandwidth class. `validate_lte_combos` rejects a `None`, so an
    // omitted `u` always means the explicit zero and never a dropped absent field.
    if let Some(ul) = comp.ul_bw_class_mimo.filter(|&v| v != 0) {
        node.push(KdlEntry::new_prop(
            lte_sub_block::UL_MIMO,
            format_class_mimo(ul)?.as_str(),
        ));
    }
    Ok(node)
}

fn emit_dl_feature(f: &ShannonFeatureSetDlPerCcNr) -> KdlNode {
    let mut node = KdlNode::new(nr_doc::DL_FEATURE);
    opt_int_prop(&mut node, dl_catalog::MAX_SCS, f.max_scs);
    opt_int_prop(&mut node, dl_catalog::MAX_MIMO, f.max_mimo);
    opt_int_prop(&mut node, dl_catalog::MAX_BW, f.max_bw);
    opt_int_prop(&mut node, dl_catalog::MAX_MOD_ORDER, f.max_mod_order);
    opt_bool_prop(
        &mut node,
        dl_catalog::BW_90MHZ_SUPPORTED,
        f.bw_90mhz_supported,
    );
    node
}

fn emit_ul_feature(f: &ShannonFeatureSetUlPerCcNr) -> KdlNode {
    let mut node = KdlNode::new(nr_doc::UL_FEATURE);
    opt_int_prop(&mut node, ul_catalog::MAX_SCS, f.max_scs);
    opt_int_prop(&mut node, ul_catalog::MAX_MIMO_CB, f.max_mimo_cb);
    opt_int_prop(&mut node, ul_catalog::MAX_BW, f.max_bw);
    opt_int_prop(&mut node, ul_catalog::MAX_MOD_ORDER, f.max_mod_order);
    opt_bool_prop(
        &mut node,
        ul_catalog::BW_90MHZ_SUPPORTED,
        f.bw_90mhz_supported,
    );
    opt_int_prop(&mut node, ul_catalog::MAX_MIMO_NON_CB, f.max_mimo_non_cb);
    node
}

/// The value an absent `bcs-intra-endc` re-derives to on read: `Some(0)` exactly when the
/// combo advertises intra-band EN-DC (`intra-band-en-dc-support == 1`), else `None`. Single
/// source of truth shared by `emit_nr_combo` (omit-when-equal) and `read_combo` (re-derive),
/// so the write and read sides cannot silently disagree. See
/// DESIGN.md.
fn derive_bcs_intra_endc(intra_band_en_dc_support: Option<i32>) -> Option<u32> {
    if intra_band_en_dc_support == Some(1) {
        Some(0)
    } else {
        None
    }
}

fn emit_nr_combo(combo: &NrSourceCombo) -> Result<KdlNode> {
    let mut node = KdlNode::new(nr_doc::COMBO);
    // `power-class`/`bcs-nr`/`bcs-eutra`/`intra-band-en-dc-support` are corpus-verified
    // always `Some` on a real combo header (never `None`), so `Some(0)` is omitted here and
    // re-defaulted to `Some(0)` by `read_combo` below (Task 8 omit-when-0).
    opt_int_prop(
        &mut node,
        combo::POWER_CLASS,
        combo.power_class.filter(|&v| v != 0),
    );
    opt_int_prop(&mut node, combo::BCS_NR, combo.bcs_nr.filter(|&v| v != 0));
    // Task 2: `bcs-intra-endc` is the BCS index for intra-band EN-DC; a combo carries it
    // exactly when it advertises that mode (`intra-band-en-dc-support == 1`). Derive the
    // common `Some(0)` from that flag and omit it; write only the ~20 exceptional zeros
    // (intra_band != 1) and every nonzero explicitly. The one unrepresentable state
    // (`None` + intra_band == 1, 0 corpus cases) fails closed. See spec
    // DESIGN.md.
    let derived_bcs_intra_endc = derive_bcs_intra_endc(combo.intra_band_en_dc_support);
    match combo.bcs_intra_endc {
        actual if actual == derived_bcs_intra_endc => {} // omit: derivable zeros + every None
        Some(v) => opt_int_prop(&mut node, combo::BCS_INTRA_ENDC, Some(i128::from(v))),
        None => bail!(
            "bcs_intra_endc=None with ie=1 cannot be represented by \
             omission (would re-derive as Some(0)); this combo is unexpected \
             (sub-block bands {:?}) — see \
             DESIGN.md",
            combo
                .sub_blocks
                .iter()
                .map(NrSourceSubBlock::band)
                .collect::<Vec<_>>()
        ),
    }
    opt_int_prop(
        &mut node,
        combo::BCS_EUTRA,
        combo.bcs_eutra.filter(|&v| v != 0),
    );
    opt_int_prop(
        &mut node,
        combo::INTRA_BAND_EN_DC_SUPPORT,
        combo.intra_band_en_dc_support.filter(|&v| v != 0),
    );
    if combo.selection.is_some() || !combo.sub_blocks.is_empty() {
        let kids = node.ensure_children();
        if let Some(sel) = &combo.selection {
            for rect in sel {
                kids.nodes_mut().push(selection_to_node(rect));
            }
        }
        for cc in &combo.sub_blocks {
            kids.nodes_mut().push(cc_to_node(cc)?);
        }
    }
    Ok(node)
}

fn emit_lte_combo(combo: &LteSourceCombo) -> Result<KdlNode> {
    let mut node = KdlNode::new(lte_doc::COMBO);
    opt_int_prop(&mut node, lte_combo::BCS, combo.bcs);
    opt_int_prop(&mut node, lte_combo::UNKNOWN1, combo.unknown1);
    opt_int_prop(&mut node, lte_combo::UNKNOWN2, combo.unknown2);
    if combo.selection.is_some() || !combo.components.is_empty() {
        let kids = node.ensure_children();
        if let Some(sel) = &combo.selection {
            for rect in sel {
                kids.nodes_mut().push(selection_to_node(rect));
            }
        }
        for comp in &combo.components {
            kids.nodes_mut().push(lte_cc_to_node(comp)?);
        }
    }
    Ok(node)
}

/// One `bitmask-fingerprint N { carriers … }` node: which bitmask-folder carriers share a
/// given legacy fingerprint.
fn fingerprint_node(fp: &BitmaskFingerprint) -> KdlNode {
    let mut node = KdlNode::new(nr_doc::BITMASK_FINGERPRINT);
    node.push(KdlEntry::new(fp.fingerprint as i128));
    node.ensure_children()
        .nodes_mut()
        .push(str_list_node(fingerprint::CARRIERS, &fp.carriers));
    node
}

/// One carrier's `plmns` children: either a bare, childless `plmns` marker for a
/// present-but-empty list (distinguishing it from no list at all — see `read_carrier`'s
/// inverse), or one `plmn mcc=… mnc=…` node per entry.
fn plmn_child_nodes(plmns: &[String]) -> Result<Vec<KdlNode>> {
    if plmns.is_empty() {
        Ok(vec![str_list_node(carrier::PLMNS, plmns)])
    } else {
        plmns.iter().map(|p| plmn_to_node(p)).collect()
    }
}

/// One `profile "KEY" multiplier=… unknown=…` node.
fn profile_node(key: &str, p: &ProfileSource) -> KdlNode {
    let mut node = KdlNode::new(carrier::PROFILE);
    node.push(KdlEntry::new(key));
    node.push(KdlEntry::new_prop(
        profile::MULTIPLIER,
        p.multiplier.0 as i128,
    ));
    node.push(KdlEntry::new_prop(profile::UNKNOWN, p.unknown.0 as i128));
    node
}

/// One `carrier "NAME" …` node, with its `plmns`/`profile` children when it has either.
fn carrier_node(name: &str, c: &CarrierSource) -> Result<KdlNode> {
    let mut node = KdlNode::new(nr_doc::CARRIER);
    node.push(KdlEntry::new(name));
    opt_int_prop(&mut node, carrier::BITMASK_ID, c.bitmask_id);
    opt_int_prop(&mut node, carrier::PROFILED_ID, c.profiled_id);
    opt_int_prop(&mut node, carrier::MAPPING_ID, c.mapping_id);
    opt_int_prop(
        &mut node,
        carrier::SIGNATURE,
        c.signature.map(|v| v.0 as i128),
    );
    opt_str_prop(&mut node, carrier::TIER, c.tier.map(tier_to_str));
    if c.plmns.is_some() || !c.profiles.is_empty() {
        let kids = node.ensure_children();
        if let Some(plmns) = &c.plmns {
            for n in plmn_child_nodes(plmns)? {
                kids.nodes_mut().push(n);
            }
        }
        for (key, p) in &c.profiles {
            kids.nodes_mut().push(profile_node(key, p));
        }
    }
    Ok(node)
}

pub(crate) fn nr_to_kdl(nr: &NrDocument) -> Result<String> {
    let mut doc = KdlDocument::new();

    let mut version = KdlNode::new(nr_doc::VERSION);
    version.push(KdlEntry::new(nr.version as i128));
    doc.nodes_mut().push(version);

    doc.nodes_mut().push(str_list_node(
        nr_doc::BITMASK_CARRIERS,
        &nr.bitmask_carriers,
    ));

    for fp in &nr.bitmask_fingerprints {
        doc.nodes_mut().push(fingerprint_node(fp));
    }

    for (name, c) in &nr.carriers {
        doc.nodes_mut().push(carrier_node(name, c)?);
    }

    for f in &nr.dl_features {
        doc.nodes_mut().push(emit_dl_feature(f));
    }

    for f in &nr.ul_features {
        doc.nodes_mut().push(emit_ul_feature(f));
    }

    for combo in &nr.combo {
        doc.nodes_mut().push(emit_nr_combo(combo)?);
    }

    Ok(finish_doc(doc))
}

fn read_fingerprint(node: &KdlNode) -> Result<BitmaskFingerprint> {
    let mut r = NodeReader::new(node);
    let fingerprint = r.key_int::<u64>()?;
    let carriers = read_str_list(
        r.opt_child(fingerprint::CARRIERS)?
            .ok_or_else(|| anyhow!("`bitmask-fingerprint` missing `carriers`"))?,
    )?;
    r.finish()?;
    Ok(BitmaskFingerprint {
        fingerprint,
        carriers,
    })
}

fn read_profile(node: &KdlNode) -> Result<(String, ProfileSource)> {
    let mut r = NodeReader::new(node);
    let key = r.key_str()?;
    let multiplier = DecimalU64(r.req_int::<u64>(profile::MULTIPLIER)?);
    let unknown = DecimalU64(r.req_int::<u64>(profile::UNKNOWN)?);
    r.finish()?;
    Ok((
        key,
        ProfileSource {
            multiplier,
            unknown,
        },
    ))
}

fn read_carrier(node: &KdlNode) -> Result<(String, CarrierSource)> {
    let mut r = NodeReader::new(node);
    let name = r.key_str()?;
    let bitmask_id = r.opt_int::<i64>(carrier::BITMASK_ID)?;
    let profiled_id = r.opt_int::<i64>(carrier::PROFILED_ID)?;
    let mapping_id = r.opt_int::<u64>(carrier::MAPPING_ID)?;
    let signature = r.opt_int::<u64>(carrier::SIGNATURE)?.map(DecimalU64);
    let tier = match r.opt_str(carrier::TIER)? {
        None => None,
        Some(s) => Some(str_to_tier(&s)?),
    };
    // Non-empty PLMN lists are repeated `plmn mcc=… mnc=…` sibling nodes; a present-but-
    // empty list (`Some(vec![])`, a validated mapping-only carrier state) instead leaves a
    // bare, childless `plmns` marker so it stays distinguishable from no list at all
    // (`None`, when neither node is present). See the writer above.
    let plmn_nodes = r.children(carrier::PLMN);
    let plmns = if !plmn_nodes.is_empty() {
        Some(
            plmn_nodes
                .iter()
                .map(|n| read_plmn(n))
                .collect::<Result<Vec<_>>>()?,
        )
    } else if let Some(marker) = r.opt_child(carrier::PLMNS)? {
        // Bare marker = an empty-but-present PLMN list. It must be truly bare: reject a
        // stale `plmns "a" "b"` list rather than silently dropping its entries — PLMNs are
        // now `plmn mcc=… mnc=…` nodes (regenerate this file with `decompose` to migrate).
        NodeReader::new(marker).finish()?;
        Some(Vec::new())
    } else {
        None
    };
    let mut profiles = BTreeMap::new();
    for pnode in r.children(carrier::PROFILE) {
        let (k, v) = read_profile(pnode)?;
        if profiles.insert(k.clone(), v).is_some() {
            bail!("duplicate profile `{k}` in carrier `{name}`");
        }
    }
    r.finish()?;
    Ok((
        name,
        CarrierSource {
            bitmask_id,
            profiled_id,
            mapping_id,
            plmns,
            signature,
            tier,
            profiles,
        },
    ))
}

fn read_dl_feature(node: &KdlNode) -> Result<ShannonFeatureSetDlPerCcNr> {
    let mut r = NodeReader::new(node);
    let out = ShannonFeatureSetDlPerCcNr {
        max_scs: r.opt_int::<i32>(dl_catalog::MAX_SCS)?,
        max_mimo: r.opt_int::<i32>(dl_catalog::MAX_MIMO)?,
        max_bw: r.opt_int::<i32>(dl_catalog::MAX_BW)?,
        max_mod_order: r.opt_int::<i32>(dl_catalog::MAX_MOD_ORDER)?,
        bw_90mhz_supported: r.opt_bool(dl_catalog::BW_90MHZ_SUPPORTED)?,
    };
    r.finish()?;
    Ok(out)
}

fn read_ul_feature(node: &KdlNode) -> Result<ShannonFeatureSetUlPerCcNr> {
    let mut r = NodeReader::new(node);
    let out = ShannonFeatureSetUlPerCcNr {
        max_scs: r.opt_int::<i32>(ul_catalog::MAX_SCS)?,
        max_mimo_cb: r.opt_int::<i32>(ul_catalog::MAX_MIMO_CB)?,
        max_bw: r.opt_int::<i32>(ul_catalog::MAX_BW)?,
        max_mod_order: r.opt_int::<i32>(ul_catalog::MAX_MOD_ORDER)?,
        bw_90mhz_supported: r.opt_bool(ul_catalog::BW_90MHZ_SUPPORTED)?,
        max_mimo_non_cb: r.opt_int::<i32>(ul_catalog::MAX_MIMO_NON_CB)?,
    };
    r.finish()?;
    Ok(out)
}

fn read_selection(node: &KdlNode) -> Result<SelectionRect> {
    let mut r = NodeReader::new(node);
    let carriers = match r.opt_child(selection::CARRIERS)? {
        None => None,
        Some(n) => Some(read_str_list(n)?),
    };
    let skus = match r.opt_child(selection::SKUS)? {
        None => None,
        Some(n) => Some(read_str_list(n)?),
    };
    r.finish()?;
    Ok(SelectionRect { carriers, skus })
}

fn read_sub_block(node: &KdlNode) -> Result<NrSourceSubBlock> {
    let name = node.name().value();
    // The band comes from the node name, so there is no positional argument to read.
    let (kind, band) = if let Some(band) = parse_sub_block_name(name, combo::NR_PREFIX) {
        (SubBlockKind::Nr, band)
    } else if let Some(band) = parse_sub_block_name(name, combo::LTE_PREFIX) {
        (SubBlockKind::Lte, band)
    } else {
        bail!(
            "`{name}` is not a sub-block node name (expected `{}<band>` or `{}<band>`)",
            combo::NR_PREFIX,
            combo::LTE_PREFIX
        )
    };
    let mut r = NodeReader::new(node);
    let dl = r
        .opt_str(sub_block::DL)?
        .map(|raw| parse_direction(&raw, sub_block::DL))
        .transpose()?;
    let ul = r
        .opt_str(sub_block::UL)?
        .map(|raw| parse_direction(&raw, sub_block::UL))
        .transpose()?;

    // Arity depends on the kind. NR indices are one per CC and must match `cc_count`; an
    // E-UTRA sub-block carries a single `parseLteFeatureIndex` value regardless of class. An
    // empty list is the all-zero placeholder and is checked by neither.
    for (label, parsed) in [(sub_block::DL, dl.as_ref()), (sub_block::UL, ul.as_ref())] {
        let Some(parsed) = parsed else { continue };
        if parsed.indices.is_empty() {
            continue;
        }
        match kind {
            SubBlockKind::Nr => {
                // Catalog references are 1-based on NR. (On E-UTRA the value is a
                // `parseLteFeatureIndex` MIMO code, where 0 is legitimate.)
                ensure!(
                    parsed.indices.iter().all(|&index| index >= 1),
                    "`{name}` property `{label}` has a 0 index; NR per-CC catalog references \
                     are 1-based"
                );
                let expected = cc_count(SubBlockKind::Nr, parsed.bw_class)?;
                ensure!(
                    parsed.indices.len() == expected,
                    "`{name}` property `{label}` has {} per-CC index/indices but bandwidth class \
                     {} implies {expected}",
                    parsed.indices.len(),
                    (b'A' + parsed.bw_class - 1) as char
                );
            }
            SubBlockKind::Lte => ensure!(
                parsed.indices.len() == 1,
                "`{name}` property `{label}` takes at most one index on an E-UTRA sub-block, \
                 found {}",
                parsed.indices.len()
            ),
        }
    }

    let dl_bw_class = dl.as_ref().map(|d| d.bw_class);
    // Absent `ul` is the writer's omitted zero, not a genuine absence.
    let ul_bw_class = ul.as_ref().map_or(Some(0), |d| Some(d.bw_class));
    let cc: NrSourceSubBlock = match kind {
        SubBlockKind::Lte => SourceLteSubBlock {
            band,
            dl_bw_class,
            ul_bw_class,
            dl_feature: dl.as_ref().and_then(|d| d.indices.first().copied()),
            // An absent E-UTRA index re-defaults to `Some(0)` (omit-when-0, LTE-only).
            ul_feature: Some(
                ul.as_ref()
                    .and_then(|d| d.indices.first().copied())
                    .unwrap_or(0),
            ),
        }
        .into(),
        SubBlockKind::Nr => SourceNrSubBlock {
            band,
            dl_bw_class,
            ul_bw_class,
            dl_feature: dl
                .as_ref()
                .map(|d| d.indices.iter().map(|&i| i as usize).collect())
                .unwrap_or_default(),
            ul_feature: ul
                .as_ref()
                .map(|d| d.indices.iter().map(|&i| i as usize).collect())
                .unwrap_or_default(),
            srs_tx_switch: r.opt_int(sub_block::SRS_TX_SWITCH)?,
        }
        .into(),
    };
    r.finish()?; // now rejects any stray dl-cc-id / *-feature-index as unknown properties
    // The raw selector bytes are NOT reconstructed here. An unresolved direction only ever
    // carries the all-zero placeholder, which is a pure function of kind + `bw_class`, so
    // `cc_to_node` omits it and `NrSourceSubBlock::resolve` (the single provision-path boundary
    // that needs the bytes) re-derives it.
    Ok(cc)
}

fn read_combo(node: &KdlNode) -> Result<NrSourceCombo> {
    let mut r = NodeReader::new(node);
    // `power-class`/`bcs-nr`/`bcs-eutra`/`intra-band-en-dc-support` are corpus-verified
    // always `Some`: an absent property is the writer's omitted-zero (Task 8), so it
    // defaults back to `Some(0)`. `bcs-intra-endc` derives from `intra-band-en-dc-support` —
    // see below.
    let power_class = r.opt_int::<i32>(combo::POWER_CLASS)?.or(Some(0));
    let bcs_nr = r.opt_int::<u32>(combo::BCS_NR)?.or(Some(0));
    let bcs_eutra = r.opt_int::<u32>(combo::BCS_EUTRA)?.or(Some(0));
    let intra_band_en_dc_support = r
        .opt_int::<i32>(combo::INTRA_BAND_EN_DC_SUPPORT)?
        .or(Some(0));
    // An absent `bcs-intra-endc` re-derives via the shared `derive_bcs_intra_endc` — the
    // inverse of the omit rule in `emit_nr_combo`. Kept AFTER `intra-band-en-dc-support`,
    // the field it depends on.
    let bcs_intra_endc = match r.opt_int::<u32>(combo::BCS_INTRA_ENDC)? {
        Some(v) => Some(v),
        None => derive_bcs_intra_endc(intra_band_en_dc_support),
    };
    let mut selection = Vec::new();
    for snode in r.children(combo::SELECTION) {
        selection.push(read_selection(snode)?);
    }
    // Reading NR before E-UTRA preserves the previous behaviour; `validate_nr_combos` sorts
    // sub-blocks by `RawSubBlockKey` regardless, which is why the reader could already read
    // NR-first while real documents store E-UTRA first and still round-trip byte-identically.
    let mut sub_blocks = Vec::new();
    for cnode in r.children_matching("nr<band>", |name| {
        parse_sub_block_name(name, combo::NR_PREFIX).is_some()
    }) {
        sub_blocks.push(read_sub_block(cnode)?);
    }
    for cnode in r.children_matching("lte<band>", |name| {
        parse_sub_block_name(name, combo::LTE_PREFIX).is_some()
    }) {
        sub_blocks.push(read_sub_block(cnode)?);
    }
    r.finish()?;
    Ok(NrSourceCombo {
        selection: if selection.is_empty() {
            None
        } else {
            Some(selection)
        },
        power_class,
        bcs_nr,
        bcs_intra_endc,
        bcs_eutra,
        intra_band_en_dc_support,
        sub_blocks,
    })
}

/// Parses a `version N` node's payload, or errors if `version` was already read once — shared
/// by `nr_from_kdl` and `lte_from_kdl`, whose sole `version` node is identical in shape. Takes
/// the field's current value (not a pre-computed flag) so the duplicate check runs BEFORE
/// parsing, matching the single inline check this replaces in both readers: a malformed
/// second `version` node still reports "duplicate", not a parse error.
fn read_version(node: &KdlNode, existing: Option<u32>) -> Result<u32> {
    if existing.is_some() {
        bail!("duplicate `version`");
    }
    let mut r = NodeReader::new(node);
    let v = r.key_int::<u32>()?;
    r.finish()?;
    Ok(v)
}

/// Parses `bitmask-carriers`, or errors if it already appeared once — same duplicate-before-
/// parse order as `read_version`.
fn read_bitmask_carriers(node: &KdlNode, existing: Option<&[String]>) -> Result<Vec<String>> {
    if existing.is_some() {
        bail!("duplicate `bitmask-carriers`");
    }
    read_str_list(node)
}

/// Inserts an already-parsed `(name, value)` pair into a top-level map keyed by name, erroring
/// if `name` was already present. Shared by `nr_from_kdl`'s `carrier` and `lte_from_kdl`'s
/// `file` nodes, whose duplicate-name shape is otherwise identical; the caller parses before
/// calling, so a malformed duplicate still reports its own parse error first.
fn insert_unique<T>(map: &mut BTreeMap<String, T>, what: &str, (k, v): (String, T)) -> Result<()> {
    if map.insert(k.clone(), v).is_some() {
        bail!("duplicate {what} `{k}`");
    }
    Ok(())
}

pub(crate) fn nr_from_kdl(text: &str) -> Result<NrDocument> {
    let doc: KdlDocument = text.parse().context("nr.kdl is not valid KDL")?;
    let mut version: Option<u32> = None;
    let mut bitmask_carriers: Option<Vec<String>> = None;
    let mut bitmask_fingerprints = Vec::new();
    let mut carriers = BTreeMap::new();
    let mut dl_features = Vec::new();
    let mut ul_features = Vec::new();
    let mut combo = Vec::new();
    for node in doc.nodes() {
        // An if/else chain rather than a `match`: the arms compare against `kdl_keys`
        // constants, which are not valid `match` patterns.
        let name = node.name().value();
        if name == nr_doc::VERSION {
            version = Some(read_version(node, version)?);
        } else if name == nr_doc::BITMASK_CARRIERS {
            bitmask_carriers = Some(read_bitmask_carriers(node, bitmask_carriers.as_deref())?);
        } else if name == nr_doc::BITMASK_FINGERPRINT {
            bitmask_fingerprints.push(read_fingerprint(node)?);
        } else if name == nr_doc::CARRIER {
            insert_unique(&mut carriers, nr_doc::CARRIER, read_carrier(node)?)?;
        } else if name == nr_doc::DL_FEATURE {
            dl_features.push(read_dl_feature(node)?);
        } else if name == nr_doc::UL_FEATURE {
            ul_features.push(read_ul_feature(node)?);
        } else if name == nr_doc::COMBO {
            combo.push(read_combo(node)?);
        } else {
            bail!("unknown top-level node `{name}` in nr.kdl");
        }
    }
    Ok(NrDocument {
        version: version.ok_or_else(|| anyhow!("nr.kdl missing `version`"))?,
        bitmask_carriers: bitmask_carriers
            .ok_or_else(|| anyhow!("nr.kdl missing `bitmask-carriers`"))?,
        bitmask_fingerprints,
        carriers,
        dl_features,
        ul_features,
        combo,
    })
}

pub(crate) fn lte_to_kdl(lte: &LteDocument) -> Result<String> {
    let mut doc = KdlDocument::new();

    let mut version = KdlNode::new(lte_doc::VERSION);
    version.push(KdlEntry::new(lte.version as i128));
    doc.nodes_mut().push(version);

    for (key, f) in &lte.files {
        let mut node = KdlNode::new(lte_doc::FILE);
        node.push(KdlEntry::new(key.as_str()));
        node.push(KdlEntry::new_prop(
            lte_file::FINGERPRINT,
            f.fingerprint as i128,
        ));
        node.push(KdlEntry::new_prop(lte_file::BITMASK, f.bitmask as i128));
        doc.nodes_mut().push(node);
    }

    for combo in &lte.combo {
        doc.nodes_mut().push(emit_lte_combo(combo)?);
    }

    Ok(finish_doc(doc))
}

fn read_file(node: &KdlNode) -> Result<(String, LteFileSource)> {
    let mut r = NodeReader::new(node);
    let key = r.key_str()?;
    let fingerprint = r.req_int::<u64>(lte_file::FINGERPRINT)?;
    let bitmask = r.req_int::<u64>(lte_file::BITMASK)?;
    r.finish()?;
    Ok((
        key,
        LteFileSource {
            fingerprint,
            bitmask,
        },
    ))
}

fn read_lte_cc(node: &KdlNode) -> Result<LteComponent> {
    let name = node.name().value();
    let band = parse_sub_block_name(name, lte_combo::SUB_BLOCK_PREFIX).with_context(|| {
        format!(
            "`{name}` is not a sub-block node name (expected `{}<band>`)",
            lte_combo::SUB_BLOCK_PREFIX
        )
    })?;
    let band = i32::from(band);
    let mut r = NodeReader::new(node);
    let dl_bw_class_mimo = parse_class_mimo(
        &r.opt_str(lte_sub_block::DL_MIMO)?.ok_or_else(|| {
            anyhow!(
                "`{name}` missing required property `{}`",
                lte_sub_block::DL_MIMO
            )
        })?,
        lte_sub_block::DL_MIMO,
    )?;
    // Omit-when-0: an absent UL property is UL disabled. `parse_class_mimo` never returns 0, so
    // this is the sole route to one and the value-to-spelling mapping stays one-to-one.
    let ul_bw_class_mimo = Some(
        r.opt_str(lte_sub_block::UL_MIMO)?
            .map(|raw| parse_class_mimo(&raw, lte_sub_block::UL_MIMO))
            .transpose()?
            .unwrap_or(0),
    );
    r.finish()?;
    Ok(LteComponent {
        band,
        dl_bw_class_mimo,
        ul_bw_class_mimo,
    })
}

fn read_lte_combo(node: &KdlNode) -> Result<LteSourceCombo> {
    let mut r = NodeReader::new(node);
    let bcs = r.opt_int::<u64>(lte_combo::BCS)?;
    let unknown1 = r.opt_int::<u64>(lte_combo::UNKNOWN1)?;
    let unknown2 = r.opt_int::<u64>(lte_combo::UNKNOWN2)?;
    let mut selection = Vec::new();
    for snode in r.children(lte_combo::SELECTION) {
        selection.push(read_selection(snode)?);
    }
    let mut components = Vec::new();
    for cnode in r.children_matching("subblock<band>", |name| {
        parse_sub_block_name(name, lte_combo::SUB_BLOCK_PREFIX).is_some()
    }) {
        components.push(read_lte_cc(cnode)?);
    }
    r.finish()?;
    Ok(LteSourceCombo {
        selection: if selection.is_empty() {
            None
        } else {
            Some(selection)
        },
        bcs,
        unknown1,
        unknown2,
        components,
    })
}

pub(crate) fn lte_from_kdl(text: &str) -> Result<LteDocument> {
    let doc: KdlDocument = text.parse().context("lte.kdl is not valid KDL")?;
    let mut version: Option<u32> = None;
    let mut files = BTreeMap::new();
    let mut combo = Vec::new();
    for node in doc.nodes() {
        // See the `nr_from_kdl` twin: constants are not valid `match` patterns.
        let name = node.name().value();
        if name == lte_doc::VERSION {
            version = Some(read_version(node, version)?);
        } else if name == lte_doc::FILE {
            insert_unique(&mut files, lte_doc::FILE, read_file(node)?)?;
        } else if name == lte_doc::COMBO {
            combo.push(read_lte_combo(node)?);
        } else {
            bail!("unknown top-level node `{name}` in lte.kdl");
        }
    }
    Ok(LteDocument {
        version: version.ok_or_else(|| anyhow!("lte.kdl missing `version`"))?,
        files,
        combo,
    })
}

#[cfg(test)]
mod combinator_tests {
    use super::*;

    fn node(text: &str) -> kdl::KdlNode {
        let doc: KdlDocument = text.parse().unwrap();
        doc.nodes().first().unwrap().clone()
    }

    #[test]
    fn reads_key_props_and_children_then_finishes() {
        let n = node("cr VZW bi=1 t=main {\n    ps \"311-480\"\n}\n");
        let mut r = NodeReader::new(&n);
        assert_eq!(r.key_str().unwrap(), "VZW");
        assert_eq!(r.opt_int::<i64>(carrier::BITMASK_ID).unwrap(), Some(1));
        assert_eq!(r.opt_str(carrier::TIER).unwrap().as_deref(), Some("main"));
        let plmns = r.opt_child(carrier::PLMNS).unwrap().unwrap();
        assert_eq!(read_str_list(plmns).unwrap(), vec!["311-480".to_string()]);
        r.finish().unwrap();
    }

    #[test]
    fn finish_rejects_unknown_property() {
        let n = node("cr VZW bogus=9\n");
        let mut r = NodeReader::new(&n);
        assert_eq!(r.key_str().unwrap(), "VZW");
        assert!(r.finish().is_err());
    }

    /// `nr.kdl`/`lte.kdl` are the only editing surface in the tool, and a duplicated property
    /// was silently last-wins: `node.get(key)` returns the last entry and `props_used` then
    /// marks the key consumed, so `finish()` could not object either. A hand edit that
    /// duplicated a line lost a value with no diagnostic.
    #[test]
    fn finish_rejects_a_duplicate_property() {
        let n = node("node 78 dl-bw-class=1 dl-bw-class=2\n");
        let mut r = NodeReader::new(&n);
        assert_eq!(r.key_int::<u16>().unwrap(), 78);
        assert_eq!(r.opt_int::<u8>("dl-bw-class").unwrap(), Some(2));

        let error = r.finish().unwrap_err().to_string();

        assert!(error.contains("dl-bw-class"), "{error}");
        assert!(error.contains("more than once"), "{error}");
    }

    /// The shadowed entry is never type-checked either, so a duplicate could smuggle an
    /// outright type error past the reader.
    #[test]
    fn finish_rejects_a_duplicate_property_whose_shadowed_value_is_ill_typed() {
        let n = node("p mcc=\"oops\" mcc=310\n");
        let mut r = NodeReader::new(&n);
        assert_eq!(r.opt_int::<u16>("mcc").unwrap(), Some(310));

        assert!(r.finish().is_err());
    }

    /// Dynamic child names (a sub-block is spelled `nr257`, not `nr` + a positional) cannot be looked up by
    /// name, so `NodeReader` needs a predicate — and `finish()` must treat what the predicate
    /// consumed as known, or every sub-block would read as an unknown child.
    #[test]
    fn children_matching_consumes_by_predicate() {
        let n = node("c {\n    n257 x=1\n    n41 x=2\n    B66 x=3\n}\n");
        let mut r = NodeReader::new(&n);

        let nr = r.children_matching("n<band>", |name| {
            name.strip_prefix('n').is_some_and(|band| !band.is_empty())
        });
        assert_eq!(nr.len(), 2);
        let lte = r.children_matching("B<band>", |name| {
            name.strip_prefix('B').is_some_and(|band| !band.is_empty())
        });
        assert_eq!(lte.len(), 1);

        r.finish().expect("predicate-consumed children are known");
    }

    /// A child no predicate claimed is still rejected — the strictness must survive the move
    /// from name-set membership to pattern matching.
    #[test]
    fn finish_still_rejects_a_child_no_predicate_claimed() {
        let n = node("c {\n    n257 x=1\n    mystery 1\n}\n");
        let mut r = NodeReader::new(&n);
        let _ = r.children_matching("n<band>", |name| name.starts_with('n'));

        let error = r.finish().unwrap_err().to_string();

        assert!(error.contains("mystery"), "{error}");
    }

    #[test]
    fn finish_rejects_unknown_child() {
        let n = node("cr VZW {\n    mystery 1\n}\n");
        let mut r = NodeReader::new(&n);
        assert_eq!(r.key_str().unwrap(), "VZW");
        assert!(r.finish().is_err());
    }

    #[test]
    fn opt_int_range_checks() {
        let n = node("x v=-1\n");
        let mut r = NodeReader::new(&n);
        assert!(r.opt_int::<u64>("v").is_err());
    }
}

#[cfg(test)]
mod nr_tests {
    use super::*;
    use crate::compiler::{
        features::{NrSourceSubBlock, SourceNrSubBlock},
        schema::{
            BitmaskFingerprint, CarrierSource, DecimalU64, NrDocument, NrSourceCombo, ProfileSource,
        },
        selection::SelectionRect,
    };
    use std::collections::BTreeMap;

    fn sample() -> NrDocument {
        NrDocument {
            version: crate::compiler::schema::SOURCE_FORMAT_VERSION,
            bitmask_carriers: vec!["ATT".into(), "VZW".into()],
            bitmask_fingerprints: vec![BitmaskFingerprint {
                fingerprint: 715_188_856,
                carriers: vec!["VZW".into()],
            }],
            carriers: BTreeMap::from([(
                "VZW".to_string(),
                CarrierSource {
                    bitmask_id: Some(1),
                    profiled_id: Some(0),
                    mapping_id: Some(u64::MAX),
                    plmns: Some(vec!["311-480".into()]),
                    signature: Some(DecimalU64(1)),
                    tier: Some(CarrierTier::Main),
                    profiles: BTreeMap::from([(
                        "66813533".to_string(),
                        ProfileSource {
                            multiplier: DecimalU64(66_813_533),
                            unknown: DecimalU64(0),
                        },
                    )]),
                },
            )]),
            dl_features: vec![ShannonFeatureSetDlPerCcNr {
                max_scs: Some(3),
                max_mimo: Some(2),
                max_bw: Some(100),
                max_mod_order: Some(2),
                bw_90mhz_supported: Some(true),
            }],
            ul_features: vec![ShannonFeatureSetUlPerCcNr {
                max_scs: Some(2),
                max_mimo_cb: Some(1),
                max_bw: Some(100),
                max_mod_order: None,
                bw_90mhz_supported: None,
                max_mimo_non_cb: None,
            }],
            combo: vec![NrSourceCombo {
                selection: Some(vec![SelectionRect {
                    carriers: Some(vec!["VZW".into()]),
                    skus: Some(vec!["legacy".into()]),
                }]),
                power_class: Some(3),
                bcs_nr: Some(1),
                bcs_intra_endc: None,
                bcs_eutra: None,
                intra_band_en_dc_support: None,
                sub_blocks: vec![
                    SourceNrSubBlock {
                        band: 78,
                        dl_bw_class: Some(1),
                        // 1-based: the sole catalog entry. (This was `vec![0]`, an invalid
                        // reference the old format round-tripped without noticing because
                        // nothing checked it until `validate_documents`.)
                        dl_feature: vec![1],
                        ..Default::default()
                    }
                    .into(),
                ],
            }],
        }
    }

    #[test]
    fn nr_round_trips_byte_identically() {
        let doc = sample();
        let text = nr_to_kdl(&doc).unwrap();
        let back = nr_from_kdl(&text).expect("read back");
        assert_eq!(nr_to_kdl(&back).unwrap(), text, "byte-identity");
        // spot-check the readable shape:
        assert!(text.contains("cr VZW bi=1"), "{text}");
        assert!(text.contains("pf \"66813533\" x=66813533 u=0"), "{text}");
        assert!(text.contains("n78"), "{text}");
        assert!(
            text.contains("d=A1"),
            "class and per-CC list merge into one value: {text}"
        );
        assert!(
            !text.contains("dl-cc-id") && !text.contains("dl-feature-index"),
            "removed escape-hatch surface keys must not appear: {text}"
        );
        assert!(text.contains("mi=18446744073709551615"), "{text}");
    }

    #[test]
    fn nr_sub_block_repeated_features_and_lte_placeholder_round_trip() {
        // Two-CC NR sub-block: repeated `dl-feature=` reads back as one usize per CC.
        // LTE sub-block: no per-CC references at all. The all-zero placeholder selector
        // that the binary carries for it is NOT part of the source model — the reader
        // leaves it out entirely (`NrSourceSubBlock::resolve` derives it from
        // `dl-bw-class=1` on the provision path; see `resolve_derives_the_omitted_placeholder`
        // in `compiler::features`) — and re-emitting must stay a byte-identical fixed
        // point.
        let text = "version 1\nbc ATT\nc {\n    n48 d=B5,8\n    B66 d=A\n}\n";
        let doc = nr_from_kdl(text).expect("parse");
        let cc = &doc.combo[0].sub_blocks;
        let NrSourceSubBlock::Nr(nr) = &cc[0] else {
            panic!("first sub-block is an `nr` node, got {:?}", cc[0])
        };
        assert_eq!(nr.dl_feature, vec![5, 8], "repeated dl-feature");
        // The `lte` variant has no per-CC feature list to be empty — that is the point.
        assert!(matches!(cc[1], NrSourceSubBlock::Lte(_)));
        assert_eq!(
            nr_to_kdl(&doc).unwrap(),
            text,
            "re-emitting must not resurrect the omitted LTE placeholder (fixed point)"
        );
    }

    #[test]
    fn lte_sub_block_scalar_feature_names_and_ul_omit_when_zero_round_trip() {
        // On an `lte` node the proto-4/5 index is spelled `dl-feature`/`ul-feature` (no
        // `-index`), single-valued; `ul-feature=0` is omitted and re-defaults to `Some(0)`.
        // No per-CC list is read on LTE. Byte-identical fixed point.
        let text = "version 1\nbc ATT\nc {\n    B7 d=B1 u=A2\n    B66 d=A3\n}\n";
        let doc = nr_from_kdl(text).expect("parse");
        let cc = &doc.combo[0].sub_blocks;
        // Both are `lte` nodes, so neither can carry a per-CC feature list at all — the
        // variant simply has no such field.
        let [NrSourceSubBlock::Lte(first), NrSourceSubBlock::Lte(second)] = &cc[..] else {
            panic!("both sub-blocks are `lte` nodes, got {cc:?}")
        };
        assert_eq!(first.dl_feature, Some(1));
        assert_eq!(first.ul_feature, Some(2));
        assert_eq!(second.dl_feature, Some(3));
        assert_eq!(
            second.ul_feature,
            Some(0),
            "absent ul-feature on an lte node defaults to Some(0)"
        );
        assert_eq!(
            nr_to_kdl(&doc).unwrap(),
            text,
            "byte-identical fixed point: ul=0 stays omitted, no -index suffix"
        );
    }

    #[test]
    fn carrier_plmns_use_structured_plmn_nodes() {
        let mut doc = sample();
        doc.carriers.get_mut("VZW").unwrap().plmns =
            Some(vec!["311-480".into(), "310-004".into(), "228-ff".into()]);
        let text = nr_to_kdl(&doc).unwrap();
        assert!(text.contains("p mcc=311 mnc=480"), "{text}");
        assert!(text.contains("p mcc=310 mnc=4 mnc-digits=3"), "{text}");
        assert!(
            text.contains("p mcc=228\n") || text.contains("p mcc=228 "),
            "{text}"
        );
        assert!(
            !text.contains("ps "),
            "old plmns list node must be gone: {text}"
        );
        // round-trips
        let back = nr_from_kdl(&text).unwrap();
        let carrier = back.carriers.values().next().unwrap();
        assert_eq!(
            carrier.plmns.as_deref().unwrap(),
            ["311-480", "310-004", "228-ff"]
        );
    }

    #[test]
    fn carrier_plmns_empty_some_round_trips_distinct_from_none() {
        let mut doc = sample();
        doc.carriers.get_mut("VZW").unwrap().plmns = Some(Vec::new());
        doc.carriers
            .insert("NOPLMN".to_string(), CarrierSource::default());
        let text = nr_to_kdl(&doc).unwrap();
        let back = nr_from_kdl(&text).unwrap();
        assert_eq!(
            back.carriers["VZW"].plmns,
            Some(Vec::new()),
            "present-but-empty PLMN list must survive the round trip: {text}"
        );
        assert_eq!(
            back.carriers["NOPLMN"].plmns, None,
            "carrier with no plmns/plmn nodes at all must read back as None: {text}"
        );
    }

    #[test]
    fn nr_rejects_a_plmns_node_carrying_stale_list_args() {
        // A `plmns` node must be a *bare* marker (present-but-empty PLMN list). A stale
        // pre-migration `plmns "a" "b"` list node — this writer's old shape, and something
        // a hand-editor could still type — must be a hard parse error, never a silent
        // `Some(vec![])` that drops the listed PLMNs.
        let text = "version 1\nbc \"LEGACY\"\ncr \"MAP\" mi=7 {\n    ps \"310-260\"\n}\n";
        let err = format!("{:#}", nr_from_kdl(text).unwrap_err());
        assert!(err.contains("ps"), "{err}");
    }

    #[test]
    fn nr_rejects_unknown_property() {
        let text = nr_to_kdl(&sample()).unwrap().replace("n78", "n78 bogus=9");
        assert!(nr_from_kdl(&text).is_err());
    }

    /// This began as the sharpest instance of the duplicate-property hole: `dl-feature` was a
    /// per-CC *list* on an `nr` node but a single scalar on an `lte` node, so the identical
    /// spelling meant "both values" under one and "silently keep the last" under the other.
    ///
    /// Merging class and list into one value removes the repeated key entirely, so the mistake
    /// it guarded now takes a different shape — a multi-index value on an E-UTRA sub-block,
    /// which carries one `parseLteFeatureIndex` scalar whatever its class.
    #[test]
    fn lte_sub_block_rejects_a_repeated_feature_property() {
        let text = "version 1\nbc ATT\nc {\n    B66 d=B3,4\n}\n";

        let error = nr_from_kdl(text).unwrap_err().to_string();

        assert!(error.contains("at most one index"), "{error}");
        assert!(error.contains("E-UTRA"), "{error}");
    }

    /// The band is the node name's suffix, so a sub-block line reads as its 3GPP band
    /// designation — the same convention `SubBlockKind::band_label` uses everywhere else.
    #[test]
    fn sub_block_node_name_carries_the_band() {
        let text = "version 1\nbc ATT\nc {\n    n257 d=G1,1 u=A1\n    B66 d=A2\n}\n";

        let doc = nr_from_kdl(text).expect("bands parse out of the node name");
        let combo = &doc.combo[0];

        assert_eq!(combo.sub_blocks[0].band(), 257);
        assert_eq!(combo.sub_blocks[0].kind(), SubBlockKind::Nr);
        assert_eq!(combo.sub_blocks[1].band(), 66);
        assert_eq!(combo.sub_blocks[1].kind(), SubBlockKind::Lte);

        let out = nr_to_kdl(&doc).unwrap();
        assert!(out.contains("n257 "), "{out}");
        assert!(out.contains("B66 "), "{out}");
    }

    /// A malformed band must be rejected, not silently read as band 0 or waved through as an
    /// unknown child.
    #[test]
    fn malformed_sub_block_node_names_are_rejected() {
        for bad in ["nr", "n257x", "nrfoo", "lte", "x99", "n99999999"] {
            let text = format!("version 1\nbc ATT\nc {{\n    {bad} dl-bw-class=1 df=1\n}}\n");
            assert!(
                nr_from_kdl(&text).is_err(),
                "`{bad}` must not parse as a sub-block"
            );
        }
    }

    #[test]
    fn compiler_source_rejects_removed_props() {
        // The four escape-hatch surface keys were dropped (proto machinery kept). The strict
        // reader must now reject each as an unknown property.
        for key in [
            "dl-feature-index=1",
            "ul-feature-index=1",
            "dl-cc-id=1",
            "ul-cc-id=1",
        ] {
            let text = format!("version 1\nbc ATT\nc {{\n    n78 d=A {key}\n}}\n");
            let err = nr_from_kdl(&text).unwrap_err().to_string();
            assert!(
                err.contains("unknown property"),
                "{key} should be rejected: {err}"
            );
        }
    }

    #[test]
    fn nr_rejects_unknown_top_level_node() {
        let text = format!("{}\nmystery 1\n", nr_to_kdl(&sample()).unwrap());
        assert!(nr_from_kdl(&text).is_err());
    }

    #[test]
    fn nr_rejects_duplicate_carrier() {
        let text = format!("{}\ncr VZW\n", nr_to_kdl(&sample()).unwrap());
        assert!(nr_from_kdl(&text).is_err());
    }

    #[test]
    fn nr_rejects_unknown_cc_kind() {
        let text = nr_to_kdl(&sample()).unwrap().replace("n78", "bogus78");
        assert!(nr_from_kdl(&text).is_err());
    }

    #[test]
    fn nr_rejects_unknown_tier() {
        let text = nr_to_kdl(&sample()).unwrap().replace("t=main", "t=bogus");
        assert!(nr_from_kdl(&text).is_err());
    }

    #[test]
    fn nr_rejects_missing_version() {
        let text = nr_to_kdl(&sample()).unwrap().replacen("version 1\n", "", 1);
        let err = format!("{:#}", nr_from_kdl(&text).unwrap_err());
        assert!(err.contains("missing `version`"), "{err}");
    }

    #[test]
    fn nr_rejects_duplicate_version() {
        let text = format!("version 1\n{}", nr_to_kdl(&sample()).unwrap());
        let err = format!("{:#}", nr_from_kdl(&text).unwrap_err());
        assert!(err.contains("duplicate `version`"), "{err}");
    }

    #[test]
    fn nr_rejects_bare_numeric_profile_key() {
        // A map key (the profile anchor) must be a quoted string arg, never a bare
        // integer — the reader rejects `profile 66813533` in favor of `profile "66813533"`.
        let text = nr_to_kdl(&sample())
            .unwrap()
            .replace("pf \"66813533\"", "pf 66813533");
        assert!(nr_from_kdl(&text).is_err());
    }

    fn combo_with(
        bcs_intra_endc: Option<u32>,
        intra_band_en_dc_support: Option<i32>,
    ) -> NrSourceCombo {
        NrSourceCombo {
            selection: None,
            power_class: Some(0),
            bcs_nr: Some(0),
            bcs_intra_endc,
            bcs_eutra: Some(0),
            intra_band_en_dc_support,
            sub_blocks: vec![],
        }
    }

    fn parse_combo(text: &str) -> KdlNode {
        let doc: KdlDocument = text.parse().expect("c KDL parses");
        doc.nodes().first().expect("one combo node").clone()
    }

    #[test]
    fn bcs_intra_endc_derivable_zero_is_omitted_and_restored() {
        // Some(0) + intra=1: derived == actual → omitted on disk, re-derived on read.
        let node = emit_nr_combo(&combo_with(Some(0), Some(1))).unwrap();
        let text = node.to_string();
        assert!(
            !text.contains("bcs-intra-endc"),
            "derivable zero omitted: {text}"
        );
        let back = read_combo(&parse_combo(&text)).unwrap();
        assert_eq!(back.bcs_intra_endc, Some(0));
        assert_eq!(back.intra_band_en_dc_support, Some(1));
    }

    #[test]
    fn bcs_intra_endc_exceptional_zero_stays_explicit() {
        // Some(0) + intra=0: derived is None → the zero is written explicitly (the ~20).
        let node = emit_nr_combo(&combo_with(Some(0), Some(0))).unwrap();
        let text = node.to_string();
        assert!(text.contains("bi=0"), "exception zero explicit: {text}");
        let back = read_combo(&parse_combo(&text)).unwrap();
        assert_eq!(back.bcs_intra_endc, Some(0));
        assert_eq!(back.intra_band_en_dc_support, Some(0));
    }

    #[test]
    fn bcs_intra_endc_nonzero_stays_explicit() {
        let node = emit_nr_combo(&combo_with(Some(7), Some(1))).unwrap();
        let text = node.to_string();
        assert!(text.contains("bi=7"), "{text}");
        let back = read_combo(&parse_combo(&text)).unwrap();
        assert_eq!(back.bcs_intra_endc, Some(7));
    }

    #[test]
    fn bcs_intra_endc_none_without_intra1_is_omitted() {
        let node = emit_nr_combo(&combo_with(None, Some(0))).unwrap();
        let text = node.to_string();
        assert!(!text.contains("bcs-intra-endc"), "{text}");
        let back = read_combo(&parse_combo(&text)).unwrap();
        assert_eq!(back.bcs_intra_endc, None);
    }

    #[test]
    fn bcs_intra_endc_none_with_intra1_fails_closed() {
        // The single unrepresentable state (0 corpus cases): omission would re-derive
        // Some(0), so the writer must bail rather than silently corrupt.
        let err = emit_nr_combo(&combo_with(None, Some(1))).unwrap_err();
        assert!(
            format!("{err:#}").contains("cannot be represented"),
            "{err:#}"
        );
    }

    #[test]
    fn bw_class_is_direction_first_and_old_spelling_rejected() {
        // New direction-first spelling round-trips byte-identically.
        let text = "version 1\nbc ATT\nc {\n    n78 d=A u=A\n}\n";
        let doc = nr_from_kdl(text).expect("parse new spelling");
        assert_eq!(
            nr_to_kdl(&doc).unwrap(),
            text,
            "new spelling is a fixed point"
        );

        // The old suffix spelling is now an unknown property (strict reader, no alias).
        let old = "version 1\nbc ATT\nc {\n    n78 bw-class-dl=1 bw-class-ul=1\n}\n";
        let err = nr_from_kdl(old).unwrap_err().to_string();
        assert!(
            err.contains("unknown property") && err.contains("bw-class-dl"),
            "old spelling must be rejected, got: {err}"
        );
    }

    #[test]
    /// Property order is load-bearing for byte-identity. The merge collapsed each direction's
    /// class and feature list into one property, so what remains to pin is that DL precedes UL.
    fn nr_emits_direction_grouped_order() {
        let text = "version 1\nbc ATT\nc {\n    n78 d=A2 u=A3\n}\n";
        let doc = nr_from_kdl(text).expect("parse");
        let out = nr_to_kdl(&doc).unwrap();
        let dl = out.find("d=A2").expect("DL property present");
        let ul = out.find("u=A3").expect("UL property present");
        assert!(dl < ul, "expected dl before ul:\n{out}");
    }

    /// `d=`/`u=` mean different things depending on which document reads them: in `nr.kdl` a
    /// bandwidth class plus a per-CC feature-index list (`parse_direction`); in `lte.kdl` a
    /// bandwidth class plus a MIMO-width bitfield (`parse_class_mimo`). Nothing else in the
    /// suite ties the two codecs together, so an editor who noticed the shared spelling and
    /// "unified" them would break nothing visible here — only every real `lte.kdl` combo,
    /// silently. The collision is a deliberate trade, not an oversight: the *document* fixes
    /// the interpretation, the same way a sub-block node name carries no radio-kind tag. This
    /// test is where that trade is meant to be learned before anyone "fixes" it.
    #[test]
    fn identical_d_equals_text_means_different_things_in_each_document() {
        // The exact same property text, `d=C2`, fed to each document's reader.
        let nr_text = "version 1\nbc ATT\nc {\n    B66 d=C2\n}\n";
        let lte_text = "version 1\nc {\n    B66 d=C2\n}\n";

        // nr.kdl: `d=C2` is bandwidth class C (3) plus per-CC feature index 2. A `B66` node
        // always parses to the `Lte` variant of `NrSourceSubBlock`.
        let nr_doc = nr_from_kdl(nr_text).expect("nr.kdl parses");
        let NrSourceSubBlock::Lte(sub_block) = &nr_doc.combo[0].sub_blocks[0] else {
            panic!(
                "a `B66` node parses to the `Lte` variant, got {:?}",
                nr_doc.combo[0].sub_blocks[0]
            )
        };
        assert_eq!(sub_block.band, 66);
        assert_eq!(sub_block.dl_bw_class, Some(3));
        assert_eq!(sub_block.dl_feature, Some(2));

        // lte.kdl: the identical text `d=C2` is class C + 2x2 MIMO, the bitfield 8192.
        let lte_doc = lte_from_kdl(lte_text).expect("lte.kdl parses");
        let component = &lte_doc.combo[0].components[0];
        assert_eq!(component.band, 66);
        assert_eq!(component.dl_bw_class_mimo, 8192);

        // Spell out that these are different INTERPRETATIONS of identical text, not merely
        // different incidental numbers: nr.kdl's value is a small 1-based catalog reference,
        // lte.kdl's is a bitfield. If the two `d=` codecs were ever unified, this is the
        // assertion that would catch it.
        assert_ne!(
            i64::from(sub_block.dl_feature.unwrap()),
            i64::from(component.dl_bw_class_mimo),
            "nr.kdl's per-CC feature index and lte.kdl's class+MIMO bitfield must stay disjoint \
             decodings of the same `d=C2` text — if they ever match, the two codecs have merged"
        );

        // `B66 d=A` parses in nr.kdl: class A (1), with an empty per-CC list (the all-zero
        // placeholder that `resolve` re-materializes later; not asserted here, see report).
        let nr_placeholder = nr_from_kdl("version 1\nbc ATT\nc {\n    B66 d=A\n}\n")
            .expect("`d=A` parses as a class with no per-CC list");
        let NrSourceSubBlock::Lte(placeholder_sub_block) = &nr_placeholder.combo[0].sub_blocks[0]
        else {
            panic!("still a `B66`/`Lte` sub-block")
        };
        assert_eq!(placeholder_sub_block.dl_bw_class, Some(1));

        // The identical `B66 d=A` is rejected by lte_from_kdl: a class+MIMO value always needs
        // the MIMO digit that nr.kdl's bare-class placeholder spelling never carries.
        let error = lte_from_kdl("version 1\nc {\n    B66 d=A\n}\n")
            .unwrap_err()
            .to_string();
        assert!(error.contains("MIMO width"), "{error}");
    }
}

#[cfg(test)]
mod lte_tests {
    use super::*;
    use crate::compiler::{
        schema::{LteDocument, LteFileSource, LteSourceCombo},
        selection::SelectionRect,
    };
    use std::collections::BTreeMap;

    fn sample() -> LteDocument {
        LteDocument {
            version: crate::compiler::schema::SOURCE_FORMAT_VERSION,
            files: BTreeMap::from([(
                "3".to_string(),
                LteFileSource {
                    fingerprint: 715_188_856,
                    bitmask: 1,
                },
            )]),
            combo: vec![LteSourceCombo {
                selection: Some(vec![SelectionRect {
                    carriers: None,
                    skus: Some(vec!["legacy".into()]),
                }]),
                bcs: Some(2),
                unknown1: None,
                unknown2: None,
                components: vec![
                    LteComponent {
                        band: 1,
                        dl_bw_class_mimo: 32769,
                        ul_bw_class_mimo: Some(32769),
                    },
                    LteComponent {
                        band: 3,
                        dl_bw_class_mimo: 32769,
                        // Omit-when-0: this renders with no `u` at all.
                        ul_bw_class_mimo: Some(0),
                    },
                ],
            }],
        }
    }

    #[test]
    fn lte_round_trips_byte_identically() {
        let doc = sample();
        let text = lte_to_kdl(&doc).unwrap();
        let back = lte_from_kdl(&text).expect("read back");
        assert_eq!(lte_to_kdl(&back).unwrap(), text, "byte-identity");
        assert!(text.contains("f \"3\" fp=715188856 bm=1"), "{text}");
        assert!(text.contains("B1 d=A4 u=A4"), "{text}");
        assert!(text.contains("B3 d=A4\n"), "{text}");
    }

    #[test]
    fn lte_rejects_unknown_property() {
        let text = lte_to_kdl(&sample()).unwrap().replace("b=2", "b=2 bogus=9");
        assert!(lte_from_kdl(&text).is_err());
    }

    #[test]
    fn lte_rejects_superseded_direction_property_spellings() {
        // Companion to `bw_class_is_direction_first_and_old_spelling_rejected`, which covers only
        // the NR combo pair. Two generations are dead here: the direction-last `md`/`mu`, and the
        // `dm`/`um` that carried the class+MIMO encoding in the key.
        for (current, dead) in [("d=", "md="), ("u=", "mu="), ("d=", "dm="), ("u=", "um=")] {
            // Additive: keep the required current property and append the dead one, so the reader
            // reports the unknown property rather than a missing required one.
            let text = lte_to_kdl(&sample())
                .unwrap()
                .replace(current, &format!("{dead}1 {current}"));
            let err = lte_from_kdl(&text).unwrap_err().to_string();
            assert!(
                err.contains("unknown property") && err.contains(dead.trim_end_matches('=')),
                "{dead} must be rejected, got: {err}"
            );
        }
    }

    #[test]
    fn lte_rejects_duplicate_file() {
        let text = format!("{}\nf \"3\" fp=1 bm=1\n", lte_to_kdl(&sample()).unwrap());
        assert!(lte_from_kdl(&text).is_err());
    }

    #[test]
    fn lte_rejects_missing_version() {
        let text = lte_to_kdl(&sample())
            .unwrap()
            .replacen("version 1\n", "", 1);
        let err = format!("{:#}", lte_from_kdl(&text).unwrap_err());
        assert!(err.contains("missing `version`"), "{err}");
    }

    #[test]
    fn lte_rejects_bare_numeric_file_key() {
        // The LTE file id is a map key: a quoted string arg, not a bare integer.
        let text = lte_to_kdl(&sample()).unwrap().replace("f \"3\"", "f 3");
        assert!(lte_from_kdl(&text).is_err());
    }
}
