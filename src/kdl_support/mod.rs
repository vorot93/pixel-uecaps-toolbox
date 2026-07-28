//! Crate-level KDL toolkit: strict `NodeReader` combinator + writer helpers,
//! shared by the compiler's (de)serializers.

use std::collections::BTreeSet;

use anyhow::{Result, anyhow, bail};
use kdl::{KdlDocument, KdlEntry, KdlNode, KdlValue};

use crate::compiler::kdl_keys::{carrier, plmn as plmn_keys};

// ---- writer helpers ----
/// Generic over the integer width so call sites pass their natural type (`i32`/`u32`/`u64`)
/// instead of an `as i128` cast at each one.
pub(crate) fn opt_int_prop<T: Into<i128>>(node: &mut KdlNode, key: &str, v: Option<T>) {
    if let Some(v) = v {
        node.push(KdlEntry::new_prop(key, v.into()));
    }
}
pub(crate) fn opt_str_prop(node: &mut KdlNode, key: &str, v: Option<&str>) {
    if let Some(v) = v {
        node.push(KdlEntry::new_prop(key, v));
    }
}
pub(crate) fn opt_bool_prop(node: &mut KdlNode, key: &str, v: Option<bool>) {
    if let Some(v) = v {
        node.push(KdlEntry::new_prop(key, v));
    }
}
pub(crate) fn str_list_node(name: &str, items: &[String]) -> KdlNode {
    let mut n = KdlNode::new(name);
    for it in items {
        n.push(KdlEntry::new(it.as_str()));
    }
    n
}
/// Collapse any run of trailing newlines to exactly one — KDL documents end with a single `\n`.
fn one_trailing_newline(mut text: String) -> String {
    while text.ends_with('\n') {
        text.pop();
    }
    text.push('\n');
    text
}
pub(crate) fn finish_doc(mut doc: KdlDocument) -> String {
    doc.autoformat();
    one_trailing_newline(doc.to_string())
}

// ---- reader combinator ----
/// Reads one `KdlNode`, tracking which positional args, properties, and child-node
/// names were consumed so `finish()` can reject anything unexpected (the strict
/// `deny_unknown_fields` equivalent).
/// A claimed child *shape*: a diagnostic label plus the predicate that recognises the name.
type ChildPredicate = (&'static str, fn(&str) -> bool);

pub(crate) struct NodeReader<'a> {
    node: &'a KdlNode,
    args_used: usize,
    props_used: BTreeSet<String>,
    children_used: BTreeSet<&'static str>,
    /// Predicates that claimed children, with a human label for diagnostics. The parallel of
    /// `children_used` for child names that are computed rather than fixed.
    child_predicates: Vec<ChildPredicate>,
}

impl<'a> NodeReader<'a> {
    pub(crate) fn new(node: &'a KdlNode) -> Self {
        Self {
            node,
            args_used: 0,
            props_used: BTreeSet::new(),
            children_used: BTreeSet::new(),
            child_predicates: Vec::new(),
        }
    }

    fn positional(&self) -> Vec<&'a KdlValue> {
        self.node
            .entries()
            .iter()
            .filter(|e| e.name().is_none())
            .map(|e| e.value())
            .collect()
    }

    /// The next unconsumed positional arg, advancing the cursor. Shared preamble of
    /// [`key_str`](Self::key_str) and [`key_int`](Self::key_int).
    fn next_arg(&mut self) -> Result<&'a KdlValue> {
        let v = *self.positional().get(self.args_used).ok_or_else(|| {
            anyhow!(
                "`{}` is missing a required argument",
                self.node.name().value()
            )
        })?;
        self.args_used += 1;
        Ok(v)
    }

    /// Next positional arg as an owned string (advances the arg cursor).
    pub(crate) fn key_str(&mut self) -> Result<String> {
        let name = self.node.name().value().to_string();
        Ok(self
            .next_arg()?
            .as_string()
            .ok_or_else(|| anyhow!("`{name}` argument must be a string"))?
            .to_string())
    }

    /// Next positional arg as a range-checked integer (advances the arg cursor).
    pub(crate) fn key_int<T: TryFrom<i128>>(&mut self) -> Result<T> {
        let name = self.node.name().value().to_string();
        let i = self
            .next_arg()?
            .as_integer()
            .ok_or_else(|| anyhow!("`{name}` argument must be an integer"))?;
        T::try_from(i).map_err(|_| anyhow!("`{name}` argument {i} out of range"))
    }

    /// All remaining positional args as strings (consumes them). For list nodes.
    fn rest_strings(&mut self) -> Result<Vec<String>> {
        let args = self.positional();
        let mut out = Vec::new();
        for v in &args[self.args_used..] {
            out.push(
                v.as_string()
                    .ok_or_else(|| {
                        anyhow!("`{}` arguments must be strings", self.node.name().value())
                    })?
                    .to_string(),
            );
        }
        self.args_used = args.len();
        Ok(out)
    }

    pub(crate) fn opt_str(&mut self, key: &str) -> Result<Option<String>> {
        self.props_used.insert(key.to_string());
        self.node
            .get(key)
            .map(|v| {
                v.as_string()
                    .ok_or_else(|| anyhow!("property `{key}` must be a string"))
                    .map(str::to_string)
            })
            .transpose()
    }

    pub(crate) fn opt_int<T: TryFrom<i128>>(&mut self, key: &str) -> Result<Option<T>> {
        self.props_used.insert(key.to_string());
        self.node
            .get(key)
            .map(|v| {
                let i = v
                    .as_integer()
                    .ok_or_else(|| anyhow!("property `{key}` must be an integer"))?;
                T::try_from(i).map_err(|_| anyhow!("property `{key}` value {i} out of range"))
            })
            .transpose()
    }

    pub(crate) fn req_int<T: TryFrom<i128>>(&mut self, key: &str) -> Result<T> {
        self.opt_int(key)?.ok_or_else(|| {
            anyhow!(
                "`{}` missing required property `{key}`",
                self.node.name().value()
            )
        })
    }

    pub(crate) fn opt_bool(&mut self, key: &str) -> Result<Option<bool>> {
        self.props_used.insert(key.to_string());
        self.node
            .get(key)
            .map(|v| {
                v.as_bool()
                    .ok_or_else(|| anyhow!("property `{key}` must be a boolean"))
            })
            .transpose()
    }

    /// All child nodes with this name (marks the name consumed).
    pub(crate) fn children(&mut self, name: &'static str) -> Vec<&'a KdlNode> {
        self.children_used.insert(name);
        match self.node.children() {
            None => Vec::new(),
            Some(doc) => doc
                .nodes()
                .iter()
                .filter(|n| n.name().value() == name)
                .collect(),
        }
    }

    /// All child nodes whose name satisfies `matches` (marks them consumed).
    ///
    /// The name-based [`children`](Self::children) cannot express a computed child name — a
    /// sub-block is spelled `nr257`, one distinct node name per band. `label` names the shape
    /// in diagnostics (e.g. `"nr<band>"`).
    pub(crate) fn children_matching(
        &mut self,
        label: &'static str,
        matches: fn(&str) -> bool,
    ) -> Vec<&'a KdlNode> {
        self.child_predicates.push((label, matches));
        match self.node.children() {
            None => Vec::new(),
            Some(doc) => doc
                .nodes()
                .iter()
                .filter(|n| matches(n.name().value()))
                .collect(),
        }
    }

    /// Zero-or-one child node with this name.
    pub(crate) fn opt_child(&mut self, name: &'static str) -> Result<Option<&'a KdlNode>> {
        let mut kids = self.children(name);
        if kids.len() > 1 {
            bail!(
                "`{}` has more than one `{name}` child",
                self.node.name().value()
            );
        }
        Ok(kids.pop())
    }

    /// Error on any positional arg, property, or child node not consumed above, and on any
    /// property repeated that is not read through [`repeated_int`](Self::repeated_int).
    ///
    /// The duplicate check is what makes the reader honest about hand edits. `node.get(key)`
    /// returns the *last* matching entry, and each `opt_*` reader then marks the key consumed,
    /// so a duplicated property used to be silently last-wins with nothing left for `finish` to
    /// object to — and the shadowed entry was never even type-checked, so `mcc="oops" mcc=310`
    /// parsed clean. Since `nr.kdl`/`lte.kdl` are the only editing surface in the tool, that
    /// turned a duplicated line into silent data loss.
    ///
    /// The check is unconditional: no property in either document is multi-valued. It once
    /// exempted keys read through a `repeated_int` reader — the per-CC feature list, spelled as
    /// repeated `dl-feature=` entries — but that list is now one comma-separated value, so both
    /// the reader and the exemption are gone.
    pub(crate) fn finish(self) -> Result<()> {
        let total = self.positional().len();
        if self.args_used < total {
            bail!(
                "`{}` has {} unexpected extra argument(s)",
                self.node.name().value(),
                total - self.args_used
            );
        }
        let mut seen = BTreeSet::new();
        for entry in self.node.entries() {
            if let Some(name) = entry.name() {
                if !self.props_used.contains(name.value()) {
                    bail!(
                        "`{}` has unknown property `{}`",
                        self.node.name().value(),
                        name.value()
                    );
                }
                if !seen.insert(name.value().to_string()) {
                    bail!(
                        "`{}` sets property `{}` more than once; only the last value would be \
                         read, silently discarding the others",
                        self.node.name().value(),
                        name.value()
                    );
                }
            }
        }
        if let Some(doc) = self.node.children() {
            for child in doc.nodes() {
                let cn = child.name().value();
                // Claimed either by an exact name (`children`) or by a shape
                // (`children_matching`) — a computed child name has no fixed spelling to
                // record, so the predicate itself is what marks it known.
                let claimed = self.children_used.contains(cn)
                    || self.child_predicates.iter().any(|(_, matches)| matches(cn));
                if !claimed {
                    bail!(
                        "`{}` has unknown child node `{cn}`",
                        self.node.name().value()
                    );
                }
            }
        }
        Ok(())
    }
}

/// Read the single positional value of a scalar-list child (e.g. `bitmask-carriers "a" "b"`).
pub(crate) fn read_str_list(node: &KdlNode) -> Result<Vec<String>> {
    let mut r = NodeReader::new(node);
    let list = r.rest_strings()?;
    r.finish()?;
    Ok(list)
}

// ---- plmn node codec ----
use crate::mapping::Plmn;

/// Parse a decimal-only MCC/MNC field into its integer value; `None` if it
/// contains any non-decimal digit (e.g. the `ff` wildcard or a hex nibble).
fn decimal_field(s: &str) -> Option<u32> {
    if !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()) {
        s.parse().ok()
    } else {
        None
    }
}

/// Build a `plmn mcc=… mnc=…` node from a canonical `Plmn` string (`"mcc-mnc"`).
/// Wildcard MNC (`ff`) omits `mnc=`; a leading-zero 3-digit MNC adds `mnc-digits=3`.
pub(crate) fn plmn_to_node(plmn: &str) -> Result<KdlNode> {
    let (mcc, mnc) = plmn
        .split_once('-')
        .ok_or_else(|| anyhow!("PLMN `{plmn}` is not `mcc-mnc`"))?;
    let mcc_val =
        decimal_field(mcc).ok_or_else(|| anyhow!("PLMN `{plmn}` has a non-decimal MCC"))?;
    let mut node = KdlNode::new(carrier::PLMN);
    node.push(KdlEntry::new_prop(plmn_keys::MCC, i128::from(mcc_val)));
    if !mnc.eq_ignore_ascii_case("ff") {
        let mnc_val =
            decimal_field(mnc).ok_or_else(|| anyhow!("PLMN `{plmn}` has a non-decimal MNC"))?;
        node.push(KdlEntry::new_prop(plmn_keys::MNC, i128::from(mnc_val)));
        if mnc.len() == 3 && mnc_val < 100 {
            node.push(KdlEntry::new_prop(plmn_keys::MNC_DIGITS, 3i128));
        }
    }
    Ok(node)
}

/// The zero-padding width for one present MNC value: an explicit `mnc-digits=3` in the
/// source pins 3 (the only legal override — it exists solely to preserve a leading-zero
/// 3-digit MNC that would otherwise print as 2 digits); otherwise infer from magnitude
/// (`>= 100` needs 3 digits, else 2).
fn mnc_width(v: u32, mnc_digits: Option<u32>) -> Result<usize> {
    match mnc_digits {
        None => Ok(if v >= 100 { 3 } else { 2 }),
        Some(3) => Ok(3),
        Some(other) => bail!("`plmn` `mnc-digits` must be 3, got {other}"),
    }
}

/// Read a `plmn` node back to its canonical `Plmn` string, validating via `Plmn::from_str`.
pub(crate) fn read_plmn(node: &KdlNode) -> Result<String> {
    let mut r = NodeReader::new(node);
    let mcc: u32 = r.req_int(plmn_keys::MCC)?;
    let mnc: Option<u32> = r.opt_int(plmn_keys::MNC)?;
    let mnc_digits: Option<u32> = r.opt_int(plmn_keys::MNC_DIGITS)?;
    r.finish()?;
    let mcc_s = format!("{mcc:03}");
    let mnc_s = match mnc {
        None => {
            if mnc_digits.is_some() {
                bail!("`plmn` has `mnc-digits` without `mnc`");
            }
            "ff".to_string()
        }
        Some(v) => {
            let width = mnc_width(v, mnc_digits)?;
            format!("{v:0width$}")
        }
    };
    let s = format!("{mcc_s}-{mnc_s}");
    s.parse::<Plmn>()
        .map_err(|e| anyhow!("`plmn` reconstructs to invalid PLMN `{s}`: {e}"))?;
    Ok(s)
}

#[cfg(test)]
mod reader_tests {

    #[test]
    fn autoformat_keeps_leading_positional_arg() {
        // Several nr.kdl/lte.kdl nodes carry a sole leading positional arg — `carrier <name>`,
        // `bitmask-fingerprint <n>`, `profile "<n>"`, `file "<n>"`. (Sub-blocks no longer do:
        // their band is part of the node name.) The autoformatter must keep that arg leading
        // and stay idempotent, or a reformatted source would misparse.
        let src = "carrier ALPHA bitmask-id=1 tier=main\n";
        let mut doc: kdl::KdlDocument = src.parse().unwrap();
        doc.autoformat();
        let once = doc.to_string();
        assert!(
            once.contains("carrier ALPHA"),
            "the leading positional arg stays leading: {once}"
        );
        let mut doc2: kdl::KdlDocument = once.parse().unwrap();
        doc2.autoformat();
        assert_eq!(
            once,
            doc2.to_string(),
            "autoformat idempotent with a leading positional arg"
        );
    }
}

#[cfg(test)]
mod plmn_tests {
    use super::*;

    fn round_trip(plmn: &str) -> String {
        let node = plmn_to_node(plmn).expect("to_node");
        read_plmn(&node).expect("from_node")
    }

    #[test]
    fn plmn_forms_round_trip() {
        for p in [
            "311-480", "310-260", "202-01", "310-04", "310-004", "334-030", "228-ff",
        ] {
            assert_eq!(round_trip(p), p, "round-trip {p}");
        }
    }

    #[test]
    fn wildcard_omits_mnc() {
        let node = plmn_to_node("228-ff").unwrap();
        assert!(node.get("mnc").is_none(), "wildcard must omit mnc=");
        assert_eq!(node.get("mcc").unwrap().as_integer(), Some(228));
    }

    #[test]
    fn leading_zero_three_digit_gets_marker() {
        let node = plmn_to_node("310-004").unwrap();
        assert_eq!(node.get("mnc").unwrap().as_integer(), Some(4));
        assert_eq!(node.get("mnc-digits").unwrap().as_integer(), Some(3));
    }

    #[test]
    fn two_digit_and_big_three_digit_omit_marker() {
        assert!(plmn_to_node("310-04").unwrap().get("mnc-digits").is_none());
        assert!(plmn_to_node("302-220").unwrap().get("mnc-digits").is_none());
    }

    #[test]
    fn mnc_digits_without_mnc_is_rejected() {
        let mut n = KdlNode::new("plmn");
        n.push(KdlEntry::new_prop("mcc", 310i128));
        n.push(KdlEntry::new_prop("mnc-digits", 3i128));
        assert!(read_plmn(&n).is_err());
    }
}
