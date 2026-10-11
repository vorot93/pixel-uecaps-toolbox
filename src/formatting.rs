use std::fmt::{self, Display, Formatter};

pub(crate) struct Optional<T>(pub(crate) Option<T>);

impl<T: Display> Display for Optional<T> {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        match &self.0 {
            Some(value) => value.fmt(f),
            None => f.write_str("absent"),
        }
    }
}

pub(crate) struct Separated<'a, T> {
    pub(crate) values: &'a [T],
    pub(crate) separator: &'a str,
}

impl<T: Display> Display for Separated<'_, T> {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        for (i, value) in self.values.iter().enumerate() {
            if i != 0 {
                f.write_str(self.separator)?;
            }
            value.fmt(f)?;
        }
        Ok(())
    }
}
