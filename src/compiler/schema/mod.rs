mod canonicalize;
mod source;
#[cfg(test)]
mod test_support;
mod validate;
mod validated;
pub(in crate::compiler) use canonicalize::nr_source_combo;
#[cfg(test)]
pub(in crate::compiler) use canonicalize::to_kdl;
pub(in crate::compiler) use source::{
    BitmaskFingerprint, CarrierSource, LteDocument, LteFileSource, LteSourceCombo, NrDocument,
    NrSourceCombo, ProfileSource, SOURCE_FORMAT_VERSION, SourceDocument,
};
pub(in crate::compiler) use validate::{
    legend_root, parse_sources, validate_carrier_name, validate_documents,
};
#[cfg(test)]
pub(in crate::compiler) use validated::ValidatedNrCombo;
pub use validated::ValidatedSources;
pub(in crate::compiler) use validated::{ValidatedLte, ValidatedLteCombo, ValidatedNr};
