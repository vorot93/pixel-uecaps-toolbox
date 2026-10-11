//! Strict source-document KDL facade.
mod plmn;
mod read;
#[cfg(test)]
mod tests;
mod write;
pub(in crate::compiler) use read::read_sources;
pub(in crate::compiler) use write::write_sources;
// ---- NR document mapping ----
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

/// The value `bcs-intra-endc` takes when the property is absent: `Some(0)` exactly when the
/// combo advertises intra-band EN-DC (`intra-band-en-dc-support == 1`), else `None`. Single
/// source of truth for all three users, so no two of them can silently disagree:
/// `write_nr_combo` omits the property when it equals this, and `read_nr_combo` both re-derives an
/// absent property from it and rejects an explicit one that states it. See DESIGN.md.
fn derive_bcs_intra_endc(intra_band_en_dc_support: Option<i32>) -> Option<u32> {
    if intra_band_en_dc_support == Some(1) {
        Some(0)
    } else {
        None
    }
}
