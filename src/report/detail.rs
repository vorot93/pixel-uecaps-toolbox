//! How much a report shows. The CLI's `--full` / `--common` flags become these at the
//! dispatch boundary and travel as named values from there.

/// How much detail a report renders.
///
/// `pub`, not `pub(crate)`: `main.rs` is a separate crate from the library, and
/// `report::inspect` / `report::compare` are its public entry points, so this type appears in
/// a public signature. `main.rs` never names `Detail` itself — `bool::into()` infers it from
/// the parameter type — but the type still has to be as visible as the functions that carry
/// it (`private_interfaces` is a hard error at that boundary, not a lint).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Detail {
    /// The default view.
    Summary,
    /// `--full`: per-component combo detail and the SKU-selection math.
    Full,
}

impl Detail {
    /// For the handful of sites that guard a whole block on "is this the full view".
    pub(crate) const fn is_full(self) -> bool {
        matches!(self, Self::Full)
    }
}

/// One of the two places a `bool` parameter is allowed to survive, named: `clap` parses
/// `--full` as a flag, and this is the single boundary where that flag becomes a [`Detail`].
impl From<bool> for Detail {
    fn from(full: bool) -> Self {
        if full { Self::Full } else { Self::Summary }
    }
}

/// Whether `compare` also lists the combos present in both files.
///
/// `pub` for the same reason as [`Detail`]: it crosses into `main.rs`'s public call to
/// `report::compare`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Common {
    Hide,
    /// `--common`: list shared combos too (`=` identical, `~` caps differ).
    Show,
}

impl Common {
    pub(crate) const fn is_shown(self) -> bool {
        matches!(self, Self::Show)
    }
}

/// The other one: `clap` parses `--common` as a flag, and this is the single boundary where
/// that flag becomes a [`Common`].
impl From<bool> for Common {
    fn from(show: bool) -> Self {
        if show { Self::Show } else { Self::Hide }
    }
}

#[cfg(test)]
mod tests {
    use super::{Common, Detail};

    #[test]
    fn flags_convert_at_the_cli_boundary_only() {
        assert_eq!(Detail::from(false), Detail::Summary);
        assert_eq!(Detail::from(true), Detail::Full);
        assert_eq!(Common::from(false), Common::Hide);
        assert_eq!(Common::from(true), Common::Show);
        assert!(!Detail::Summary.is_full());
        assert!(Detail::Full.is_full());
    }
}
