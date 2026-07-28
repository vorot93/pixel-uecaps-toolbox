# Contributing to pixel-uecaps-toolbox

How to build, test, and work on this codebase, plus the conventions and traps that aren't
obvious from the code. For the architecture and the reverse-engineered wire formats see
[DESIGN.md](DESIGN.md); for how to *use* the tool see the [README](README.md).

## Docs are the durable memory

This project's durable memory is the reverse-engineered `.binarypb` knowledge — the wire
formats, the SKU/fingerprint math, the invariants that keep edits faithful, the
LTE-fallback selection table, and the traps. Keep it current so a wiped context can resume
from the docs alone. **When code and a doc disagree, the code wins — fix the doc.**

Where each kind of knowledge lives:

- **Public usage and editable-file syntax** → [README.md](README.md).
- **Architecture, wire formats, invariants, design rationale** → [DESIGN.md](DESIGN.md).
- **Build/test workflow, conventions, gotchas, performance** → this file.
- **A fact local to one wire field** → a doc comment on the field in `src/proto.rs` or the
  code that enforces it.

**Assume commit history may be squashed and transient plans removed.** Preserve
non-reconstructible evidence, rejected inferences, and the *reason* for a constraint in
these committed docs — do not leave them only in a commit message or a dated task document.
Corpus-measured statistics and rejected-hypothesis reasoning are exactly this kind of
non-reconstructible knowledge; when you measure something that justifies a rule, record the
number next to the rule (see DESIGN.md's format sections for the pattern).

The tool is feature-complete and corpus-validated, with no open review backlog. For the
commit-level state — including any local commits not yet on `origin` — run
`git log --oneline origin/master..master` (empty = in sync); pushing is at the user's
discretion. Don't pin a baseline SHA in the docs; it self-stales on the next commit.

## Build

- **Rust edition 2024**, stable toolchain (uses `let`-chains — see [Gotchas](#gotchas)).
- **No codegen — the protobuf types are hand-written.** `src/proto.rs` defines every message as a `#[derive(prost::Message)]` struct with `#[prost(...)]` field attributes. To add or change a field, edit the struct directly — the attribute states the wire behavior (a bare scalar is proto3 default-skip; `optional` keeps explicit presence; `packed = "false"` keeps a repeated field unpacked). Field tags must match the observed wire layout; the in-file `#[cfg(test)]` byte tests and the opt-in corpus tests guard that.
- **`pixel-bands` is pinned by an explicit `rev` in `Cargo.toml`** (`{ git = "https://github.com/vorot93/pixel-bands", rev = "…" }`). The `rev` alone fixes the exact upstream commit, so a fresh clone or CI run builds identical `PIXEL_BANDS` data and a regression is bisectable; bumping the dependency is editing the `rev`. **`Cargo.lock` is gitignored**, so other crates.io deps resolve to the latest semver-compatible versions per build. A first build needs network access unless that revision is cached. It provides `PIXEL_BANDS: phf::Map<&str, Bands>` — compile-time band data keyed by Google 5-char model code — consumed by `PHONE_MODELS` and by `report/selftest.rs`.
- **Direct deps:** `thiserror`, `prost`, `num-prime`, `num-integer`, `clap` (derive), `csv`, `anyhow`, `zip` (`default-features=false`, `deflate`), `pixel-bands`, `tempfile`, `compact_str`, `kdl` (`default-features=false`, `span` feature only — the crate's `serde` feature is deliberately unused; see [Source format: KDL, hand-mapped](DESIGN.md#source-format-kdl-hand-mapped-not-serde)). There are no `[build-dependencies]`, and no `[lib]`/`[[bin]]`/`[features]` sections — targets are auto-detected. **`serde`/`serde_derive` and `toml` are deliberately absent**: every persisted/emitted format is hand-mapped KDL, so there is no serde consumer left. Don't reintroduce them.
- **Pure-Rust build is a stated value.** Do not add a C-toolchain dependency (a C-compiled allocator like mimalloc/jemalloc, a native protobuf compiler, etc.) without surfacing the tradeoff first — a measured allocator win was deliberately reverted to keep the no-C-compiler build (see [Performance](#performance-readability-first-then-re-optimize)).

## Tests & CI gates

CI (`.github/workflows/main.yml`, on PR and push to `master`) runs, in order:

1. `cargo fmt --all --check -- --config=imports_granularity=Crate` — **non-default fmt config** (imports grouped and alphabetized per crate). Match it locally or the gate fails; plain `cargo fmt` does **not** match.
2. `cargo hack clippy --workspace --each-feature -- -D warnings` — needs `cargo-hack`; note it is **not** `--all-targets`. (With no `[features]` table, `--each-feature` is one pass, so `cargo clippy --workspace -- -D warnings` is an equivalent local proxy.)
3. `cargo hack test --workspace --each-feature`.

Also run `cargo run -- self-test` (subcommand is `self-test`): data-independent checks (factorizer, fingerprint→tier, a protobuf decode) that print `ALL TESTS PASSED` and return non-zero on failure.

**Tests are hermetic by default** — fixtures are built in-code or under a temp dir, and
`pixel-bands` is compile-time data, so real codes like `GUL82` work with no external
files. Full-corpus compiler verification activates only when **both**
`UECAPS_BITMASK_CORPUS` and `UECAPS_PROFILED_CORPUS` point at their respective input
directories; with either unset, that one test (`tests/compiler_corpus.rs`) prints a single
skip note and returns. With both set, it compares two independently decomposed canonical
sources, checks the observed LTE invariants, provisions every registered `PHONE_MODELS` target,
and inspects every generated NR file for fully referenced, strictly canonical compact
feature catalogs and one-byte resolved selectors. A skipped pass validates only the guard:
claim real-corpus coverage only from a run in which those assertions actually execute.

```sh
env UECAPS_BITMASK_CORPUS=/path/to/uecapconfig-bitmask \
    UECAPS_PROFILED_CORPUS=/path/to/uecapconfig-profiled \
    cargo test --release --test compiler_corpus
```

## Performance: readability first, then re-optimize

**Policy (2026-07-23): readability outranks speed.** If a clearer expression costs performance,
take it — the bar is a *large* readability gain, not any gain. This reverses the previous
"don't de-optimize" rule. It is a **sequencing** decision, not an abandonment of performance: the
speed is meant to come back, on top of the clearer code, without re-proceduralizing it.

**Record every trade.** A change that gives up speed must say what it gave up and roughly what it
cost, in the commit body *and* in the ledger below. An unrecorded de-optimization is
indistinguishable from an accident, and the re-optimization pass cannot act on it.

**Three items below are NOT covered by that permission.** They govern **byte-identity**, not
throughput; reverting them changes generated bytes, and the corpus test is what catches it:

- **`NrDomain::new` interns carriers/skus in sorted order.** An id's numeric order equals its
  value's `Ord`, so `canonical_selection` de-interns in the exact canonical order. Interning in
  insertion order — or reverting the sets to `(String, Sku)`/`(CompactString, Sku)` — changes
  canonical output order. (`Sku::Model` holds a `CompactString` for the *speed* reason; that part
  is tradeable.)
- **`NrSelectionIndex`'s combo indices are ascending.** The index itself may go; the ordering may
  not — ascending indices reproduce the previous `combo.iter().filter(..)` order. The index also
  points at `combo` positions and is valid only while `combo` is unchanged: test combo surgery
  must go through `ValidatedNr::set_combos`, which rebuilds `features` and `selection_index`
  together.
- **`decompose` validates twice, not three times.** The two-validate structure carries the
  byte-idempotence assertion (`canonical == nr_text`), which fails closed if canonicalization
  isn't a fixed point. Do not route decompose's final serialize back through the free `to_kdl`
  (now `#[cfg(test)]`) — that re-adds a third validate.

**And one is a test-infrastructure cliff, not a micro-optimization.** Keep the `load_sources` +
`provision_from_sources` split. Collapsing it into a per-model `provision()` loop reinstates an ×N
re-parse of the ~19 MB source and takes the corpus test from **~35 s to ~426 s**. The corpus test
is also fanned out with `rayon` (a `[dev-dependencies]` entry only, never in the shipped binary):
the two byte-idempotence `decompose` calls run via `rayon::join` and the per-model provisions via
`PHONE_MODELS.par_iter().for_each` over the shared immutable `&ValidatedSources`. Don't revert the
loop to a serial `for` to "simplify" — the parallel form is the point.

**Fair to trade for clarity:** the `BTreeSet`-vs-`HashSet` key choice, the `NrDomain`
carrier/sku/row projection cache, boundary interning (`NrDomain::{carrier_id, sku_id, probe,
relation}`), `sort_by_cached_key` at the deep-clone NR sort sites, and the direct
`RawNrPayload::from_proto_combo` ingest (rather than a report `Combo`/`SubBlock` DTO round-trip).

**The KDL parser is the floor; measure a single `decompose`/`provision`, not the corpus.** A leaf
profile of one `provision` is ~93% `load_sources` (parse + validate the ~19 MB `nr.kdl`), and
~55% of that is the `kdl` v2 winnow parser: ~20% error/span machinery (`Recoverable` +
`LocatingSlice` + `span` overhead, paid per token even on the happy path), ~13%
whitespace/newline (proportional to document size), ~11% `Alt::choice`.
`validate_documents` (the selection algebra) is ~8%. Two consequences:

- **The corpus win was parsing once, not micro-optimizing the parser.** The test provisions all `PHONE_MODELS` by calling `provision()` per model, each re-reading + re-parsing the ~19 MB source; it now parses once via `load_sources` and generates each model with `provision_from_sources(&ValidatedSources, …)` (both `pub`; `ValidatedSources` is an opaque handle), so per-model cost is the ~0.4 s generate+zip+write. `provision()` stays the single-model wrapper over `load_sources` + `provision_from_sources`.
- **The ~20% error/recovery machinery is not reachable via the `span` feature** (investigated + measured): `kdl` 6.7.1 has one parse path whose input is hardwired to `Recoverable<LocatingSlice<&str>>`, and `span` only gates whether nodes *store* a span — unused here (we read no `.span()`), and dropping it measured **zero** decompose delta. So there is no further product-parse lever inside `kdl` short of forking it or swapping the crate; treat a single `decompose`/`provision`'s parse cost as effectively the crate's floor. That parse **is** the "serialization is a validation boundary" guarantee, so it can't be skipped.

**Rejected experiments — don't re-attempt without new evidence:**

- **`HashSet`/`Hash` for the `RawNrPayloadKey` dedup sets** (`canonical_payloads` + `validate_nr_combos`): byte-identical but **~7% *slower*** (min-of-5, interleaved). The `BTreeSet`'s `Ord` compares short-circuit on the first differing field (`kind`/`band`); hashing traverses the whole key (every `RawSubBlockKey`, both `Vec<u8>` id-lists). The identity-key ops are at their practical floor — don't re-try HashMap/HashSet dedup or per-cc-id interning without a fundamentally cheaper key.
- **mimalloc as `#[global_allocator]`**: byte-identical, **~5.6% *faster*** decompose (min-of-5) — a real win, but **reverted to keep the pure-Rust / no-C-compiler build** (a stated value). Don't re-add a C-dependency allocator without revisiting that tradeoff. The next real lever on the floor is forking/swapping `kdl`.
- **De-allocating `NodeReader::positional()`'s per-call `Vec`**: the suspicion that this was a hot cost was **wrong** (<3% of the profile) — don't spend effort on it.
- **Packing the `selection { carriers …; skus … }` lists into strings** (as was done for the machine-noise per-cc-id blocks): declined. A combo's `selection` lists are the *human-relevant, diffable* part of the format (a real `skus` line runs ~250 chars); string-packing lengthens the line and turns a one-SKU edit into a whole-line re-diff, degrading the one readable part of the combo section for a secondary saving. `nr.kdl` is not meant to be hand-authorable and the residual ~11–14 MB of carrier/sku/plmn string lists is irreducible bulk anyway. Don't revisit without a readability-preserving encoding.

**Method for a perf change.** To prove byte-identity *and* measure: capture golden `sha256`
of `decompose`'s `nr.kdl`/`lte.kdl` (and one model's built zip) from the current tip; after the
change, re-`decompose` the full corpus and diff the source hashes (isolates ingest/decode), and
re-`provision` one model from a *fixed* golden source and diff the zip hash (isolates
generation). `provision` is deterministic. A single `decompose` run's wall-clock drifts ±~1 s run-to-run
(CPU freq, page cache), so a lone before/after pair is unreliable below ~5%: build both
binaries, run them **interleaved** (`for i in 1..5: time HEAD; time NEW`) and compare
**minimums** (noise only adds time). Build the comparison HEAD binary via `git stash push` →
`cargo build --release` → copy the binary aside → `git stash pop` (`git diff > x.patch`
first as insurance). Neither `/usr/bin/time` nor `bc` is installed here — time with
`date +%s%N` and integer millisecond math.

**Getting a CPU profile here.** Build with `CARGO_PROFILE_RELEASE_DEBUG=2` (adds DWARF
symbols to the release profile without changing codegen), and record with
`perf record -e cpu-clock -F 299 --call-graph dwarf,32768`. `perf` works on this WSL2 kernel
**only via the `cpu-clock` software event** (no hardware PMU — the default `cycles` event
captures nothing). DWARF stacks frequently truncate before the top-level frames, so the
inclusive/`--children` call tree is unreliable: attribute by **leaf** function
(`perf report --stdio --no-children`) or fold `perf script` stacks and bucket by a marker
symbol.

### Baseline (captured at the end of the strip-to-compiler removal phase)

Measured on the real corpora (89 bitmask carriers; ~1,390 profiled + 8 LTE carriers): release
build, on an otherwise-idle 24-core machine, min-of-*N* interleaved runs (`date +%s%N` and
integer-millisecond arithmetic — neither `/usr/bin/time` nor `bc` is installed; see Method above):

| Measurement | Value |
| --- | --- |
| `decompose` (single run, min-of-5) | 18463 ms |
| `provision GUL82` (single run, min-of-5) | 5215 ms |
| corpus test wall-clock (min-of-3) | 36242 ms |
| `sha256 nr.kdl` | `e66178b987fdff7817afa16f891de599811ad5c776afe5171e39d55735e483a2` |
| `sha256 lte.kdl` | `b0e14205bf834b0ebb49d5bc53dd9e0eceb6e64dcba4c535bb52557566e5b07f` |
| `sha256` GUL82 module ZIP | `8370491be05da0148eb20865835762bb0ca8d2c2253550c3c6af23cd6f5ac8ff` |

Structural baseline at this commit — functions over 60 lines / nested deeper than 4 / `bool`
parameters: **`long=20 deep=16 bool_params=11`** (two of the 20 long functions,
`model::mcc_country` and `report::selftest::self_test`, are pure data/check tables, not
procedural sprawl, and stay exempt from a "shorten this" reading of the number).

### Trade ledger

Every readability change that measurably cost speed, newest first. Re-optimization works this list
by measured cost. A trade that cost nothing measurable stays traded — the clearer code wins by
default, and that outcome is recorded here too.

| Change | Commit | What it gave up | Measured cost |
| --- | --- | --- | --- |
| **`lte.kdl` direction keys**: `dm`/`um` → `d`/`u`, the `off` token deleted, an absent `u` meaning the explicit zero | the `feat(kdl)` follow-up series | **A visible distinction between two encodings.** `B66 d=C2` now reads differently in `nr.kdl` and `lte.kdl`; the document decides, and the Rust constants `DL_MIMO`/`UL_MIMO` are the only place that is recorded. Paid for by making the omit-when-0 rule fail-closed instead of assumed — `validate_lte_combos` rejects a proto `dl_bw_class_mimo == 0` or `ul_bw_class_mimo == None`, naming the combo and band — so the round trip is value-faithful by construction. The NR sub-block's equivalent rule remains assumption-only; that gap is deliberate and unclaimed. | _`lte.kdl` 779 916 → 705 912 bytes (**9.49% smaller**); 8 281 of 12 159 corpus sub-blocks shed a `um=off` carrying no information._ |
| **KDL short vocabulary**: every source key abbreviated, band folded into the sub-block node name, bandwidth class merged with its per-CC list and rendered as its 3GPP letter | the `feat(kdl)` series on `feat/kdl-short-vocabulary` | **Readable key names.** This trades *against* "readability outranks performance" and is recorded as such. Two things make it defensible: the class *letter* adds meaning the integer lacked (`n257 d=G30,30` reads as FR2, 2 CC, both on catalog entry 30, where `dl-bw-class=7` needed a lookup table), and the per-carrier header vocabulary — where names carry semantic weight — stays legible. README gained a reading guide so a hand-editor does not need the design doc. | _Corpus KDL 12 685 804 → 7 646 422 bytes (**39.7% smaller**; nr.kdl 39.4%, lte.kdl 42.6%). `decompose` min-of-5 **17 401 ms → 15 586 ms, 10.4% faster**, measured back-to-back on one machine. Note the speedup is about half what a bytes-proportional estimate predicted: the token COUNT is unchanged — same nodes and properties, shorter names — so the parser walks the same structure and the win tracks tokens more than size._ |
| **Clarity/idiom review pass** (2026-07-23): dead DTO→payload ingest path deleted; `report::combos::SubBlock` 21 fields → 6 (11 derived display projections + 4 never-read wire fields dropped, projection moved into `fmt_cc_features`); `NrSourceSubBlock`, `ValidatedCarrier` and the LTE identity types re-shaped as sums/proven pairs; `Direction`/`Sku: Display`/`split_raw_band`/`legend_root`/`catalog_indices` introduced as single sources | see the `clarity/idiom review` commits | Nothing measurable | _`decompose` min-of-5 **16225 ms** vs. the 18463 ms baseline; `provision` min-of-5 **4999 ms** vs. 5215 ms — both faster (load average 2.02 vs. the baseline capture's 2.50, so read this as "no regression", not a win). All three golden hashes byte-identical; all six report outputs (`check` ×2, `matrix`, `inspect --full`, `inspect` legend, `compare --full --common`) byte-identical against unmodified master; full corpus test green_ |
| **Phase 3 total** (Task 20 gate) | `7ece840..5e0b892` | the idiomatic-Rust pass, as itemized above (9 tasks, 26 commits) | _`decompose` min-of-5 17767 ms vs. 18463 ms baseline (−3.8%); `provision` min-of-5 5191 ms vs. 5215 ms baseline (−0.5%); corpus min-of-5 28933 ms vs. 36242 ms (min-of-3) baseline (−20.2%); all three faster or flat, no regression, and all three golden hashes still byte-identical_ |
| Task 18: split the last 5 over-long/over-nested functions outside the compiler (`report/compare.rs`, `report/combos.rs`, `kdl_support`, `wire.rs`, `raw_nr.rs`), one commit each. `render_diff_body` was the crate's deepest function at nesting level 7 | `bce8d37..c2d87c7` | Nothing measurable | `decompose` min-of-5: 17654 ms vs. the 18463 ms baseline above — no measurable delta |
| Task 17: split 12 procedural compiler functions across 8 `src/compiler/*` modules, one commit each (readability only — same generated bytes, error messages, and error order) | `221eab3..eeee4d8` | Nothing measurable | `decompose` min-of-5: 18153 ms vs. the 18463 ms baseline above — no measurable delta |

**Net effect of the whole idiomatic-Rust pass:** ~30 functions split across 13 modules took the
structural counts from `long=20 deep=16 bool_params=11` to `long=2 deep=0 bool_params=3` (the two
remaining long functions are the exempt data tables; the three bools are the `From<bool>` CLI
boundaries) at **no measurable cost** — every generated byte identical, every report byte
identical. The permission to de-optimize for readability was never actually spent.

**Task 20 gate (2026-07-23), confirmed — not a new baseline.** Re-running the structural survey at
this pass's final commit (`5e0b892`) reproduces `long=2 deep=0 bool_params=3` exactly, matching the
"Net effect" figures above. The frozen Baseline table above this ledger is unmodified and still
reads `long=20 deep=16 bool_params=11`; Phase 4 measures against that, not against this entry. All
six Phase-3 acceptance criteria pass: the two long functions are exactly `model::mcc_country` and
`report::selftest::self_test`; 0 functions nested deeper than 4; all 3 surviving `bool` parameters
are `From<bool>` impls, each with a justifying comment at its definition (`outcome.rs`,
`report/detail.rs` ×2); 0 `Result<i32>` in any signature; `nr.rs`'s
`.expect("profiled number checked above")` is gone (removed in Task 14, `511b95b`, by splitting
`DecodedNrFile` into `LegacyNrFile` (no `number` field) and `ProfiledNrFile` (`number: u64`),
giving the profiled case a type-level number invariant instead of a runtime check); and the broad
review-citation grep (prose form included, not just the parenthesised `(R9)` form) returns exactly
one hit — `mapping/plmn.rs`'s `M2 M1 N3 M3 N2 N1`, PLMN wire-nibble notation, not a citation. All
three golden hashes (`nr.kdl`, `lte.kdl`, GUL82 `base.zip`) reproduced byte-for-byte against the
Baseline table's values, and the corpus test re-verified the same 1,741,849 LTE / 1,715,899 NR
component counts. The corpus test's outsized-looking −20.2% is not attributed to a code change —
`decompose`/`provision` alone moved only ~0–4%, within noise — and is more likely page-cache
warmth (the corpus was already re-read several times earlier in the same gate run) plus a quieter
machine (load average 1.22 vs. the Baseline capture's 2.50) than anything in this pass; recorded so
Phase 4 doesn't mistake it for a real win to preserve.

**Phase 4 (the re-optimization pass) ran and closed with an empty worklist.** The policy at the
top of this section promises the speed comes back *after* the readability pass, on the trade
ledger's evidence — and the ledger above is that evidence: `Phase 3 total` cost nothing measurable
(`decompose` 17767 ms vs. the 18463 ms baseline, `provision` 5191 ms vs. 5215 ms), and neither did
Task 17 or Task 18 individually. A re-optimization pass acts on what the ledger says was spent;
with nothing spent, there was nothing to win back, so Phase 4's re-optimization tasks were skipped
by explicit decision, not forgotten. Re-optimization ran; it just had no work.

That does **not** mean there is no further lever, only that this branch didn't spend one it could
win back. The KDL-parser breakdown above already measured the one that remains: a single
`decompose`/`provision` run is ~93% `load_sources`, ~55% of which is the `kdl` v2 winnow parser, and
forking or swapping `kdl` is the only further parse-time lever available. Keep the two apart: that
lever is **new optimization**, not recovery of anything phase 3 traded away — phase 3 never touched
the parser, so nothing here is this branch's own spending to reclaim. Treat a `kdl` fork or swap as
a from-scratch project with its own cost/benefit case, not as an item on this ledger.

## Single-source helpers — call them, don't re-duplicate

Every fact that used to be duplicated across the compiler↔report surfaces (and the since-removed
`patch`) has one home. When you need one of these, **call the helper**; re-inlining it is exactly
the drift that consolidation removed.

- **Band prefix (`n`/`B`).** `raw_nr::SubBlockKind::band_label(band)` (a method on the kind, beside its `raw_band`/`split_raw_band` siblings) is the source. The free `report::combos::band_label(raw_band)` *infers* the kind from `NR_BAND_OFFSET` and delegates to the method; every caller that already **knows** the kind (`RawSubBlock::band_label` and `raw_nr`'s validation/guard messages) calls the method directly. **Do not** route a known-kind band through the inferring free `band_label` — it would mislabel an out-of-range value (a stray LTE band ≥ offset would render as NR). This infer-vs-assert split is the subtle trap here. `report::lte` is statically single-kind and now routes every band through the method too (via its local `eutra` helper): it used to format `B` inline for the per-CC lines but call the *inferring* free function for the combo label, so a crafted `lte_*.binarypb` carrying band 10078 printed `n78A↓` and `B10078` for the same component in one report. "Statically single-kind" is a reason to call the method, not a licence to skip it.
- **Fingerprint ↔ (family, tier).** `model::FINGERPRINTS` is the table; `fp_info` (inverse), `fingerprint_for` (forward), `tier_fingerprints`, and the compiler's `modern_fingerprint` wrapper all derive from it.
- **Per-tier profile counts (16/14).** `model::tier_profile_count` + `MAIN_ONLY_ANCHORS` (the Alt tier lacks anchors `2912407`/`3539`); `check` derives its 16/14 from these.
- **Selector-only-stays-unresolved check (1-based leading byte).** `report::combos::feature_index(ids, len)` — used by the compiler's generated-file self-verification (`verify_compact_feature_list`, `src/compiler/nr.rs`), its only caller, to confirm a raw selector-only array still does *not* resolve against a (possibly shrunk) list; **not** the real per-CC resolution path — that's `resolve_all(ids, list)` (also in `report::combos`), which resolves a whole direction's array all-or-nothing across every byte. `feature_index`'s first-byte check is sufficient here only because, by construction, a resolved reference's bytes are all-or-none catalog indices.
- **DL/UL display projection.** `report::combos::fmt_cc_features`. The decoded display values
  (SCS kHz, MIMO/mod-order labels, max BW, 90 MHz) are **not** stored on
  `report::combos::SubBlock` — they are pure functions of its `dl_features`/`ul_features`
  records and are projected only here. Don't re-add pre-rendered scalar fields to the DTO;
  that is two representations of one fact with nothing keeping them in agreement.
- **Raw protobuf band → (kind, plain band).** `raw_nr::SubBlockKind::split_raw_band(raw)`, the
  inverse of `SubBlockKind::raw_band(band)`. Every site that *asserts* a component's kind from
  its raw band goes through it, so the `>= NR_BAND_OFFSET` comparison and the `- NR_BAND_OFFSET`
  subtraction each have one home.
- **The DL/UL axis.** `raw_nr::Direction`, with `Display` for the prose spelling (`DL`/`UL`) and
  `lowercase()` for field names (`dl`/`ul`). Don't take a direction as a `&str` — a typo in a
  string literal reaches a user-facing message with nothing to catch it.
- **An SKU's canonical text.** `Display for compiler::selection::Sku`. `parse_nr_sku` /
  `parse_lte_sku` are the inverse but need the surrounding domain to disambiguate, so there is
  deliberately no `FromStr`.
- **`ComboHeader` combo header.** `raw_nr::RawNrPayload::header()`.
- **Shortest-decimal `u64`.** `compiler::parse_shortest_u64`. **Exception:** `compiler::decompose::parse_filename_number` keeps its own two-step form on purpose — it needs a distinct, *tested* "does not fit u64" vs "must be shortest decimal" message the `Option` primitive can't express. Don't "finish the dedup" by folding it in.
- **File-or-stdout output.** `report::matrix` is now the only file-or-stdout site and keeps its own inline `match` on `Option<&Path>`. There is no shared helper any more (`output::write_out` had one caller left and went with `magisk`); don't reintroduce one for a single site.
- **Carrier-name validation.** `compiler::schema::validate_carrier_name`.
- **The PLMN legend projection.** `compiler::schema::legend_root(carriers)` — the id-ordered
  `MappingRoot` every validated carrier with a legend entry contributes. `provision`'s
  `generate_mapping_file` ships those bytes, `decompose`'s `rebuild_mapping` self-checks the
  originals against them, and `validate_mapping_projection` proves they re-encode. Building the
  projection separately in each of the three is how they could silently disagree.
- **Payload → `nr.kdl` combo.** `compiler::schema::nr_source_combo(payload, relation, domain,
  features)`, called by both the ingest side (`compiler::nr::finish_nr_document`) and the
  canonicalize side (`nr_source_combos`), so a sixth combo-header field can't be added to one
  and forgotten in the other.
- **1-based catalog references.** `compiler::features::catalog_indices` — the four DL/UL ×
  local-plan/global-catalog `binary_search(..) + 1` lookups are one generic helper.

Historically, correctness findings clustered on the single-file editing surfaces while the
compiler (`src/compiler/**`, `src/wire.rs`) stayed clean. Those surfaces have been removed; the
compiler's strict discipline is now the crate's baseline, and a new shared helper should meet it
rather than the old report-side leniency.

## Corpus wire-format evidence

Measured over the full opt-in corpus (1487 files, 80 MB, 3.46M sub-blocks) with throwaway
scanners. These numbers justify several fail-closed guards, and re-deriving them costs a corpus
pass each — so they live here rather than in a commit message.

**Nothing real violates any of these**, which is why `wire::scan` can enforce all of them:

| property | occurrences in the corpus |
|---|---|
| duplicate occurrence of a singular field | 0 |
| overlong (non-minimal) varint | 0 |
| descending tag order | 0 |
| interleaved repeated field | 0 |
| `uint32` / `bool` value out of range | 0 |
| unknown field / wrong wire type | 0 |
| proto field 8 (`srstxswitch`) on an E-UTRA sub-block | 0 |
| proto field 8 on an **NR** sub-block | 0 |
| `int32` value failing the sign-extension round-trip | 0 (largest `int32` anywhere is 32769) |
| proto fields 4/5 (`dl`/`ul_feature_index`) absent | 0 (field 4 is never even zero) |

**Selector presence tracks `bw_class` exactly** — the biconditional `RawSubBlock::validate`
enforces. `class >= 1` always carries proto field 6/7; `class == 0` never does; there are no
absent classes and no zero-length selectors:

| | `class >= 1` | `class == 0` |
|---|---|---|
| LTE DL | present ×1 741 849 | — |
| LTE UL | present ×709 707 | absent ×1 032 142 |
| NR DL | present ×1 715 899 | — |
| NR UL | present ×1 028 461 | absent ×687 438 |

Two consequences worth keeping:

- **NR DL is never class 0**, so the realistic explicit-zero *class* case is `ul_bw_class`, not
  `dl_bw_class`. A fixture using DL for that tests a shape the corpus does not contain.
- **`Combo.bitmask` differs by layout.** Every profiled combo carries 0 (877 266 of them); the
  bitmask folder carries ~150 distinct values and **never** the all-ones sentinel 65535 that
  `InputLayout::Legacy` writes. That is why the legacy NR target cannot be byte-compared
  against its input — see below.

### Why the NR self-checks compare file sets, not bytes

`verify_lte_targets` and the mapping check compare bytes; both NR targets compare only the file
set. That asymmetry is deliberate and matches DESIGN.md's "NR carrier files: value-level
fidelity, *not* byte-identity". NR ingest **normalizes**: it discards each legacy combo's input
bitmask and merges the resulting duplicates, sorts every combo's sub-blocks by `RawSubBlockKey`,
deduplicates combos, and prunes/renumbers the feature catalogs.

Measured, so nobody re-attempts it: regenerating the real
`1_1_DE_3379443364558429875.binarypb` yields a file **390 bytes shorter**, and at the first
difference the regenerated combo begins with E-UTRA band 1 where the original begins with NR
band 10001. A byte comparison there fails on genuine input. What the checks *must* do — and now
do — is compare against the folder's actual basenames rather than a list re-derived from the
same source field the generator iterates, which is what made them unable to fail at all.

## Settled — do not re-investigate without new evidence

These are verified correct against the real code paths and stay guarded by the tests and
invariants documented here — don't re-audit them absent a concrete new symptom: the PLMN
packed-BCD math (both directions, all five documented vectors), the canonical
rectangle/selection algebra, Kahn topological-ordering determinism, ZIP output determinism,
`atomic.rs` temp/rename semantics, the strict-vs-lenient PLMN duplicate polarity, LTE
class-letter bit decoding, and the clap argument relationships.

## Gotchas

- **`let`-chains require edition 2024** (`&& let` chains in `compiler/schema.rs` and `report/inspect.rs`). Down-editioning won't compile.
- **A shared NR anchor does not identify a model.** Pixel 10 Pro XL codes `GUL82`, `G45RY`, `GYPW4` all use `nr_anchor = 3616442437` but differ in `lte_id` (`GUL82` mmWave-US = `1254026417`; the sub-6 pair = `4017061044`).
- **Magisk path layout.** There is one destination and it is not configurable: generated files land at `system/vendor/firmware/uecapconfig/<basename>`, and the module always carries the `.replace` marker. `--dest` and its validation are gone (they defended a user-supplied value that no longer exists). What remains user-supplied is `--name`, which is interpolated as `name=<name>` in `module.prop` and is rejected for control/line-separator characters, and the generated basenames, which `validate_module_basename` rejects for path separators, `.`/`..`, and control characters.
- **`self-test`** is the CLI name (kebab-case of the `SelfTest` variant); the string to grep for is `ALL TESTS PASSED`.
- **Key spellings come from `compiler/kdl_keys.rs`; only the golden *documents* are literal.** Reader and writer both consume that module, so a rename is one table edit. The goldens themselves are still spread across `compiler/kdl_source.rs` (unit goldens), `compiler/schema.rs` (reader-input strings), `compiler/test_support.rs` (`EXPECTED_NR_KDL`/`EXPECTED_LTE_KDL`, byte-compared in `compiler::decompose`), `compiler/provision.rs`, `compiler/decompose.rs` and `tests/compiler_cli.rs` — `report/inspect.rs` carries none. `grep -rn` (without piping to `head`) to find every site.
- **Never bulk-rewrite KDL keys across the test files with a regex.** Three distinct ways it goes wrong, all hit during the short-vocabulary rename: (1) a pattern anchored to line starts matches Rust *identifiers*, turning `combo != 0` into `c != 0`; (2) Rust digit separators defeat `(\d+)` — `32_768` matches as `32` and becomes `8193_768`; (3) worst, it rewrites **diagnostics** as well as keys, so `"carrier `{c}` has no role"` becomes `"cr `{c}` ..."` *and* the assertion checking it changes too — the suite stays green while user-facing errors degrade. A key spelling belongs in `kdl_keys`; an error message is prose. They live in the same string literals and only context separates them. After any such change, audit production `bail!`/`ensure!`/`anyhow!` strings for stray short tokens.
- The crate is a lib + bin, so `pub` items are the library's API and reachable — `dead_code` won't fire on the exported surface, so `pub` items rarely need `#[allow(dead_code)]` (the source has none).
