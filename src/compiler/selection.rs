use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Context, Result, bail, ensure};
use compact_str::CompactString;

use crate::model::PHONE_MODELS;

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) enum Sku {
    Legacy,
    // Inline (SSO) string: model codes are 5 chars and this variant is a set key touched
    // hundreds of millions of times per decompose (the selection-algebra hot path), so keeping
    // the bytes inline avoids per-clone heap alloc and the compare-time pointer chase. Ord is
    // byte-lexical, identical to `String`, so canonical output is unchanged.
    Model(CompactString),
    Prime(u64),
    Lte(u64),
}

impl Sku {
    pub(crate) fn token(&self) -> String {
        match self {
            Self::Legacy => "legacy".into(),
            Self::Model(code) => code.to_string(),
            Self::Prime(anchor) => format!("prime:{anchor}"),
            Self::Lte(id) => format!("lte:{id}"),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SelectionRect {
    pub(crate) carriers: Option<Vec<String>>,
    pub(crate) skus: Option<Vec<String>>,
}

/// Domain-local dense ids for carriers and skus. Assigned in sorted order by `NrDomain::new`, so
/// an id's numeric order equals its value's `Ord` — the invariant that lets the id-keyed
/// `canonical_selection` emit rectangles in the exact string / `Sku` order (byte-identical output)
/// while the hot membership sets compare 4-byte keys instead of 24-byte inline strings.
pub(crate) type CarrierId = u16;
pub(crate) type SkuId = u16;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct NrDomain {
    // Interners: `carrier_names[id]` / `sku_values[id]`, sorted ascending (id order == value
    // order); `binary_search` is the reverse value->id lookup.
    carrier_names: Vec<CompactString>,
    sku_values: Vec<Sku>,
    // Hot membership sets, id-keyed. Projections precomputed once at construction:
    // `from_selection` / `canonical_selection` run per-combo across the validate passes and would
    // otherwise rebuild these. `carriers`/`skus` are the full id ranges; `rows` groups skus per
    // carrier.
    members: BTreeSet<(CarrierId, SkuId)>,
    carriers: BTreeSet<CarrierId>,
    skus: BTreeSet<SkuId>,
    rows: BTreeMap<CarrierId, BTreeSet<SkuId>>,
}

impl NrDomain {
    pub(crate) fn new(members: BTreeSet<(CompactString, Sku)>) -> Self {
        let carrier_names: Vec<CompactString> = members
            .iter()
            .map(|(carrier, _)| carrier.clone())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        let sku_values: Vec<Sku> = members
            .iter()
            .map(|(_, sku)| sku.clone())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        assert!(
            carrier_names.len() <= CarrierId::MAX as usize + 1
                && sku_values.len() <= SkuId::MAX as usize + 1,
            "NR domain exceeds the carrier/sku interner id space"
        );
        let member_ids: BTreeSet<(CarrierId, SkuId)> = members
            .iter()
            .map(|(carrier, sku)| {
                let carrier = carrier_names
                    .binary_search(carrier)
                    .expect("carrier collected from members")
                    as CarrierId;
                let sku = sku_values
                    .binary_search(sku)
                    .expect("sku collected from members") as SkuId;
                (carrier, sku)
            })
            .collect();
        let carriers = (0..carrier_names.len() as CarrierId).collect();
        let skus = (0..sku_values.len() as SkuId).collect();
        let mut rows = BTreeMap::<CarrierId, BTreeSet<SkuId>>::new();
        for &(carrier, sku) in &member_ids {
            rows.entry(carrier).or_default().insert(sku);
        }
        Self {
            carrier_names,
            sku_values,
            members: member_ids,
            carriers,
            skus,
            rows,
        }
    }

    fn carrier_id(&self, carrier: &str) -> Option<CarrierId> {
        self.carrier_names
            .binary_search_by(|candidate| candidate.as_str().cmp(carrier))
            .ok()
            .map(|index| index as CarrierId)
    }

    fn sku_id(&self, sku: &Sku) -> Option<SkuId> {
        self.sku_values
            .binary_search(sku)
            .ok()
            .map(|index| index as SkuId)
    }

    /// Intern a `(carrier, sku)` probe (built once, then tested against many combos in
    /// `selected_payloads`). `None` if either value is outside the domain — no relation can
    /// contain it.
    pub(crate) fn probe(&self, carrier: &str, sku: &Sku) -> Option<(CarrierId, SkuId)> {
        Some((self.carrier_id(carrier)?, self.sku_id(sku)?))
    }

    /// Intern a raw `(carrier, sku)` member set (every pair must already be a domain member) into
    /// an id-keyed relation. Used by ingest, which builds each payload's relation from the same
    /// values the domain was built from.
    pub(crate) fn relation(&self, members: BTreeSet<(CompactString, Sku)>) -> NrRelation {
        NrRelation(
            members
                .iter()
                .map(|(carrier, sku)| {
                    (
                        self.carrier_id(carrier)
                            .expect("relation carrier is a domain member"),
                        self.sku_id(sku).expect("relation sku is a domain member"),
                    )
                })
                .collect(),
        )
    }

    #[cfg(test)]
    pub(crate) fn denorm_members(&self) -> BTreeSet<(CompactString, Sku)> {
        self.members
            .iter()
            .map(|&(carrier, sku)| self.denorm(carrier, sku))
            .collect()
    }

    #[cfg(test)]
    pub(crate) fn denorm_relation(&self, relation: &NrRelation) -> BTreeSet<(CompactString, Sku)> {
        relation
            .0
            .iter()
            .map(|&(carrier, sku)| self.denorm(carrier, sku))
            .collect()
    }

    #[cfg(test)]
    fn denorm(&self, carrier: CarrierId, sku: SkuId) -> (CompactString, Sku) {
        (
            self.carrier_names[carrier as usize].clone(),
            self.sku_values[sku as usize].clone(),
        )
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct LteDomain(BTreeSet<Sku>);

impl LteDomain {
    pub(crate) fn new(members: BTreeSet<Sku>) -> Self {
        Self(members)
    }

    pub(crate) fn iter(&self) -> impl Iterator<Item = &Sku> {
        self.0.iter()
    }
}

/// One named carrier resolved to its domain id, or an error naming the 1-based rectangle
/// index — every `from_selection` error is rectangle-relative, never file-relative.
fn resolve_rectangle_carrier(domain: &NrDomain, carrier: &str, index: usize) -> Result<CarrierId> {
    domain.carrier_id(carrier).with_context(|| {
        format!(
            "selection rectangle {} references unknown carrier `{carrier}`",
            index + 1
        )
    })
}

/// The carrier-id axis for one selection rectangle: every domain carrier when `carriers` is
/// absent (a wildcard axis), or the named carriers translated to ids.
fn rectangle_carrier_axis(
    domain: &NrDomain,
    carriers: Option<&[String]>,
    index: usize,
) -> Result<BTreeSet<CarrierId>> {
    match carriers {
        Some(carriers) => {
            ensure!(
                !carriers.is_empty(),
                "selection rectangle {} has an empty carriers axis",
                index + 1
            );
            carriers
                .iter()
                .map(|carrier| resolve_rectangle_carrier(domain, carrier, index))
                .collect()
        }
        None => Ok(domain.carriers.clone()),
    }
}

/// The sku-id axis for one selection rectangle: every domain sku when `skus` is absent (a
/// wildcard axis), or the named tokens parsed and translated to ids.
fn rectangle_sku_axis(
    domain: &NrDomain,
    skus: Option<&[String]>,
    index: usize,
) -> Result<BTreeSet<SkuId>> {
    match skus {
        Some(tokens) => {
            ensure!(
                !tokens.is_empty(),
                "selection rectangle {} has an empty skus axis",
                index + 1
            );
            tokens
                .iter()
                .map(|token| parse_nr_sku(token, domain))
                .collect()
        }
        None => Ok(domain.skus.clone()),
    }
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct NrRelation(BTreeSet<(CarrierId, SkuId)>);

impl NrRelation {
    /// This relation's `(carrier_id, sku_id)` members in ascending order. Consumed once by
    /// `NrSelectionIndex::build`, which inverts these into an O(1) `selected_payloads` lookup —
    /// superseding the per-combo `contains` probe interned via `NrDomain::probe`.
    pub(crate) fn members(&self) -> impl Iterator<Item = (CarrierId, SkuId)> + '_ {
        self.0.iter().copied()
    }

    pub(crate) fn from_selection(
        domain: &NrDomain,
        selection: Option<&[SelectionRect]>,
    ) -> Result<Self> {
        let Some(rectangles) = selection else {
            return Ok(Self(domain.members.clone()));
        };
        ensure!(
            !rectangles.is_empty(),
            "selection must not be an empty array"
        );

        let mut relation = BTreeSet::new();

        for (index, rectangle) in rectangles.iter().enumerate() {
            ensure!(
                rectangle.carriers.is_some() || rectangle.skus.is_some(),
                "selection rectangle {} is an empty object",
                index + 1
            );

            let carriers = rectangle_carrier_axis(domain, rectangle.carriers.as_deref(), index)?;
            let skus = rectangle_sku_axis(domain, rectangle.skus.as_deref(), index)?;

            let expanded: BTreeSet<(CarrierId, SkuId)> = domain
                .members
                .iter()
                .filter(|(carrier, sku)| carriers.contains(carrier) && skus.contains(sku))
                .copied()
                .collect();
            ensure!(
                !expanded.is_empty(),
                "selection rectangle {} has an empty intersection with the NR domain",
                index + 1
            );
            relation.extend(expanded);
        }

        Ok(Self(relation))
    }

    pub(crate) fn canonical_selection(
        &self,
        domain: &NrDomain,
    ) -> Result<Option<Vec<SelectionRect>>> {
        ensure!(
            self.0.is_subset(&domain.members),
            "NR relation contains members outside the NR domain"
        );
        if self.0 == domain.members {
            return Ok(None);
        }
        ensure!(
            !self.0.is_empty(),
            "an empty NR relation cannot be represented as a selection"
        );

        let mut selected_rows = BTreeMap::<CarrierId, BTreeSet<SkuId>>::new();
        for &(carrier, sku) in &self.0 {
            selected_rows.entry(carrier).or_default().insert(sku);
        }

        let mut groups = BTreeMap::<Option<BTreeSet<SkuId>>, BTreeSet<CarrierId>>::new();
        for (carrier, selected_skus) in selected_rows {
            let eligible_skus = &domain.rows[&carrier];
            let constraint = (selected_skus != *eligible_skus).then_some(selected_skus);
            groups.entry(constraint).or_default().insert(carrier);
        }

        let mut canonical = Vec::with_capacity(groups.len());
        for (skus, carriers) in groups {
            let carriers = (carriers != domain.carriers).then_some(carriers);
            ensure!(
                skus.is_some() || carriers.is_some(),
                "canonical NR selection would contain an empty object"
            );
            canonical.push(CanonicalRect { skus, carriers });
        }
        canonical.sort_by(|left, right| {
            (&left.skus, &left.carriers).cmp(&(&right.skus, &right.carriers))
        });

        Ok(Some(
            canonical
                .into_iter()
                .map(|rect| rect.into_selection_rect(domain))
                .collect(),
        ))
    }
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct LteRelation(BTreeSet<Sku>);

impl LteRelation {
    pub(crate) fn new(members: BTreeSet<Sku>) -> Self {
        Self(members)
    }

    pub(crate) fn from_selection(
        domain: &LteDomain,
        selection: Option<&[SelectionRect]>,
    ) -> Result<Self> {
        let Some(rectangles) = selection else {
            return Ok(Self(domain.0.clone()));
        };
        ensure!(
            !rectangles.is_empty(),
            "selection must not be an empty array"
        );

        let mut relation = BTreeSet::new();
        for (index, rectangle) in rectangles.iter().enumerate() {
            ensure!(
                rectangle.carriers.is_none(),
                "LTE selection rectangle {} must not contain carriers",
                index + 1
            );
            let Some(tokens) = &rectangle.skus else {
                bail!("selection rectangle {} is an empty object", index + 1);
            };
            ensure!(
                !tokens.is_empty(),
                "selection rectangle {} has an empty skus axis",
                index + 1
            );

            let expanded = tokens
                .iter()
                .map(|token| parse_lte_sku(token, &domain.0))
                .collect::<Result<BTreeSet<_>>>()?;
            ensure!(
                !expanded.is_empty(),
                "selection rectangle {} has an empty intersection with the LTE domain",
                index + 1
            );
            relation.extend(expanded);
        }

        Ok(Self(relation))
    }

    pub(crate) fn canonical_selection(
        &self,
        domain: &LteDomain,
    ) -> Result<Option<Vec<SelectionRect>>> {
        ensure!(
            self.0.is_subset(&domain.0),
            "LTE relation contains members outside the LTE domain"
        );
        if self.0 == domain.0 {
            return Ok(None);
        }
        ensure!(
            !self.0.is_empty(),
            "an empty LTE relation cannot be represented as a selection"
        );

        Ok(Some(vec![SelectionRect {
            carriers: None,
            skus: Some(self.0.iter().map(Sku::token).collect()),
        }]))
    }

    pub(crate) fn iter(&self) -> impl Iterator<Item = &Sku> {
        self.0.iter()
    }
}

struct CanonicalRect {
    skus: Option<BTreeSet<SkuId>>,
    carriers: Option<BTreeSet<CarrierId>>,
}

impl CanonicalRect {
    fn into_selection_rect(self, domain: &NrDomain) -> SelectionRect {
        SelectionRect {
            carriers: self.carriers.map(|carriers| {
                carriers
                    .into_iter()
                    .map(|carrier| domain.carrier_names[carrier as usize].to_string())
                    .collect()
            }),
            skus: self.skus.map(|skus| {
                skus.into_iter()
                    .map(|sku| domain.sku_values[sku as usize].token())
                    .collect()
            }),
        }
    }
}

fn parse_nr_sku(token: &str, domain: &NrDomain) -> Result<SkuId> {
    let sku = if token == "legacy" {
        Sku::Legacy
    } else if token.starts_with("lte:") {
        bail!("SKU token `{token}` is not valid in an NR selection");
    } else if let Some(decimal) = token.strip_prefix("prime:") {
        Sku::Prime(parse_shortest_decimal(token, decimal)?)
    } else {
        ensure!(
            PHONE_MODELS.iter().any(|model| model.code == token),
            "unknown model `{token}` in NR selection"
        );
        Sku::Model(token.into())
    };
    domain
        .sku_id(&sku)
        .with_context(|| format!("SKU token `{token}` is not eligible in this NR domain"))
}

fn parse_lte_sku(token: &str, eligible: &BTreeSet<Sku>) -> Result<Sku> {
    let sku = if token == "legacy" || token.starts_with("prime:") {
        bail!("SKU token `{token}` is not valid in an LTE selection");
    } else if let Some(decimal) = token.strip_prefix("lte:") {
        Sku::Lte(parse_shortest_decimal(token, decimal)?)
    } else {
        ensure!(
            PHONE_MODELS.iter().any(|model| model.code == token),
            "unknown model `{token}` in LTE selection"
        );
        Sku::Model(token.into())
    };
    ensure!(
        eligible.contains(&sku),
        "SKU token `{token}` is not eligible in this LTE domain"
    );
    Ok(sku)
}

fn parse_shortest_decimal(token: &str, decimal: &str) -> Result<u64> {
    super::parse_shortest_u64(decimal)
        .with_context(|| format!("SKU token `{token}` must use a shortest decimal value"))
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};

    use super::{CompactString, LteDomain, LteRelation, NrDomain, NrRelation, SelectionRect, Sku};

    fn rect(carriers: Option<&[&str]>, skus: Option<&[&str]>) -> SelectionRect {
        SelectionRect {
            carriers: carriers.map(|values| values.iter().map(|value| (*value).into()).collect()),
            skus: skus.map(|values| values.iter().map(|value| (*value).into()).collect()),
        }
    }

    fn nr_domain(pairs: &[(&str, Sku)]) -> NrDomain {
        NrDomain::new(
            pairs
                .iter()
                .map(|(carrier, sku)| ((*carrier).into(), sku.clone()))
                .collect(),
        )
    }

    fn nr_relation(domain: &NrDomain, pairs: &[(&str, Sku)]) -> NrRelation {
        domain.relation(
            pairs
                .iter()
                .map(|(carrier, sku)| ((*carrier).into(), sku.clone()))
                .collect(),
        )
    }

    fn lte_domain(skus: &[Sku]) -> LteDomain {
        LteDomain::new(skus.iter().cloned().collect())
    }

    fn lte_relation(skus: &[Sku]) -> LteRelation {
        LteRelation::new(skus.iter().cloned().collect())
    }

    fn grid() -> Vec<(CompactString, Sku)> {
        ["A", "B"]
            .into_iter()
            .flat_map(|carrier| {
                [Sku::Legacy, Sku::Model("G2YBB".into()), Sku::Prime(8969)]
                    .into_iter()
                    .map(move |sku| (carrier.into(), sku))
            })
            .collect()
    }

    fn singleton_rectangles(relation: &BTreeSet<(CompactString, Sku)>) -> Vec<SelectionRect> {
        relation
            .iter()
            .map(|(carrier, sku)| SelectionRect {
                carriers: Some(vec![carrier.to_string(), carrier.to_string()]),
                skus: Some(vec![sku.token(), sku.token()]),
            })
            .collect()
    }

    fn row_rectangles(relation: &BTreeSet<(CompactString, Sku)>) -> Vec<SelectionRect> {
        let mut rows = BTreeMap::<String, Vec<Sku>>::new();
        for (carrier, sku) in relation {
            rows.entry(carrier.to_string())
                .or_default()
                .push(sku.clone());
        }
        rows.into_iter()
            .map(|(carrier, mut skus)| {
                skus.reverse();
                let mut tokens: Vec<_> = skus.into_iter().map(|sku| sku.token()).collect();
                tokens.extend(tokens.clone());
                SelectionRect {
                    carriers: Some(vec![carrier.clone(), carrier]),
                    skus: Some(tokens),
                }
            })
            .collect()
    }

    fn column_rectangles(relation: &BTreeSet<(CompactString, Sku)>) -> Vec<SelectionRect> {
        let mut columns = BTreeMap::<Sku, Vec<String>>::new();
        for (carrier, sku) in relation {
            columns
                .entry(sku.clone())
                .or_default()
                .push(carrier.to_string());
        }
        columns
            .into_iter()
            .map(|(sku, mut carriers)| {
                carriers.reverse();
                carriers.extend(carriers.clone());
                SelectionRect {
                    carriers: Some(carriers),
                    skus: Some(vec![sku.token(), sku.token()]),
                }
            })
            .collect()
    }

    #[test]
    fn every_small_relation_has_one_canonical_selection() {
        let grid = grid();
        let mut cases = 0;

        for domain_mask in 1_u64..(1 << grid.len()) {
            let domain_pairs: BTreeSet<_> = grid
                .iter()
                .enumerate()
                .filter(|(index, _)| domain_mask & (1 << index) != 0)
                .map(|(_, pair)| pair.clone())
                .collect();
            let domain = NrDomain::new(domain_pairs.clone());

            let mut relation_mask = domain_mask;
            while relation_mask != 0 {
                cases += 1;
                let relation_pairs: BTreeSet<_> = grid
                    .iter()
                    .enumerate()
                    .filter(|(index, _)| relation_mask & (1 << index) != 0)
                    .map(|(_, pair)| pair.clone())
                    .collect();
                let relation = domain.relation(relation_pairs.clone());
                let canonical = relation.canonical_selection(&domain).unwrap();
                let reparsed = NrRelation::from_selection(&domain, canonical.as_deref()).unwrap();
                assert_eq!(
                    reparsed, relation,
                    "canonical output did not reparse for domain={domain_pairs:?}, relation={relation_pairs:?}"
                );

                let mut singletons = singleton_rectangles(&relation_pairs);
                singletons.reverse();
                let mut duplicated = singletons.clone();
                duplicated.extend(singletons.clone());

                let mut rows = row_rectangles(&relation_pairs);
                rows.reverse();
                let mut columns = column_rectangles(&relation_pairs);
                columns.reverse();
                let mut overlapping = rows.clone();
                overlapping.extend(columns.clone());
                overlapping.extend(rows.clone());
                overlapping.reverse();

                for rectangles in [singletons, duplicated, rows, columns, overlapping] {
                    let parsed = NrRelation::from_selection(&domain, Some(&rectangles)).unwrap();
                    assert_eq!(
                        parsed, relation,
                        "domain={domain_pairs:?}, input={rectangles:?}"
                    );
                    assert_eq!(
                        parsed.canonical_selection(&domain).unwrap(),
                        canonical,
                        "domain={domain_pairs:?}, relation={relation_pairs:?}"
                    );
                }

                relation_mask = (relation_mask - 1) & domain_mask;
            }
        }

        assert_eq!(cases, 665);
    }

    #[test]
    fn overlapping_permuted_lte_rectangles_have_one_canonical_selection() {
        let domain = lte_domain(&[
            Sku::Model("G2YBB".into()),
            Sku::Model("GR83Y".into()),
            Sku::Lte(92),
        ]);
        let rectangles = vec![
            rect(None, Some(&["lte:92", "G2YBB", "lte:92"])),
            rect(None, Some(&["GR83Y", "G2YBB"])),
            rect(None, Some(&["GR83Y"])),
        ];

        let relation = LteRelation::from_selection(&domain, Some(&rectangles)).unwrap();
        assert_eq!(
            relation,
            lte_relation(&[
                Sku::Model("G2YBB".into()),
                Sku::Model("GR83Y".into()),
                Sku::Lte(92),
            ])
        );
        assert_eq!(relation.canonical_selection(&domain).unwrap(), None);
        assert_eq!(
            LteRelation::from_selection(&domain, None).unwrap(),
            relation
        );
    }

    #[test]
    fn omitted_selection_is_the_complete_universe() {
        let domain = nr_domain(&[
            ("B", Sku::Legacy),
            ("B", Sku::Model("G2YBB".into())),
            ("A", Sku::Prime(8969)),
        ]);
        let relation = nr_relation(
            &domain,
            &[
                ("B", Sku::Legacy),
                ("B", Sku::Model("G2YBB".into())),
                ("A", Sku::Prime(8969)),
            ],
        );

        assert_eq!(NrRelation::from_selection(&domain, None).unwrap(), relation);
        assert_eq!(relation.canonical_selection(&domain).unwrap(), None);

        let lte_domain = lte_domain(&[Sku::Model("G2YBB".into()), Sku::Lte(564_260_317)]);
        let lte_relation = lte_relation(&[Sku::Model("G2YBB".into()), Sku::Lte(564_260_317)]);
        assert_eq!(
            LteRelation::from_selection(&lte_domain, None).unwrap(),
            lte_relation
        );
        assert_eq!(lte_relation.canonical_selection(&lte_domain).unwrap(), None);
    }

    #[test]
    fn unrestricted_rows_are_grouped_and_carriers_sort_lexically() {
        let domain = nr_domain(&[
            ("ZETA", Sku::Legacy),
            ("ZETA", Sku::Model("G2YBB".into())),
            ("BETA", Sku::Prime(8969)),
            ("ALPHA", Sku::Legacy),
            ("ALPHA", Sku::Model("G2YBB".into())),
        ]);
        let relation = nr_relation(
            &domain,
            &[
                ("ZETA", Sku::Legacy),
                ("ZETA", Sku::Model("G2YBB".into())),
                ("ALPHA", Sku::Legacy),
                ("ALPHA", Sku::Model("G2YBB".into())),
            ],
        );

        assert_eq!(
            relation.canonical_selection(&domain).unwrap(),
            Some(vec![rect(Some(&["ALPHA", "ZETA"]), None)])
        );
    }

    #[test]
    fn equal_sku_constraints_are_grouped() {
        let skus = [Sku::Legacy, Sku::Model("G2YBB".into()), Sku::Prime(8969)];
        let domain_pairs: Vec<_> = ["A", "B", "C"]
            .into_iter()
            .flat_map(|carrier| skus.iter().cloned().map(move |sku| (carrier, sku)))
            .collect();
        let domain = nr_domain(&domain_pairs);
        let relation = nr_relation(
            &domain,
            &[
                ("A", Sku::Legacy),
                ("A", Sku::Model("G2YBB".into())),
                ("C", Sku::Legacy),
                ("C", Sku::Model("G2YBB".into())),
                ("B", Sku::Prime(8969)),
            ],
        );

        assert_eq!(
            relation.canonical_selection(&domain).unwrap(),
            Some(vec![
                rect(Some(&["A", "C"]), Some(&["legacy", "G2YBB"])),
                rect(Some(&["B"]), Some(&["prime:8969"])),
            ])
        );
    }

    #[test]
    fn a_restricted_sku_group_spanning_all_carriers_omits_carriers() {
        let domain = nr_domain(&[
            ("B", Sku::Legacy),
            ("B", Sku::Model("G2YBB".into())),
            ("A", Sku::Legacy),
            ("A", Sku::Model("G2YBB".into())),
        ]);
        let relation = nr_relation(
            &domain,
            &[
                ("B", Sku::Model("G2YBB".into())),
                ("A", Sku::Model("G2YBB".into())),
            ],
        );

        assert_eq!(
            relation.canonical_selection(&domain).unwrap(),
            Some(vec![rect(None, Some(&["G2YBB"]))])
        );
    }

    #[test]
    fn sku_members_use_semantic_order_not_token_lexical_order() {
        let domain = nr_domain(&[
            ("A", Sku::Prime(688_679)),
            ("A", Sku::Prime(224_309)),
            ("A", Sku::Model("GUL82".into())),
            ("A", Sku::Legacy),
            ("A", Sku::Prime(8969)),
            ("A", Sku::Model("G2YBB".into())),
        ]);
        let relation = nr_relation(
            &domain,
            &[
                ("A", Sku::Prime(224_309)),
                ("A", Sku::Model("GUL82".into())),
                ("A", Sku::Legacy),
                ("A", Sku::Prime(8969)),
                ("A", Sku::Model("G2YBB".into())),
            ],
        );

        assert_eq!(
            relation.canonical_selection(&domain).unwrap(),
            Some(vec![rect(
                None,
                Some(&["legacy", "G2YBB", "GUL82", "prime:8969", "prime:224309",])
            )])
        );

        let lte_domain = lte_domain(&[
            Sku::Lte(4_210_990_300),
            Sku::Model("GUL82".into()),
            Sku::Lte(2_160_127_815),
            Sku::Model("G2YBB".into()),
            Sku::Lte(400_907_661),
        ]);
        let lte_relation = lte_relation(&[
            Sku::Model("GUL82".into()),
            Sku::Lte(2_160_127_815),
            Sku::Model("G2YBB".into()),
            Sku::Lte(400_907_661),
        ]);

        assert_eq!(
            lte_relation.canonical_selection(&lte_domain).unwrap(),
            Some(vec![rect(
                None,
                Some(&["G2YBB", "GUL82", "lte:400907661", "lte:2160127815"])
            )])
        );
    }

    #[test]
    fn rectangle_order_uses_skus_then_carriers_with_omission_first() {
        let domain = nr_domain(&[
            ("A", Sku::Legacy),
            ("A", Sku::Model("G2YBB".into())),
            ("A", Sku::Prime(8969)),
            ("B", Sku::Legacy),
            ("B", Sku::Model("G2YBB".into())),
            ("B", Sku::Prime(8969)),
            ("C", Sku::Legacy),
            ("C", Sku::Model("G2YBB".into())),
            ("C", Sku::Prime(8969)),
        ]);
        let relation = nr_relation(
            &domain,
            &[
                ("C", Sku::Legacy),
                ("C", Sku::Model("G2YBB".into())),
                ("C", Sku::Prime(8969)),
                ("A", Sku::Legacy),
                ("B", Sku::Prime(8969)),
            ],
        );

        assert_eq!(
            relation.canonical_selection(&domain).unwrap(),
            Some(vec![
                rect(Some(&["C"]), None),
                rect(Some(&["A"]), Some(&["legacy"])),
                rect(Some(&["B"]), Some(&["prime:8969"])),
            ])
        );
    }

    #[test]
    fn selection_rejects_empty_structures() {
        let domain = nr_domain(&[("A", Sku::Legacy)]);

        assert!(NrRelation::from_selection(&domain, Some(&[])).is_err());
        assert!(
            NrRelation::from_selection(&domain, Some(&[rect(None, None)]))
                .unwrap_err()
                .to_string()
                .contains("empty object")
        );
        assert!(
            NrRelation::from_selection(
                &domain,
                Some(&[SelectionRect {
                    carriers: Some(Vec::new()),
                    skus: None,
                }]),
            )
            .is_err()
        );
        assert!(
            NrRelation::from_selection(
                &domain,
                Some(&[SelectionRect {
                    carriers: Some(vec!["A".into()]),
                    skus: Some(Vec::new()),
                }]),
            )
            .is_err()
        );
    }

    #[test]
    fn selection_rejects_unknown_and_ineligible_members() {
        let domain = nr_domain(&[("A", Sku::Legacy), ("B", Sku::Model("G2YBB".into()))]);

        let unknown_carrier =
            NrRelation::from_selection(&domain, Some(&[rect(Some(&["C"]), None)]))
                .unwrap_err()
                .to_string();
        assert!(unknown_carrier.contains("unknown carrier"));

        let unknown_model =
            NrRelation::from_selection(&domain, Some(&[rect(None, Some(&["NOPE1"]))]))
                .unwrap_err()
                .to_string();
        assert!(unknown_model.contains("unknown model"));

        let ineligible_model =
            NrRelation::from_selection(&domain, Some(&[rect(None, Some(&["GUL82"]))]))
                .unwrap_err()
                .to_string();
        assert!(ineligible_model.contains("not eligible"));

        let ineligible_prime =
            NrRelation::from_selection(&domain, Some(&[rect(None, Some(&["prime:8969"]))]))
                .unwrap_err()
                .to_string();
        assert!(ineligible_prime.contains("not eligible"));
    }

    #[test]
    fn tokens_are_context_sensitive_and_shortest_decimal() {
        let nr_domain = nr_domain(&[("A", Sku::Prime(8969))]);
        for token in ["prime:08969", "prime:+8969", "prime:"] {
            assert!(
                NrRelation::from_selection(&nr_domain, Some(&[rect(None, Some(&[token]))]))
                    .unwrap_err()
                    .to_string()
                    .contains("shortest decimal")
            );
        }
        assert!(
            NrRelation::from_selection(&nr_domain, Some(&[rect(None, Some(&["lte:8969"]))]),)
                .unwrap_err()
                .to_string()
                .contains("not valid in an NR selection")
        );

        let lte_domain = lte_domain(&[Sku::Lte(400_907_661)]);
        for token in ["lte:0400907661", "lte:+400907661", "lte:"] {
            assert!(
                LteRelation::from_selection(&lte_domain, Some(&[rect(None, Some(&[token]))]))
                    .unwrap_err()
                    .to_string()
                    .contains("shortest decimal")
            );
        }
        for token in ["prime:400907661", "legacy"] {
            assert!(
                LteRelation::from_selection(&lte_domain, Some(&[rect(None, Some(&[token]))]))
                    .unwrap_err()
                    .to_string()
                    .contains("not valid in an LTE selection")
            );
        }
    }

    #[test]
    fn axis_members_are_validated_before_rectangle_intersection() {
        let domain = nr_domain(&[("A", Sku::Legacy), ("B", Sku::Model("G2YBB".into()))]);

        let unknown = NrRelation::from_selection(
            &domain,
            Some(&[rect(Some(&["UNKNOWN"]), Some(&["G2YBB"]))]),
        )
        .unwrap_err()
        .to_string();
        assert!(unknown.contains("unknown carrier"));
        assert!(!unknown.contains("empty intersection"));

        let empty =
            NrRelation::from_selection(&domain, Some(&[rect(Some(&["A"]), Some(&["G2YBB"]))]))
                .unwrap_err()
                .to_string();
        assert!(empty.contains("empty intersection"));
    }

    #[test]
    fn lte_rejects_carriers_and_empty_sku_axes() {
        let domain = lte_domain(&[Sku::Model("G2YBB".into())]);

        assert!(
            LteRelation::from_selection(&domain, Some(&[rect(Some(&["A"]), None)]))
                .unwrap_err()
                .to_string()
                .contains("carriers")
        );
        assert!(
            LteRelation::from_selection(
                &domain,
                Some(&[SelectionRect {
                    carriers: None,
                    skus: Some(Vec::new()),
                }]),
            )
            .is_err()
        );
        assert!(LteRelation::from_selection(&domain, Some(&[])).is_err());
        assert!(
            LteRelation::from_selection(&domain, Some(&[rect(None, None)]))
                .unwrap_err()
                .to_string()
                .contains("empty object")
        );
    }

    #[test]
    fn serialization_rejects_relations_outside_their_domains() {
        // Intern the relation against a domain that contains carrier `B`, then confirm a domain
        // that lacks `B` rejects it (its ids are not a subset) rather than serializing it.
        let wide = nr_domain(&[("A", Sku::Legacy), ("B", Sku::Legacy)]);
        let nr_relation = nr_relation(&wide, &[("B", Sku::Legacy)]);
        let nr_domain = nr_domain(&[("A", Sku::Legacy)]);
        assert!(
            nr_relation
                .canonical_selection(&nr_domain)
                .unwrap_err()
                .to_string()
                .contains("outside the NR domain")
        );

        let lte_domain = lte_domain(&[Sku::Model("G2YBB".into())]);
        let lte_relation = lte_relation(&[Sku::Lte(400_907_661)]);
        assert!(
            lte_relation
                .canonical_selection(&lte_domain)
                .unwrap_err()
                .to_string()
                .contains("outside the LTE domain")
        );
    }
}
