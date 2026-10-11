mod generate;
mod ingest;
#[cfg(test)]
pub(in crate::compiler) mod test_support;
mod verify;
use crate::{compiler::selection::Sku, proto::UeCaps};
pub(in crate::compiler) use generate::generate_nr_files;
pub(in crate::compiler) use ingest::ingest_nr;

/// An unnumbered `<CARRIER>.binarypb` from the bitmask folder. Legacy files have no filename
/// number *by construction*, so there is nothing to validate downstream.
pub(in crate::compiler) struct LegacyNrFile {
    pub(in crate::compiler) carrier: String,
    pub(in crate::compiler) caps: UeCaps,
}

/// A numbered `<CARRIER>_<NUMBER>.binarypb` from the profiled folder. The number is not
/// optional: `decompose`'s classifier already parsed it out of the filename to decide this
/// file belongs here at all, so the type records what the classifier proved.
pub(in crate::compiler) struct ProfiledNrFile {
    pub(in crate::compiler) carrier: String,
    pub(in crate::compiler) number: u64,
    pub(in crate::compiler) caps: UeCaps,
}

pub(in crate::compiler) enum NrTarget {
    Legacy,
    Profile { anchor: u64, sku: Sku },
}

struct NrFileIdentity {
    basename: String,
    fingerprint: u64,
    carrier_id: Option<i32>,
    reference: u64,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum InputLayout {
    Legacy,
    Profiled,
}

impl InputLayout {
    /// The single bitmask a generated combo carries for this layout: the legacy all-ones sentinel
    /// vs. the profiled zero. Previously threaded in as a redundant argument beside `layout`.
    fn bitmask(self) -> u32 {
        match self {
            InputLayout::Legacy => 65_535,
            InputLayout::Profiled => 0,
        }
    }
}
