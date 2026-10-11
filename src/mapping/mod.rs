//! Typed PLMN values and compiler mapping schema. Reports retain original protobuf maps.
mod error;
mod plmn;
mod schema;

pub use error::Error;
pub use plmn::Plmn;
pub(crate) use schema::{
    MappingEntry, MappingRoot, encode_root_verified, map_to_root, root_to_map,
};
