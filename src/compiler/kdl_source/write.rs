use super::{derive_bcs_intra_endc, plmn::write_plmn, sub_block_node_name};
use crate::{
    compiler::{
        decimal_key_entries,
        features::SourceSubBlock,
        kdl_bcs::format_bcs,
        kdl_direction::{format_class_mimo, format_direction},
        kdl_keys::{
            carrier, dl_catalog, doc, fingerprint, lte_combo, lte_file, nr_combo, profile,
            selection, sub_block, ul_catalog,
        },
        schema::{
            BitmaskFingerprint, CarrierSource, LteDocument, LteFileSource, LteSourceCombo,
            NrDocument, NrSourceCombo, ProfileSource, SOURCE_FORMAT_VERSION,
        },
        selection::SelectionRect,
    },
    kdl_support::{finish_doc, opt_bool_prop, opt_int_prop, opt_str_prop, str_list_node},
    mapping::Plmn,
    model::Tier,
    proto::{LteComponent, ShannonFeatureSetDlPerCcNr, ShannonFeatureSetUlPerCcNr},
    raw_nr::{Direction, SubBlockKind},
};
use anyhow::{Context, Result, bail, ensure};
use kdl::{KdlDocument, KdlEntry, KdlNode};
/// A `s { c …; m … }` block, shared by NR and LTE combos. Both scopes spell it `"s"`; this names
/// the NR one because a helper has to pick one, not because the block is NR-only.
fn write_selection(rect: &SelectionRect) -> KdlNode {
    let mut node = KdlNode::new(nr_combo::SELECTION);
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

/// Emit one NR-combo sub-block node. The node name is the kind prefix (`n`/`B`) plus the band
/// (`n78`, `B66`); DL and UL are then positional arguments — DL required, UL omitted when its
/// bandwidth class is 0 — followed by `srs-tx-switch`. Order is load-bearing for byte-identity.
///
/// The two node kinds spell proto 4/5 and 6/7 differently, which is why the source model is a
/// sum type and this matches on it once:
///   * `nr`: the proto-4/5 index is NOT surfaced — NR derives it from its feature set on
///     provision. The per-CC catalog list becomes the comma-separated tail of the positional
///     DL/UL value (`format_direction`), one 1-based reference per CC. An unresolved NR
///     selector is only ever the all-zero placeholder (corpus: 0 of 1.74M non-zero), omitted
///     here and re-derived by the reader.
///   * `lte`: the index becomes the single trailing number in the positional DL/UL value (the
///     LTE MIMO × CC-count value). LTE never carries a per-CC list. `ul_feature` is
///     always-`Some` on LTE with `Some(0)` ⟺ no UL, so its zero is omitted (Task 8
///     omit-when-0) by dropping the whole UL argument, and the reader re-defaults it. LTE has
///     no `srs-tx-switch`.
fn write_sub_block(cc: &SourceSubBlock) -> Result<KdlNode> {
    let prefix = match cc.kind() {
        SubBlockKind::Nr => nr_combo::NR_PREFIX,
        SubBlockKind::Lte => nr_combo::LTE_PREFIX,
    };
    // The band is the node NAME's suffix, not a positional argument.
    let mut node = KdlNode::new(sub_block_node_name(prefix, cc.band()));

    let (dl_features, ul_features, srs_tx_switch): (Vec<usize>, Vec<usize>, Option<i32>) = match cc
    {
        SourceSubBlock::Lte(cc) => (
            cc.dl_feature_index.map(usize::from).into_iter().collect(),
            cc.ul_feature_index
                .filter(|&v| v != 0)
                .map(usize::from)
                .into_iter()
                .collect(),
            None,
        ),
        SourceSubBlock::Nr(cc) => (
            cc.dl_feature_indices.clone(),
            cc.ul_feature_indices.clone(),
            cc.srs_tx_switch,
        ),
    };

    // Class and per-CC list are one value per direction, and both are positional:
    // `n78 G30,30 A5`. An empty list is the all-zero placeholder, which the source omits and
    // `resolve` re-materialises.
    //
    // DL is required. With the keys gone there is nothing to distinguish the two arguments
    // but their order, so an omitted DL would shift UL into first place and silently change
    // the sub-block's meaning. `Some(0)` already fails inside `format_direction`; this
    // catches the `None` that would otherwise vanish. Corpus: 0 of 93,679 sub-blocks.
    let dl_class = cc.bw_class(Direction::Dl).with_context(|| {
        format!(
            "sub-block band {} omits its DL bandwidth class; the source format spells DL as the \
             first positional argument and cannot represent its absence",
            cc.band()
        )
    })?;
    node.push(KdlEntry::new(
        format_direction(dl_class, &dl_features)?.as_str(),
    ));
    // `ul_bw_class` is corpus-verified always `Some` on a real sub-block, so `Some(0)` is
    // omitted here and re-defaulted to `Some(0)` by `read_sub_block` (omit-when-0) — a
    // value-faithful round trip, not a lossy one. An absent second argument means class 0.
    if let Some(class) = cc.bw_class(Direction::Ul).filter(|&v| v != 0) {
        node.push(KdlEntry::new(
            format_direction(class, &ul_features)?.as_str(),
        ));
    }
    opt_int_prop(&mut node, sub_block::SRS_TX_SWITCH, srs_tx_switch);
    Ok(node)
}

fn write_lte_component(comp: &LteComponent) -> Result<KdlNode> {
    let band = u16::try_from(comp.band).with_context(|| {
        format!(
            "LTE component band {} does not fit the source format",
            comp.band
        )
    })?;
    let mut node = KdlNode::new(sub_block_node_name(lte_combo::SUB_BLOCK_PREFIX, band));
    // The bitfield becomes `<letter><mimo>`: 32769 -> `A4`. The number told you nothing
    // without the table in `report::lte::lte_class`. Positional, DL first — the same shape
    // NR-combo sub-blocks use.
    node.push(KdlEntry::new(
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
    // information, so it is omitted and re-defaulted by `read_lte_component`. Same omit-when-0 rule the
    // NR sub-block uses for its UL bandwidth class. `validate_lte_combos` rejects a `None`, so an
    // omitted second argument always means the explicit zero and never a dropped absent field.
    if let Some(ul) = comp.ul_bw_class_mimo.filter(|&v| v != 0) {
        node.push(KdlEntry::new(format_class_mimo(ul)?.as_str()));
    }
    Ok(node)
}

fn write_dl_feature(f: &ShannonFeatureSetDlPerCcNr) -> KdlNode {
    let mut node = KdlNode::new(doc::DL_FEATURE);
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

fn write_ul_feature(f: &ShannonFeatureSetUlPerCcNr) -> KdlNode {
    let mut node = KdlNode::new(doc::UL_FEATURE);
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

pub(super) fn write_nr_combo(combo: &NrSourceCombo) -> Result<KdlNode> {
    let mut node = KdlNode::new(doc::NR_COMBO);
    // `power-class`/`bcs-nr`/`bcs-eutra`/`intra-band-en-dc-support` are corpus-verified
    // always `Some` on a real combo header (never `None`), so `Some(0)` is omitted here and
    // re-defaulted to `Some(0)` by `read_nr_combo` below (Task 8 omit-when-0).
    opt_int_prop(
        &mut node,
        nr_combo::POWER_CLASS,
        combo.header.power_class.filter(|&v| v != 0),
    );
    opt_str_prop(
        &mut node,
        nr_combo::BCS_NR,
        combo
            .header
            .bcs_nr
            .filter(|&v| v != 0)
            .map(format_bcs)
            .as_deref(),
    );
    // Task 2: `bcs-intra-endc` is the BCS index for intra-band EN-DC; a combo carries it
    // exactly when it advertises that mode (`intra-band-en-dc-support == 1`). Derive the
    // common `Some(0)` from that flag and omit it; write only the ~20 exceptional zeros
    // (intra_band != 1) and every nonzero explicitly. The one unrepresentable state
    // (`None` + intra_band == 1, 0 corpus cases) fails closed. See spec
    // DESIGN.md.
    let derived_bcs_intra_endc = derive_bcs_intra_endc(combo.header.intra_band_en_dc_support);
    match combo.header.bcs_intra_endc {
        actual if actual == derived_bcs_intra_endc => {} // omit: derivable zeros + every None
        Some(v) => {
            let spelling = format_bcs(v);
            opt_str_prop(&mut node, nr_combo::BCS_INTRA_ENDC, Some(spelling.as_str()));
        }
        None => bail!(
            "absent intra-ENDC BCS with intra-band EN-DC support 1 cannot be represented: \
             omission derives a present value of 0; supply that BCS value or correct the \
             support flag (sub-block bands {:?})",
            combo
                .sub_blocks
                .iter()
                .map(SourceSubBlock::band)
                .collect::<Vec<_>>()
        ),
    }
    opt_str_prop(
        &mut node,
        nr_combo::BCS_EUTRA,
        combo
            .header
            .bcs_eutra
            .filter(|&v| v != 0)
            .map(format_bcs)
            .as_deref(),
    );
    opt_int_prop(
        &mut node,
        nr_combo::INTRA_BAND_EN_DC_SUPPORT,
        combo.header.intra_band_en_dc_support.filter(|&v| v != 0),
    );
    append_combo_children(
        &mut node,
        combo.selection.as_deref(),
        &combo.sub_blocks,
        write_sub_block,
    )?;
    Ok(node)
}

fn write_lte_combo(combo: &LteSourceCombo) -> Result<KdlNode> {
    let mut node = KdlNode::new(doc::LTE_COMBO);
    // `LteCombo.bcs` is `uint64` on the wire but carries a 32-bit left-aligned 3GPP BIT
    // STRING; every one of the 3,878 corpus values fits. Fail closed rather than invent a
    // spelling for a width that has never been observed — the same stance `format_direction`
    // and `format_class_mimo` take. See DESIGN.md.
    if let Some(bcs) = combo.bcs {
        let bits = u32::try_from(bcs).with_context(|| {
            format!(
                "LTE combo bcs {bcs} does not fit the 32-bit BCS bit string the source format \
                 spells (never observed; refusing to invent an encoding)"
            )
        })?;
        let spelling = format_bcs(bits);
        opt_str_prop(&mut node, lte_combo::BCS, Some(spelling.as_str()));
    }
    opt_int_prop(&mut node, lte_combo::UNKNOWN1, combo.unknown1);
    opt_int_prop(&mut node, lte_combo::UNKNOWN2, combo.unknown2);
    append_combo_children(
        &mut node,
        combo.selection.as_deref(),
        &combo.components,
        write_lte_component,
    )?;
    Ok(node)
}

fn append_combo_children<T>(
    node: &mut KdlNode,
    selection: Option<&[SelectionRect]>,
    components: &[T],
    render: impl Fn(&T) -> Result<KdlNode>,
) -> Result<()> {
    if selection.is_some() || !components.is_empty() {
        let kids = node.ensure_children();
        if let Some(sel) = selection {
            for rect in sel {
                kids.nodes_mut().push(write_selection(rect));
            }
        }
        for comp in components {
            kids.nodes_mut().push(render(comp)?);
        }
    }
    Ok(())
}

/// One `bitmask-fingerprint N { carriers … }` node: which bitmask-folder carriers share a
/// given legacy fingerprint.
fn write_fingerprint(fp: &BitmaskFingerprint) -> KdlNode {
    let mut node = KdlNode::new(doc::BITMASK_FINGERPRINT);
    node.push(KdlEntry::new(fp.fingerprint as i128));
    node.ensure_children()
        .nodes_mut()
        .push(str_list_node(fingerprint::CARRIERS, &fp.carriers));
    node
}

/// One `f "KEY" fp=… bm=…` node — the LTE file whitelist entry.
fn write_lte_file(key: u64, f: &LteFileSource) -> KdlNode {
    let mut node = KdlNode::new(doc::LTE_FILE);
    node.push(KdlEntry::new(key.to_string()));
    node.push(KdlEntry::new_prop(
        lte_file::FINGERPRINT,
        f.fingerprint as i128,
    ));
    node.push(KdlEntry::new_prop(lte_file::BITMASK, f.bitmask as i128));
    node
}

/// One carrier's `plmns` children: either a bare, childless `plmns` marker for a
/// present-but-empty list (distinguishing it from no list at all — see `read_carrier`'s
/// inverse), or one `plmn mcc=… mnc=…` node per entry.
fn write_plmn_children(plmns: &[Plmn]) -> Result<Vec<KdlNode>> {
    if plmns.is_empty() {
        Ok(vec![KdlNode::new(carrier::PLMNS)])
    } else {
        plmns.iter().copied().map(write_plmn).collect()
    }
}

/// One `profile "KEY" multiplier=… unknown=…` node.
fn write_profile(key: u64, p: &ProfileSource) -> KdlNode {
    let mut node = KdlNode::new(carrier::PROFILE);
    node.push(KdlEntry::new(key.to_string()));
    node.push(KdlEntry::new_prop(
        profile::MULTIPLIER,
        i128::from(p.multiplier),
    ));
    node.push(KdlEntry::new_prop(profile::UNKNOWN, i128::from(p.unknown)));
    node
}

/// One `carrier "NAME" …` node, with its `plmns`/`profile` children when it has either.
fn write_carrier(name: &str, c: &CarrierSource) -> Result<KdlNode> {
    let mut node = KdlNode::new(doc::CARRIER);
    node.push(KdlEntry::new(name));
    opt_int_prop(&mut node, carrier::BITMASK_ID, c.bitmask_id);
    opt_int_prop(&mut node, carrier::PROFILED_ID, c.profiled_id);
    opt_int_prop(&mut node, carrier::MAPPING_ID, c.mapping_id);
    opt_int_prop(&mut node, carrier::SIGNATURE, c.signature);
    opt_str_prop(&mut node, carrier::TIER, c.tier.map(Tier::as_str));
    if c.plmns.is_some() || !c.profiles.is_empty() {
        let kids = node.ensure_children();
        if let Some(plmns) = &c.plmns {
            for n in write_plmn_children(plmns)? {
                kids.nodes_mut().push(n);
            }
        }
        for (&key, p) in decimal_key_entries(&c.profiles) {
            kids.nodes_mut().push(write_profile(key, p));
        }
    }
    Ok(node)
}

/// Serialize one source document. Node order is canonical and load-bearing: the version marker,
/// then the whitelist and identity nodes (`bc`, `bf`, `c`, `f`), then the two feature catalogs —
/// which sit immediately above the combos that index into them — then every NR combo, then every
/// LTE combo. Within each group, use `BTreeMap` iteration for carriers, decimal-text order
/// for numeric profile/file keys, and the caller's `Vec` order for the rest.
///
/// Takes the halves by reference rather than a `&SourceDocument`: `ValidatedSources` stores them
/// separately, and `decompose` calls this twice, so a wrapper parameter would mean cloning the
/// whole multi-megabyte model each time.
pub(in crate::compiler) fn write_sources(nr: &NrDocument, lte: &LteDocument) -> Result<String> {
    let mut document = KdlDocument::new();

    // The constant, not a parameter: this build only ever writes the format it reads, and
    // `checked_version` has already proven any parsed input equal to it.
    let mut version_node = KdlNode::new(doc::VERSION);
    version_node.push(KdlEntry::new(i128::from(SOURCE_FORMAT_VERSION)));
    document.nodes_mut().push(version_node);

    document
        .nodes_mut()
        .push(str_list_node(doc::BITMASK_CARRIERS, &nr.bitmask_carriers));

    for fp in &nr.bitmask_fingerprints {
        document.nodes_mut().push(write_fingerprint(fp));
    }

    for (name, c) in &nr.carriers {
        document.nodes_mut().push(write_carrier(name, c)?);
    }

    for (&key, f) in decimal_key_entries(&lte.files) {
        document.nodes_mut().push(write_lte_file(key, f));
    }

    for f in &nr.dl_features {
        document.nodes_mut().push(write_dl_feature(f));
    }

    for f in &nr.ul_features {
        document.nodes_mut().push(write_ul_feature(f));
    }

    for combo in &nr.combos {
        document.nodes_mut().push(write_nr_combo(combo)?);
    }

    for combo in &lte.combos {
        document.nodes_mut().push(write_lte_combo(combo)?);
    }

    Ok(finish_doc(document))
}
