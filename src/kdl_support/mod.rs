//! Crate-level KDL toolkit: strict `NodeReader` combinator + writer helpers,
//! shared by the compiler's (de)serializers.

use anyhow::{Result, anyhow, bail};
use kdl::{KdlDocument, KdlEntry, KdlNode, KdlValue};
use std::collections::BTreeSet;

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
pub(crate) struct NodeReader<'a> {
    node: &'a KdlNode,
    args_used: usize,
    props_used: BTreeSet<&'static str>,
    children_used: BTreeSet<&'static str>,
    /// Predicates for child names computed rather than fixed.
    child_predicates: Vec<fn(&str) -> bool>,
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
    /// [`arg_str`](Self::arg_str) and [`arg_int`](Self::arg_int).
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
    pub(crate) fn arg_str(&mut self) -> Result<String> {
        let name = self.node.name().value().to_string();
        Ok(self
            .next_arg()?
            .as_string()
            .ok_or_else(|| anyhow!("`{name}` argument must be a string"))?
            .to_string())
    }

    /// Next positional arg as a range-checked integer (advances the arg cursor).
    pub(crate) fn arg_int<T: TryFrom<i128>>(&mut self) -> Result<T> {
        let name = self.node.name().value().to_string();
        let i = self
            .next_arg()?
            .as_integer()
            .ok_or_else(|| anyhow!("`{name}` argument must be an integer"))?;
        T::try_from(i).map_err(|_| anyhow!("`{name}` argument {i} out of range"))
    }

    /// The next positional arg as an owned string, or `None` when the node has no more.
    ///
    /// The optional counterpart of [`arg_str`](Self::arg_str), for a node whose trailing
    /// argument is omitted rather than defaulted — a sub-block's UL direction, say, where
    /// absence means bandwidth class 0.
    pub(crate) fn opt_arg_str(&mut self) -> Result<Option<String>> {
        let Some(value) = self.positional().get(self.args_used).copied() else {
            return Ok(None);
        };
        self.args_used += 1;
        Ok(Some(
            value
                .as_string()
                .ok_or_else(|| anyhow!("`{}` argument must be a string", self.node.name().value()))?
                .to_string(),
        ))
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

    pub(crate) fn opt_str(&mut self, key: &'static str) -> Result<Option<String>> {
        self.props_used.insert(key);
        self.node
            .get(key)
            .map(|v| {
                v.as_string()
                    .ok_or_else(|| anyhow!("property `{key}` must be a string"))
                    .map(str::to_string)
            })
            .transpose()
    }

    pub(crate) fn opt_int<T: TryFrom<i128>>(&mut self, key: &'static str) -> Result<Option<T>> {
        self.props_used.insert(key);
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

    pub(crate) fn req_int<T: TryFrom<i128>>(&mut self, key: &'static str) -> Result<T> {
        self.opt_int(key)?.ok_or_else(|| {
            anyhow!(
                "`{}` missing required property `{key}`",
                self.node.name().value()
            )
        })
    }

    pub(crate) fn opt_bool(&mut self, key: &'static str) -> Result<Option<bool>> {
        self.props_used.insert(key);
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
    /// sub-block is spelled `n257`, one distinct node name per band.
    pub(crate) fn children_matching(&mut self, matches: fn(&str) -> bool) -> Vec<&'a KdlNode> {
        self.child_predicates.push(matches);
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
    /// repeated property. Source properties are singular, including those claimed by a reader.
    ///
    /// The duplicate check is what makes the reader honest about hand edits. `node.get(key)`
    /// returns the *last* matching entry, and each `opt_*` reader then marks the key consumed,
    /// so accepting duplicate properties would silently discard earlier values, even ill-typed
    /// ones such as the first value in `mcc="oops" mcc=310`.
    ///
    /// The check is unconditional: no source property is multi-valued.
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
                if !seen.insert(name.value()) {
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
                    || self.child_predicates.iter().any(|matches| matches(cn));
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

#[cfg(test)]
mod reader_tests {

    #[test]
    fn autoformat_keeps_leading_positional_arg() {
        // Several source-document nodes carry a sole leading positional arg — `carrier <name>`,
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

/// `NodeReader` combinator behaviour in isolation. The node names in these fixtures (`cr`, …)
/// are illustrative scaffolding for the combinators, not source-format vocabulary — nothing here
/// tracks the document's node names, so a grep for an old spelling that lands in this module has
/// found a parser fixture, not a missed rename.
#[cfg(test)]
mod combinator_tests {
    use super::*;

    fn node(text: &str) -> kdl::KdlNode {
        let doc: KdlDocument = text.parse().unwrap();
        doc.nodes().first().unwrap().clone()
    }

    #[test]
    fn reads_key_props_and_children_then_finishes() {
        let n = node("cr VZW bi=1 t=main {\n    ps \"311-480\"\n}\n");
        let mut r = NodeReader::new(&n);
        assert_eq!(r.arg_str().unwrap(), "VZW");
        assert_eq!(r.opt_int::<i64>("bi").unwrap(), Some(1));
        assert_eq!(r.opt_str("t").unwrap().as_deref(), Some("main"));
        let plmns = r.opt_child("ps").unwrap().unwrap();
        assert_eq!(read_str_list(plmns).unwrap(), vec!["311-480".to_string()]);
        r.finish().unwrap();
    }

    #[test]
    fn finish_rejects_unknown_property() {
        let n = node("cr VZW bogus=9\n");
        let mut r = NodeReader::new(&n);
        assert_eq!(r.arg_str().unwrap(), "VZW");
        assert!(r.finish().is_err());
    }

    /// The source document is the only editing surface in the tool, and a duplicated property
    /// was silently last-wins: `node.get(key)` returns the last entry and `props_used` then
    /// marks the key consumed, so `finish()` could not object either. A hand edit that
    /// duplicated a line lost a value with no diagnostic.
    #[test]
    fn finish_rejects_a_duplicate_property() {
        let n = node("node 78 dl-bw-class=1 dl-bw-class=2\n");
        let mut r = NodeReader::new(&n);
        assert_eq!(r.arg_int::<u16>().unwrap(), 78);
        assert_eq!(r.opt_int::<u8>("dl-bw-class").unwrap(), Some(2));

        let error = r.finish().unwrap_err().to_string();

        assert!(error.contains("dl-bw-class"), "{error}");
        assert!(error.contains("more than once"), "{error}");
    }

    /// The shadowed entry is never type-checked either, so a duplicate could smuggle an
    /// outright type error past the reader.
    #[test]
    fn finish_rejects_a_duplicate_property_whose_shadowed_value_is_ill_typed() {
        let n = node("p mcc=\"oops\" mcc=310\n");
        let mut r = NodeReader::new(&n);
        assert_eq!(r.opt_int::<u16>("mcc").unwrap(), Some(310));

        assert!(r.finish().is_err());
    }

    /// Dynamic child names (a sub-block is spelled `n257`, not `n` + a positional) cannot be looked up by
    /// name, so `NodeReader` needs a predicate — and `finish()` must treat what the predicate
    /// consumed as known, or every sub-block would read as an unknown child.
    #[test]
    fn children_matching_consumes_by_predicate() {
        let n = node("c {\n    n257 x=1\n    n41 x=2\n    B66 x=3\n}\n");
        let mut r = NodeReader::new(&n);

        let nr =
            r.children_matching(|name| name.strip_prefix('n').is_some_and(|band| !band.is_empty()));
        assert_eq!(nr.len(), 2);
        let lte =
            r.children_matching(|name| name.strip_prefix('B').is_some_and(|band| !band.is_empty()));
        assert_eq!(lte.len(), 1);

        r.finish().expect("predicate-consumed children are known");
    }

    /// A child no predicate claimed is still rejected — the strictness must survive the move
    /// from name-set membership to pattern matching.
    #[test]
    fn finish_still_rejects_a_child_no_predicate_claimed() {
        let n = node("c {\n    n257 x=1\n    mystery 1\n}\n");
        let mut r = NodeReader::new(&n);
        let _ = r.children_matching(|name| name.starts_with('n'));

        let error = r.finish().unwrap_err().to_string();

        assert!(error.contains("mystery"), "{error}");
    }

    #[test]
    fn finish_rejects_unknown_child() {
        let n = node("cr VZW {\n    mystery 1\n}\n");
        let mut r = NodeReader::new(&n);
        assert_eq!(r.arg_str().unwrap(), "VZW");
        assert!(r.finish().is_err());
    }

    #[test]
    fn opt_int_range_checks() {
        let n = node("x v=-1\n");
        let mut r = NodeReader::new(&n);
        assert!(r.opt_int::<u64>("v").is_err());
    }
}
