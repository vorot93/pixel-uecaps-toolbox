use super::{derive_bcs_intra_endc, parse_sub_block_name, plmn::read_plmn};
use crate::{
    compiler::{
        features::{SourceLteSubBlock, SourceNrSubBlock, SourceSubBlock},
        kdl_bcs::parse_bcs,
        kdl_direction::{DirectionArg, parse_class_mimo, parse_direction},
        kdl_keys::{
            carrier, dl_catalog, doc, fingerprint, lte_combo, lte_file, nr_combo, profile,
            selection, sub_block, ul_catalog,
        },
        parse_shortest_u64,
        schema::{
            BitmaskFingerprint, CarrierSource, LteDocument, LteFileSource, LteSourceCombo,
            NrDocument, NrSourceCombo, ProfileSource, SOURCE_FORMAT_VERSION, SourceDocument,
        },
        selection::SelectionRect,
    },
    kdl_support::{NodeReader, read_str_list},
    proto::{ComboHeader, LteComponent, ShannonFeatureSetDlPerCcNr, ShannonFeatureSetUlPerCcNr},
    raw_nr::{Direction, SubBlockKind, bw_class_letter, cc_count},
};
use anyhow::{Context, Result, anyhow, bail, ensure};
use kdl::{KdlDocument, KdlNode};
use std::collections::BTreeMap;
fn read_fingerprint(node: &KdlNode) -> Result<BitmaskFingerprint> {
    let mut r = NodeReader::new(node);
    let fingerprint = r.arg_int::<u64>()?;
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

fn read_profile(node: &KdlNode, carrier: &str) -> Result<(u64, ProfileSource)> {
    let mut r = NodeReader::new(node);
    let key = r.arg_str()?;
    let multiplier = r.req_int::<u64>(profile::MULTIPLIER)?;
    let unknown = r.req_int::<u64>(profile::UNKNOWN)?;
    r.finish()?;
    let key = parse_shortest_u64(&key).with_context(|| {
        format!("profile key `{key}` for carrier `{carrier}` must be a shortest-decimal u64")
    })?;
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
    let name = r.arg_str()?;
    let bitmask_id = r.opt_int::<i64>(carrier::BITMASK_ID)?;
    let profiled_id = r.opt_int::<i64>(carrier::PROFILED_ID)?;
    let mapping_id = r.opt_int::<u64>(carrier::MAPPING_ID)?;
    let signature = r.opt_int::<u64>(carrier::SIGNATURE)?;
    let tier = r.opt_str(carrier::TIER)?.map(|s| s.parse()).transpose()?;
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
        let (k, v) = read_profile(pnode, &name)?;
        if profiles.insert(k, v).is_some() {
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
    let carriers = r
        .opt_child(selection::CARRIERS)?
        .map(read_str_list)
        .transpose()?;
    let skus = r
        .opt_child(selection::SKUS)?
        .map(read_str_list)
        .transpose()?;
    r.finish()?;
    Ok(SelectionRect { carriers, skus })
}

fn sub_block_identity(name: &str) -> Result<(SubBlockKind, u16)> {
    // The band comes from the node name, so there is no positional argument to read.
    if let Some(band) = parse_sub_block_name(name, nr_combo::NR_PREFIX) {
        Ok((SubBlockKind::Nr, band))
    } else if let Some(band) = parse_sub_block_name(name, nr_combo::LTE_PREFIX) {
        Ok((SubBlockKind::Lte, band))
    } else {
        bail!(
            "`{name}` is not a sub-block node name (expected `{}<band>` or `{}<band>`)",
            nr_combo::NR_PREFIX,
            nr_combo::LTE_PREFIX
        )
    }
}

fn read_directions(
    r: &mut NodeReader<'_>,
    name: &str,
    kind: SubBlockKind,
) -> Result<(DirectionArg, Option<DirectionArg>)> {
    // DL then UL, positional. `arg_str` errors when the argument is missing, which is what
    // makes an argument-free sub-block a hard failure rather than a silently classless one.
    let dl = parse_direction(
        &r.arg_str()
            .with_context(|| format!("`{name}` is missing its DL bandwidth class"))?,
        Direction::Dl,
    )?;
    let ul = r
        .opt_arg_str()?
        .map(|raw| parse_direction(&raw, Direction::Ul))
        .transpose()?;

    // Arity depends on the kind. NR indices are one per CC and must match `cc_count`; an
    // E-UTRA sub-block carries a single `parseLteFeatureIndex` value regardless of class. An
    // empty list is the all-zero placeholder and is checked by neither.
    for (label, parsed) in [(Direction::Dl, Some(&dl)), (Direction::Ul, ul.as_ref())] {
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
                    "`{name}` {label} has a 0 index; NR per-CC catalog references \
                     are 1-based"
                );
                let expected = cc_count(SubBlockKind::Nr, parsed.bw_class)?;
                ensure!(
                    parsed.indices.len() == expected,
                    "`{name}` {label} has {} per-CC index/indices but bandwidth class \
                     {} implies {expected}",
                    parsed.indices.len(),
                    bw_class_letter(parsed.bw_class).expect("parsed class is an uppercase letter")
                );
            }
            SubBlockKind::Lte => ensure!(
                parsed.indices.len() == 1,
                "`{name}` {label} takes at most one index on an E-UTRA sub-block, \
                 found {}",
                parsed.indices.len()
            ),
        }
    }

    Ok((dl, ul))
}

fn lte_feature_index(
    argument: Option<&DirectionArg>,
    name: &str,
    direction: Direction,
) -> Result<Option<u16>> {
    argument
        .and_then(|d| d.indices.first().copied())
        .map(|index| {
            u16::try_from(index).with_context(|| {
                format!("`{name}` LTE {direction} feature index {index} does not fit u16")
            })
        })
        .transpose()
}

fn source_lte_sub_block(
    name: &str,
    band: u16,
    dl: DirectionArg,
    ul: Option<DirectionArg>,
) -> Result<SourceLteSubBlock> {
    Ok(SourceLteSubBlock {
        band,
        dl_bw_class: Some(dl.bw_class),
        // Absent UL is the writer's omitted zero, not a genuine absence.
        ul_bw_class: Some(ul.as_ref().map_or(0, |d| d.bw_class)),
        dl_feature_index: lte_feature_index(Some(&dl), name, Direction::Dl)?,
        // An absent E-UTRA UL index re-defaults to Some(0), including a bare UL class.
        ul_feature_index: Some(lte_feature_index(ul.as_ref(), name, Direction::Ul)?.unwrap_or(0)),
    })
}

fn source_nr_sub_block(
    r: &mut NodeReader<'_>,
    band: u16,
    dl: DirectionArg,
    ul: Option<DirectionArg>,
) -> Result<SourceNrSubBlock> {
    Ok(SourceNrSubBlock {
        band,
        dl_bw_class: Some(dl.bw_class),
        ul_bw_class: Some(ul.as_ref().map_or(0, |d| d.bw_class)),
        dl_feature_indices: dl.indices,
        ul_feature_indices: ul.map(|d| d.indices).unwrap_or_default(),
        srs_tx_switch: r.opt_int(sub_block::SRS_TX_SWITCH)?,
    })
}

fn read_sub_block(node: &KdlNode) -> Result<SourceSubBlock> {
    let name = node.name().value();
    let (kind, band) = sub_block_identity(name)?;
    let mut r = NodeReader::new(node);
    let (dl, ul) = read_directions(&mut r, name, kind)?;
    let cc = match kind {
        SubBlockKind::Lte => source_lte_sub_block(name, band, dl, ul)?.into(),
        SubBlockKind::Nr => source_nr_sub_block(&mut r, band, dl, ul)?.into(),
    };
    r.finish()?; // now rejects any stray dl-cc-id / *-feature-index as unknown properties
    // The raw selector bytes are NOT reconstructed here. An unresolved direction only ever
    // carries the all-zero placeholder, which is a pure function of kind + `bw_class`, so
    // `write_sub_block` omits it and `SourceSubBlock::resolve` (the single provision-path boundary
    // that needs the bytes) re-derives it.
    Ok(cc)
}

/// Read a BCS property whose zero is spelled by omitting it. `key` is the KDL property to look
/// up; `label` is the descriptive name (`bcs-nr`, `bcs-eutra`, …) diagnostics report instead —
/// DESIGN.md's "a key spelling is not a diagnostic" rule.
///
/// An explicit empty value is refused rather than accepted as zero: omission already spells
/// that value, and a format with two spellings for one value cannot round-trip byte-stably.
/// The writer never emits one — `write_nr_combo` filters `Some(0)` out first — so this guards
/// a hand-edited document.
fn read_omitted_zero_bcs(
    r: &mut NodeReader<'_>,
    key: &'static str,
    label: &'static str,
) -> Result<Option<u32>> {
    let Some(raw) = r.opt_str(key)? else {
        return Ok(None);
    };
    ensure!(
        !raw.is_empty(),
        "property `{label}` is empty; the empty BCS set is spelled by omitting the property, \
         so an explicit empty value would give one value two spellings"
    );
    Ok(Some(parse_bcs(&raw, label)?))
}

pub(super) fn read_nr_combo(node: &KdlNode) -> Result<NrSourceCombo> {
    let mut r = NodeReader::new(node);
    // `power-class`/`bcs-nr`/`bcs-eutra`/`intra-band-en-dc-support` are corpus-verified
    // always `Some`: an absent property is the writer's omitted-zero (Task 8), so it
    // defaults back to `Some(0)`. `bcs-intra-endc` derives from `intra-band-en-dc-support` —
    // see below.
    let power_class = r.opt_int::<i32>(nr_combo::POWER_CLASS)?.or(Some(0));
    let bcs_nr = read_omitted_zero_bcs(&mut r, nr_combo::BCS_NR, "bcs-nr")?.or(Some(0));
    let bcs_eutra = read_omitted_zero_bcs(&mut r, nr_combo::BCS_EUTRA, "bcs-eutra")?.or(Some(0));
    let intra_band_en_dc_support = r
        .opt_int::<i32>(nr_combo::INTRA_BAND_EN_DC_SUPPORT)?
        .or(Some(0));
    // An absent `bcs-intra-endc` re-derives via the shared `derive_bcs_intra_endc` — the
    // inverse of the omit rule in `write_nr_combo`. Kept AFTER `intra-band-en-dc-support`,
    // the field it depends on.
    let bcs_intra_endc = match r.opt_str(nr_combo::BCS_INTRA_ENDC)? {
        Some(raw) => {
            let value = parse_bcs(&raw, "bcs-intra-endc")?;
            // Spelling out the derived value would give it two spellings. `write_nr_combo`
            // omits exactly this case, so a document containing it was hand-edited.
            // Names the fields in words, not their abbreviated key spellings — the same rule
            // `parse_bcs` above follows, and the one DESIGN.md states.
            ensure!(
                Some(value) != derive_bcs_intra_endc(intra_band_en_dc_support),
                "property `bcs-intra-endc` states the value already derived from \
                 `intra-band-en-dc-support`; omit it, so that each value has one spelling"
            );
            Some(value)
        }
        None => derive_bcs_intra_endc(intra_band_en_dc_support),
    };
    let selection = r
        .children(nr_combo::SELECTION)
        .into_iter()
        .map(read_selection)
        .collect::<Result<Vec<_>>>()?;
    // Reading NR before E-UTRA preserves the previous behaviour; `validate_nr_combos` sorts
    // sub-blocks by `RawSubBlockKey` regardless, which is why the reader could already read
    // NR-first while real documents store E-UTRA first and still round-trip byte-identically.
    let mut sub_blocks = Vec::new();
    for cnode in
        r.children_matching(|name| parse_sub_block_name(name, nr_combo::NR_PREFIX).is_some())
    {
        sub_blocks.push(read_sub_block(cnode)?);
    }
    for cnode in
        r.children_matching(|name| parse_sub_block_name(name, nr_combo::LTE_PREFIX).is_some())
    {
        sub_blocks.push(read_sub_block(cnode)?);
    }
    r.finish()?;
    Ok(NrSourceCombo {
        selection: if selection.is_empty() {
            None
        } else {
            Some(selection)
        },
        header: ComboHeader {
            power_class,
            bcs_nr,
            bcs_intra_endc,
            bcs_eutra,
            intra_band_en_dc_support,
        },
        sub_blocks,
    })
}

/// Parses a `version N` node's payload, or errors if `version` was already read once. Takes the
/// field's current value (not a pre-computed flag) so the duplicate check runs BEFORE parsing: a
/// malformed second `version` node still reports "duplicate", not a parse error.
fn read_version(node: &KdlNode, existing: Option<u32>) -> Result<u32> {
    if existing.is_some() {
        bail!("duplicate `version`");
    }
    let mut r = NodeReader::new(node);
    let v = r.arg_int::<u32>()?;
    r.finish()?;
    Ok(v)
}

/// Read and check the format version before any other node is mapped.
///
/// The ordering is the whole point. The mapping that follows is what rejects an unknown
/// vocabulary (`unknown property`, `missing required property`), and every format change so far
/// has also changed the vocabulary — so a stale source tree used to die there, unhelpfully, while
/// the check that exists to diagnose exactly that case sat downstream in `validate_documents` and
/// never ran. Scanning for the marker up front is also order-independent, so the diagnosis holds
/// for a hand-written document that puts `version` somewhere other than the top.
///
/// Returns nothing: success means the number equals [`SOURCE_FORMAT_VERSION`], so handing it back
/// would only propagate a value every caller already has as a constant.
fn checked_version(document: &KdlDocument, key: &str) -> Result<()> {
    let mut found: Option<u32> = None;
    for node in document
        .nodes()
        .iter()
        .filter(|node| node.name().value() == key)
    {
        found = Some(read_version(node, found)?);
    }
    let version = found.ok_or_else(|| anyhow!("the source document is missing `{key}`"))?;
    ensure!(
        version == SOURCE_FORMAT_VERSION,
        "the source document is source-format version {version} but this build reads version \
         {SOURCE_FORMAT_VERSION}; re-run `decompose` to regenerate it"
    );
    Ok(())
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
/// if `name` was already present. Shared by the carrier and LTE-file nodes, whose duplicate-name
/// shape is otherwise identical; the caller parses before calling, so a malformed duplicate still
/// reports its own parse error first.
///
/// `what` is a **word** — "carrier", "LTE file" — never a key spelling. See DESIGN.md: a key
/// spelling is not a diagnostic.
fn insert_unique<K: Ord + std::fmt::Display, T>(
    map: &mut BTreeMap<K, T>,
    what: &str,
    (k, v): (K, T),
) -> Result<()> {
    match map.entry(k) {
        std::collections::btree_map::Entry::Vacant(entry) => {
            entry.insert(v);
        }
        std::collections::btree_map::Entry::Occupied(entry) => {
            bail!("duplicate {what} `{}`", entry.key())
        }
    }
    Ok(())
}

fn read_lte_file(node: &KdlNode) -> Result<(u64, LteFileSource)> {
    let mut r = NodeReader::new(node);
    let key = r.arg_str()?;
    let fingerprint = r.req_int::<u64>(lte_file::FINGERPRINT)?;
    let bitmask = r.req_int::<u64>(lte_file::BITMASK)?;
    r.finish()?;
    let key = parse_shortest_u64(&key)
        .with_context(|| format!("LTE file key `{key}` must be a shortest-decimal u64"))?;
    Ok((
        key,
        LteFileSource {
            fingerprint,
            bitmask,
        },
    ))
}

fn read_lte_component(node: &KdlNode) -> Result<LteComponent> {
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
        &r.arg_str()
            .with_context(|| format!("`{name}` is missing its DL class+MIMO value"))?,
        Direction::Dl,
    )?;
    // Omit-when-0: an absent UL argument is UL disabled. `parse_class_mimo` never returns 0,
    // so this is the sole route to one and the value-to-spelling mapping stays one-to-one.
    let ul_bw_class_mimo = Some(
        r.opt_arg_str()?
            .map(|raw| parse_class_mimo(&raw, Direction::Ul))
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
    // `""` is the all-zero bit string; a `b`-list is the bit string itself. Those are the only
    // two states a live combo has — an omitted `b` means a genuinely absent proto field, which
    // no corpus combo has and which the air interface cannot express for a combo that exists
    // (`supportedBandCombinationExt-r10` is positionally parallel to the band-combination list,
    // so every combo has an entry; see DESIGN.md). It stays representable because the LTE round
    // trip is bit-for-bit: present-zero and absent are different bytes.
    let bcs = r
        .opt_str(lte_combo::BCS)?
        .map(|raw| parse_bcs(&raw, "bcs").map(u64::from))
        .transpose()?;
    let unknown1 = r.opt_int::<u64>(lte_combo::UNKNOWN1)?;
    let unknown2 = r.opt_int::<u64>(lte_combo::UNKNOWN2)?;
    let selection = r
        .children(lte_combo::SELECTION)
        .into_iter()
        .map(read_selection)
        .collect::<Result<Vec<_>>>()?;
    let components = r
        .children_matching(|name| parse_sub_block_name(name, lte_combo::SUB_BLOCK_PREFIX).is_some())
        .into_iter()
        .map(read_lte_component)
        .collect::<Result<Vec<_>>>()?;
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

/// Read one source document. The inverse of [`super::write_sources`], and order-independent: node
/// *order* is the writer's canonical form, not something the reader relies on.
pub(crate) fn read_sources(text: &str) -> Result<SourceDocument> {
    let document: KdlDocument = text
        .parse()
        .context("the source document is not valid KDL")?;
    checked_version(&document, doc::VERSION)?;
    let mut bitmask_carriers: Option<Vec<String>> = None;
    let mut bitmask_fingerprints = Vec::new();
    let mut carriers = BTreeMap::new();
    let mut files = BTreeMap::new();
    let mut dl_features = Vec::new();
    let mut ul_features = Vec::new();
    let mut nr_combo = Vec::new();
    let mut lte_combo = Vec::new();
    for node in document.nodes() {
        // An if/else chain rather than a `match`: the arms compare against `kdl_keys`
        // constants, which are not valid `match` patterns.
        let name = node.name().value();
        if name == doc::VERSION {
            // Already read, duplicate-checked, and version-checked by `checked_version` above.
        } else if name == doc::BITMASK_CARRIERS {
            bitmask_carriers = Some(read_bitmask_carriers(node, bitmask_carriers.as_deref())?);
        } else if name == doc::BITMASK_FINGERPRINT {
            bitmask_fingerprints.push(read_fingerprint(node)?);
        } else if name == doc::CARRIER {
            insert_unique(&mut carriers, "carrier", read_carrier(node)?)?;
        } else if name == doc::LTE_FILE {
            insert_unique(&mut files, "LTE file", read_lte_file(node)?)?;
        } else if name == doc::DL_FEATURE {
            dl_features.push(read_dl_feature(node)?);
        } else if name == doc::UL_FEATURE {
            ul_features.push(read_ul_feature(node)?);
        } else if name == doc::NR_COMBO {
            nr_combo.push(read_nr_combo(node)?);
        } else if name == doc::LTE_COMBO {
            lte_combo.push(read_lte_combo(node)?);
        } else {
            bail!("unknown top-level node `{name}` in the source document");
        }
    }
    Ok(SourceDocument {
        nr: NrDocument {
            bitmask_carriers: bitmask_carriers
                .ok_or_else(|| anyhow!("the source document is missing `bitmask-carriers`"))?,
            bitmask_fingerprints,
            carriers,
            dl_features,
            ul_features,
            combos: nr_combo,
        },
        lte: LteDocument {
            files,
            combos: lte_combo,
        },
    })
}
