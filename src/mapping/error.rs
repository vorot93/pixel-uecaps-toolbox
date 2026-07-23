use thiserror::Error;

/// Errors from the PLMN-mapping codec: PLMN parsing/formatting, and the legend's proto ↔
/// editable-model conversion (`mapping::schema`), used by the compiler and `check`.
#[derive(Debug, Error)]
pub enum Error {
    #[error("protobuf decode error: {0}")]
    Decode(#[from] prost::DecodeError),
    #[error("PLMN value {0} is out of the 24-bit range")]
    PlmnOutOfRange(u64),
    #[error("invalid PLMN string `{0}` (expected MCC-MNC, e.g. 250-01)")]
    PlmnFormat(String),
    #[error("duplicate carrier id {0} in the mapping legend")]
    DuplicateId(u64),
    #[error("duplicate mapping name `{0}` in the mapping legend")]
    DuplicateName(String),
    #[error("mapping #{0} has an empty name")]
    EmptyName(u64),
}
