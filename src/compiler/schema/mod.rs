mod canonicalize;
mod source;
#[cfg(test)]
mod test_support;
mod validate;
mod validated;
pub(crate) use canonicalize::nr_source_combo;
#[cfg(test)]
pub(crate) use canonicalize::to_kdl;
pub(crate) use source::{
    BitmaskFingerprint, CarrierSource, LteDocument, LteFileSource, LteSourceCombo, NrDocument,
    NrSourceCombo, ProfileSource, SOURCE_FORMAT_VERSION, SourceDocument,
};
pub(crate) use validate::{legend_root, parse_sources, validate_carrier_name, validate_documents};
pub use validated::ValidatedSources;
pub(crate) use validated::{ValidatedLte, ValidatedLteCombo, ValidatedNr, ValidatedNrCombo};
