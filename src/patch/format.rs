//! The KDL combo-patch format: a `kind`-tagged ADT over the carrier (`nr`) and LTE patches.

use crate::{
    kdl_support::{
        NodeReader, cckind_to_str, finish_doc, opt_bool_prop, opt_int_prop, str_to_cckind,
    },
    proto::{LteCombo, LteComponent, ShannonFeatureSetDlPerCcNr, ShannonFeatureSetUlPerCcNr},
    report::{
        combos::{Combo, NR_BAND_OFFSET, SubBlock, combo_key},
        lte::lte_combo_key,
    },
};
use anyhow::Context;
use kdl::{KdlDocument, KdlEntry, KdlNode};
use std::collections::BTreeSet;

pub(crate) use crate::raw_nr::{RawSubBlock as PatchSubBlock, SubBlockKind};

/// Patch discriminator, the top-level `kind nr`/`kind lte` node.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Kind {
    Nr,
    Lte,
}

impl Kind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Nr => "nr",
            Self::Lte => "lte",
        }
    }

    fn parse(s: &str) -> anyhow::Result<Self> {
        match s {
            "nr" => Ok(Self::Nr),
            "lte" => Ok(Self::Lte),
            other => anyhow::bail!("unknown patch kind `{other}` (expected `nr` or `lte`)"),
        }
    }
}

/// A combo patch — one of the two formats, discriminated by `kind`.
#[derive(Debug)]
pub(crate) enum Patch {
    Nr(NrPatch),
    Lte(LtePatch),
}

/// A combo patch document: delete the listed keys, set the listed keys to full
/// definitions. Generic over the set-entry type — `SetEntry` (NR) or `LteSetEntry`
/// (LTE); the `kind` field discriminates the two.
#[derive(Debug)]
pub(crate) struct PatchDoc<E> {
    pub(crate) kind: Kind,
    pub(crate) version: u32,
    pub(crate) delete: Vec<String>,
    pub(crate) set: Vec<E>,
}

/// A set-entry's add/change marker — the KDL node name (`add { … }` / `change { … }`),
/// tightened from the old `Option<String>` since production always emits one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SetKind {
    Add,
    Change,
}

impl SetKind {
    pub(crate) fn node_name(self) -> &'static str {
        match self {
            Self::Add => "add",
            Self::Change => "change",
        }
    }

    pub(crate) fn parse(s: &str) -> anyhow::Result<Self> {
        match s {
            "add" => Ok(Self::Add),
            "change" => Ok(Self::Change),
            other => anyhow::bail!("unknown set-entry kind `{other}`"),
        }
    }
}

/// One "set this derived key to these combos" operation. Generic over the combo
/// type — `PatchCombo` (NR) or `LtePatchCombo` (LTE).
#[derive(Debug)]
pub(crate) struct Entry<C> {
    pub(crate) kind: SetKind,
    pub(crate) combo: Vec<C>,
}

/// Carrier/NR patch (`kind = nr`) and its set entry.
pub(crate) type NrPatch = PatchDoc<SetEntry>;
pub(crate) type SetEntry = Entry<PatchCombo>;

const FORMAT_VERSION: u32 = 1;

impl Patch {
    /// The patch's format version, regardless of kind.
    const fn version(&self) -> u32 {
        match self {
            Self::Nr(p) => p.version,
            Self::Lte(p) => p.version,
        }
    }
}

// ---------------------------------------------------------------------------
// Writer
// ---------------------------------------------------------------------------

/// Serialize a patch to KDL text (the variant struct carries the `kind` field).
pub(crate) fn to_kdl(patch: &Patch) -> anyhow::Result<String> {
    let mut doc = KdlDocument::new();

    let (kind, version, delete): (Kind, u32, &[String]) = match patch {
        Patch::Nr(p) => (p.kind, p.version, &p.delete),
        Patch::Lte(p) => (p.kind, p.version, &p.delete),
    };

    let mut kind_node = KdlNode::new("kind");
    kind_node.push(KdlEntry::new(kind.as_str()));
    doc.nodes_mut().push(kind_node);

    let mut version_node = KdlNode::new("version");
    version_node.push(KdlEntry::new(i128::from(version)));
    doc.nodes_mut().push(version_node);

    for key in delete {
        let mut node = KdlNode::new("delete");
        node.push(KdlEntry::new(key.as_str()));
        doc.nodes_mut().push(node);
    }

    match patch {
        Patch::Nr(p) => {
            for e in &p.set {
                doc.nodes_mut().push(nr_entry_to_node(e));
            }
        }
        Patch::Lte(p) => {
            for e in &p.set {
                doc.nodes_mut().push(lte_entry_to_node(e));
            }
        }
    }
    Ok(finish_doc(doc))
}

fn nr_entry_to_node(e: &SetEntry) -> KdlNode {
    let mut node = KdlNode::new(e.kind.node_name());
    if !e.combo.is_empty() {
        let kids = node.ensure_children();
        for combo in &e.combo {
            kids.nodes_mut().push(nr_combo_to_node(combo));
        }
    }
    node
}

fn nr_combo_to_node(combo: &PatchCombo) -> KdlNode {
    let mut node = KdlNode::new("combo");
    node.push(KdlEntry::new_prop("bit-mask", i128::from(combo.bit_mask)));
    if combo.group != 0 {
        node.push(KdlEntry::new_prop("group", combo.group as i128));
    }
    if combo.index != 0 {
        node.push(KdlEntry::new_prop("index", combo.index as i128));
    }
    // `power-class`/`bcs-nr`/`bcs-eutra`/`intra-band-en-dc-support` are corpus-verified
    // always `Some` on a real combo header (never `None`), so `Some(0)` is omitted here and
    // re-defaulted to `Some(0)` by `read_nr_combo` below (Task 8 omit-when-0). `bcs-intra-endc`
    // has genuine `None` in the corpus and keeps the plain `opt_int_prop`/`opt_int` path.
    opt_int_prop(
        &mut node,
        "power-class",
        combo.power_class.filter(|&v| v != 0).map(i128::from),
    );
    opt_int_prop(
        &mut node,
        "bcs-nr",
        combo.bcs_nr.filter(|&v| v != 0).map(i128::from),
    );
    opt_int_prop(
        &mut node,
        "bcs-intra-endc",
        combo.bcs_intra_endc.map(i128::from),
    );
    opt_int_prop(
        &mut node,
        "bcs-eutra",
        combo.bcs_eutra.filter(|&v| v != 0).map(i128::from),
    );
    opt_int_prop(
        &mut node,
        "intra-band-en-dc-support",
        combo
            .intra_band_en_dc_support
            .filter(|&v| v != 0)
            .map(i128::from),
    );
    if !combo.sub_blocks.is_empty() {
        let kids = node.ensure_children();
        for cc in &combo.sub_blocks {
            kids.nodes_mut().push(sub_block_to_node(cc));
        }
    }
    node
}

fn sub_block_to_node(cc: &PatchSubBlock) -> KdlNode {
    let mut node = KdlNode::new(cckind_to_str(cc.kind));
    // Leading positional `band` (mirrors compiler `cc_to_node`).
    node.push(KdlEntry::new(i128::from(cc.band)));
    opt_int_prop(&mut node, "dl-bw-class", cc.dl_bw_class.map(i128::from));
    // Per-kind spelling of the proto-4/5 index — mirrors `compiler::kdl_source::cc_to_node`.
    // `lte` uses scalar `dl-feature`/`ul-feature` (patch NR features are `dl-cc`/`ul-cc`
    // children, so these names are free); `ul-feature=0` is omitted (LTE always-`Some`, reader
    // re-defaults). `nr` no longer surfaces the index — NR derives it on build; the
    // `dl-feature-index`/`ul-feature-index` source override was dropped (proto field kept).
    // Emitted direction-grouped (mirrors `cc_to_node`): `dl-bw-class`, `dl-feature`,
    // `ul-bw-class`, `ul-feature`, so DL and UL each read as a contiguous group.
    if cc.kind == SubBlockKind::Lte {
        opt_int_prop(&mut node, "dl-feature", cc.dl_feature_index.map(i128::from));
    }
    // Corpus-verified always-`Some` (Task 8 omit-when-0) — see `cc_to_node`'s counterpart in
    // `compiler::kdl_source`.
    opt_int_prop(
        &mut node,
        "ul-bw-class",
        cc.ul_bw_class.filter(|&v| v != 0).map(i128::from),
    );
    if cc.kind == SubBlockKind::Lte {
        opt_int_prop(
            &mut node,
            "ul-feature",
            cc.ul_feature_index.filter(|&v| v != 0).map(i128::from),
        );
    }
    opt_int_prop(&mut node, "srs-tx-switch", cc.srs_tx_switch.map(i128::from));
    // Per-CC feature child nodes are the only per-CC representation now (the raw `dl-cc-id`/
    // `ul-cc-id` selector fallback was dropped): a resolved feature set emits `dl-cc`/`ul-cc`
    // children, an unresolved one emits nothing.
    if !cc.dl_features.is_empty() || !cc.ul_features.is_empty() {
        let kids = node.ensure_children();
        for f in &cc.dl_features {
            kids.nodes_mut().push(dl_cc_to_node(f));
        }
        for f in &cc.ul_features {
            kids.nodes_mut().push(ul_cc_to_node(f));
        }
    }
    node
}

/// One DL per-CC feature-set child node (`dl-cc max-scs=… …`).
fn dl_cc_to_node(f: &ShannonFeatureSetDlPerCcNr) -> KdlNode {
    let mut node = KdlNode::new("dl-cc");
    opt_int_prop(&mut node, "max-scs", f.max_scs.map(i128::from));
    opt_int_prop(&mut node, "max-mimo", f.max_mimo.map(i128::from));
    opt_int_prop(&mut node, "max-bw", f.max_bw.map(i128::from));
    opt_int_prop(&mut node, "max-mod-order", f.max_mod_order.map(i128::from));
    opt_bool_prop(&mut node, "bw-90mhz-supported", f.bw_90mhz_supported);
    node
}

/// One UL per-CC feature-set child node (`ul-cc max-scs=… …`).
fn ul_cc_to_node(f: &ShannonFeatureSetUlPerCcNr) -> KdlNode {
    let mut node = KdlNode::new("ul-cc");
    opt_int_prop(&mut node, "max-scs", f.max_scs.map(i128::from));
    opt_int_prop(&mut node, "max-mimo-cb", f.max_mimo_cb.map(i128::from));
    opt_int_prop(&mut node, "max-bw", f.max_bw.map(i128::from));
    opt_int_prop(&mut node, "max-mod-order", f.max_mod_order.map(i128::from));
    opt_int_prop(
        &mut node,
        "max-mimo-non-cb",
        f.max_mimo_non_cb.map(i128::from),
    );
    opt_bool_prop(&mut node, "bw-90mhz-supported", f.bw_90mhz_supported);
    node
}

fn lte_entry_to_node(e: &LteSetEntry) -> KdlNode {
    let mut node = KdlNode::new(e.kind.node_name());
    if !e.combo.is_empty() {
        let kids = node.ensure_children();
        for combo in &e.combo {
            kids.nodes_mut().push(lte_combo_to_node(combo));
        }
    }
    node
}

fn lte_combo_to_node(combo: &LtePatchCombo) -> KdlNode {
    let mut node = KdlNode::new("combo");
    node.push(KdlEntry::new_prop("bcs", i128::from(combo.bcs)));
    node.push(KdlEntry::new_prop("unknown1", i128::from(combo.unknown1)));
    node.push(KdlEntry::new_prop("unknown2", i128::from(combo.unknown2)));
    if !combo.components.is_empty() {
        let kids = node.ensure_children();
        for comp in &combo.components {
            kids.nodes_mut().push(lte_cc_to_node(comp));
        }
    }
    node
}

fn lte_cc_to_node(comp: &LtePatchComponent) -> KdlNode {
    let mut node = KdlNode::new("subblock");
    node.push(KdlEntry::new(i128::from(comp.band)));
    node.push(KdlEntry::new_prop(
        "dl-bw-class-mimo",
        i128::from(comp.dl_bw_class_mimo),
    ));
    node.push(KdlEntry::new_prop(
        "ul-bw-class-mimo",
        i128::from(comp.ul_bw_class_mimo),
    ));
    node
}

// ---------------------------------------------------------------------------
// Reader
// ---------------------------------------------------------------------------

/// Parse a patch: peek `kind`, parse the matching variant, reject an unrecognized `version`,
/// and validate each set entry's derived key.
pub(crate) fn from_kdl(text: &str) -> anyhow::Result<Patch> {
    let doc: KdlDocument = text.parse().context("patch is not valid KDL")?;
    let kind = peek_kind(&doc)?;
    let patch = match kind {
        Kind::Nr => Patch::Nr(read_nr_patch_doc(&doc, kind)?),
        Kind::Lte => Patch::Lte(read_lte_patch_doc(&doc, kind)?),
    };
    let version = patch.version();
    if version != FORMAT_VERSION {
        anyhow::bail!(
            "unsupported patch version {version} (this build understands version {FORMAT_VERSION})"
        );
    }
    validate_patch(&patch)?;
    Ok(patch)
}

/// Read the top-level `kind` node without consuming the rest of the document, so the
/// caller can pick which variant's set-entry/combo shape to parse the rest as.
fn peek_kind(doc: &KdlDocument) -> anyhow::Result<Kind> {
    let node = doc
        .nodes()
        .iter()
        .find(|n| n.name().value() == "kind")
        .ok_or_else(|| anyhow::anyhow!("patch missing `kind`"))?;
    let mut r = NodeReader::new(node);
    let kind = Kind::parse(&r.key_str()?)?;
    r.finish()?;
    Ok(kind)
}

fn read_delete_key(node: &KdlNode) -> anyhow::Result<String> {
    let mut r = NodeReader::new(node);
    let key = r.key_str()?;
    r.finish()?;
    Ok(key)
}

fn read_nr_patch_doc(doc: &KdlDocument, kind: Kind) -> anyhow::Result<NrPatch> {
    let mut version = None;
    let mut kind_seen = false;
    let mut delete = Vec::new();
    let mut set = Vec::new();
    for node in doc.nodes() {
        match node.name().value() {
            "kind" => {
                if kind_seen {
                    anyhow::bail!("duplicate `kind`");
                }
                kind_seen = true;
            }
            "version" => {
                if version.is_some() {
                    anyhow::bail!("duplicate `version`");
                }
                let mut r = NodeReader::new(node);
                version = Some(r.key_int::<u32>()?);
                r.finish()?;
            }
            "delete" => delete.push(read_delete_key(node)?),
            "add" | "change" => set.push(read_nr_entry(node)?),
            other => anyhow::bail!("unknown top-level node `{other}` in nr patch"),
        }
    }
    Ok(NrPatch {
        kind,
        version: version.ok_or_else(|| anyhow::anyhow!("nr patch missing `version`"))?,
        delete,
        set,
    })
}

fn read_lte_patch_doc(doc: &KdlDocument, kind: Kind) -> anyhow::Result<LtePatch> {
    let mut version = None;
    let mut kind_seen = false;
    let mut delete = Vec::new();
    let mut set = Vec::new();
    for node in doc.nodes() {
        match node.name().value() {
            "kind" => {
                if kind_seen {
                    anyhow::bail!("duplicate `kind`");
                }
                kind_seen = true;
            }
            "version" => {
                if version.is_some() {
                    anyhow::bail!("duplicate `version`");
                }
                let mut r = NodeReader::new(node);
                version = Some(r.key_int::<u32>()?);
                r.finish()?;
            }
            "delete" => delete.push(read_delete_key(node)?),
            "add" | "change" => set.push(read_lte_entry(node)?),
            other => anyhow::bail!("unknown top-level node `{other}` in lte patch"),
        }
    }
    Ok(LtePatch {
        kind,
        version: version.ok_or_else(|| anyhow::anyhow!("lte patch missing `version`"))?,
        delete,
        set,
    })
}

fn read_nr_entry(node: &KdlNode) -> anyhow::Result<SetEntry> {
    let kind = SetKind::parse(node.name().value())?;
    let mut r = NodeReader::new(node);
    let combo = r
        .children("combo")
        .into_iter()
        .map(read_nr_combo)
        .collect::<anyhow::Result<Vec<_>>>()?;
    r.finish()?;
    Ok(SetEntry { kind, combo })
}

fn read_nr_combo(node: &KdlNode) -> anyhow::Result<PatchCombo> {
    let mut r = NodeReader::new(node);
    let bit_mask = r.req_int::<u32>("bit-mask")?;
    let group = r.opt_int::<usize>("group")?.unwrap_or(0);
    let index = r.opt_int::<usize>("index")?.unwrap_or(0);
    // Corpus-verified always-`Some` (Task 8 omit-when-0) — see `read_combo`'s counterpart
    // in `compiler::kdl_source`. `bcs-intra-endc` has genuine `None` in the corpus and
    // stays a plain `opt_int` here — note the compiler counterpart now *derives* it from
    // `intra-band-en-dc-support` (omit-when-0-iff-intra-band-EN-DC); the patch deliberately
    // keeps it explicit (single-file editing surface — see DESIGN.md).
    let power_class = r.opt_int::<i32>("power-class")?.or(Some(0));
    let bcs_nr = r.opt_int::<u32>("bcs-nr")?.or(Some(0));
    let bcs_intra_endc = r.opt_int::<u32>("bcs-intra-endc")?;
    let bcs_eutra = r.opt_int::<u32>("bcs-eutra")?.or(Some(0));
    let intra_band_en_dc_support = r.opt_int::<i32>("intra-band-en-dc-support")?.or(Some(0));
    // `nr`/`lte` children can interleave within one EN-DC combo (e.g. `lte 66 …`
    // then `nr 77 …`). Register both names as consumed for `finish()`'s
    // unknown-child check, but walk the raw child list to build `sub_blocks` in
    // DOCUMENT order — `NodeReader::children` groups same-named siblings together,
    // which would silently reorder a mixed nr/lte combo (and its derived key with it).
    r.children("nr");
    r.children("lte");
    let mut sub_blocks = Vec::new();
    if let Some(kids) = node.children() {
        for child in kids.nodes() {
            if matches!(child.name().value(), "nr" | "lte") {
                sub_blocks.push(read_sub_block(child)?);
            }
        }
    }
    r.finish()?;
    Ok(PatchCombo {
        group,
        index,
        power_class,
        bcs_nr,
        bcs_intra_endc,
        bcs_eutra,
        intra_band_en_dc_support,
        bit_mask,
        sub_blocks,
    })
}

fn read_sub_block(node: &KdlNode) -> anyhow::Result<PatchSubBlock> {
    let kind = str_to_cckind(node.name().value(), "NR/EN-DC component kind")?;
    let mut r = NodeReader::new(node);
    let band = r.key_int::<i32>()?;
    let dl_bw_class = r.opt_int::<i32>("dl-bw-class")?;
    // Corpus-verified always-`Some` (Task 8 omit-when-0) — see `read_sub_block`'s
    // counterpart in `compiler::kdl_source`.
    let ul_bw_class = r.opt_int::<i32>("ul-bw-class")?.or(Some(0));
    // Kind-aware inverse of `sub_block_to_node`'s index emit. `lte`: scalar `dl-feature`/
    // `ul-feature`, absent `ul-feature` re-defaults to `Some(0)` (LTE-only). `nr` no longer
    // surfaces the index (derived on build); a stray `dl-feature`/`ul-feature`/`*-feature-index`
    // prop stays unconsumed and `finish()` rejects it (patch NR features are child nodes).
    let (dl_feature_index, ul_feature_index) = match kind {
        SubBlockKind::Lte => (
            r.opt_int::<i32>("dl-feature")?,
            r.opt_int::<i32>("ul-feature")?.or(Some(0)),
        ),
        _ => (None, None),
    };
    let srs_tx_switch = r.opt_int::<i32>("srs-tx-switch")?;
    // `dl-cc`/`ul-cc` child nodes are the only per-CC representation (the raw `dl-cc-id`/
    // `ul-cc-id` selector fallback was dropped — `finish()` now rejects those keys). Unlike
    // the compiler reader, the patch never reconstructs an omitted placeholder: no children
    // means `dl_cc_ids`/`ul_cc_ids` stay `None`.
    let dl_cc_nodes = r.children("dl-cc");
    let ul_cc_nodes = r.children("ul-cc");
    r.finish()?;

    let dl_features = dl_cc_nodes
        .into_iter()
        .map(read_dl_cc)
        .collect::<anyhow::Result<Vec<_>>>()?;
    let ul_features = ul_cc_nodes
        .into_iter()
        .map(read_ul_cc)
        .collect::<anyhow::Result<Vec<_>>>()?;

    Ok(PatchSubBlock {
        kind,
        band,
        dl_bw_class,
        ul_bw_class,
        dl_feature_index,
        ul_feature_index,
        dl_cc_ids: None,
        ul_cc_ids: None,
        srs_tx_switch,
        dl_features,
        ul_features,
    })
}

/// One `dl-cc` child node — a single CC's resolved DL feature set. `finish()` rejects any
/// stray property, the strictness equivalent applied at every nesting level.
fn read_dl_cc(node: &KdlNode) -> anyhow::Result<ShannonFeatureSetDlPerCcNr> {
    let mut r = NodeReader::new(node);
    let max_scs = r.opt_int::<i32>("max-scs")?;
    let max_mimo = r.opt_int::<i32>("max-mimo")?;
    let max_bw = r.opt_int::<i32>("max-bw")?;
    let max_mod_order = r.opt_int::<i32>("max-mod-order")?;
    let bw_90mhz_supported = r.opt_bool("bw-90mhz-supported")?;
    r.finish()?;
    Ok(ShannonFeatureSetDlPerCcNr {
        max_scs,
        max_mimo,
        max_bw,
        max_mod_order,
        bw_90mhz_supported,
    })
}

/// One `ul-cc` child node — a single CC's resolved UL feature set. See [`read_dl_cc`].
fn read_ul_cc(node: &KdlNode) -> anyhow::Result<ShannonFeatureSetUlPerCcNr> {
    let mut r = NodeReader::new(node);
    let max_scs = r.opt_int::<i32>("max-scs")?;
    let max_mimo_cb = r.opt_int::<i32>("max-mimo-cb")?;
    let max_bw = r.opt_int::<i32>("max-bw")?;
    let max_mod_order = r.opt_int::<i32>("max-mod-order")?;
    let max_mimo_non_cb = r.opt_int::<i32>("max-mimo-non-cb")?;
    let bw_90mhz_supported = r.opt_bool("bw-90mhz-supported")?;
    r.finish()?;
    Ok(ShannonFeatureSetUlPerCcNr {
        max_scs,
        max_mimo_cb,
        max_bw,
        max_mod_order,
        bw_90mhz_supported,
        max_mimo_non_cb,
    })
}

fn read_lte_entry(node: &KdlNode) -> anyhow::Result<LteSetEntry> {
    let kind = SetKind::parse(node.name().value())?;
    let mut r = NodeReader::new(node);
    let combo = r
        .children("combo")
        .into_iter()
        .map(read_lte_combo)
        .collect::<anyhow::Result<Vec<_>>>()?;
    r.finish()?;
    Ok(LteSetEntry { kind, combo })
}

fn read_lte_combo(node: &KdlNode) -> anyhow::Result<LtePatchCombo> {
    let mut r = NodeReader::new(node);
    let bcs = r.req_int::<u64>("bcs")?;
    let unknown1 = r.req_int::<u64>("unknown1")?;
    let unknown2 = r.req_int::<u64>("unknown2")?;
    let components = r
        .children("subblock")
        .into_iter()
        .map(read_lte_cc)
        .collect::<anyhow::Result<Vec<_>>>()?;
    r.finish()?;
    Ok(LtePatchCombo {
        components,
        bcs,
        unknown1,
        unknown2,
    })
}

fn read_lte_cc(node: &KdlNode) -> anyhow::Result<LtePatchComponent> {
    let mut r = NodeReader::new(node);
    let band = r.key_int::<i32>()?;
    let dl_bw_class_mimo = r.req_int::<i32>("dl-bw-class-mimo")?;
    let ul_bw_class_mimo = r.req_int::<i32>("ul-bw-class-mimo")?;
    r.finish()?;
    Ok(LtePatchComponent {
        band,
        dl_bw_class_mimo,
        ul_bw_class_mimo,
    })
}

pub(crate) fn validate_patch(patch: &Patch) -> anyhow::Result<()> {
    match patch {
        Patch::Nr(p) => {
            let mut seen_delete = BTreeSet::new();
            for k in &p.delete {
                if !seen_delete.insert(k.clone()) {
                    anyhow::bail!("duplicate delete key {k:?}");
                }
            }
            let mut seen = BTreeSet::new();
            for entry in &p.set {
                for combo in &entry.combo {
                    for cc in &combo.sub_blocks {
                        validate_component_band(cc.kind, cc.band)?;
                    }
                }
                let key = set_entry_key(entry)?;
                if !seen.insert(key.clone()) {
                    anyhow::bail!("duplicate set entry for key {key:?}");
                }
            }
        }
        Patch::Lte(p) => {
            let mut seen_delete = BTreeSet::new();
            for k in &p.delete {
                if !seen_delete.insert(k.clone()) {
                    anyhow::bail!("duplicate delete key {k:?}");
                }
            }
            let mut seen = BTreeSet::new();
            for entry in &p.set {
                for combo in &entry.combo {
                    for comp in &combo.components {
                        validate_component_band(SubBlockKind::Lte, comp.band)?;
                    }
                }
                let key = lte_set_entry_key(entry)?;
                if !seen.insert(key.clone()) {
                    anyhow::bail!("duplicate set entry for key {key:?}");
                }
            }
        }
    }
    Ok(())
}

/// Patch components always store the plain band number (E-UTRA `1..NR_BAND_OFFSET`,
/// or an NR plain band `< NR_BAND_OFFSET` per 3GPP TS 38.104 — never the raw protobuf
/// `NR_BAND_OFFSET + n` encoding). Reject a value that is not positive or that would
/// wrap when compared against a model's `u16` band set — e.g. 65602 wrapping to 66
/// (R10). This is the explicit, uniform guard for both kinds; `RawSubBlock::validate`
/// enforces the same range later via `set_entry_key` for NR, but running it first
/// here yields a clearer error and covers LTE symmetrically.
fn validate_component_band(kind: SubBlockKind, band: i32) -> anyhow::Result<()> {
    let what = if matches!(kind, SubBlockKind::Nr) {
        "NR"
    } else {
        "LTE"
    };
    anyhow::ensure!(
        (1..NR_BAND_OFFSET).contains(&band),
        "{what} component band {band} is out of range (expected 1..{NR_BAND_OFFSET})"
    );
    Ok(())
}

fn derived_entry_key<T>(
    combos: &[T],
    key_of: impl Fn(&T) -> anyhow::Result<String>,
) -> anyhow::Result<String> {
    let mut iter = combos.iter();
    let first = iter.next().context("set entry has no combo variants")?;
    let key = key_of(first)?;
    if key.is_empty() {
        anyhow::bail!("set entry derives an empty key");
    }
    for combo in iter {
        let other = key_of(combo)?;
        if other != key {
            anyhow::bail!("set entry mixes derived keys {key:?} and {other:?}");
        }
    }
    Ok(key)
}

/// The derived key for one NR/carrier set entry.
pub(crate) fn set_entry_key(entry: &SetEntry) -> anyhow::Result<String> {
    derived_entry_key(&entry.combo, |combo| Ok(combo_key(&combo.to_combo()?)))
}

/// Convert one NR/carrier set entry's combos into the internal raw-band model.
pub(crate) fn set_entry_combos(entry: &SetEntry) -> anyhow::Result<Vec<Combo>> {
    entry.combo.iter().map(PatchCombo::to_combo).collect()
}

/// Convert one serialized LTE patch combo into the proto shape used by the LTE
/// combo-key renderer and patch applier.
///
/// The optional fields rehydrate as `Some(value)` per the documented write convention, so a
/// transplanted combo whose source had an *absent* field materializes an explicit zero. That
/// is intentional and value-preserving (a `Some(0)` reads the same as `None`); see the
/// `patch::lte::CanonLteCombo` note and DESIGN.md "Invariants".
pub(crate) fn lte_combo_from_patch(p: &LtePatchCombo) -> LteCombo {
    LteCombo {
        components: p
            .components
            .iter()
            .map(|x| LteComponent {
                band: x.band,
                dl_bw_class_mimo: x.dl_bw_class_mimo,
                ul_bw_class_mimo: Some(x.ul_bw_class_mimo),
            })
            .collect(),
        bcs: Some(p.bcs),
        unknown1: Some(p.unknown1),
        unknown2: Some(p.unknown2),
    }
}

/// The derived key for one LTE set entry.
pub(crate) fn lte_set_entry_key(entry: &LteSetEntry) -> anyhow::Result<String> {
    derived_entry_key(&entry.combo, |combo| {
        Ok(lte_combo_key(&lte_combo_from_patch(combo)))
    })
}

/// One variant under a set entry in an NR/carrier patch.
#[derive(Clone, Default, Debug)]
pub(crate) struct PatchCombo {
    pub(crate) group: usize,
    pub(crate) index: usize,
    pub(crate) power_class: Option<i32>,
    pub(crate) bcs_nr: Option<u32>,
    pub(crate) bcs_intra_endc: Option<u32>,
    pub(crate) bcs_eutra: Option<u32>,
    pub(crate) intra_band_en_dc_support: Option<i32>,
    pub(crate) bit_mask: u32,
    pub(crate) sub_blocks: Vec<PatchSubBlock>,
}

impl PatchCombo {
    pub(crate) fn from_combo(combo: &Combo) -> Self {
        Self {
            group: combo.group,
            index: combo.index,
            power_class: combo.power_class,
            bcs_nr: combo.bcs_nr,
            bcs_intra_endc: combo.bcs_intra_endc,
            bcs_eutra: combo.bcs_eutra,
            intra_band_en_dc_support: combo.intra_band_en_dc_support,
            bit_mask: combo.bit_mask,
            sub_blocks: combo
                .sub_blocks
                .iter()
                .map(|cc| {
                    let mut pc = PatchSubBlock::from_sub_block(cc);
                    pc.dl_feature_index = pc.source_dl_feature_index();
                    pc.ul_feature_index = pc.source_ul_feature_index();
                    pc
                })
                .collect(),
        }
    }

    pub(crate) fn to_combo(&self) -> anyhow::Result<Combo> {
        let sub_blocks: Vec<SubBlock> = self
            .sub_blocks
            .iter()
            .map(PatchSubBlock::to_sub_block)
            .collect::<anyhow::Result<_>>()?;
        Ok(Combo {
            group: self.group,
            index: self.index,
            bands: self
                .sub_blocks
                .iter()
                .map(PatchSubBlock::component_label)
                .collect::<Vec<_>>()
                .join(" + "),
            power_class: self.power_class,
            bcs_nr: self.bcs_nr,
            bcs_intra_endc: self.bcs_intra_endc,
            bcs_eutra: self.bcs_eutra,
            intra_band_en_dc_support: self.intra_band_en_dc_support,
            bit_mask: self.bit_mask,
            sub_blocks,
        })
    }
}

/// LTE-fallback patch (`kind = lte`) and its set entry.
pub(crate) type LtePatch = PatchDoc<LteSetEntry>;
pub(crate) type LteSetEntry = Entry<LtePatchCombo>;

#[derive(Debug)]
pub(crate) struct LtePatchCombo {
    pub(crate) components: Vec<LtePatchComponent>,
    pub(crate) bcs: u64,
    pub(crate) unknown1: u64,
    pub(crate) unknown2: u64,
}

#[derive(Debug)]
pub(crate) struct LtePatchComponent {
    pub(crate) band: i32,
    pub(crate) dl_bw_class_mimo: i32,
    pub(crate) ul_bw_class_mimo: i32,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_nr() -> Patch {
        Patch::Nr(NrPatch {
            kind: Kind::Nr,
            version: 1,
            delete: vec!["n41A".to_string()],
            set: vec![SetEntry {
                kind: SetKind::Add,
                combo: vec![PatchCombo {
                    group: 0,
                    index: 0,
                    bit_mask: 0,
                    sub_blocks: vec![PatchSubBlock {
                        kind: SubBlockKind::Nr,
                        band: 2,
                        dl_bw_class: Some(1),
                        ul_bw_class: Some(1),
                        dl_features: vec![ShannonFeatureSetDlPerCcNr {
                            max_bw: Some(40),
                            max_mimo: Some(2),
                            max_mod_order: Some(2),
                            ..Default::default()
                        }],
                        ..Default::default()
                    }],
                    ..Default::default()
                }],
            }],
        })
    }

    fn sample_lte() -> Patch {
        Patch::Lte(LtePatch {
            kind: Kind::Lte,
            version: 1,
            delete: vec!["B5A↓".to_string()],
            set: vec![LteSetEntry {
                kind: SetKind::Change,
                combo: vec![LtePatchCombo {
                    components: vec![LtePatchComponent {
                        band: 1,
                        dl_bw_class_mimo: 32768,
                        ul_bw_class_mimo: 0,
                    }],
                    bcs: 7,
                    unknown1: 8,
                    unknown2: 9,
                }],
            }],
        })
    }

    #[test]
    fn from_combo_omits_matching_nr_feature_index() {
        let combo = Combo {
            sub_blocks: vec![SubBlock {
                band: "n78".to_string(),
                dl_feature_index: Some(2), // matches derived (FR2)
                dl_features: vec![ShannonFeatureSetDlPerCcNr {
                    max_scs: Some(4),
                    ..Default::default()
                }],
                ..Default::default()
            }],
            ..Default::default()
        };
        let patch_combo = PatchCombo::from_combo(&combo);
        assert_eq!(patch_combo.sub_blocks[0].dl_feature_index, None); // omitted
        // Sanity: the inline feature set is still carried so apply can re-derive.
        assert_eq!(patch_combo.sub_blocks[0].dl_features[0].max_scs, Some(4));
    }

    #[test]
    fn nr_patch_round_trips_through_kdl() {
        let text = to_kdl(&sample_nr()).unwrap();
        assert!(text.contains("kind nr"), "{text}");
        assert!(text.contains("version 1"), "{text}");
        // "n41A" is a valid bare KDL identifier, so autoformat renders it unquoted —
        // same convention as the compiler's `carrier VZW`/mapping's `name=VZW`.
        assert!(text.contains("delete n41A"), "{text}");
        assert!(text.contains("add {"), "{text}");
        assert!(text.contains("nr 2"), "{text}"); // component node = kind
        assert!(text.contains("dl-cc "), "per-CC child node: {text}");
        assert!(text.contains("max-bw=40"), "{text}");
        assert!(text.contains("max-mimo=2"), "{text}");
        assert!(text.contains("max-mod-order=2"), "{text}");
        assert!(!text.contains("set "), "no set wrapper: {text}");
        assert!(!text.contains("dl_max_bw"), "no snake_case leakage: {text}");

        let Patch::Nr(back) = from_kdl(&text).unwrap() else {
            panic!("expected nr variant")
        };
        assert_eq!(back.version, 1);
        assert_eq!(back.delete, vec!["n41A".to_string()]);
        assert_eq!(set_entry_key(&back.set[0]).unwrap(), "n2A");
        assert_eq!(back.set[0].kind, SetKind::Add);
        assert_eq!(back.set[0].combo[0].sub_blocks[0].kind, SubBlockKind::Nr);
        assert_eq!(back.set[0].combo[0].sub_blocks[0].band, 2);
    }

    #[test]
    fn lte_patch_round_trips_through_kdl() {
        let text = to_kdl(&sample_lte()).unwrap();
        assert!(text.contains("kind lte"), "{text}");
        assert!(text.contains("change {"), "{text}");
        assert!(
            text.contains("subblock 1 dl-bw-class-mimo=32768 ul-bw-class-mimo=0"),
            "{text}"
        );
        assert!(text.contains("bcs=7 unknown1=8 unknown2=9"), "{text}");
        assert!(!text.contains("set "), "no set wrapper: {text}");

        let Patch::Lte(back) = from_kdl(&text).unwrap() else {
            panic!("expected lte variant")
        };
        assert_eq!(back.version, 1);
        assert_eq!(back.delete, vec!["B5A↓".to_string()]);
        assert_eq!(lte_set_entry_key(&back.set[0]).unwrap(), "B1A↓");
        assert_eq!(back.set[0].kind, SetKind::Change);
        assert_eq!(back.set[0].combo[0].bcs, 7);
        assert_eq!(back.set[0].combo[0].unknown1, 8);
        assert_eq!(back.set[0].combo[0].unknown2, 9);
    }

    #[test]
    fn nr_combo_preserves_mixed_component_order() {
        // An EN-DC combo mixes `lte`/`nr` children; the reader must not reorder them by
        // grouping same-named siblings (see `read_nr_combo`'s document-order walk).
        let text = "kind nr\nversion 1\nchange {\n    combo bit-mask=0 {\n        lte 66 dl-bw-class=1 ul-bw-class=1\n        nr 77 dl-bw-class=1 ul-bw-class=1\n    }\n}\n";
        let Patch::Nr(p) = from_kdl(text).unwrap() else {
            panic!("expected nr variant")
        };
        let kinds: Vec<SubBlockKind> = p.set[0].combo[0]
            .sub_blocks
            .iter()
            .map(|cc| cc.kind)
            .collect();
        assert_eq!(kinds, vec![SubBlockKind::Lte, SubBlockKind::Nr]);
        assert_eq!(set_entry_key(&p.set[0]).unwrap(), "B66A + n77A");
    }

    #[test]
    fn nr_patch_combo_rejects_unrecognized_component_node_name() {
        // A component's radio kind is now the node name (`nr`/`lte`); any other name
        // (e.g. a stray `cc`, valid only in the LTE-fallback patch) must be rejected.
        let text = "kind nr\nversion 1\nadd {\n    combo bit-mask=0 {\n        cc 2 dl-bw-class=1 ul-bw-class=1\n    }\n}\n";
        let err = format!("{:#}", from_kdl(text).unwrap_err());
        assert!(
            err.contains("unknown child node") && err.contains("cc"),
            "{err}"
        );
    }

    #[test]
    fn nr_patch_rejects_old_decoded_nr_cap_fields() {
        let text = r#"
kind nr
version 1
add {
    combo bit-mask=0 {
        nr 78 dl-bw-class=1 ul-bw-class=1 dl-mimo="4x4"
    }
}
"#;
        let err = format!("{:#}", from_kdl(text).unwrap_err());
        assert!(err.contains("unknown property"), "{err}");
    }

    #[test]
    fn patch_source_rejects_removed_props() {
        // The escape-hatch surface keys were dropped from the patch format too (proto
        // machinery kept). The strict reader must now reject each as an unknown property.
        for key in [
            "dl-feature-index=1",
            "ul-feature-index=1",
            "dl-cc-id=1",
            "ul-cc-id=1",
        ] {
            let text = format!(
                "kind nr\nversion 1\nadd {{\n    combo bit-mask=0 {{\n        nr 78 dl-bw-class=1 {key}\n    }}\n}}\n"
            );
            let err = from_kdl(&text).unwrap_err().to_string();
            assert!(
                err.contains("unknown property"),
                "{key} should be rejected: {err}"
            );
        }
    }

    #[test]
    fn patch_source_rejects_old_bw_class_spellings() {
        // The direction-first rename applies to the patch format too, and the patch reader
        // had no old-spelling guard at all. NR components carry `dl/ul-bw-class`; LTE
        // sub-blocks carry `dl/ul-bw-class-mimo`. All four pre-rename spellings must be
        // unknown properties now.
        for key in ["bw-class-dl=1", "bw-class-ul=1"] {
            let text = format!(
                "kind nr\nversion 1\nadd {{\n    combo bit-mask=0 {{\n        nr 78 dl-bw-class=1 {key}\n    }}\n}}\n"
            );
            let err = from_kdl(&text).unwrap_err().to_string();
            assert!(
                err.contains("unknown property") && err.contains(key.trim_end_matches("=1")),
                "{key} should be rejected: {err}"
            );
        }
        for (new, old) in [
            ("dl-bw-class-mimo=", "bw-class-mimo-dl="),
            ("ul-bw-class-mimo=", "bw-class-mimo-ul="),
        ] {
            // Both mimo props are required by the LTE sub-block reader, so append the old
            // spelling rather than substituting it — otherwise the missing-required error
            // fires first and hides the unknown-property rejection under test.
            let text = to_kdl(&sample_lte())
                .unwrap()
                .replace(new, &format!("{old}1 {new}"));
            let err = from_kdl(&text).unwrap_err().to_string();
            assert!(
                err.contains("unknown property") && err.contains(old.trim_end_matches('=')),
                "{old} should be rejected: {err}"
            );
        }
    }

    #[test]
    fn cc_kdl_does_not_expose_compiler_catalog_reference_fields() {
        // The compiler's per-`cc` catalog-reference properties (`dl-feature`/`ul-feature`,
        // 1-based positions into nr.kdl's global feature catalogs — `compiler::features::
        // NrSourceSubBlock`) are a different vocabulary from the patch's own raw/resolved fields.
        // A patch must neither emit them nor silently accept one hand-pasted from a compiler
        // `cc` node — it must reject it as unknown. Replaces the old TOML-era
        // `patch_serde_does_not_expose_compiler_reference_fields` guard.
        let patch = Patch::Nr(NrPatch {
            kind: Kind::Nr,
            version: 1,
            delete: vec![],
            set: vec![SetEntry {
                kind: SetKind::Add,
                combo: vec![PatchCombo {
                    bit_mask: 0,
                    sub_blocks: vec![PatchSubBlock {
                        kind: SubBlockKind::Nr,
                        band: 78,
                        dl_features: vec![ShannonFeatureSetDlPerCcNr {
                            max_scs: Some(3),
                            ..Default::default()
                        }],
                        ..Default::default()
                    }],
                    ..Default::default()
                }],
            }],
        });
        let text = to_kdl(&patch).unwrap();
        assert!(text.contains("dl-cc "), "{text}");
        assert!(text.contains("max-scs=3"), "{text}");
        assert!(!text.contains("dl-feature="), "{text}");
        assert!(!text.contains("ul-feature="), "{text}");

        let bad = "kind nr\nversion 1\nadd {\n    combo bit-mask=0 {\n        nr 78 dl-bw-class=1 ul-bw-class=1 dl-feature=1\n    }\n}\n";
        let err = format!("{:#}", from_kdl(bad).unwrap_err());
        assert!(err.contains("unknown property"), "{err}");
    }

    #[test]
    fn set_entry_with_mixed_variant_keys_is_rejected() {
        let text = r#"
kind nr
version 1
change {
    combo bit-mask=0 {
        nr 2 dl-bw-class=1 ul-bw-class=1
    }
    combo bit-mask=0 {
        nr 78 dl-bw-class=1 ul-bw-class=1
    }
}
"#;
        let err = from_kdl(text).unwrap_err().to_string();
        assert!(
            err.contains("mixes derived keys"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn empty_set_entry_is_rejected() {
        // A set entry must contain at least one combo variant; an empty `add { }` block
        // would otherwise derive no key and silently produce nothing.
        let text = r#"
kind nr
version 1
add {
}
"#;
        let err = from_kdl(text).unwrap_err().to_string();
        assert!(err.contains("no combo variants"), "unexpected error: {err}");
    }

    #[test]
    fn nr_duplicate_set_entry_key_is_rejected() {
        let text = r#"
kind nr
version 1
add {
    combo bit-mask=0 {
        nr 78 dl-bw-class=1 ul-bw-class=1
    }
}
add {
    combo bit-mask=0 {
        nr 78 dl-bw-class=1 ul-bw-class=1
    }
}
"#;
        let err = from_kdl(text).unwrap_err().to_string();
        assert!(
            err.contains("duplicate set entry"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn nr_duplicate_delete_key_is_rejected() {
        let text = r#"
kind nr
version 1
delete n78A
delete n78A
"#;
        let err = from_kdl(text).unwrap_err().to_string();
        assert!(err.contains("duplicate delete"), "unexpected error: {err}");
    }

    #[test]
    fn lte_duplicate_delete_key_is_rejected() {
        let text = r#"
kind lte
version 1
delete B1A
delete B1A
"#;
        let err = from_kdl(text).unwrap_err().to_string();
        assert!(err.contains("duplicate delete"), "unexpected error: {err}");
    }

    #[test]
    fn lte_duplicate_set_entry_key_is_rejected() {
        let text = r#"
kind lte
version 1
add {
    combo bcs=0 unknown1=0 unknown2=0 {
        subblock 1 dl-bw-class-mimo=32768 ul-bw-class-mimo=0
    }
}
add {
    combo bcs=0 unknown1=0 unknown2=0 {
        subblock 1 dl-bw-class-mimo=32768 ul-bw-class-mimo=0
    }
}
"#;
        let err = from_kdl(text).unwrap_err().to_string();
        assert!(
            err.contains("duplicate set entry"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn lte_component_rejects_nr_only_fields() {
        let text = r#"
kind nr
version 1
add {
    combo bit-mask=0 {
        lte 66 dl-bw-class=1 ul-bw-class=1 {
            dl-cc max-bw=100
        }
    }
}
"#;
        let err = from_kdl(text).unwrap_err().to_string();
        assert!(err.contains("NR-only"), "unexpected error: {err}");
    }

    #[test]
    fn lte_component_allows_feature_indexes() {
        let text = r#"
kind nr
version 1
add {
    combo bit-mask=0 {
        lte 1 dl-bw-class=1 ul-bw-class=1 dl-feature=1 ul-feature=2
    }
}
"#;
        let Patch::Nr(p) = from_kdl(text).unwrap() else {
            panic!("expected nr variant")
        };
        let cc = &p.set[0].combo[0].sub_blocks[0];
        assert_eq!(cc.kind, SubBlockKind::Lte);
        assert_eq!(cc.dl_feature_index, Some(1));
        assert_eq!(cc.ul_feature_index, Some(2));
        assert_eq!(set_entry_key(&p.set[0]).unwrap(), "B1A");
    }

    #[test]
    fn unknown_version_is_rejected() {
        let text = "kind nr\nversion 2\n";
        assert!(from_kdl(text).is_err());
    }

    #[test]
    fn missing_kind_is_rejected() {
        assert!(from_kdl("version 1\n").is_err());
    }

    #[test]
    fn duplicate_kind_is_rejected() {
        let text = "kind nr\nkind nr\nversion 1\n";
        let err = format!("{:#}", from_kdl(text).unwrap_err());
        assert!(err.contains("duplicate"), "{err}");
    }

    #[test]
    fn duplicate_version_is_rejected() {
        let text = "kind nr\nversion 1\nversion 1\n";
        let err = format!("{:#}", from_kdl(text).unwrap_err());
        assert!(err.contains("duplicate"), "{err}");
    }

    #[test]
    fn unknown_top_level_node_is_rejected() {
        let text = "kind nr\nversion 1\nbogus 1\n";
        let err = format!("{:#}", from_kdl(text).unwrap_err());
        assert!(err.contains("unknown top-level node"), "{err}");
    }

    #[test]
    fn lte_patch_rejects_out_of_range_band() {
        // R10: an LTE component band that overflows u16 (65602) must be rejected at
        // parse, not silently wrapped to 66 ("supported B66") and shipped.
        let text = r#"
kind lte
version 1
add {
    combo bcs=0 unknown1=0 unknown2=0 {
        subblock 65602 dl-bw-class-mimo=32768 ul-bw-class-mimo=0
    }
}
"#;
        let err = format!("{:#}", from_kdl(text).unwrap_err());
        assert!(
            err.contains("out of range") || err.contains("65602"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn nr_patch_rejects_out_of_range_band() {
        // Symmetric NR guard: a typo'd or hand-authored `nr 100000` must be rejected
        // at parse, not silently shipped for `provision` to render with a wrapped u16 label.
        let text = r#"
kind nr
version 1
add {
    combo bit-mask=0 {
        nr 100000 dl-bw-class=1 ul-bw-class=1
    }
}
"#;
        let err = format!("{:#}", from_kdl(text).unwrap_err());
        assert!(
            err.contains("out of range") || err.contains("100000"),
            "unexpected error: {err}"
        );
    }

    /// R2: a non-uniform two-CC NR sub-block — two `dl-cc` children with distinct
    /// `max-scs` — must round-trip `read_sub_block(sub_block_to_node(x)) == x`,
    /// preserving BOTH features. This is the model this task exists to fix: the old
    /// flat single-CC props physically could not express two different DL feature sets
    /// on one sub-block (colliding property names).
    #[test]
    fn patch_sub_block_per_cc_non_uniform_dl_round_trips_both_features() {
        let cc = PatchSubBlock {
            kind: SubBlockKind::Nr,
            band: 48,
            dl_bw_class: Some(2),
            // Corpus-verified always-`Some`: `read_sub_block` now defaults an absent
            // `ul-bw-class` back to `Some(0)` (Task 8 omit-when-0), so a `None` here would
            // not round-trip byte-for-byte through the writer/reader pair under test.
            ul_bw_class: Some(0),
            dl_features: vec![
                ShannonFeatureSetDlPerCcNr {
                    max_scs: Some(1),
                    max_bw: Some(40),
                    ..Default::default()
                },
                ShannonFeatureSetDlPerCcNr {
                    max_scs: Some(2),
                    max_bw: Some(100),
                    ..Default::default()
                },
            ],
            ..Default::default()
        };

        let node = sub_block_to_node(&cc);
        let text = node.to_string();
        assert_eq!(text.matches("dl-cc ").count(), 2, "{text}");

        let back = read_sub_block(&node).unwrap();
        assert_eq!(
            back, cc,
            "round-trip must preserve both per-CC feature sets"
        );
        assert_eq!(back.dl_features[0].max_scs, Some(1));
        assert_eq!(back.dl_features[1].max_scs, Some(2));
    }

    /// Non-uniform DL + a distinct UL child, mixed with the raw UL fallback absent —
    /// exercises `dl-cc`/`ul-cc` emitted together in one sub-block.
    #[test]
    fn patch_sub_block_per_cc_dl_and_ul_children_round_trip_together() {
        let cc = PatchSubBlock {
            kind: SubBlockKind::Nr,
            band: 78,
            dl_bw_class: Some(1),
            ul_bw_class: Some(1),
            dl_features: vec![ShannonFeatureSetDlPerCcNr {
                max_scs: Some(2),
                max_mimo: Some(2),
                max_bw: Some(100),
                max_mod_order: Some(2),
                bw_90mhz_supported: Some(true),
            }],
            ul_features: vec![ShannonFeatureSetUlPerCcNr {
                max_scs: Some(1),
                max_mimo_cb: Some(2),
                max_bw: Some(50),
                max_mod_order: Some(1),
                max_mimo_non_cb: Some(1),
                bw_90mhz_supported: Some(false),
            }],
            ..Default::default()
        };

        let node = sub_block_to_node(&cc);
        let back = read_sub_block(&node).unwrap();
        assert_eq!(back, cc);
    }

    /// An `lte` patch sub-block spells the proto-4/5 index as scalar `dl-feature`/`ul-feature`
    /// and omits `ul-feature=0`; `read_sub_block(sub_block_to_node(x)) == x` for both a zero and
    /// a non-zero UL index.
    #[test]
    fn patch_lte_sub_block_scalar_feature_names_round_trip() {
        for (ul, shows_ul) in [(Some(0), false), (Some(2), true)] {
            let cc = PatchSubBlock {
                kind: SubBlockKind::Lte,
                band: 7,
                dl_bw_class: Some(2),
                // Always-`Some` on LTE; a `None` would not round-trip (reader defaults absent→0).
                ul_bw_class: Some(1),
                dl_feature_index: Some(1),
                ul_feature_index: ul,
                ..Default::default()
            };
            let node = sub_block_to_node(&cc);
            let text = node.to_string();
            assert!(text.contains("dl-feature=1"), "{text}");
            assert!(!text.contains("dl-feature-index"), "{text}");
            assert_eq!(text.contains("ul-feature="), shows_ul, "{text}");
            let back = read_sub_block(&node).unwrap();
            assert_eq!(back, cc, "round-trip must preserve the LTE index");
        }
    }

    #[test]
    fn lte_placeholder_sub_block_carries_no_feature_children() {
        // An LTE component inside a mixed EN-DC combo carries no NR-only fields, so it
        // must round-trip with no `dl-cc`/`ul-cc` children at all.
        let cc = PatchSubBlock {
            kind: SubBlockKind::Lte,
            band: 66,
            dl_bw_class: Some(1),
            ul_bw_class: Some(1),
            ..Default::default()
        };
        let text = sub_block_to_node(&cc).to_string();
        assert!(!text.contains("dl-cc"), "{text}");
        assert!(!text.contains("ul-cc"), "{text}");
    }
}
