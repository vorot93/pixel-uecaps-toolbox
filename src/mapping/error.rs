use thiserror::Error;

/// Errors from the PLMN-mapping editor (`mapping` subcommand).
#[derive(Debug, Error)]
pub enum Error {
    #[error("protobuf decode error: {0}")]
    Decode(#[from] prost::DecodeError),
    #[error("PLMN value {0} is out of the 24-bit range")]
    PlmnOutOfRange(u64),
    #[error("invalid PLMN string `{0}` (expected MCC-MNC, e.g. 250-01)")]
    PlmnFormat(String),
    #[error("duplicate carrier id {0} in TOML")]
    DuplicateId(u64),
    #[error("duplicate mapping name `{0}` in TOML")]
    DuplicateName(String),
    #[error("mapping #{0} has an empty name")]
    EmptyName(u64),
    #[error("no mapping named `{0}`")]
    MappingNotFound(String),
}

#[cfg(test)]
mod tests {
    use super::Error;

    #[test]
    fn displays_mapping_not_found() {
        assert_eq!(
            Error::MappingNotFound("VZW".into()).to_string(),
            "no mapping named `VZW`"
        );
    }
}
