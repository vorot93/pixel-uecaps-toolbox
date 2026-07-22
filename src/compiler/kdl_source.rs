//! KDL (de)serialization boundary for the folder-compiler source documents.
//! Hand-mapped over the `kdl` crate (KDL v2); replaces the former TOML/serde path.

use std::collections::BTreeMap;

use anyhow::{Context, Result, anyhow, bail};
use kdl::{KdlDocument, KdlEntry, KdlNode};

use crate::{
    compiler::{
        features::{DlFeatureSource, NrSourceSubBlock, UlFeatureSource},
        schema::{
            BitmaskFingerprint, CarrierSource, CarrierTier, DecimalU64, LteDocument, LteFileSource,
            LteSourceCombo, LteSourceComponent, NrDocument, NrSourceCombo, ProfileSource,
        },
        selection::SelectionRect,
    },
    kdl_support::{
        NodeReader, cckind_to_str, finish_doc, opt_bool_prop, opt_int_prop, opt_str_prop,
        plmn_to_node, push_repeated_int_prop, read_plmn, read_str_list, str_list_node,
        str_to_cckind,
    },
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
        other => bail!("unknown tier `{other}` (expected `main` or `alt`)"),
    }
}

/// A `selection { carriers …; skus … }` block (shared by NR and LTE combos).
fn selection_to_node(rect: &SelectionRect) -> KdlNode {
    let mut node = KdlNode::new("selection");
    if rect.carriers.is_some() || rect.skus.is_some() {
        let kids = node.ensure_children();
        if let Some(carriers) = &rect.carriers {
            kids.nodes_mut().push(str_list_node("carriers", carriers));
        }
        if let Some(skus) = &rect.skus {
            kids.nodes_mut().push(str_list_node("skus", skus));
        }
    }
    node
}

pub(crate) fn cc_to_node(cc: &NrSourceSubBlock) -> KdlNode {
    let mut node = KdlNode::new(cckind_to_str(cc.kind));
    // `band` is the node's sole leading positional argument (`nr 78 …`), pushed before
    // any property so the autoformatter keeps it leading.
    node.push(KdlEntry::new(i128::from(cc.band)));
    opt_int_prop(&mut node, "dl-bw-class", cc.dl_bw_class.map(|v| v as i128));
    // The proto-4/5 feature index and the proto-6/7 per-CC catalog list are spelled per node
    // kind:
    //   * `nr`: the index is NOT surfaced — NR derives it from its feature set on build (the
    //     former `dl-feature-index`/`ul-feature-index` source override was dropped; the proto
    //     field is still materialized on build/decode). Per-CC list → repeated
    //     `dl-feature=`/`ul-feature=`; an unresolved NR selector is only ever the all-zero
    //     placeholder (corpus: 0 of 1.74M non-zero), omitted here and re-derived by the reader.
    //   * `lte`: index → single scalar `dl-feature`/`ul-feature` (the LTE MIMO × CC-count value).
    //     LTE never carries a per-CC list (its selectors are the all-zero placeholder — corpus:
    //     0 of 1.74M non-zero), so the un-suffixed name is free. `ul-feature` is always-`Some`
    //     on LTE with `Some(0)` ⟺ no UL, so its zero is omitted (Task 8 omit-when-0) and the
    //     reader re-defaults it. Gating the list emit by kind (rather than relying on the LTE
    //     list being empty) keeps `dl-feature` written exactly once on an `lte` node.
    // Properties are emitted direction-grouped — `dl-bw-class`, `dl-feature`, `ul-bw-class`,
    // `ul-feature` — so DL and UL each read as a contiguous group instead of interleaved
    // bw-class/feature pairs.
    match cc.kind {
        SubBlockKind::Lte => opt_int_prop(
            &mut node,
            "dl-feature",
            cc.dl_feature_index.map(|v| v as i128),
        ),
        _ => push_repeated_int_prop(
            &mut node,
            "dl-feature",
            &cc.dl_feature.iter().map(|&v| v as i128).collect::<Vec<_>>(),
        ),
    }
    // `ul-bw-class` is corpus-verified always `Some` on a real sub-block (never `None`),
    // so `Some(0)` is omitted here and re-defaulted to `Some(0)` by `read_sub_block` below
    // (Task 8 omit-when-0) — a value-faithful round trip, not a lossy one.
    opt_int_prop(
        &mut node,
        "ul-bw-class",
        cc.ul_bw_class.filter(|&v| v != 0).map(|v| v as i128),
    );
    match cc.kind {
        SubBlockKind::Lte => opt_int_prop(
            &mut node,
            "ul-feature",
            cc.ul_feature_index.filter(|&v| v != 0).map(|v| v as i128),
        ),
        _ => push_repeated_int_prop(
            &mut node,
            "ul-feature",
            &cc.ul_feature.iter().map(|&v| v as i128).collect::<Vec<_>>(),
        ),
    }
    opt_int_prop(
        &mut node,
        "srs-tx-switch",
        cc.srs_tx_switch.map(|v| v as i128),
    );
    node
}

pub(crate) fn lte_cc_to_node(comp: &LteSourceComponent) -> KdlNode {
    let mut node = KdlNode::new("subblock");
    node.push(KdlEntry::new(i128::from(comp.band)));
    node.push(KdlEntry::new_prop(
        "dl-bw-class-mimo",
        comp.dl_bw_class_mimo as i128,
    ));
    opt_int_prop(
        &mut node,
        "ul-bw-class-mimo",
        comp.ul_bw_class_mimo.map(|v| v as i128),
    );
    node
}

pub(crate) fn emit_dl_feature(f: &DlFeatureSource) -> KdlNode {
    let mut node = KdlNode::new("dl-feature");
    opt_int_prop(&mut node, "max-scs", f.max_scs.map(|v| v as i128));
    opt_int_prop(&mut node, "max-mimo", f.max_mimo.map(|v| v as i128));
    opt_int_prop(&mut node, "max-bw", f.max_bw.map(|v| v as i128));
    opt_int_prop(
        &mut node,
        "max-mod-order",
        f.max_mod_order.map(|v| v as i128),
    );
    opt_bool_prop(&mut node, "bw-90mhz-supported", f.bw_90mhz_supported);
    node
}

pub(crate) fn emit_ul_feature(f: &UlFeatureSource) -> KdlNode {
    let mut node = KdlNode::new("ul-feature");
    opt_int_prop(&mut node, "max-scs", f.max_scs.map(|v| v as i128));
    opt_int_prop(&mut node, "max-mimo-cb", f.max_mimo_cb.map(|v| v as i128));
    opt_int_prop(&mut node, "max-bw", f.max_bw.map(|v| v as i128));
    opt_int_prop(
        &mut node,
        "max-mod-order",
        f.max_mod_order.map(|v| v as i128),
    );
    opt_bool_prop(&mut node, "bw-90mhz-supported", f.bw_90mhz_supported);
    opt_int_prop(
        &mut node,
        "max-mimo-non-cb",
        f.max_mimo_non_cb.map(|v| v as i128),
    );
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

pub(crate) fn emit_nr_combo(combo: &NrSourceCombo) -> Result<KdlNode> {
    let mut node = KdlNode::new("combo");
    // `power-class`/`bcs-nr`/`bcs-eutra`/`intra-band-en-dc-support` are corpus-verified
    // always `Some` on a real combo header (never `None`), so `Some(0)` is omitted here and
    // re-defaulted to `Some(0)` by `read_combo` below (Task 8 omit-when-0).
    opt_int_prop(
        &mut node,
        "power-class",
        combo.power_class.filter(|&v| v != 0).map(|v| v as i128),
    );
    opt_int_prop(
        &mut node,
        "bcs-nr",
        combo.bcs_nr.filter(|&v| v != 0).map(|v| v as i128),
    );
    // Task 2: `bcs-intra-endc` is the BCS index for intra-band EN-DC; a combo carries it
    // exactly when it advertises that mode (`intra-band-en-dc-support == 1`). Derive the
    // common `Some(0)` from that flag and omit it; write only the ~20 exceptional zeros
    // (intra_band != 1) and every nonzero explicitly. The one unrepresentable state
    // (`None` + intra_band == 1, 0 corpus cases) fails closed. See spec
    // DESIGN.md.
    let derived_bcs_intra_endc = derive_bcs_intra_endc(combo.intra_band_en_dc_support);
    match combo.bcs_intra_endc {
        actual if actual == derived_bcs_intra_endc => {} // omit: derivable zeros + every None
        Some(v) => opt_int_prop(&mut node, "bcs-intra-endc", Some(i128::from(v))),
        None => bail!(
            "bcs_intra_endc=None with intra-band-en-dc-support=1 cannot be represented by \
             omission (would re-derive as Some(0)); this combo is unexpected \
             (sub-block bands {:?}) — see \
             DESIGN.md",
            combo.sub_blocks.iter().map(|c| c.band).collect::<Vec<_>>()
        ),
    }
    opt_int_prop(
        &mut node,
        "bcs-eutra",
        combo.bcs_eutra.filter(|&v| v != 0).map(|v| v as i128),
    );
    opt_int_prop(
        &mut node,
        "intra-band-en-dc-support",
        combo
            .intra_band_en_dc_support
            .filter(|&v| v != 0)
            .map(|v| v as i128),
    );
    if combo.selection.is_some() || !combo.sub_blocks.is_empty() {
        let kids = node.ensure_children();
        if let Some(sel) = &combo.selection {
            for rect in sel {
                kids.nodes_mut().push(selection_to_node(rect));
            }
        }
        for cc in &combo.sub_blocks {
            kids.nodes_mut().push(cc_to_node(cc));
        }
    }
    Ok(node)
}

pub(crate) fn emit_lte_combo(combo: &LteSourceCombo) -> KdlNode {
    let mut node = KdlNode::new("combo");
    opt_int_prop(&mut node, "bcs", combo.bcs.map(|v| v as i128));
    opt_int_prop(&mut node, "unknown1", combo.unknown1.map(|v| v as i128));
    opt_int_prop(&mut node, "unknown2", combo.unknown2.map(|v| v as i128));
    if combo.selection.is_some() || !combo.components.is_empty() {
        let kids = node.ensure_children();
        if let Some(sel) = &combo.selection {
            for rect in sel {
                kids.nodes_mut().push(selection_to_node(rect));
            }
        }
        for comp in &combo.components {
            kids.nodes_mut().push(lte_cc_to_node(comp));
        }
    }
    node
}

pub(crate) fn nr_to_kdl(nr: &NrDocument) -> Result<String> {
    let mut doc = KdlDocument::new();

    let mut version = KdlNode::new("version");
    version.push(KdlEntry::new(nr.version as i128));
    doc.nodes_mut().push(version);

    doc.nodes_mut()
        .push(str_list_node("bitmask-carriers", &nr.bitmask_carriers));

    for fp in &nr.bitmask_fingerprints {
        let mut node = KdlNode::new("bitmask-fingerprint");
        node.push(KdlEntry::new(fp.fingerprint as i128));
        node.ensure_children()
            .nodes_mut()
            .push(str_list_node("carriers", &fp.carriers));
        doc.nodes_mut().push(node);
    }

    for (name, c) in &nr.carriers {
        let mut node = KdlNode::new("carrier");
        node.push(KdlEntry::new(name.as_str()));
        opt_int_prop(&mut node, "bitmask-id", c.bitmask_id.map(|v| v as i128));
        opt_int_prop(&mut node, "profiled-id", c.profiled_id.map(|v| v as i128));
        opt_int_prop(&mut node, "mapping-id", c.mapping_id.map(|v| v as i128));
        opt_int_prop(&mut node, "signature", c.signature.map(|v| v.0 as i128));
        opt_str_prop(&mut node, "tier", c.tier.map(tier_to_str));
        if c.plmns.is_some() || !c.profiles.is_empty() {
            let kids = node.ensure_children();
            if let Some(plmns) = &c.plmns {
                if plmns.is_empty() {
                    // A bare, childless `plmns` marker distinguishes a present-but-empty
                    // PLMN list (`Some(vec![])`, a validated mapping-only carrier state)
                    // from no PLMN list at all (`None`); see `read_carrier` below.
                    kids.nodes_mut().push(str_list_node("plmns", plmns));
                } else {
                    for p in plmns {
                        kids.nodes_mut().push(plmn_to_node(p)?);
                    }
                }
            }
            for (key, p) in &c.profiles {
                let mut pn = KdlNode::new("profile");
                pn.push(KdlEntry::new(key.as_str()));
                pn.push(KdlEntry::new_prop("multiplier", p.multiplier.0 as i128));
                pn.push(KdlEntry::new_prop("unknown", p.unknown.0 as i128));
                kids.nodes_mut().push(pn);
            }
        }
        doc.nodes_mut().push(node);
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
        r.opt_child("carriers")?
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
    let multiplier = DecimalU64(r.req_int::<u64>("multiplier")?);
    let unknown = DecimalU64(r.req_int::<u64>("unknown")?);
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
    let bitmask_id = r.opt_int::<i64>("bitmask-id")?;
    let profiled_id = r.opt_int::<i64>("profiled-id")?;
    let mapping_id = r.opt_int::<u64>("mapping-id")?;
    let signature = r.opt_int::<u64>("signature")?.map(DecimalU64);
    let tier = match r.opt_str("tier")? {
        None => None,
        Some(s) => Some(str_to_tier(&s)?),
    };
    // Non-empty PLMN lists are repeated `plmn mcc=… mnc=…` sibling nodes; a present-but-
    // empty list (`Some(vec![])`, a validated mapping-only carrier state) instead leaves a
    // bare, childless `plmns` marker so it stays distinguishable from no list at all
    // (`None`, when neither node is present). See the writer above.
    let plmn_nodes = r.children("plmn");
    let plmns = if !plmn_nodes.is_empty() {
        Some(
            plmn_nodes
                .iter()
                .map(|n| read_plmn(n))
                .collect::<Result<Vec<_>>>()?,
        )
    } else if let Some(marker) = r.opt_child("plmns")? {
        // Bare marker = an empty-but-present PLMN list. It must be truly bare: reject a
        // stale `plmns "a" "b"` list rather than silently dropping its entries — PLMNs are
        // now `plmn mcc=… mnc=…` nodes (regenerate this file with `decode` to migrate).
        NodeReader::new(marker).finish()?;
        Some(Vec::new())
    } else {
        None
    };
    let mut profiles = BTreeMap::new();
    for pnode in r.children("profile") {
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

fn read_dl_feature(node: &KdlNode) -> Result<DlFeatureSource> {
    let mut r = NodeReader::new(node);
    let out = DlFeatureSource {
        max_scs: r.opt_int::<i32>("max-scs")?,
        max_mimo: r.opt_int::<i32>("max-mimo")?,
        max_bw: r.opt_int::<i32>("max-bw")?,
        max_mod_order: r.opt_int::<i32>("max-mod-order")?,
        bw_90mhz_supported: r.opt_bool("bw-90mhz-supported")?,
    };
    r.finish()?;
    Ok(out)
}

fn read_ul_feature(node: &KdlNode) -> Result<UlFeatureSource> {
    let mut r = NodeReader::new(node);
    let out = UlFeatureSource {
        max_scs: r.opt_int::<i32>("max-scs")?,
        max_mimo_cb: r.opt_int::<i32>("max-mimo-cb")?,
        max_bw: r.opt_int::<i32>("max-bw")?,
        max_mod_order: r.opt_int::<i32>("max-mod-order")?,
        bw_90mhz_supported: r.opt_bool("bw-90mhz-supported")?,
        max_mimo_non_cb: r.opt_int::<i32>("max-mimo-non-cb")?,
    };
    r.finish()?;
    Ok(out)
}

fn read_selection(node: &KdlNode) -> Result<SelectionRect> {
    let mut r = NodeReader::new(node);
    let carriers = match r.opt_child("carriers")? {
        None => None,
        Some(n) => Some(read_str_list(n)?),
    };
    let skus = match r.opt_child("skus")? {
        None => None,
        Some(n) => Some(read_str_list(n)?),
    };
    r.finish()?;
    Ok(SelectionRect { carriers, skus })
}

/// Reconstruct the omitted all-zero placeholder (see `write_raw_cc_ids` above) when a
/// direction's bandwidth class is present: `bw_class` supplies the CC count to fill with
/// zero bytes. `None` when there is no bandwidth class to derive a count from (no data for
/// that direction at all — the pre-omission `None` state).
fn reconstruct_placeholder_ids(
    kind: SubBlockKind,
    bw_class: Option<i32>,
) -> Result<Option<Vec<u8>>> {
    match bw_class {
        Some(bw) => Ok(Some(vec![0u8; cc_count(kind, bw)?])),
        None => Ok(None),
    }
}

fn read_sub_block(node: &KdlNode) -> Result<NrSourceSubBlock> {
    let kind = str_to_cckind(node.name().value(), "NR/EN-DC component kind")?;
    let mut r = NodeReader::new(node);
    let band = r.key_int::<i32>()?;
    let dl_bw_class = r.opt_int::<i32>("dl-bw-class")?;
    // Corpus-verified always-`Some`: an absent `ul-bw-class` property is the writer's
    // omitted-zero (Task 8), not a genuine `None` — default it back to `Some(0)`.
    let ul_bw_class = r.opt_int::<i32>("ul-bw-class")?.or(Some(0));
    // Kind-aware inverse of `cc_to_node`'s feature emit. On `lte`, `dl-feature`/`ul-feature`
    // are the single scalar proto-4/5 index and there is no per-CC list; an absent `ul-feature`
    // re-defaults to `Some(0)` (Task 8 omit-when-0, LTE-only). On `nr` the index is not
    // surfaced at all — it is derived from the feature set on build — so the reader carries no
    // NR index (`None`); the source override (`dl-feature-index`/`ul-feature-index`) was dropped.
    let (dl_feature_index, ul_feature_index, dl_feature, ul_feature) = match kind {
        SubBlockKind::Lte => (
            r.opt_int::<i32>("dl-feature")?,
            r.opt_int::<i32>("ul-feature")?.or(Some(0)),
            Vec::new(),
            Vec::new(),
        ),
        _ => (
            None, // NR derives the index on build; no source override
            None,
            r.repeated_int::<usize>("dl-feature")?,
            r.repeated_int::<usize>("ul-feature")?,
        ),
    };
    let srs_tx_switch = r.opt_int::<i32>("srs-tx-switch")?;
    r.finish()?; // now rejects any stray dl-cc-id / *-feature-index as unknown properties
    // An NR unresolved selector is only ever the all-zero placeholder, which `cc_to_node` no
    // longer emits, so an empty feature list unambiguously means "reconstruct the placeholder".
    // `dl_bw_class` is always present on a real component; UL only reconstructs once
    // `ul_bw_class >= 1` (`ul_bw_class == 0` means UL disabled — no UL data at all).
    let dl_feature_per_cc_ids = dl_feature
        .is_empty()
        .then(|| reconstruct_placeholder_ids(kind, dl_bw_class))
        .transpose()?
        .flatten();
    let ul_feature_per_cc_ids = ul_feature
        .is_empty()
        .then(|| reconstruct_placeholder_ids(kind, ul_bw_class.filter(|&bw| bw >= 1)))
        .transpose()?
        .flatten();
    Ok(NrSourceSubBlock {
        kind,
        band,
        dl_bw_class,
        ul_bw_class,
        dl_feature_index,
        ul_feature_index,
        dl_feature,
        ul_feature,
        dl_feature_per_cc_ids,
        ul_feature_per_cc_ids,
        srs_tx_switch,
    })
}

fn read_combo(node: &KdlNode) -> Result<NrSourceCombo> {
    let mut r = NodeReader::new(node);
    // `power-class`/`bcs-nr`/`bcs-eutra`/`intra-band-en-dc-support` are corpus-verified
    // always `Some`: an absent property is the writer's omitted-zero (Task 8), so it
    // defaults back to `Some(0)`. `bcs-intra-endc` derives from `intra-band-en-dc-support` —
    // see below.
    let power_class = r.opt_int::<i32>("power-class")?.or(Some(0));
    let bcs_nr = r.opt_int::<u32>("bcs-nr")?.or(Some(0));
    let bcs_eutra = r.opt_int::<u32>("bcs-eutra")?.or(Some(0));
    let intra_band_en_dc_support = r.opt_int::<i32>("intra-band-en-dc-support")?.or(Some(0));
    // An absent `bcs-intra-endc` re-derives via the shared `derive_bcs_intra_endc` — the
    // inverse of the omit rule in `emit_nr_combo`. Kept AFTER `intra-band-en-dc-support`,
    // the field it depends on.
    let bcs_intra_endc = match r.opt_int::<u32>("bcs-intra-endc")? {
        Some(v) => Some(v),
        None => derive_bcs_intra_endc(intra_band_en_dc_support),
    };
    let mut selection = Vec::new();
    for snode in r.children("selection") {
        selection.push(read_selection(snode)?);
    }
    let mut sub_blocks = Vec::new();
    for cnode in r.children("nr") {
        sub_blocks.push(read_sub_block(cnode)?);
    }
    for cnode in r.children("lte") {
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
        match node.name().value() {
            "version" => {
                if version.is_some() {
                    bail!("duplicate `version`");
                }
                let mut r = NodeReader::new(node);
                version = Some(r.key_int::<u32>()?);
                r.finish()?;
            }
            "bitmask-carriers" => {
                if bitmask_carriers.is_some() {
                    bail!("duplicate `bitmask-carriers`");
                }
                bitmask_carriers = Some(read_str_list(node)?);
            }
            "bitmask-fingerprint" => bitmask_fingerprints.push(read_fingerprint(node)?),
            "carrier" => {
                let (k, v) = read_carrier(node)?;
                if carriers.insert(k.clone(), v).is_some() {
                    bail!("duplicate carrier `{k}`");
                }
            }
            "dl-feature" => dl_features.push(read_dl_feature(node)?),
            "ul-feature" => ul_features.push(read_ul_feature(node)?),
            "combo" => combo.push(read_combo(node)?),
            other => bail!("unknown top-level node `{other}` in nr.kdl"),
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

pub(crate) fn lte_to_kdl(lte: &LteDocument) -> String {
    let mut doc = KdlDocument::new();

    let mut version = KdlNode::new("version");
    version.push(KdlEntry::new(lte.version as i128));
    doc.nodes_mut().push(version);

    for (key, f) in &lte.files {
        let mut node = KdlNode::new("file");
        node.push(KdlEntry::new(key.as_str()));
        node.push(KdlEntry::new_prop("fingerprint", f.fingerprint as i128));
        node.push(KdlEntry::new_prop("bitmask", f.bitmask as i128));
        doc.nodes_mut().push(node);
    }

    for combo in &lte.combo {
        doc.nodes_mut().push(emit_lte_combo(combo));
    }

    finish_doc(doc)
}

fn read_file(node: &KdlNode) -> Result<(String, LteFileSource)> {
    let mut r = NodeReader::new(node);
    let key = r.key_str()?;
    let fingerprint = r.req_int::<u64>("fingerprint")?;
    let bitmask = r.req_int::<u64>("bitmask")?;
    r.finish()?;
    Ok((
        key,
        LteFileSource {
            fingerprint,
            bitmask,
        },
    ))
}

fn read_lte_cc(node: &KdlNode) -> Result<LteSourceComponent> {
    let mut r = NodeReader::new(node);
    let band = r.key_int::<i32>()?;
    let dl_bw_class_mimo = r.req_int::<i32>("dl-bw-class-mimo")?;
    let ul_bw_class_mimo = r.opt_int::<i32>("ul-bw-class-mimo")?;
    r.finish()?;
    Ok(LteSourceComponent {
        band,
        dl_bw_class_mimo,
        ul_bw_class_mimo,
    })
}

fn read_lte_combo(node: &KdlNode) -> Result<LteSourceCombo> {
    let mut r = NodeReader::new(node);
    let bcs = r.opt_int::<u64>("bcs")?;
    let unknown1 = r.opt_int::<u64>("unknown1")?;
    let unknown2 = r.opt_int::<u64>("unknown2")?;
    let mut selection = Vec::new();
    for snode in r.children("selection") {
        selection.push(read_selection(snode)?);
    }
    let mut components = Vec::new();
    for cnode in r.children("subblock") {
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
        match node.name().value() {
            "version" => {
                if version.is_some() {
                    bail!("duplicate `version`");
                }
                let mut r = NodeReader::new(node);
                version = Some(r.key_int::<u32>()?);
                r.finish()?;
            }
            "file" => {
                let (k, v) = read_file(node)?;
                if files.insert(k.clone(), v).is_some() {
                    bail!("duplicate file `{k}`");
                }
            }
            "combo" => combo.push(read_lte_combo(node)?),
            other => bail!("unknown top-level node `{other}` in lte.kdl"),
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
        let n = node("carrier VZW bitmask-id=1 tier=main {\n    plmns \"311-480\"\n}\n");
        let mut r = NodeReader::new(&n);
        assert_eq!(r.key_str().unwrap(), "VZW");
        assert_eq!(r.opt_int::<i64>("bitmask-id").unwrap(), Some(1));
        assert_eq!(r.opt_str("tier").unwrap().as_deref(), Some("main"));
        let plmns = r.opt_child("plmns").unwrap().unwrap();
        assert_eq!(read_str_list(plmns).unwrap(), vec!["311-480".to_string()]);
        r.finish().unwrap();
    }

    #[test]
    fn finish_rejects_unknown_property() {
        let n = node("carrier VZW bogus=9\n");
        let mut r = NodeReader::new(&n);
        assert_eq!(r.key_str().unwrap(), "VZW");
        assert!(r.finish().is_err());
    }

    #[test]
    fn finish_rejects_unknown_child() {
        let n = node("carrier VZW {\n    mystery 1\n}\n");
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
    use crate::{
        compiler::{
            features::{DlFeatureSource, NrSourceSubBlock, UlFeatureSource},
            schema::{
                BitmaskFingerprint, CarrierSource, DecimalU64, NrDocument, NrSourceCombo,
                ProfileSource,
            },
            selection::SelectionRect,
        },
        raw_nr::SubBlockKind,
    };
    use std::collections::BTreeMap;

    fn sample() -> NrDocument {
        NrDocument {
            version: 1,
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
            dl_features: vec![DlFeatureSource {
                max_scs: Some(3),
                max_mimo: Some(2),
                max_bw: Some(100),
                max_mod_order: Some(2),
                bw_90mhz_supported: Some(true),
            }],
            ul_features: vec![UlFeatureSource {
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
                sub_blocks: vec![NrSourceSubBlock {
                    kind: SubBlockKind::Nr,
                    band: 78,
                    dl_bw_class: Some(1),
                    ul_bw_class: None,
                    dl_feature_index: None,
                    ul_feature_index: None,
                    dl_feature: vec![0],
                    ul_feature: vec![],
                    dl_feature_per_cc_ids: None,
                    ul_feature_per_cc_ids: None,
                    srs_tx_switch: None,
                }],
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
        assert!(text.contains("carrier VZW bitmask-id=1"), "{text}");
        assert!(
            text.contains("profile \"66813533\" multiplier=66813533 unknown=0"),
            "{text}"
        );
        assert!(text.contains("nr 78"), "{text}");
        assert!(
            text.contains("dl-feature=0"),
            "repeated dl-feature form: {text}"
        );
        assert!(
            !text.contains("dl-cc-id") && !text.contains("dl-feature-index"),
            "removed escape-hatch surface keys must not appear: {text}"
        );
        assert!(text.contains("mapping-id=18446744073709551615"), "{text}");
    }

    #[test]
    fn nr_sub_block_repeated_features_and_lte_placeholder_ids_round_trip() {
        // Two-CC NR sub-block: repeated `dl-feature=` reads back as one usize per CC.
        // LTE sub-block: no ids at all in source — the writer's all-zero-placeholder
        // omission means the reader must derive `dl_cc_ids = [0; cc_count(Lte, 1)]`
        // (== `[0]`) purely from `dl-bw-class=1`, and re-emitting must not resurrect the
        // ids (byte-identical fixed point).
        let text = "version 1\nbitmask-carriers ATT\ncombo {\n    nr 48 dl-bw-class=2 dl-feature=5 dl-feature=8\n    lte 66 dl-bw-class=1\n}\n";
        let doc = nr_from_kdl(text).expect("parse");
        let cc = &doc.combo[0].sub_blocks;
        assert_eq!(cc[0].kind, SubBlockKind::Nr);
        assert_eq!(cc[0].dl_feature, vec![5, 8], "repeated dl-feature");
        assert!(cc[0].dl_feature_per_cc_ids.is_none());
        assert_eq!(cc[1].kind, SubBlockKind::Lte);
        assert!(cc[1].dl_feature.is_empty());
        assert_eq!(
            cc[1].dl_feature_per_cc_ids,
            Some(vec![0]),
            "LTE all-zero ids derived from cc_count(Lte, 1) despite no dl-cc-id in source"
        );
        assert_eq!(
            nr_to_kdl(&doc).unwrap(),
            text,
            "re-emitting must omit the derived LTE placeholder again (fixed point)"
        );
    }

    #[test]
    fn lte_sub_block_scalar_feature_names_and_ul_omit_when_zero_round_trip() {
        // On an `lte` node the proto-4/5 index is spelled `dl-feature`/`ul-feature` (no
        // `-index`), single-valued; `ul-feature=0` is omitted and re-defaults to `Some(0)`.
        // No per-CC list is read on LTE. Byte-identical fixed point.
        let text = "version 1\nbitmask-carriers ATT\ncombo {\n    lte 7 dl-bw-class=2 dl-feature=1 ul-bw-class=1 ul-feature=2\n    lte 66 dl-bw-class=1 dl-feature=3\n}\n";
        let doc = nr_from_kdl(text).expect("parse");
        let cc = &doc.combo[0].sub_blocks;
        assert_eq!(cc[0].kind, SubBlockKind::Lte);
        assert_eq!(cc[0].dl_feature_index, Some(1));
        assert_eq!(cc[0].ul_feature_index, Some(2));
        assert!(
            cc[0].dl_feature.is_empty() && cc[0].ul_feature.is_empty(),
            "LTE carries no per-CC feature list"
        );
        assert_eq!(cc[1].dl_feature_index, Some(3));
        assert_eq!(
            cc[1].ul_feature_index,
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
        assert!(text.contains("plmn mcc=311 mnc=480"), "{text}");
        assert!(text.contains("plmn mcc=310 mnc=4 mnc-digits=3"), "{text}");
        assert!(
            text.contains("plmn mcc=228\n") || text.contains("plmn mcc=228 "),
            "{text}"
        );
        assert!(
            !text.contains("plmns "),
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
        let text = "version 1\nbitmask-carriers \"LEGACY\"\ncarrier \"MAP\" mapping-id=7 {\n    plmns \"310-260\"\n}\n";
        let err = format!("{:#}", nr_from_kdl(text).unwrap_err());
        assert!(err.contains("plmns"), "{err}");
    }

    #[test]
    fn nr_rejects_unknown_property() {
        let text = nr_to_kdl(&sample())
            .unwrap()
            .replace("nr 78", "nr 78 bogus=9");
        assert!(nr_from_kdl(&text).is_err());
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
            let text = format!(
                "version 1\nbitmask-carriers ATT\ncombo {{\n    nr 78 dl-bw-class=1 {key}\n}}\n"
            );
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
        let text = format!("{}\ncarrier VZW\n", nr_to_kdl(&sample()).unwrap());
        assert!(nr_from_kdl(&text).is_err());
    }

    #[test]
    fn nr_rejects_unknown_cc_kind() {
        let text = nr_to_kdl(&sample()).unwrap().replace("nr 78", "bogus 78");
        assert!(nr_from_kdl(&text).is_err());
    }

    #[test]
    fn nr_rejects_unknown_tier() {
        let text = nr_to_kdl(&sample())
            .unwrap()
            .replace("tier=main", "tier=bogus");
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
            .replace("profile \"66813533\"", "profile 66813533");
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
        let doc: KdlDocument = text.parse().expect("combo KDL parses");
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
        assert!(
            text.contains("bcs-intra-endc=0"),
            "exception zero explicit: {text}"
        );
        let back = read_combo(&parse_combo(&text)).unwrap();
        assert_eq!(back.bcs_intra_endc, Some(0));
        assert_eq!(back.intra_band_en_dc_support, Some(0));
    }

    #[test]
    fn bcs_intra_endc_nonzero_stays_explicit() {
        let node = emit_nr_combo(&combo_with(Some(7), Some(1))).unwrap();
        let text = node.to_string();
        assert!(text.contains("bcs-intra-endc=7"), "{text}");
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
        let text =
            "version 1\nbitmask-carriers ATT\ncombo {\n    nr 78 dl-bw-class=1 ul-bw-class=1\n}\n";
        let doc = nr_from_kdl(text).expect("parse new spelling");
        assert_eq!(
            nr_to_kdl(&doc).unwrap(),
            text,
            "new spelling is a fixed point"
        );

        // The old suffix spelling is now an unknown property (strict reader, no alias).
        let old =
            "version 1\nbitmask-carriers ATT\ncombo {\n    nr 78 bw-class-dl=1 bw-class-ul=1\n}\n";
        let err = nr_from_kdl(old).unwrap_err().to_string();
        assert!(
            err.contains("unknown property") && err.contains("bw-class-dl"),
            "old spelling must be rejected, got: {err}"
        );
    }

    #[test]
    fn nr_emits_direction_grouped_order() {
        let text = "version 1\nbitmask-carriers ATT\ncombo {\n    nr 78 dl-bw-class=1 dl-feature=2 ul-bw-class=1 ul-feature=3\n}\n";
        let doc = nr_from_kdl(text).expect("parse");
        let out = nr_to_kdl(&doc).unwrap();
        let dl = out.find("dl-feature=2").unwrap();
        let ub = out.find("ul-bw-class=1").unwrap();
        let uf = out.find("ul-feature=3").unwrap();
        assert!(
            dl < ub && ub < uf,
            "expected dl-feature < ul-bw-class < ul-feature:\n{out}"
        );
    }
}

#[cfg(test)]
mod lte_tests {
    use super::*;
    use crate::compiler::{
        schema::{LteDocument, LteFileSource, LteSourceCombo, LteSourceComponent},
        selection::SelectionRect,
    };
    use std::collections::BTreeMap;

    fn sample() -> LteDocument {
        LteDocument {
            version: 1,
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
                    LteSourceComponent {
                        band: 1,
                        dl_bw_class_mimo: 1,
                        ul_bw_class_mimo: Some(1),
                    },
                    LteSourceComponent {
                        band: 3,
                        dl_bw_class_mimo: 1,
                        ul_bw_class_mimo: None,
                    },
                ],
            }],
        }
    }

    #[test]
    fn lte_round_trips_byte_identically() {
        let doc = sample();
        let text = lte_to_kdl(&doc);
        let back = lte_from_kdl(&text).expect("read back");
        assert_eq!(lte_to_kdl(&back), text, "byte-identity");
        assert!(
            text.contains("file \"3\" fingerprint=715188856 bitmask=1"),
            "{text}"
        );
        assert!(
            text.contains("subblock 1 dl-bw-class-mimo=1 ul-bw-class-mimo=1"),
            "{text}"
        );
        assert!(text.contains("subblock 3 dl-bw-class-mimo=1\n"), "{text}");
    }

    #[test]
    fn lte_rejects_unknown_property() {
        let text = lte_to_kdl(&sample()).replace("bcs=2", "bcs=2 bogus=9");
        assert!(lte_from_kdl(&text).is_err());
    }

    #[test]
    fn lte_rejects_the_old_mimo_bw_class_spelling() {
        // Companion to `bw_class_is_direction_first_and_old_spelling_rejected`, which
        // covers only the NR combo pair. The lte.kdl mimo pair shares the same strict
        // `finish()` and must reject the pre-rename suffix spelling just as firmly.
        for (new, old) in [
            ("dl-bw-class-mimo=", "bw-class-mimo-dl="),
            ("ul-bw-class-mimo=", "bw-class-mimo-ul="),
        ] {
            // Additive: keep the required new-spelling property and append the old one, so
            // the reader reports the unknown property rather than a missing required one.
            let text = lte_to_kdl(&sample()).replace(new, &format!("{old}1 {new}"));
            let err = lte_from_kdl(&text).unwrap_err().to_string();
            assert!(
                err.contains("unknown property") && err.contains(old.trim_end_matches('=')),
                "{old} must be rejected, got: {err}"
            );
        }
    }

    #[test]
    fn lte_rejects_duplicate_file() {
        let text = format!(
            "{}\nfile \"3\" fingerprint=1 bitmask=1\n",
            lte_to_kdl(&sample())
        );
        assert!(lte_from_kdl(&text).is_err());
    }

    #[test]
    fn lte_rejects_missing_version() {
        let text = lte_to_kdl(&sample()).replacen("version 1\n", "", 1);
        let err = format!("{:#}", lte_from_kdl(&text).unwrap_err());
        assert!(err.contains("missing `version`"), "{err}");
    }

    #[test]
    fn lte_rejects_bare_numeric_file_key() {
        // The LTE file id is a map key: a quoted string arg, not a bare integer.
        let text = lte_to_kdl(&sample()).replace("file \"3\"", "file 3");
        assert!(lte_from_kdl(&text).is_err());
    }
}
