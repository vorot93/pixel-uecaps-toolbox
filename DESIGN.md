# DESIGN — pixel-uecaps-toolbox

The architecture and reverse-engineered-format reference for this codebase: the mental
model, the observed `.binarypb` wire formats, the SKU/fingerprint math, the full-folder
compiler pipeline, and the invariants that keep edits faithful. For how to *use* the tool
see the [README](README.md); for how to build, test, and work on it see
[CONTRIBUTING](CONTRIBUTING.md).

> The `.binarypb` format is **observed, not documented** by Google. Everything below is
> reverse-engineered and verified against real Pixel dumps. When code and this doc
> disagree, the code wins — fix the doc.

## Contents

- [Orientation](#orientation)
- [Repository layout](#repository-layout)
- [Invariants that must not break](#invariants-that-must-not-break)
- [Full-folder compiler](#full-folder-compiler)
- [On-disk formats](#on-disk-formats)
- [LTE-fallback firmware selection](#lte-fallback-firmware-selection)
- [Design conventions & rationale](#design-conventions--rationale)
- [Glossary](#glossary)

## Orientation

`pixel-uecaps-toolbox` decodes, inspects, diffs, and audits the per-carrier UE-capability
protobufs a Pixel ships to tell the network which LTE/5G band combinations it supports, and
compiles a complete offline `uecapconfig` folder into a flashable Magisk replacement module.

The crate is a **library + binary**: `src/lib.rs` exposes the `pub` modules that do the
work; `src/main.rs` is a thin `clap` CLI over them.

**The library API is intentionally minimal.** It is the 7 CLI entry points —
`compiler::{decompose, provision}` and `report::{inspect, check_folder, matrix, self_test,
compare}` — plus exactly what they need to be callable and testable from outside the crate:
the `outcome::Outcome` / `report::{Common, Detail}` types those entry points take or return,
the `compiler::{load_sources, provision_from_sources}` parse-once pair and the opaque
`ValidatedSources` handle they leak (the corpus test's perf optimization; see CONTRIBUTING §
Performance), `model::PHONE_MODELS` (+
`CapabilityLayout`/`PhoneModel`) for enumerating registered targets, and `proto` for building
test fixtures. Everything else defaults to `pub(crate)` or private. An earlier plan sketched
a broader library API (a WASM-facing "Plan 2") on top of this crate; it was never built and
was dropped — do not reintroduce a speculative `pub` surface in anticipation of it.

**The mental model.** There are two modem-selected layouts. Older Tensor Pixels use one
unnumbered `<CARRIER>.binarypb` file per carrier and select combos through an in-file
bitmask. Exynos 5400 Pixels use `<CARRIER>_<NUMBER>.binarypb`, where

```
NUMBER = carrier-identity  ×  SKU-profile tag
```

Every profiled carrier ships one file per **Pixel-SKU capability profile**. A profile is
identified by a unique **anchor prime**; a Pixel loads the file whose `NUMBER` is divisible
by its own SKU's anchor. So the modern modem's NR-file choice is pure number theory, not a
hash or version. `PHONE_MODELS` is the authority `provision` resolves a target against, and
it handles either registered layout — `CapabilityLayout::Bitmask` or
`CapabilityLayout::Profiled { nr_anchor, lte_id }`.

## Repository layout

| File | Responsibility |
| --- | --- |
| `src/lib.rs` | Library crate root; `pub mod` declarations for the modules below. |
| `src/main.rs` | `clap` CLI (`Cli`/`Cmd`); `main() -> ExitCode` dispatches each subcommand to the library. |
| `src/outcome.rs` | The `Outcome` enum (`Clean`/`Findings`/`Rejected`) that replaced ad hoc `i32` exit codes crate-wide; `main` converts it to `ExitCode` exactly once. |
| `src/proto.rs` | The protobuf message types, hand-written with `#[derive(prost::Message)]`; each field's `#[prost(...)]` attribute models the proto2-origin wire format directly. |
| `src/factor.rs` | Integer helpers over `num-prime` for the trailing filename numbers: `gcd`, `is_prime`, `factorize`, `format_factors`. |
| `src/model.rs` | The reverse-engineered model: `PROFILES` (16 SKU anchors), `LTE_CONFIGS` (9), and `PHONE_MODELS` (34 bitmask + 18 profiled build targets), plus layout/reverse lookups, `fp_info`, `parse_name`, `decode_plmn`, and `mcc_country`. |
| `src/atomic.rs` | Sibling-temporary-file preparation and atomic byte replacement used at compiler output boundaries. |
| `src/raw_nr.rs` | Shared protobuf-shaped NR sub-block/payload representation, canonical identity, feature resolution, and reconstruction used across the compiler's ingest and generation paths. |
| `src/kdl_support/mod.rs` | Crate-wide KDL toolkit: the `NodeReader` strictness combinator, writer helpers (`opt_int_prop`/`opt_str_prop`/`opt_bool_prop`/`str_list_node`/`finish_doc`/`push_repeated_int_prop`), the shared `plmn_to_node`/`read_plmn` PLMN codec, and the `cckind_to_str`/`str_to_cckind` (`SubBlockKind`↔`nr`/`lte`) codec (`str_to_cckind` takes a `what:` label so each caller keeps its own error phrasing) — consumed solely by the compiler's `nr.kdl`/`lte.kdl` (de)serializers now that the combo patch and the mapping legend's own KDL layer are both gone. |
| `src/wire.rs` | Recursive modeled-field/wire-type validation and strict decoders for `UeCaps`, `LteCaps`, and `PlmnMap`. |
| `src/compiler/mod.rs` | Public folder-compiler `decompose`/`provision` entry points, generated-file type, and module boundary. |
| `src/compiler/selection.rs` | Finite NR/LTE eligibility domains, SKU tokens, expanded applicability relations, validation, and canonical rectangle serialization. |
| `src/compiler/features.rs` | Compiler-only global DL/UL feature DTOs, canonical source references, and compact per-file projection. |
| `src/compiler/schema.rs` | Strict version-1 `nr.kdl`/`lte.kdl` DTOs, shortest-decimal map-key parsing, cross-reference validation, canonical KDL, and source loading invariants. |
| `src/compiler/kdl_source.rs` | Hand-written `nr.kdl`/`lte.kdl` (de)serialization (`nr_to_kdl`/`nr_from_kdl`, `lte_to_kdl`/`lte_from_kdl`) over the shared `kdl_support` toolkit. |
| `src/compiler/nr.rs` | Bitmask/profiled NR ingestion, carrier metadata derivation, raw payload normalization, model-targeted generation, and canonical self-verification. |
| `src/compiler/lte.rs` | Ordered LTE payload identity, applicability, global DAG/topological ordering, byte-identical regeneration, and self-verification. |
| `src/compiler/decompose.rs` | Strict two-directory classification and ingestion, mapping integration, complete source self-check, and paired source writes. |
| `src/compiler/provision.rs` | Real-model resolution, complete file-set assembly, protobuf self-check, replacement-module construction, and atomic ZIP output. |
| `src/compiler/test_support.rs` | `#[cfg(test)]` miniature bitmask/profiled folders and exact canonical-source fixtures shared by compiler unit tests. |
| `src/mapping/mod.rs` | Reader for the `ap_plmn_mapping.binarypb` legend; `load_mapping(dir)` / `load_mapping_report(dir)`. |
| `src/mapping/plmn.rs` | The `Plmn(u32)` newtype: packed-BCD `from_encoded`, `nibbles`, `Display`. |
| `src/mapping/schema.rs` | The editable mapping schema (`Root`/`MappingEntry`) ↔ proto `PlmnMap` (`map_to_root`/`root_to_map`). |
| `src/mapping/error.rs` | The mapping `Error` enum (thiserror). |
| `src/magisk/mod.rs` | Assembles the compiler's generated files into a deterministic `.replace` Magisk `.zip` (overlay tree + `module.prop` + `META-INF/.../update-binary`) at the fixed `/vendor/firmware/uecapconfig` destination. |
| `src/report/mod.rs` | Reports facade (`inspect`/`compare`/`check`/`matrix`/`self_test`, plus the `Detail`/`Common` presentation types) and shared helpers (`binarypb_names`). |
| `src/report/combos.rs` | NR band-combination model + rendering (labels, class letters, capability tables). |
| `src/report/detail.rs` | The `Detail` (`Summary`/`Full`) and `Common` (`Hide`/`Show`) presentation enums that replaced `full`/`show_common` `bool` parameters; `clap`'s flags stay `bool` and convert to these at the CLI boundary. |
| `src/report/check.rs` | Folder-wide consistency check (`check_folder`). |
| `src/report/matrix.rs` | Carrier × profile matrix as CSV. |
| `src/report/lte.rs` | LTE-fallback decode + text rendering for `inspect`. |
| `src/report/compare.rs` | Diff two files' band combinations. |
| `src/report/inspect.rs` | Single-file analysis (text) for carrier / LTE / mapping files. |
| `src/report/selftest.rs` | Data-independent runtime sanity checks; prints `ALL TESTS PASSED`. |
| `tests/compiler_cli.rs` | Hermetic end-to-end CLI decompose/provision tests over miniature folders. |
| `tests/compiler_corpus.rs` | Opt-in full-corpus decompose/provision and observed LTE-order checks, guarded by two explicit environment variables. |

## Invariants that must not break

**Re-encode fidelity differs by file kind — this is the subtlest contract here.**

- **NR carrier files: value-level fidelity, *not* byte-identity.** The compiler re-encodes with a plain `encode_to_vec()`; proto3 canonicalization reorders/omits, so generated bytes legitimately differ from Google's original (byte-identity against Google's input is an explicit non-goal — byte-identity across *our own* repeated runs is not, see [Complete replacement and determinism](#complete-replacement-and-determinism)). The contract is that every *value* survives ingest: `wire::decode_uecaps` pre-scans the raw wire and rejects anything decoding could silently normalize or discard — an unmodeled field number, a wrong wire type, a varint too wide for its declared integer type, a repeat of a singular field (prost keeps only the last), a descending tag order, or a non-minimally encoded varint. Each of those decodes "successfully" under bare prost and comes back as different data or different bytes. Generation then **self-verifies** its result against the source model.

Every read path goes through that scanner, including the audit commands. `check`, `inspect`, `matrix`, `compare` and the legend loader once decoded with bare prost on the grounds that they are read-only, which left the commands whose entire purpose is finding anomalies accepting exactly the corruption the scanner exists to reject — an audit more permissive than the writer reports bad input as clean. `check` turns a rejected file into a finding and keeps scanning the folder; `inspect` names the reason for the one file it was asked about.

**Per-CC presence and `bw_class` imply each other.** Regeneration does not preserve proto field 6/7 presence — `resolve` re-derives it from the class via `placeholder_ids` — so a component whose per-CC data and class disagree cannot round-trip in either direction: data under a `0`/absent class is dropped, and a class with no data has an all-zero selector invented for it. `RawSubBlock::validate` enforces the biconditional, which makes both losses unrepresentable rather than merely detectable. This is not a new rule but the one generation always produced; only proto ingest disagreed. See CONTRIBUTING's corpus-evidence table for the counts that verify it holds on all 3.46M real sub-blocks.
- **LTE fallback and PLMN legend: genuine bit-for-bit round-trip.** LTE's four `optional` fields (`ul_bw_class_mimo`, `bcs`, `unknown1`, `unknown2`) and NR's `optional` scalars preserve *explicit zeros* — `Some(0)` re-encodes as a present field instead of being canonicalized away (plain proto3 would drop them, yielding a ~4 KB-smaller LTE file). The legend's `Carrier.plmns` is `packed = "false"` (unpacked repeated), which is **required** for bit-for-bit identity; `decompose`'s self-verification (rebuilding the legend from source and comparing it byte-for-byte against the original) proves the round-trip holds (tested). Reads use `.unwrap_or(0)`; writes set `Some(value)`.

**Every re-encoding write path fails closed on unmodeled input.** A plain `prost` decode
silently drops field numbers it doesn't model, losing data on the rewrite — so every
surface that decodes → edits → re-encodes goes through the `wire.rs` strict decoders
(`decode_uecaps` / `decode_lte_caps` / `decode_plmn_map`), which reject an unknown field
number or wrong wire type (including packed `plmns`) *before* re-encoding. Every such caller is
now in the compiler, and there are three: `decompose`'s ingest of both folders, `decompose`'s own
regeneration self-check (`verify_internal_targets`), and `provision`'s verification of each
generated file (`verify_generated_files`). The **read-only** reports
(`inspect`/`compare`/`check`/`matrix`) deliberately stay lenient (`read_ue_caps` /
`load_carrier_combos` / `load_mapping` swallow decode errors) so a junk file yields a
best-effort view, not a hard error — do not tighten those shared readers. `load_mapping`
also **collapses** duplicate carrier names (last entry wins) and **drops** empty-named
carriers, both of which `root_to_map` hard-errors on. So a report must not read *integrity*
off `load_mapping`'s deduplicated map or it will audit a corrupted legend as clean; `check`
instead uses the `load_mapping_report` companion (same lenient decode, but it also returns
`duplicate_names` / `empty_named`) and surfaces those as anomalies (exit 1). Keep that split:
the reader stays lenient; the report carries the anomaly.

**NR feature-set reconstruction (compiler generation).** `FeatureCatalogs::from_payloads` builds one **global** DL/UL catalog up front by deduplicating every resolved per-CC feature across every ingested payload; per-file generation then projects a compact **local** view via `LocalFeaturePlan::new` — containing only the records that file's own payloads reference — and reassigns each component's selector bytes to a **1-based selector byte per CC** (`LocalFeaturePlan::reconstruct_sub_block`). Each local direction's projection may hold at most 255 entries (single selector byte); a file whose own DL or UL usage exceeds that fails generation outright (`compiler::nr::generation_accepts_255_and_rejects_256_local_records_per_direction`) — the limit binds the per-file view, not the global catalog, which may be arbitrarily large.

**Registry consistency (asserted by tests).** `PROFILES` has 16 unique anchors;
`LTE_CONFIGS` has 9 unique IDs; `PHONE_MODELS` has 52 unique codes (34 bitmask + 18
profiled), all present in `pixel_bands::PIXEL_BANDS`. Every profiled target's
`nr_anchor ∈ PROFILES` and `lte_id ∈ LTE_CONFIGS`; bitmask targets deliberately have
neither.

**An `lte` component (`SubBlockKind::Lte`, spelled as the `lte` node — see the naming rule) carries no NR-only fields** (the `srs_tx_switch` + `*_max_*` set); feature *indexes* are shared and allowed (an LTE component carries its own `parseLteFeatureIndex` value; an NR component's is derived from its per-CC feature set — see the `dl/ul_feature_index` bullet under [On-disk formats](#on-disk-formats)). This is **structural** at every layer: `RawLteSubBlock` has no feature-set or `srs_tx_switch` field, and neither does its source-format counterpart `SourceLteSubBlock` (see [The sub-block model is a sum type](#the-sub-block-model-is-a-sum-type)). `NrSourceSubBlock::resolve` used to reject an `lte` node carrying an NR-only field; there is no longer any such node to reject, because the source model cannot spell one.

**Structural is not sufficient on the proto-ingest side, though**, and that gap was a live data-loss path. A type with no field for `srs_tx_switch` does not reject a *protobuf message* that carries one — it just drops it. `lte_from_proto_sub_block` read six of the seven optional fields and silently discarded field 8, and because `RawSubBlockKey` takes `srs_tx_switch` from a method that is unconditionally `None` for E-UTRA, two combos differing only in field 8 produced identical identity keys and the legacy branch dropped one of them outright. `wire`'s scanner cannot catch this either: field 8 is modeled on `SubBlock` and is perfectly legitimate for NR, so only the band distinguishes the two cases. Proto ingest therefore **validates** it explicitly now, matching what the KDL reader already did (`read_sub_block`'s `finish()` rejects `srs-tx-switch` on an `lte` node). The corpus carries field 8 zero times — on NR sub-blocks too — so this is hardening rather than a fix for observed data. LTE component **bands** are likewise validated to `1..NR_BAND_OFFSET` on parse (`RawSubBlock::validate`), because the kind↔band split is recoverable *only* while plain band numbers stay below the offset: `RawSubBlock::from_proto_sub_block` classifies a component purely by `band >= NR_BAND_OFFSET`, so a raw protobuf encoding stored where a plain number belongs would silently re-read as NR on the next decode — hence `validate`'s "must be the plain band number, not raw protobuf encoding".

**Validation results carry their own proof.** `validate_carrier_role` establishes that a
carrier's `mapping_id` and `plmns` imply each other, and that signature/tier/a non-empty profile
table imply one another. `ValidatedCarrier` is shaped to match — `legend: Option<LegendEntry>`
and `profiled: Option<ProfiledRole>` — rather than seven independent `Option`s that five
downstream sites then recovered with `expect`/`unwrap`. Keep new validation results in that
shape: if the validator proved two fields travel together, put them in one `Option`.

**A PLMN is carried as `mapping::Plmn`, not as text or a packed integer.** `Plmn` is a validated,
`Copy`, bijective newtype (full-sweep round-trip test), so `MappingEntry.plmns` and
`ValidatedCarrier`'s legend both hold `Vec<Plmn>`. `Display`/`FromStr` are used only at the two
real text boundaries — the KDL source surface (`CarrierSource.plmns: Vec<String>`) and report
rendering. Converting to text mid-pipeline is what previously forced four
`.expect("validated PLMN remains within 24 bits")` sites on values that were already proven.

## Full-folder compiler

The `decompose`/`provision` workflow is the **only** editing surface in this crate: every other
command is read-only. It normalizes two complete offline layouts into `nr.kdl` + `lte.kdl`, then
builds one complete model-selected directory replacement. Do not add partial-output modes. These
documents are normalized source, not one protobuf template per input file: each exact payload is
stored once with an applicability relation. Both layouts are required together so that relation
is derived across generations rather than composed from incomplete sources later.

### Decompose assembly walkthrough (parsed protobufs → normalized model)

The subsections after this one are the reference; this is the pipeline `decode_documents`
(`src/compiler/decompose.rs`) runs, in order, to assemble every parsed protobuf from both
folders into the one normalized model. The organizing idea: **each distinct payload is
stored exactly once, and which files carried it is accumulated into its applicability
relation** — assembly is "deduplicate payloads, accumulate relations, derive or store the
rest".

1. **Classify, then read in sorted order.** Both directories are listed; every `.binarypb`
   basename is strictly classified and path-safety-validated, and the per-directory
   presence rules are enforced (see
   [Input and output boundaries](#input-and-output-boundaries)). Processing then follows
   sorted-basename order, so OS enumeration order can never reach the output.
2. **Strict-decode everything before assembling anything.** Every file goes through the
   `wire.rs` strict decoders — one unmodeled field number or wire type anywhere fails the
   whole decode. The legend additionally round-trips `map_to_root` → `root_to_map` (unique
   IDs/names, decodable PLMNs) and must already be in increasing `mapping-id` order. The
   legend's and every LTE file's original bytes are set aside for the byte-identity checks
   in steps 8–9.
3. **Extract canonical NR payloads per carrier file** (`canonical_payloads` in
   `src/compiler/nr.rs`; one routine for both layouts):
   - Gates first: every raw band must lie in `1..NR_BAND_OFFSET` (E-UTRA) or
     `NR_BAND_OFFSET+1..2·NR_BAND_OFFSET` (NR); an empty combo group must not carry a
     value-bearing header (an empty group itself just vanishes); every combo needs at
     least one component; a legacy input mask is discarded whatever its value, while a
     profiled mask must be absent or zero.
   - Flatten the `combo_groups` nesting — group packing, combo order, and masks are
     provenance, not payload.
   - Convert each combo to a `RawNrPayload`: the five `ComboHeader` header fields plus its
     components, resolving each component's whole per-CC selector array against the
     *source file's own* feature lists, **all-or-nothing** (every byte nonzero and in
     `1..=len`: embed each record's raw values — with presence, so a referenced all-absent
     record stays a real identity — and drop the selectors). If any byte fails, the array
     is unresolved, and only the **all-zero placeholder** may survive that way: a nonzero
     unresolvable selector such as `[0, 2]` is a hard error at this boundary
     (`resolve_or_placeholder`, `src/raw_nr.rs`), not retained opaque data. No later stage
     sees per-file feature lists again.
   - Sort components by their full raw key (`RawSubBlockKey`) — NR component order is
     normalized away (LTE order is significant; step 6).
   - Deduplicate within the file by `RawNrPayloadKey`: the header fields plus the sorted
     component keys, where selector bytes count only for unresolved components and
     resolved values count field-by-field. A legacy file merges duplicates (the observed
     post-mask-discard `DISH` collapse); a profiled file fails on them.
4. **Accumulate the global payload → relation map.** Every payload lands in one
   `BTreeMap<RawNrPayloadKey, (RawNrPayload, members)>` spanning *all* files of *both*
   layouts. A legacy file contributes the member `(carrier, legacy)`; a profiled file for
   anchor `A` contributes `(carrier, code)` for every registered model code of `A`, or
   `(carrier, prime:A)` when none is registered. A combo shipped by many files is stored
   once with a many-member relation — this map is the cross-generation normalization the
   intro promises and the reason both folders are required together.
5. **Derive per-carrier metadata along the way.** While walking a profiled carrier's
   files: each `NUMBER` must be divisible by exactly one registered anchor and each anchor
   may appear at most once per carrier; `signature` = GCD of all the carrier's `NUMBER`s;
   `multiplier = NUMBER / signature` with checked reconstruction; field-2 IDs and
   fingerprint tiers must be consistent per carrier, and each file's fingerprint family
   must match its anchor's; per-anchor field 9 is stored as `unknown`. Legacy files
   contribute the observed fingerprint partition and `bitmask-id`. Finally the legend is
   merged in: each entry attaches `mapping-id` + PLMNs to its carrier, creating
   mapping-only carriers as needed. Exact rules:
   [`nr.kdl` metadata and generation](#nrkdl-metadata-and-generation).
6. **Assemble the LTE order DAG** (`ingest_lte` in `src/compiler/lte.rs`). Each exact
   ordered payload is one node stored once globally; every adjacent in-file pair adds an
   edge; a payload's SKU set unions its owner files' registered model codes (or
   `lte:<id>`). In-file duplicates and cross-file cycles are errors. Kahn's algorithm
   (full-raw-payload `Ord` as the ready-set tie-break) yields one deterministic global
   order embedding every file's sequence, and `ingest_lte` immediately proves it: every
   `(file, SKU)` pair is regenerated by filtering that order through the relation and must
   reproduce the input byte-for-byte. See
   [`lte.kdl` order and stored metadata](#ltekdl-order-and-stored-metadata).
7. **Emit the two documents.** NR: sorted `bitmask-carriers`; the fingerprint partition
   groups; the global DL/UL catalogs built from the feature sets actually embedded in
   payloads during step 3, deduplicated and sorted by complete raw value (an unreferenced
   input record was never resolved into any payload, so pruning is not a separate pass —
   such records simply never reach this stage); the combos in canonical payload-key order,
   each with its relation serialized to canonical rectangles (omitted entirely when it
   equals the universe; see
   [Applicability relation and canonical rectangles](#applicability-relation-and-canonical-rectangles))
   and each resolved CC rewritten as a repeated 1-based global catalog reference
   (`dl-feature=`/`ul-feature=`, one per CC; a component with no resolved feature set carries
   nothing — the all-zero placeholder is re-derived on provision, and the raw `dl-cc-id=`/`ul-cc-id=`
   selector fallback was removed). LTE: the ID-keyed file whitelist with stored
   `fingerprint`/`bitmask`, plus the step-6 global combo order.
8. **Serialize, reparse, require a fixed point.** The assembled documents pass through
   `validate_documents` — the same cross-reference validation a hand-edited `provision` source
   gets; decompose has no private fast path — and are serialized once. The emitted text is
   then reparsed with `parse_sources` and reserialized, and both documents must come back
   byte-identical. Serialization is itself a validation boundary: everything the ingest
   computed is re-derived from the text form and must agree (`validate_documents` therefore
   runs twice per decompose — canonicalize the ingest, then reparse as the validation
   boundary; see the perf note in
   [CONTRIBUTING](CONTRIBUTING.md#performance-readability-first-then-re-optimize)).
9. **Regenerate every internal target and compare** (`verify_internal_targets`): the
   legacy target must produce exactly the whitelist's file set; every stored anchor must
   produce exactly its carriers' numbered file set (through a registered model code, else
   the synthetic `prime:` token). Each generated NR file already self-verified during
   generation (identity fields, compact per-file catalogs with every record referenced and
   one-byte selectors, canonical payload-set equality — `verify_generated_file`) and is
   strictly re-decoded here on top; every LTE ID and the rebuilt legend must be
   byte-identical to their inputs. This is the NR-value / LTE-and-legend-bytes fidelity
   split from [Invariants that must not break](#invariants-that-must-not-break).
10. **Write last, atomically.** Only after every check passes are `nr.kdl` and `lte.kdl`
    prepared as sibling temporaries and renamed into place (the two renames are not one
    filesystem transaction — see
    [Input and output boundaries](#input-and-output-boundaries)).

`provision` reuses the same machinery in the reverse direction: `load_sources` is step 8's
parse-and-validate over the two source documents, and generation is step 9's generator
pointed at one registered model, packaged per
[Complete replacement and determinism](#complete-replacement-and-determinism).

### Source format: KDL, hand-mapped (not serde)

Every persisted or emitted format in this crate is KDL, hand-mapped the same way — the
compiler's `nr.kdl`/`lte.kdl` is now the crate's only KDL surface (the combo patch's KDL
grammar and the PLMN legend's own separate KDL encoding are both gone; the legend is pure
protobuf now) and follows this section's pattern. `serde` and `toml` are deliberately absent
from `Cargo.toml` entirely.

`nr.kdl`/`lte.kdl` are KDL v2 documents (the `kdl` crate, pinned to `6.7.1` in `Cargo.toml`),
(de)serialized by hand-written mapping in `src/compiler/kdl_source.rs` — **not** `serde`. Each
document type has one `_to_kdl`/`_from_kdl` pair (`nr_to_kdl`/`nr_from_kdl`,
`lte_to_kdl`/`lte_from_kdl`, re-exported `pub(crate)` from `compiler/mod.rs`), built over a small
set of `NodeReader` combinators in the crate-wide `src/kdl_support/` module (named
`kdl_support`, **not** `kdl`, because a crate-root module named `kdl` collides with the external
`kdl` crate); the compiler's own `nr`/`lte` (de)serializers are its only consumers now that
the combo patch and the mapping legend's KDL layer are both gone: `key_str`/`key_int` read the
leading positional argument (a record's key or identifying value); `req_int`/`opt_str`/`opt_int`/`opt_bool` read properties;
`rest_strings` drains a list node's remaining positional args;
`repeated_int`/`push_repeated_int_prop` read/write every occurrence of a same-named property in
document order (an **`nr`** sub-block's per-CC catalog refs `dl-feature=`/`ul-feature=` are
**repeated numeric properties**, one entry per CC — see the
per-CC feature model below; on an **`lte`** sub-block `dl-feature=`/`ul-feature=` are instead
single-valued — the scalar proto-4/5 index, since LTE carries no per-CC list — see the
`dl/ul_feature_index` bullet under On-disk formats); `children`/`opt_child` fetch nested nodes; and `finish()` errors on
any argument, property, or child node left unconsumed — the structural replacement for serde's
`deny_unknown_fields`. `src/kdl_support/` also hosts the shared `plmn_to_node`/`read_plmn` PLMN
codec (below).

**Do not re-attempt the `kdl`-crate `serde` route** (`kdl`'s own `serde` feature, intentionally
not enabled here). It was evaluated on a throwaway spike over real schema-shaped structs and
rejected on evidence, not preference: any struct field holding a *collection of structs*,
`Vec<Struct>` or `BTreeMap<_, Struct>` — which every one of these document types has (`carriers`,
`combo`, `cc`, `profiles`, `files`, `selection`, `set`, `mapping`, …) — fails at runtime with
`structs cannot be represented as a single KDL value`. The escape hatches don't rescue it either:
the `#args`/`#children`/`#rest` markers serialize as literal node names (a node actually named
`"#args"`), and the alternative serde-KDL crates (`kaydle`, `serde_kdl`, `club-kdl`) hit the same
structural mismatch. Hand-mapping is the only route that delivers readable nesting, strict
rejection, and byte-identical output together — don't revisit the serde route without new evidence
the ecosystem has changed.

`decompose` builds the `KdlDocument` and renders it with `kdl::KdlDocument::autoformat()`: 4-space
indent, `#true`/`#false` booleans, native i128-backed integers, and bare identifiers except where
KDL requires quoting — a numeric-leading string (a map key like `profile "66813533"`) or one with
separators. Autoformat is deterministic, so a given `kdl` version's output is byte-stable — but the
exact bare-vs-quoted and spacing rules belong to the crate, not to us, so **a `kdl` version bump can
change formatting**. `kdl` is version-pinned in `Cargo.toml` for this reason; if it's ever bumped,
treat a golden/corpus byte-diff as expected and regenerate the fixtures rather than chase it as a
regression. That byte-stability includes preserving *repeated* same-name properties in document
order (the per-CC `dl-feature=`/`ul-feature=` format relies on this) — an autoformat-stability test in
`src/kdl_support/` guards it, so a `kdl` bump could break that format specifically, not just spacing.

**Naming rule: a plain, kind-less node name when the file/document already fixes the radio kind;
explicit `nr`/`lte` node names only where a single combo can mix both.** The compiler's `lte.kdl` is
uniformly LTE — its `subblock` node carries no kind tag. Whenever a single combo can mix LTE and NR
components (an EN-DC combo), the radio kind **is** the node name (`nr 78 …` / `lte 66 …`), not a
`kind=` property: `RawSubBlock` is an *enum over the two radio kinds* for exactly
that reason (`kind()` reports the variant; the old explicit `kind: SubBlockKind` field is gone).
The naming rule is uniform across every KDL surface: NR-carrier/EN-DC combos (compiler `nr.kdl`)
spell the kind as the node name; uniformly-LTE combos (compiler `lte.kdl`) use plain `subblock`
with no kind tag. `band` is the sole leading positional argument on
`nr`/`lte`/`subblock` in the compiler's documents — written via
`KdlEntry::new` and read via `NodeReader::key_int`, before any property. **Direction-paired
properties lead with the direction** — the `dl-`/`ul-` prefix is uniform (`dl-bw-class`/`ul-bw-class`,
`dl-feature`/`ul-feature`, `dl-bw-class-mimo`/`ul-bw-class-mimo`), never a `-dl`/`-ul` suffix — and a
sub-block emits all its DL properties before its UL ones (`dl-bw-class dl-feature… ul-bw-class
ul-feature…`), with the direction-agnostic `srs-tx-switch` last. Readers are property-keyed, so this
order is an emit convention only; re-spelling or reordering is a surface change with no wire effect.

**PLMN representation: one `plmn mcc=… mnc=…` node per entry.** The compiler `nr.kdl` carrier
PLMN list is the only place a PLMN appears in KDL now (the legend's own KDL round-trip is gone;
the legend is decoded straight to/from protobuf). `mcc`/`mnc`
are **numeric**: `mcc` is always zero-padded to 3 digits on read; the all-`F` wildcard MNC (`ff`)
omits `mnc=` entirely (rejected if `mnc=` is present and equals the wildcard); and `mnc-digits=3`
marks the one ambiguous case — a 3-digit MNC below 100 (a leading zero, e.g. `310-004` →
`mcc=310 mnc=4 mnc-digits=3`) — since a bare integer can't otherwise distinguish `310-04` from
`310-004`. The codec (`plmn_to_node`/`read_plmn` in `src/kdl_support/`) reuses `mapping::Plmn`'s
existing `Display`/`FromStr` end to end (split/rebuild the `"mcc-mnc"` string, no new PLMN parsing),
and fails closed on any non-decimal, non-wildcard nibble. That fail-closed is safe, not fragile: the
entire real 427-PLMN legend (`ap_plmn_mapping.binarypb`) was surveyed and is decimal-only except the
`ff` wildcard MNC (42×), with exactly 14 leading-zero 3-digit MNCs needing `mnc-digits=3` (`310-004`,
`310-012`, `316-010`, `334-030`, …) and the other 297 two-digit + 116 three-digit-≥100 MNCs a bare
`mnc=N`. The compiler's `nr.kdl` still keeps its own separate `plmns` node name, but only as a
**bare, childless marker** distinguishing a present-but-empty PLMN list (a validated mapping-only
carrier) from no PLMN concept at all (`None`) — see `kdl_source.rs`'s `read_carrier`; a `plmns` node
carrying stale list-style arguments is rejected, not silently dropped.

**`bcs-intra-endc` derivation.** In the compiler `nr.kdl` combo header, `bcs-intra-endc` is the BCS
index for intra-band EN-DC and is present iff `intra-band-en-dc-support == 1`. An absent value
re-derives to `Some(0)` when `intra-band-en-dc-support == 1`, else `None`; the ~20 exceptional zeros
(`intra_band != 1`) and every nonzero are written explicitly, and the unrepresentable
`None` + `intra_band == 1` state fails closed (`bail!`, 0 corpus cases). One shared
`derive_bcs_intra_endc(intra)` helper (`kdl_source.rs`) is the single source of truth both the writer
(omit-when-equal) and reader (re-derive) call, so the two sides cannot silently disagree. This
mirrors the crate's other omit-when-derivable work (LTE placeholder ids, NR feature-index).

Corpus evidence for the rule (measured over the full opt-in corpus, **927,262 combo-group
headers**; retain — not reconstructable from the repo):

| `bcs_intra_endc` | intra=0 | intra=1 | intra=2 |
|---|---:|---:|---:|
| `None` | 855,577 | 0 | 0 |
| `Some(0)` | 282 | 5,553 | 0 |
| `Some(n>0)` | 662 | 63,373 | 1,815 |

`bcs_intra_endc` is genuine-`None` **855,577** times vs **5,835** explicit zeros. The safety
cross-checks that make the derivation provably non-destructive: it is present only on EN-DC combos
(**0 of 237,710** non-EN-DC groups carry it); `None` ⟹ `intra_band == 0` always; `intra_band == 2`
⟹ always `Some(n>0)`; and the load-bearing one — **0 combos carry `intra-band-en-dc-support=1`
without `bcs-intra-endc`**, so re-deriving an absent value can never overwrite a real `None`. On the
generated `nr.kdl` (deduplicated: 25,578 combos, 276,924 lines) exactly **72** lines carried
`bcs-intra-endc=0` — **52 derivable** (`intra=1`, now omitted) and **20 exceptions** (`intra≠1`, kept
explicit) — plus **1,581** nonzero lines. The surviving `Some(n>0)` values include high-bit-packed
magnitudes (the `0x8000_0000` family); they are always written explicitly and are irrelevant to the
zero-vs-absent decision.

### Observed evidence behind the compiler

The following is a source-neutral snapshot of the real legacy and profiled folders used
to derive the compiler model. These facts cannot be reconstructed from this repository
alone, so retain them even if the original dumps disappear. They explain the schema; they
are evidence, not generic format limits:

- Each observed layout contained 89 exact carrier identifiers, with 87 shared. The
  legacy-only names were `PTCRB_GCF` and `UNDEFINED`; the profiled-only names were
  `PLATFORM` and `VZWPRIVATE_US`. Carrier names therefore remain exact identifiers and
  are never aliased across layouts.
- The profiled folder contained 1,389 numbered carrier files: 71 carriers had 16
  profiles, 17 had 14, and `GOOGLE_COMCAST_` had 15. There were 72 main-tier and 17
  alt-tier carriers, so profile count cannot determine tier.
- Modern fingerprints matched `(profile family, carrier tier)` without exception.
  Field 9 varied by carrier/profile and had no verified derivation. Likewise, one
  anchor's filename multiplier varied between carriers, ruling out a global
  anchor-to-multiplier table.
- Profiled NR field 2 and the PLMN-legend index are independent. Nine mapping cases had
  no profiled field 2, while `PLATFORM` and `WILDCARD` both carried the same explicit
  field-2 zero. Profiled IDs are therefore optional and need not be unique; only the
  full-width mapping IDs are unique.
- Legacy fingerprints were non-derived partitions: `715188856` covered 73 carriers,
  `702152537` covered 14, `548015020` covered only `PTCRB_GCF`, and `773233060` covered
  only `KDDI`. Legacy field 9 was zero. The source schema stores arbitrary observed
  fingerprint partitions rather than hardcoding these four values.
- Feature-list residue was common. In the legacy folder, 70 of 89 files had
  value-bearing unreferenced records in each direction. Of 1,389 profiled files, 1,157
  had DL residue and 1,165 had UL residue. In `1_1_DE`, record 1 was unused while record
  2 was used by 110 of 2,703 components. Unreferenced records therefore are not source
  payload, regardless of whether their fields carry values.
- Legacy `DISH` combos with masks `12` and `6144` can collapse to one payload when the
  deliberately discarded mask is removed from identity. Only these legacy post-mask
  duplicates merge; a duplicate canonical payload within one profiled file remains an
  error.
- Every observed legend name had a profiled carrier file, but the normalized schema
  deliberately permits a future mapping-only carrier instead of turning that snapshot
  into a format restriction.
- The profiled folder had eight LTE files and 3,878 distinct exact ordered payloads,
  with no duplicate payload inside one file and an acyclic combined relative-order
  graph. In 482 combos, component order differed from full-field sorted order, proving
  that LTE component order cannot be canonicalized away. The two observed RoW/Fold
  pairs shared byte-identical combo sequences while retaining different file-level
  bitmasks.

These observations drove exact carrier names, independent profiled/mapping IDs,
referenced-only feature catalogs, legacy-only duplicate collapse, stored
tier/field-9/multiplier metadata, mapping-only schema support, and the LTE ordering DAG.
Do not turn the snapshot counts into new validation rules. When real folders are
supplied, the opt-in corpus test pins the eight-file and 3,878-payload counts, absence of
within-file duplicates, acyclic global order, and equal-sequence pairs; it does not
recompute the historical feature-residue or 482 component-order counts. A skipped test
run proves only that its environment-variable guard works.

### Input and output boundaries

- `decompose --bitmask DIR --profiled DIR -o DIR` always requires both inputs. The bitmask
  directory must contain at least one unnumbered `<CARRIER>.binarypb` and rejects every
  numbered carrier, mapping, LTE, empty carrier name, or otherwise unsupported
  `.binarypb` name. The profiled directory must contain exactly one
  `ap_plmn_mapping.binarypb`, at least one canonical `lte_<id>.binarypb`, and at least
  one canonical `<CARRIER>_<NUMBER>.binarypb`; numeric filename parts are shortest
  decimal `u64`. Cross-layout and unsupported `.binarypb` files fail. Top-level
  non-`.binarypb` entries are ignored.
- `wire.rs` recursively validates field numbers and wire types at every modeled message
  depth before prost decoding. Unknown fields must fail closed; allowing prost to drop
  them would make source normalization lossy.
- The input legend must already be ordered by increasing `mapping-id`. `nr.kdl` stores
  carrier metadata by name while generation canonically rebuilds legend entries by that
  independent ID; accepting a different original order would violate the legend's
  byte-identity contract.
- Decompose reads and validates both directories, serializes both documents, strictly
  reparses/reserializes them, regenerates every internal legacy/anchor/LTE/mapping
  target, and self-checks fidelity before preparing output. Expected generated basename
  lists are sorted before exact comparison, including for prefix-related carrier names.
  Normal validation or encoding failure leaves pre-existing `nr.kdl` and `lte.kdl`
  unchanged. Both sibling temporaries are prepared before persistence, but the two final
  renames are not one filesystem transaction: an OS failure after the first rename is
  reported as an I/O error and can leave only the first document replaced.
- `provision MODEL SOURCE -o ZIP` reads and validates **both** source documents before model
  resolution. `MODEL` is a real registered Google hardware code, never `legacy`,
  `prime:<anchor>`, or `lte:<id>`. Generation, protobuf re-decoding, complete file-set
  verification, and ZIP assembly finish in memory before the ZIP is atomically replaced.

Every source DTO is validated by `kdl_source.rs`'s `NodeReader::finish()`, which rejects any
unconsumed argument, property, or child node (the structural replacement for
`#[serde(deny_unknown_fields)]`), and requires `version = 1`. One strictness nuance: a *repeated
property* on a node is **last-wins**, not rejected (`node.get` returns the last value), so a
hand-edited `nr 78 dl-bw-class=1 dl-bw-class=2` silently takes `dl-bw-class=2`; repeated *map-key
child nodes* (`carrier`/`profile`/`file`) **are** rejected via explicit guards. The per-CC catalog
refs (`dl-feature=`/`ul-feature=`) are a
deliberate, explicit **exception** to last-wins: `NodeReader::repeated_int` (`src/kdl_support/mod.rs`)
collects *every* occurrence of the named property in document order instead of collapsing to the last
one, because each occurrence is one CC's value, not an accidental duplicate — every other property
keeps last-wins. This only affects hand-edited `provision` input — `decompose` never emits duplicates and its
reparse/idempotence self-check guards that path. Carrier names and selection tokens are
case-sensitive canonical identifiers in source; only CLI model lookup trims and uppercases input.
Filename factors and opaque filename-related `u64` values are native KDL integers (i128-backed, so
the full `u64` range fits without string-encoding); the two remaining string map keys (profile
anchor, LTE file id) are quoted positional arguments, still parsed by `parse_decimal_key`'s
shortest-decimal check. Optional protobuf scalars use KDL presence exactly: omission is absent, while
`0` is present-zero.

### Applicability relation and canonical rectangles

`selection` is a serialization of a finite relation, not an ordered expression to
replay. Construct the eligibility universe first:

- NR contains `(carrier, legacy)` for each `bitmask-carriers` entry and
  `(carrier, model-code)` for every stored carrier/profile anchor expanded through the
  registered profiled models for that anchor. An anchor with no registered model uses
  one synthetic `prime:<anchor>` SKU. Mapping-only carriers are not combo-eligible.
- LTE contains the registered model codes whose `lte_id` exists in `[files]`; an LTE ID
  with no registered model contributes one synthetic `lte:<id>` SKU.

Parsing a rectangle expands the product of its axes and intersects it with that
universe. An omitted axis is unrestricted. The rectangles are unioned into a set, so
composition order, duplicate members, duplicate rectangles, and overlaps cannot affect
meaning. A present set of `selection` child nodes must be nonempty; each node must
constrain at least one axis; each present axis must be nonempty and known; and each
rectangle must intersect the domain. LTE rejects a `carriers` axis. Omitting
`selection` entirely means the complete universe; an empty relation has no
representation.

NR serialization is a deterministic carrier-row normal form:

1. Compute the selected SKU set per carrier.
2. Replace a row equal to that carrier's eligible SKU set with an unrestricted SKU
   constraint.
3. Group carriers with the same normalized SKU constraint.
4. Omit `carriers` only if the group covers every eligible carrier; retain sorted
   carriers otherwise.
5. Sort SKU tokens as enum order (`legacy`, real model codes lexically, then numeric
   `prime:`/`lte:` kinds) and rectangles by `(skus, carriers)`, with omitted axes before
   present axes.
6. Omit the whole `selection` field when the relation equals the universe; never emit
   an empty object.

LTE is one-axis, so its canonical non-universal relation is at most one `selection`
node with a sorted `skus` list. Exhaustive small-domain tests reparse every canonical output and
prove that permutations and alternate rectangle compositions preserve the exact
relation.

### `nr.kdl` metadata and generation

The schema distinguishes metadata that is safely derived from opaque values that must
be stored:

- `bitmask-carriers` is the exact legacy file whitelist.
  Repeated `bitmask-fingerprint` nodes store the observed non-derived legacy fingerprint groups;
  they must be nonempty, disjoint, and partition the whitelist exactly. Optional
  `bitmask-id` preserves legacy field 2 and must fit protobuf `int32`. Legacy field 9
  must be zero on ingest and is regenerated as zero. Input combo masks are intentionally
  discarded; every generated legacy combo gets the catch-all `65535` until individual
  model bits are mapped.
- For each profiled carrier, decompose derives `signature` as the GCD of all its numbered
  filenames and stores the exact per-anchor `multiplier = NUMBER / signature`.
  Generation uses checked `signature * multiplier`; the result must reconstruct the
  original number, be divisible by exactly the keyed registered anchor, and by no other
  anchor. The presence of a carrier's `profile` child node is the output whitelist for
  that anchor; absence is an intentional skip.
- Profile family is registry-derived from the anchor. `tier` is stored per carrier, so
  the modern fingerprint is derived from `(family, tier)` using the four observed
  values. `profiled-id` preserves exact optional modern field 2, including absence and
  present-zero; one carrier's profiles must agree, but the value may repeat across
  carriers. The per-carrier/profile field-9 value has no derivation and is stored as
  `unknown`.
- `plmns` **presence** selects legend membership. Omission means no `plmns` child node;
  an empty `plmns` child node preserves an entry with no networks. A present `plmns`
  node requires `mapping-id`, the independent globally unique legend index. Mapping-only
  carriers may have only `mapping-id` + `plmns`. Profiled NR field 2 must fit protobuf
  `int32`, while `mapping-id` is a native KDL integer retaining the legend's full `u64`
  range. Entry order is increasing `mapping-id`; PLMN order and duplicates are retained.
- NR source stores canonical raw combo payload values, not group/index provenance,
  fingerprints, derived band labels/keys, or masks. Profiled input accepts absent or
  explicit-zero combo masks and always regenerates explicit `0`; a nonzero profiled
  mask is unsupported.
- Top-level `dl-feature` and `ul-feature` nodes are shared compiler-only source
  catalogs. Decompose resolves a component's whole per-CC selector-byte array
  **all-or-nothing** (`resolve_all`, `src/report/combos.rs`): it resolves only when
  *every* byte in the array is nonzero and lies in `1..=list.len()`; if any single byte
  fails, the entire raw array stays unresolved (`[0, 2]` and `[2, 99]` both fail to
  resolve). `resolve_all` itself only reports that; the decompose boundary that consumes it
  (`resolve_or_placeholder`) then **rejects** any unresolved array that is not the
  all-zero placeholder, so neither `[0, 2]` nor `[2, 99]` can survive decompose as raw
  bytes. Decompose then deduplicates and sorts each catalog by complete raw field
  identity. A referenced all-absent record remains a real identity; every unreferenced
  input record is ignored, whether default, explicit-zero/false, or value-bearing. A
  sub-block node's `dl-feature`/`ul-feature` properties are **repeated**, one canonical
  1-based global position per CC, in CC order (a single-CC sub-block still emits exactly
  one `dl-feature=N`). The raw `dl-cc-id=`/`ul-cc-id=` selector fallback for unresolved
  components was removed (proto field 6/7 is still carried in the model): a component with
  no resolved feature set surfaces nothing for that direction.
- **LTE placeholder per-CC ids are omitted, not stored — and not modeled.** An LTE
  sub-block's per-CC selector bytes are always the all-zero placeholder in the corpus (LTE
  has no per-CC feature catalog entry), fully redundant with `bw_class`/`cc_count`. The
  compiler emitter (`cc_to_node`, `src/compiler/kdl_source.rs`) never surfaces per-CC
  selector bytes at all (the raw `dl-cc-id=`/`ul-cc-id=` fallback was removed), and the
  reader does not reconstruct them either: **the source model carries no selector-byte
  field.** `NrSourceSubBlock` (`src/compiler/features.rs`) holds only catalog *references*,
  and `NrSourceSubBlock::resolve` — the single boundary that turns source into a
  `RawSubBlock` bound for the binary — materializes `[0; cc_count]` via `placeholder_ids`
  when a direction's `dl-feature=`/`ul-feature=` list is empty. `ul_bw_class == 0` (UL
  disabled) short-circuits the UL side to no data at all, never a derived placeholder.
  Deriving at `resolve` rather than at parse is what keeps the three per-direction
  encodings from coexisting on one type: a source sub-block can hold the LTE scalar index
  (`dl_feature_index`, proto 4/5) or the NR per-CC reference list (`dl_feature`, proto 6/7)
  — discriminated by `kind` — and never raw selector bytes for a direction that already has
  a reference. That state was previously representable and guarded only by a runtime
  `ensure!`; it is now unrepresentable, and the guard is gone.
- Generation filters the global catalogs, in canonical order, into compact per-file DL
  and UL lists containing only records used by that carrier/SKU. Resolved components get
  one 1-based local selector byte per CC (a CC-count-long array, not a single byte). The
  global catalogs may exceed 255 records, but one generated file may use at most 255
  records independently in each direction.
- Selector-only bytes remain exact, and the only one that can reach generation is the
  all-zero placeholder — the KDL source format cannot express anything else.
  `NrSourceSubBlock::resolve` (`src/compiler/features.rs`) builds a direction's per-CC ids
  with `placeholder_ids`, which returns `[0; cc_count]` and nothing else; the removed
  `dl-cc-id`/`ul-cc-id` escape hatches have no replacement, and the strict reader rejects
  any stray property. Generation therefore no longer re-checks the collision case.
  (Decompose fails closed on a nonzero unresolvable selector too, via
  `resolve_or_placeholder`, but that governs ingest and is not what protects generation.)
  The hazard the removed check guarded
  against still stands as a rule for any future change here: inserting default filler to
  reserve a selector's index would make a previously out-of-range byte resolve and
  silently change its meaning, so filler or reserved-slot generation must not return.

NR compiler fidelity is canonical modeled-value equality, not protobuf byte identity.
Feature-list layout, selectors, group packing, component order, and canonical proto3
encoding may change; all modeled raw identity values must survive except the explicit
legacy/modern mask normalizations above. Decompose rejects a value-bearing header on an
otherwise empty group, but unreferenced feature records carry no modeled payload and
normalize away regardless of their values. A referenced all-absent feature record does
carry modeled identity and survives. Canonically equal payloads merge only within a
legacy file after mask discard; duplicates within a profiled file remain errors because
the normalized relation cannot represent multiplicity.

### `lte.kdl` order and stored metadata

Repeated `file` nodes (keyed by a quoted `<id>` string argument) are the exact LTE
whitelist. The ID is the firmware's hardcoded `lte_file_id`, not derived from content.
Per-file `fingerprint` and `bitmask` are stored because this model has no independent
derivation for them. A profiled target requires the registry-selected ID to be present
and emits exactly that one LTE file.

An LTE payload's identity includes component **order** plus optional presence for
`ul-bw-class-mimo`, `bcs`, `unknown1`, and `unknown2`; explicit zero is distinct from
absence. Each exact ordered payload is stored once globally. To reproduce every input
sequence, ingest makes each payload a DAG node and every adjacent in-file pair an edge.
It rejects a duplicate payload within one file and any cycle across files. Kahn
topological sorting uses the full raw payload order as a deterministic tie-breaker;
generation filters that one global order by the target SKU relation. Unedited
decompose/provision must therefore reproduce each `lte_*.binarypb` byte-for-byte.

### Complete replacement and determinism

A bitmask target emits every `bitmask-carriers` name as an unnumbered NR file, even
when its selected payload set is empty, and emits no mapping or LTE file because that
generation's PLMN selection comes from editable modem NVRAM. A profiled target emits a
numbered NR file for every carrier with the selected anchor, the full source-selected
legend (including mapping-only and empty-PLMN entries), and exactly its registered LTE
file.

Every generated file is self-verified before packaging: `verify_generated_file`
(`src/compiler/nr.rs`) re-decodes each NR file's just-encoded bytes and compares them
field-by-field against the in-memory model that produced them (identity fields, bitmask,
both feature lists, and full per-file catalog coverage), and `provision`'s own
`verify_generated_files` (`src/compiler/provision.rs`) then re-decodes every generated
file — NR, LTE, and the legend alike — through the `wire.rs` strict decoders as a second,
format-level check before the ZIP is assembled.

The resulting Magisk ZIP has a fixed destination and order:

```text
module.prop
META-INF/com/google/android/update-binary
META-INF/com/google/android/updater-script
system/vendor/firmware/uecapconfig/.replace
system/vendor/firmware/uecapconfig/<sorted generated basenames>
```

There is no compiler `--dest` and no mode without `.replace`. Entries use Deflate level
9, `DateTime::default()` timestamps, mode `0755` for `update-binary`, and `0644` for
everything else. Basenames are sorted and path/control/line-separator injection is
rejected. For identical source bytes, model, and module name, the archive is
byte-identical. Remember that `.replace` hides every stock file not regenerated, so
partial file sets are a correctness bug.

### Registry evidence rule

`PHONE_MODELS` is the sole model/layout table used by the folder compiler and the
profiled `provision` path. Its exact tested population is 34 legacy bitmask codes from
the pinned `pixel-bands` snapshot plus the existing 18 evidence-backed profiled
`(nr_anchor, lte_id)` mappings. Multiple model codes may share one anchor or LTE ID but
remain distinct applicability tokens.

Do **not** derive a model's anchor from equal NR payloads or its LTE ID from content, and
do not classify a newer unverified model as bitmask to make it build. The nine published
profile-layout codes `GLBW0`, `GL066`, `GK2MP`, `G4QUR`, `GN4F5`, `GEHN3`, `GE1GQ`,
`GV0BP`, and `G4H7L` intentionally remain absent until independent evidence establishes
their anchors. Decompose preserves unbuildable known anchors/IDs with `prime:<anchor>` and
`lte:<id>` tokens. Adding a profiled target requires evidence for both its registered
anchor and LTE ID plus presence in `PIXEL_BANDS`; update the exact registry tests and
all consumers together.

## On-disk formats

All three formats are defined as `#[derive(prost::Message)]` types in `src/proto.rs`. Field
numbers below are the wire tags. The types reconstruct a **proto2-origin wire format**:
repeated scalars are unpacked (`packed = "false"` on `plmns`) and some scalars carry explicit
presence for their default value, so those fields are `optional` (wrapped in `Option`) to keep
the explicit zero on re-encode.

### NR carrier file — `<CARRIER>_<NUMBER>.binarypb` (`UeCaps`)

```
UeCaps { version=1 (u64 fingerprint), id=2 (carrier ID), combo_groups=3,
         dl_feature_per_cc_list=6, ul_feature_per_cc_list=7, unknown=9 (stub ref) }
ComboGroup { combo_header=1 (ComboHeader), combo=2 (repeated Combo) }
  ComboHeader { bcs_nr, bcs_intra_endc, bcs_eutra, power_class, intra_band_en_dc_support }
  Combo { sub_blocks=1 (repeated SubBlock), bitmask=2 (optional) }
    SubBlock { band=1, dl_bw_class=2, ul_bw_class=3, dl_feature_index=4,
                    ul_feature_index=5, dl_feature_per_cc_ids=6 (bytes),
                    ul_feature_per_cc_ids=7 (bytes), srstxswitch=8 }
ShannonFeatureSetDlPerCCNr { max_scs, max_mimo, max_bw, max_mod_order, bw_90mhz_supported }
ShannonFeatureSetUlPerCCNr { max_scs, max_mimo_cb, max_bw, max_mod_order, bw_90mhz_supported, max_mimo_non_cb }
```

- **Band encoding.** `SubBlock.band ≥ 10000` (`NR_BAND_OFFSET`) is an **NR** band (`band − 10000`); `< 10000` is an **E-UTRA/LTE** band. NR combos are **EN-DC** and mix LTE (`B…`) and NR (`n…`) components.
### The sub-block model is a sum type

`raw_nr::RawSubBlock` is an `enum { Lte(RawLteSubBlock), Nr(RawNrSubBlock) }`, and each variant's
two directions are their own structs. The point is that the three ways of spelling a direction's
features are alternatives, and a flat struct let all three sit side by side:

- **Kind.** `RawLteSubBlock` has no per-CC feature sets and no `srs_tx_switch`; `RawNrSubBlock` has
  no stored `dl/ul_feature_index` (NR derives it — see the `dl/ul_feature_index` bullet below).
  Neither can hold the other's data, so the old runtime `has_nr_only_fields` check inside
  `validate` is gone.
- **Direction.** Resolved-vs-selector is **per direction**, not per sub-block: a DL-resolved,
  UL-disabled component is ordinary (`ul_bw_class == 0`), so the choice lives in
  `NrDirection::features`, an `Option<PerCc<T>>` where `PerCc` is `Selector(Vec<u8>) |
  Resolved(Vec<T>)`. **Absence is the `Option`, not a third variant** — it mirrors the wire,
  where field 6/7 is simply missing, and it keeps the two axes separate: presence outside,
  encoding inside. (An `Absent` variant was tried first and produced four sites that marshalled
  between the flat enum and an `Option` in one direction or the other.) `Resolved` is never
  empty; an empty resolution is "did not resolve". The per-CC
  accessors live on `NrDirection` rather than `PerCc` because every length question is also a
  `bw_class` question — `per_cc_len()` is what `validate_cc_count` compares against
  `cc_count(kind, bw_class)`.
- **What did *not* move.** `RawSubBlockKey` stays a flat struct with its original field order,
  because it is an *ordering projection*, not a state model — component order inside a generated
  combo is `sort_by(RawSubBlockKey)`, so regrouping its fields would silently change generated
  bytes. Build it from the enum, never restructure it.
- **`report::combos::SubBlock` is output-only.** It is a *display* DTO and never an ingest input:
  `RawNrPayload::from_proto_combo` is the only path from wire to model. So it holds the band
  label, the two bandwidth classes, the resolved per-CC feature records and `srs_tx_switch` —
  and nothing derived. The decoded display values (SCS in kHz, the MIMO/modulation labels, max
  bandwidth, 90 MHz) are pure functions of the feature records, so `fmt_cc_features` projects
  them at the single point of rendering rather than storing a second copy the type cannot keep
  in agreement. A lenient DTO→payload conversion used to exist (`From<&Combo> for RawNrPayload`
  via `RawSubBlock::from_sub_block`): it had no callers, it parsed the band back out of the
  *rendered* label `"n78"` behind an `.expect`, and it bypassed both the unresolvable-selector
  and the feature-index-derivation guard. It is gone — don't reintroduce a way to turn a report
  string back into a model value.
- **Reading.** Code that treats both kinds alike uses the accessors (`band()`, `dl_bw_class()`,
  `dl_features()`, `dl_selector()`, `dl_feature_index()`). Code that *builds* a component matches
  on the variant, so it can only fill fields that kind actually has.
- **The raw model's ints are its true widths; `i32` is only the wire's.** `band` is `u16`,
  `bw_class` `u8`, and the LTE `feature_index` `u16` — those are the real domains (a plain band
  stays `< NR_BAND_OFFSET`, classes are single-digit). Protobuf carries all three as `i32`, so the
  single decode boundary (`RawSubBlock::from_proto_sub_block`) narrows via `narrow_field`, failing
  closed on an out-of-range value rather than truncating (the same stance as the `ul_bw_class`
  presence check); encode widens back to `i32` for the wire. The narrowing **stops at the model on
  purpose**: `report::combos::SubBlock` and the shared `band_label`/`cc_class` formatters stay
  `i32` (they only ever format a value, and the `inspect` render path is not `Result`). Don't
  narrow the display DTO to `u8`/`u16` — it only adds a second fail-closed-or-truncate boundary in
  non-`Result` display code, with nothing to gain.
- **The source model is a sum too.** `compiler::features::NrSourceSubBlock` — how `nr.kdl` spells a
  sub-block — is `enum { Lte(SourceLteSubBlock), Nr(SourceNrSubBlock) }` for the same reason. An
  `lte` node has the scalar proto-4/5 index and no catalog list; an `nr` node has the per-CC list,
  no index (NR derives 4/5), and is the only kind with `srs_tx_switch`. While it was a flat
  kind-tagged struct, two runtime `ensure!`s ("LTE component carries NR-only fields", "NR component
  stores a feature index") stood in for what the variants now make unwritable, and
  `RawSubBlock` needed `source_dl_feature_index()`/`source_ul_feature_index()` accessors that
  existed only to feed it. Both checks and both accessors are gone.

- **A `SubBlock` is one band+CA-bandwidth-class entry, not one component carrier** — it contains `cc_count(kind, bw_class)` physical CCs (e.g. band 78 class C = `n78C` = 2 CCs; this is how a Pixel expresses `7C-3A`, not just `7A-3A`). `cc_count(kind, bw_class)` (`src/raw_nr.rs`) is a fail-closed lookup over the Samsung Shannon `bw_class` enumeration; NR and LTE tables (`NR_CC_COUNTS`/`LTE_CC_COUNTS`) are distinct and non-monotonic relative to each other (NR class 2/3/7 all count 2; LTE class 2/3 both count 2 — the class carries strictly more information than the CC count, which is why `bw_class` is never derived from it), and an unknown class errors rather than mis-deriving a length (the tables are corpus-validated with zero exceptions across all **3.46M sub-blocks**). `RawSubBlock::validate` fails closed (checked on both source parse and regenerated output) if a stored per-CC list's length doesn't equal `cc_count` for its `bw_class`, and separately if the sub-block's CCs would derive disagreeing `dl_feature_index`/`ul_feature_index` values (physically you cannot mix FR1+FR2 or mixed MIMO-presence within one band+class entry).
- **Feature-set indirection is per-CC, resolved all-or-nothing.** Per-CC capabilities are stored once in the two top-level `*_feature_per_cc_list`s; each sub-block carries one selector byte per CC (in CC order) pointing at a list entry (**1-based**: byte `k ≥ 1` → list index `k − 1`; `0`/absent/out-of-range → none). `resolve_all` (`src/report/combos.rs`) resolves a whole direction's array **iff every byte** in it is in `1..=list.len()`; any single out-of-range byte keeps the **entire raw array** unresolved (`[2, 99]` stays raw rather than resolving byte 2 and dropping 99). A first-byte-only rule here is a real data-loss bug — corpus-verified on **13.8% of multi-CC NR DL sub-blocks (13,927 of 100,904)**, where CCs reference *different* feature records (first seen as ATT's `n48` class B → `dl_ids=[22, 23]`); NR UL multi-CC sub-blocks (46,608 of them) were **100% uniform** in the corpus, so the bug was DL-only in practice. Two alternative data models were rejected: **inline per-CC feature sets** (abandons the shared catalog and bloats `nr.kdl`), and **keeping raw per-file `dl-cc-ids` for NR** (reintroduces the per-file feature lists the decompose pipeline deliberately eliminates) — the per-CC 1-based reference into the global catalog was chosen instead.
- **`dl/ul_feature_index` (fields 4/5) is a MIMO feature index, NOT opaque, used by BOTH kinds.** LTE: the `parseLteFeatureIndex` MIMO × CC-count encoding (<https://raw.githubusercontent.com/NXij/pixel-pb/refs/heads/main/index.html>), kept explicit — spelled `dl-feature`/`ul-feature` in KDL, dropping the `-index` suffix (LTE-only — an
`nr` node carries no feature index in source at all), with `ul-feature=0` omitted per the
omit-when-0 rule below. NR: a value fully **derived** from the per-CC feature set (corpus-verified, 0 mismatches / 1.72M) — DL `0`=no set / `1`=FR1 (`max_scs < 4`) / `2`=FR2 (`max_scs ≥ 4`); UL `0`=no set / `1`=`max_mimo_cb != 2` / `2`=`max_mimo_cb == 2`. So NR KDL source (compiler `nr.kdl`) carries **no** index, and neither does the model: `RawNrSubBlock` has no index field, `SourceNrSubBlock` (the `nr.kdl` shape) has none either, and `RawSubBlock::dl_feature_index()` returns the derivation for NR and the stored `parseLteFeatureIndex` value for LTE. The old source override was removed, so a decoded NR index that disagrees with the derivation is a hard decode error (`RawNrSubBlock::ensure_feature_index_derivable`, `src/raw_nr.rs`) — corpus-verified impossible on real files — while proto field 4/5 is still materialized on decompose/provision. Round-trip is by value, not bytes: an NR UL index that was absent in the original binary (no UL feature set) is rebuilt as an explicit `0` — value-preserving and invisible to canonical-key checks. **Verification consistency:** `RawSubBlockKey::from` and `LocalFeaturePlan::reconstruct_sub_block` both read the index through the same `dl_feature_index()`/`ul_feature_index()` accessors, so dedup and generation cannot drift apart — there is no longer a stored value for them to disagree about. Keep it that way: derive at the accessor, not at each call site.
  - *Corpus scope behind the "0 mismatches" claim:* 1,487 files (9 non-`UeCaps` decode skips), **1,715,899 NR components** and **1,741,849 LTE components**. 0 of the 1.74M LTE components carry a field-6/7 per-CC selector; every NR component carries a DL one and ~60% carry a UL one.
  - *Why these keys (rejected hypotheses):* DL keys off `max_scs` (FR1/FR2), **not** `max_mimo` — a cross-tab against `max_mimo` looked noisy precisely because MIMO is not the key. UL keys off `max_mimo_cb`; `max_mimo_non_cb` moves in lockstep in the data but `max_mimo_cb` is the definitional key.
  - *LTE `parseLteFeatureIndex` cross-check* (for the record; the LTE index is kept explicit, not derived): DL `count = ceil(fi/2)` (fi 1/2→1CC, 3/6→2CC, 7/8→4CC, 9/10→5CC), even fi = 2-layer MIMO, odd = 1-layer, with 5/6 the special B/C 2-CC case; UL only ever `0` or `2` in this corpus.
- **Omit-when-0 normalization (`ul_bw_class` + four combo-header fields).** Five fields are
  corpus-verified **always `Some`** on a real decoded sub-block/combo — never `None`:
  `ul_bw_class` (per sub-block) and the combo header's `power_class`, `bcs_nr`, `bcs_eutra`,
  `intra_band_en_dc_support`. (`bcs_intra_endc` does **not** qualify — it has genuine `None`
  in the corpus and stays a plain optional field everywhere.) Because these five are never
  absent, `Some(0)` carries no information a KDL reader can't recover: `nr.kdl`'s writers
  (`compiler::kdl_source`) omit the property
  when the value is `Some(0)`, and the paired readers default an absent property back to
  `Some(0)` (`r.opt_int(key)?.or(Some(0))`) rather than `None`. A DL-only NR sub-block
  (`ul_bw_class == Some(0)`) therefore carries no `ul-bw-class=` at all in `nr.kdl`.
  A sixth, **kind-scoped** omit-when-0 applies on `lte` sub-blocks only: `ul_feature_index` is
  corpus-verified always-`Some` on LTE (1.74M sub-blocks; `Some(0)` ⟺ no UL, ≈59%), so `lte`
  writers omit `ul-feature=0` and `lte` readers default an absent `ul-feature` back to `Some(0)`.
  It is *not* applied on `nr` (which carries no index in source at all — it always derives on
  provision), and it
  does not extend to `dl_feature_index` (never `0` on LTE — the rule would be dead code). Unlike
  the five fields above it has no decode-time `ensure!`; the `lte_feature_index_is_always_some_in_corpus`
  test (`tests/compiler_corpus.rs`) is its guard.
  The strict decode boundary that builds a `RawSubBlock`/`RawNrPayload` from a real
  `.binarypb` — `raw_nr::RawSubBlock::from_proto_sub_block` and the header read in
  `raw_nr::RawNrPayload::from_proto_combo` — asserts (`anyhow::ensure!`) that these fields
  are actually `Some` on the decoded proto, failing closed instead of silently normalizing an
  unobserved `None` shape to `0`; both fns are therefore fallible and only called from the
  compiler's strict `canonical_payloads` path (`compiler/nr.rs`), never from a lenient report
  reader (`build_combos`/inspect/compare/check stay lenient on purpose — no assertions
  there). Because the four header fields are now always `Some` once a payload is built either
  way, `RawNrPayload::header()` always returns `Some(ComboHeader{..})` (the `Option` return
  type is kept only because `ComboGroup::combo_header` itself is `Option<ComboHeader>`).
  Corpus-verified: every real combo header's `power_class` is in fact always `0` (never
  appears in generated `nr.kdl` at all), so don't be surprised if `power-class=` is entirely
  absent from a real decode. **Guardrail — do not extend `=0` dropping blindly:** the only
  safely-droppable zeros are ones where `None` provably never occurs. Do not drop `=0` on any
  other field without the same *"`None` never occurs"* corpus proof, and **never** on a field
  where `None` is a meaningful distinct value (`bcs_intra_endc`, the carrier `id`, the feature
  catalog). Secondary counts behind the LTE-placeholder omission: the LTE per-CC ids are the
  all-zero placeholder on ~1.74M DL and ~710K UL entries; the UL-disabled short-circuit
  (`ul_bw_class == 0`) covers 687K NR + 1.03M LTE sub-blocks.
- **Numeric codes** (rendered as labels by `report/combos.rs`; `—` for `0`/absent, `(n)` out-of-table):

  | Field | code → value |
  | --- | --- |
  | `max_scs` | 1=15, 2=30, 3=60, 4=120, 5=240 kHz |
  | `max_mimo` (DL) | 1=2×2, 2=4×4, 3=8×8 |
  | `max_mimo_cb` (UL) | 1=No, 2=Yes |
  | `max_mod_order` | 1=QAM64, 2=QAM256 |
  | `max_bw` | bandwidth in MHz (raw integer) |
  | `bw_90mhz_supported` | bool |

- A file with empty `combo_groups` is a **reference stub** (fingerprint + `unknown` field-9 delegation, no payload) — alt-tier operators ship these and delegate to `EU_COMMON1`.

### LTE fallback file — `lte_<id>.binarypb` (`LteCaps`)

```
LteCaps { fingerprint=1 (u64), combos=2 (repeated LteCombo), bitmask=3 (u64) }
LteCombo { components=1, bcs=2 (optional), unknown1=3 (optional), unknown2=4 (optional) }
LteComponent { band=1, dl_bw_class_mimo=2, ul_bw_class_mimo=3 (optional) }
```

E-UTRA only (1–5 CA, no NR). Opaque numeric fields are `uint64` (real files carry 64-bit
values in `unknown1`). **Class + MIMO encoding** (`report/lte.rs`, one field per direction):

- `value == 0` → that direction **disabled**.
- Letter from the high bits (`value & ~1`): `32768→A, 16384→B, 8192→C, 4096→D, 2048→E, 1024→F`.
- MIMO from the **low bit**: `value & 1` → 4×4, else 2×2. So `32768` = A 2×2, `32769` = A 4×4.

Observed DL uses A–F × {2×2, 4×4}; observed UL uses only `0/A/B/C`. LTE fingerprints seen:
`874888686` (family A, main) and `862505271` (family B, main).

### PLMN legend — `ap_plmn_mapping.binarypb` (`PlmnMap`)

```
PlmnMap { carriers=1 }
Carrier { plmns=1 (repeated u64, unpacked), index=2, name=3 }
```

`name` equals the carrier-config filename prefix. A PLMN is a 24-bit **packed-BCD** value
(`Plmn(u32)`). For MCC `M1 M2 M3` and MNC `N1 N2 (N3)`, the nibbles from most- to
least-significant are `M2 M1 N3 M3 N2 N1`; a 2-digit MNC sets `N3 = 0xF` filler. Hex
nibbles `0xA–0xF` render as `*` (wildcard). Exact vectors:

| String | Encoded | Note |
| --- | --- | --- |
| `302-220` | `197154` | 3-digit MNC |
| `250-01` | `5435408` | 2-digit MNC (`N3 = F`) |
| `450-05` | `5566544` | |
| `999-99` | `10090905` | |
| `228-ff` | `2291967` | wildcard MNC |

### Fingerprint & SKU math

`version` (field 1) is the in-file capability **fingerprint**; `fp_info` maps it to
`(Family, Tier)`:

| Fingerprint | Family | Tier |
| --- | --- | --- |
| `874888686` | A | Main |
| `862505271` | B | Main |
| `707802847` | A | Alt |
| `627223094` | B | Alt |

A carrier file belongs to a profile **iff the profile's anchor prime divides its
`NUMBER`** (`identify_profile`). `NUMBER = carrier-signature (the common factor of all
that carrier's files) × SKU portion`. The **Main** tier has 16 profiles; the **Alt** tier
14 (it lacks anchors `2912407` and `3539`) and serves India/emerging markets via
reference stubs. `lte_*` files sit **outside** this scheme (no anchor divides them).

## LTE-fallback firmware selection

Which `lte_<id>.binarypb` a device loads is decided by its **hardware/SKU category
code**, **not** by SIM or MCC. The mapping is burned into the Samsung **Shannon** modem
firmware (`g5400c-main.bin`), mirrored here as `LTE_CONFIGS`:

| `lte_<id>` | family | category codes | model |
| --- | --- | --- | --- |
| `400907661` | `mmw` | `0x111 0x121 0x141` | Pixel 9 / 9 Pro / 9 Pro XL, mmWave (US) |
| `2160127815` | `sub6` | `0x112 0x122 0x142` | Pixel 9 / 9 Pro / 9 Pro XL, sub-6 (RoW) |
| `4210990300` | `ct3` | `0x181` | Pixel 9 Pro Fold |
| `564260317` | `tki3` | `0x211` | — (no file in dumps; listed so a future file is recognized) |
| `1254026417` | `mmw_p25` | `0x411 0x421 0x441` | Pixel 10 / 10 Pro / 10 Pro XL, mmWave (US) |
| `4017061044` | `sub6_p25` | `0x412 0x422 0x442` | Pixel 10 / 10 Pro / 10 Pro XL, sub-6 (RoW) |
| `2306930561` | `rg5` | `0x481` | Pixel 10 Pro Fold |
| `844857560` | `sta5_na` | `0x812` | — (unconfirmed) |
| `1534561764` | `sta5_jp` | `0x814` | — (unconfirmed) |

**Category code** is a `platform | sku | variant` nibble bitfield. The middle (SKU)
nibble `1/2/4` = base/Pro/Pro XL all route to the **same** file (so mmWave-US Pixel 9,
9 Pro, and 9 Pro XL share `mmw`), and `8` = Fold. Corroborating evidence: **B32 presence
marks sub-6/RoW** (US files lack B32), and each Fold's combos are **byte-identical** to
its same-generation RoW phone (`ct3` ≡ `sub6`, `rg5` ≡ `sub6_p25`).

The trailing `<id>` is **not** computed from the file (every CRC/FNV/hash over names and
content was ruled out) — it is a hardcoded `lte_file_id` from a selection table in the
firmware. Observed disassembly anchors in that build: the selector at `0x424EEDD4` writes
`lte_file_id = *(this + 0x7A13C)`; the filename builder is at `0x424EEA68`; the file is
served over the UecapFile RPC at `0x42E07046`.

## Design conventions & rationale

- **Selector bytes stay `Option<Vec<u8>>` (protobuf-shaped) end-to-end**; hex is a
  rendering concern only at human-output edges. The same philosophy drove NR cap fields
  to protobuf-numeric.
- **Wider integer types** (`uint64`) on opaque and fingerprint fields avoid truncation
  while staying value-identical.
- **`PHONE_MODELS` evidence.** The legacy set is the exact cellular Tensor population in
  the pinned `pixel-bands` snapshot. The 18 profiled `(nr_anchor, lte_id)` mappings retain
  the existing support-page + modem-table evidence used by `provision`; payload equality
  is never evidence for a new mapping. Every code is validated against
  `pixel_bands::PIXEL_BANDS`. Region labels are cosmetic — band filtering uses the exact
  band sets, not the label. See [Registry evidence rule](#registry-evidence-rule).
- **Scope discipline.** `inspect` decodes LTE, but `compare`/`check` do not;
  `check` only counts `lte_*` files; the `magisk` module packages raw bytes without
  decoding them.

## Glossary

- **UE-caps** — the device-advertised capability blobs (`.binarypb`): which bands and CA
  combinations a Pixel offers.
- **PLMN** — Public Land Mobile Network, an MCC-MNC pair; the legend maps PLMNs → carriers.
- **Legend** — `ap_plmn_mapping.binarypb`, the PLMN→carrier map (one per dump).
- **SKU profile / anchor prime** — each SKU profile has a unique anchor prime; a carrier
  NR file belongs to it iff its `NUMBER` is divisible by that anchor.
- **Fingerprint / version** — the field-1 value; `fp_info` maps it to `(family A|B, tier)`.
- **Carrier signature** — the common factor of all of a carrier's file numbers; the SKU
  portion is `NUMBER ÷ signature`.
- **Band combo / CA combination** — an aggregated set of component carriers (one
  `Combo` / `LteCombo`).
- **Sub-block** — one band+CA-bandwidth-class entry within a combo (`SubBlock` /
  `LteComponent`); NOT one CC — it physically contains `cc_count(kind, bw_class)` CCs
  (e.g. `n78C` = 2 CCs).
- **CC / component carrier** — one physical carrier inside a sub-block; a sub-block's
  `dl_features`/`ul_features` hold one feature-set entry per CC.
- **Feature set / per-CC caps** — a DL or UL capability record (SCS, MIMO, max BW, mod
  order, 90 MHz), stored once globally and referenced per CC by selector bytes.
- **bcs** — bandwidth combination set (a per-combo opaque numeric).
- **Bandwidth class** — the A–F letter (with MIMO branches) describing a CC's aggregation
  class.
- **EN-DC** — E-UTRA-NR Dual Connectivity; an NR combo mixing LTE (`B…`) and NR (`n…`)
  components.
- **LTE-fallback config** — the `lte_*.binarypb` a modem loads by hardware category;
  LTE-only, no NR.
- **Magisk module** — the flashable `.zip` the tool emits, overlaying edited files at an
  on-device destination.
