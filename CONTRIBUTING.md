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
- **A fact local to one wire field** → a comment in `proto/ue_caps.proto` or the code that
  enforces it.

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

## Build & codegen

- **Rust edition 2024**, stable toolchain (uses `let`-chains — see [Gotchas](#gotchas)).
- **Codegen is pure Rust, at build time.** `build.rs` runs `protox::compile(["proto/ue_caps.proto"], ["proto"])` to a `FileDescriptorSet`, then `prost_build::Config::new().compile_fds(...)` emits Rust into `OUT_DIR`, surfaced through `src/proto.rs`. No system `protoc` is needed. `build.rs` emits `cargo:rerun-if-changed=proto/ue_caps.proto`, so **editing the proto requires a rebuild before the new types exist** — a stale `cargo check` against not-yet-generated fields will mislead you.
- **`pixel-bands` is pinned by an explicit `rev` in `Cargo.toml`** (`{ git = "https://github.com/vorot93/pixel-bands", rev = "…" }`). The `rev` alone fixes the exact upstream commit, so a fresh clone or CI run builds identical `PIXEL_BANDS` data and a regression is bisectable; bumping the dependency is editing the `rev`. **`Cargo.lock` is gitignored**, so other crates.io deps resolve to the latest semver-compatible versions per build. A first build needs network access unless that revision is cached. It provides `PIXEL_BANDS: phf::Map<&str, Bands>` — compile-time band data keyed by Google 5-char model code — consumed by `PHONE_MODELS` and by provision's band-compatibility filtering.
- **Direct deps:** `thiserror`, `prost`, `num-prime`, `num-integer`, `clap` (derive), `csv`, `anyhow`, `zip` (`default-features=false`, `deflate`), `pixel-bands`, `tempfile`, `compact_str`, `kdl` (`default-features=false`, `span` feature only — the crate's `serde` feature is deliberately unused; see [Source format: KDL, hand-mapped](DESIGN.md#source-format-kdl-hand-mapped-not-serde)). **Build deps:** `prost-build`, `protox`. No `[lib]`/`[[bin]]`/`[features]` sections — targets are auto-detected. **`serde`/`serde_derive` and `toml` are deliberately absent**: every persisted/emitted format is hand-mapped KDL, so there is no serde consumer left. Don't reintroduce them.
- **Pure-Rust build is a stated value.** Do not add a C-toolchain dependency (a C-compiled allocator like mimalloc/jemalloc, a native protobuf compiler, etc.) without surfacing the tradeoff first — a measured allocator win was deliberately reverted to keep the no-C-compiler build (see [Performance](#performance-dont-de-optimize)).

## Tests & CI gates

CI (`.github/workflows/main.yml`, on PR and push to `master`) runs, in order:

1. `cargo fmt --all --check -- --config=imports_granularity=Crate` — **non-default fmt config** (imports grouped and alphabetized per crate). Match it locally or the gate fails; plain `cargo fmt` does **not** match.
2. `cargo hack clippy --workspace --each-feature -- -D warnings` — needs `cargo-hack`; note it is **not** `--all-targets`. (With no `[features]` table, `--each-feature` is one pass, so `cargo clippy --workspace -- -D warnings` is an equivalent local proxy.)
3. `cargo hack test --workspace --each-feature`.

Also run `cargo run -- self-test` (subcommand is `self-test`): data-independent checks (factorizer, fingerprint→tier, a protobuf decode) that print `ALL TESTS PASSED` and return non-zero on failure.

**Tests are hermetic by default** — fixtures are built in-code or under a temp dir, and
`pixel-bands` is compile-time data, so real codes like `GUL82` work with no external
files. One optional PLMN round-trip activates only when `UECAPS_PLMN_FIXTURE` points at
a real legend file. Full-corpus compiler verification activates only when **both**
`UECAPS_BITMASK_CORPUS` and `UECAPS_PROFILED_CORPUS` point at their respective input
directories; with either unset, that one test (`tests/compiler_corpus.rs`) prints a single
skip note and returns. With both set, it compares two independently decoded canonical
sources, checks the observed LTE invariants, builds every registered `PHONE_MODELS` target,
and inspects every generated NR file for fully referenced, strictly canonical compact
feature catalogs and one-byte resolved selectors. A skipped pass validates only the guard:
claim real-corpus coverage only from a run in which those assertions actually execute.

```sh
env UECAPS_BITMASK_CORPUS=/path/to/uecapconfig-bitmask \
    UECAPS_PROFILED_CORPUS=/path/to/uecapconfig-profiled \
    cargo test --release --test compiler_corpus
```

## Performance: don't de-optimize

The corpus test is also the **perf-regression guard** for the compiler hot paths — every
optimization below is byte-identical and guarded by it. The corpora are 89 bitmask-based
carriers and ~1,390 profiled + 8 LTE carriers. **Do not de-optimize these:**

- **The selection algebra keys on interned integer ids, not strings.** `NrDomain::new` interns the domain's carriers and skus to dense `CarrierId`/`SkuId` (`u16`) **in sorted order** (an id's numeric order equals its value's `Ord`), so `NrRelation` (`BTreeSet<(CarrierId, SkuId)>`) and the domain sets do 4-byte integer set ops instead of 24-byte string compares, while `canonical_selection` de-interns back to string tokens in the exact canonical order. **The sorted-id assignment is load-bearing for byte-identity** — do not intern in insertion order, and do not revert these sets to `(String, Sku)`/`(CompactString, Sku)`. `Sku::Model` holds a `CompactString` (SSO/inline) for the same reason.
- **`selected_payloads` reads a prebuilt inverted index, not a scan.** `ValidatedNr.selection_index` (`NrSelectionIndex`, a `HashMap<(CarrierId, SkuId), Vec<u32>>`) is built once in `validate_documents` and maps each interned target to the `combo` indices whose relation contains it, ascending. It replaced a per-target linear scan over every combo (the #1 leaf of a full-corpus `decode`, since generation calls `selected_payloads` once per carrier per target). Ascending indices reproduce the old `combo.iter().filter(..)` order, so output is bit-identical. **The index points at `combo` positions** and is valid only while `combo` is unchanged: production sets both once (`canonicalize_sources` reads but never reorders `combo`); test combo surgery must go through `ValidatedNr::set_combos`, which rebuilds `features` and `selection_index` together. Do not revert to a per-combo scan.
- **`decode` validates twice, not three times.** `decode_documents` calls `validate_documents` on the ingest (canonicalize → `nr_text`), then `parse_sources` (the reparse validation boundary), then serializes the reparsed source **directly** via `ValidatedSources::to_kdl`. An earlier third `to_kdl(&validated.source)` re-validated an already-validated source purely to serialize it. The byte-idempotence assertion (`canonical == nr_text`) still runs and fails closed if canonicalization isn't a fixed point. Do not route decode's final serialize back through the free `to_kdl` (now `#[cfg(test)]`) — that re-adds the third validate.
- Also load-bearing and byte-identical: the `NrDomain` carrier/sku/row projection cache read by `NrRelation::{from_selection, canonical_selection}`; interning inputs once at the boundary (`NrDomain::{carrier_id, sku_id, probe, relation}`); the direct `RawNrPayload::from_proto_combo` ingest (not a report `Combo`/`SubBlock` DTO round-trip); and `sort_by_cached_key` at the deep-clone NR sort sites.

**The KDL parser is the floor; measure a single `decode`/`build`, not the corpus.** A leaf
profile of one `build` is ~93% `load_sources` (parse + validate the ~19 MB `nr.kdl`), and
~55% of that is the `kdl` v2 winnow parser: ~20% error/span machinery (`Recoverable` +
`LocatingSlice` + `span` overhead, paid per token even on the happy path), ~13%
whitespace/newline (proportional to document size), ~11% `Alt::choice`.
`validate_documents` (the selection algebra) is ~8%. Two consequences:

- **The corpus win was parsing once, not micro-optimizing the parser.** The test builds all `PHONE_MODELS` by calling `build()` per model, each re-reading + re-parsing the ~19 MB source; it now parses once via `load_sources` and generates each model with `build_from_sources(&ValidatedSources, …)` (both `pub`; `ValidatedSources` is an opaque handle), so per-model cost is the ~0.4 s generate+zip+write. `build()` stays the single-model wrapper over `load_sources` + `build_from_sources`. **Do not re-collapse this into a per-model `build()` loop** — that reinstates the ×N parse. The corpus test itself is fanned out with `rayon` (a `[dev-dependencies]` entry only, never in the shipped binary): the two byte-idempotence decodes run via `rayon::join` and the per-model builds via `PHONE_MODELS.par_iter().for_each` over the shared immutable `&ValidatedSources`. Don't revert the loop to a serial `for` to "simplify" — the parallel form is the point.
- **The ~20% error/recovery machinery is not reachable via the `span` feature** (investigated + measured): `kdl` 6.7.1 has one parse path whose input is hardwired to `Recoverable<LocatingSlice<&str>>`, and `span` only gates whether nodes *store* a span — unused here (we read no `.span()`), and dropping it measured **zero** decode delta. So there is no further product-parse lever inside `kdl` short of forking it or swapping the crate; treat a single `decode`/`build`'s parse cost as effectively the crate's floor. That parse **is** the "serialization is a validation boundary" guarantee, so it can't be skipped.

**Rejected experiments — don't re-attempt without new evidence:**

- **`HashSet`/`Hash` for the `RawNrPayloadKey` dedup sets** (`canonical_payloads` + `validate_nr_combos`): byte-identical but **~7% *slower*** (min-of-5, interleaved). The `BTreeSet`'s `Ord` compares short-circuit on the first differing field (`kind`/`band`); hashing traverses the whole key (every `RawSubBlockKey`, both `Vec<u8>` id-lists). The identity-key ops are at their practical floor — don't re-try HashMap/HashSet dedup or per-cc-id interning without a fundamentally cheaper key.
- **mimalloc as `#[global_allocator]`**: byte-identical, **~5.6% *faster*** decode (min-of-5) — a real win, but **reverted to keep the pure-Rust / no-C-compiler build** (a stated value). Don't re-add a C-dependency allocator without revisiting that tradeoff. The next real lever on the floor is forking/swapping `kdl`.
- **De-allocating `NodeReader::positional()`'s per-call `Vec`**: the suspicion that this was a hot cost was **wrong** (<3% of the profile) — don't spend effort on it.
- **Packing the `selection { carriers …; skus … }` lists into strings** (as was done for the machine-noise per-cc-id blocks): declined. A combo's `selection` lists are the *human-relevant, diffable* part of the format (a real `skus` line runs ~250 chars); string-packing lengthens the line and turns a one-SKU edit into a whole-line re-diff, degrading the one readable part of the combo section for a secondary saving. `nr.kdl` is not meant to be hand-authorable and the residual ~11–14 MB of carrier/sku/plmn string lists is irreducible bulk anyway. Don't revisit without a readability-preserving encoding.

**Method for a perf change.** To prove byte-identity *and* measure: capture golden `sha256`
of `decode`'s `nr.kdl`/`lte.kdl` (and one model's built zip) from the current tip; after the
change, re-`decode` the full corpus and diff the source hashes (isolates ingest/decode), and
re-`build` one model from a *fixed* golden source and diff the zip hash (isolates
generation). `build` is deterministic. Single-decode wall-clock drifts ±~1 s run-to-run
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

## Single-source helpers — call them, don't re-duplicate

Every fact that used to be duplicated across the compiler↔patch↔report surfaces has one
home. When you need one of these, **call the helper**; re-inlining it is exactly the drift
that consolidation removed.

- **Band prefix (`n`/`B`).** `report::combos::band_label_for(is_nr, band)` is the source. `band_label(raw_band)` *infers* the kind from `NR_BAND_OFFSET`; every caller that already **knows** the kind (`RawSubBlock::band_label`, provision band-drop labels, `patch filter` LTE matching) calls `band_label_for` with its kind. **Do not** route a known-kind band through the inferring `band_label` — it would mislabel an out-of-range value (a stray LTE band ≥ offset would render as NR). This infer-vs-assert split is the subtle trap here. **Exception:** `report::lte` and `patch::show::render_lte` format `B` inline instead of calling `band_label_for` — both are statically single-kind (LTE-only), so no NR component can reach them. Don't "finish the dedup" by folding them in.
- **Fingerprint ↔ (family, tier).** `model::FINGERPRINTS` is the table; `fp_info` (inverse), `fingerprint_for` (forward), `tier_fingerprints`, and the compiler's `modern_fingerprint` wrapper all derive from it.
- **Per-tier profile counts (16/14).** `model::tier_profile_count` + `MAIN_ONLY_ANCHORS` (the Alt tier lacks anchors `2912407`/`3539`); `check` derives its 16/14 from these.
- **Selector-only-stays-unresolved check (1-based leading byte).** `report::combos::feature_index(ids, len)` — used by the compiler's generated-file self-verification (`verify_compact_feature_list`, `src/compiler/nr.rs`), its only caller, to confirm a raw selector-only array still does *not* resolve against a (possibly shrunk) list; **not** the real per-CC resolution path — that's `resolve_all(ids, list)` (also in `report::combos`), which resolves a whole direction's array all-or-nothing across every byte and is what the patch axis uses (`src/patch/build.rs`). `feature_index`'s first-byte check is sufficient here only because, by construction, a resolved reference's bytes are all-or-none catalog indices.
- **11-field DL/UL display projection.** `report::combos::SubBlock::from_raw_fields(...)`.
- **`ComboHeader` combo header.** `raw_nr::RawNrPayload::header()`.
- **Shortest-decimal `u64`.** `compiler::parse_shortest_u64`. **Exception:** `compiler::decode::parse_filename_number` keeps its own two-step form on purpose — it needs a distinct, *tested* "does not fit u64" vs "must be shortest decimal" message the `Option` primitive can't express. Don't "finish the dedup" by folding it in.
- **Carrier NR-file selection.** `provision::select_nr_file` (both `select_files` and the CLI `run` path); it owns the nonzero-`NUMBER` guard and the sort.
- **Band-drop retain.** `provision::retain_compatible` (NR/LTE twins).
- **File-or-stdout output.** `output::write_out(bytes, out, what)` (`patch`/`filter`/`magisk`/`provision`); `what` (`"module"`/`"patch"`/`""`) selects the error-message noun.
- **Carrier-name validation.** `compiler::schema::validate_carrier_name`.

Historically, correctness findings clustered on the older single-file surfaces
(`patch`/`mapping`/`provision`/`report`) while the compiler (`src/compiler/**`, `src/wire.rs`)
stayed clean. When fixing one, prefer lifting the compiler's strict discipline into the
shared layer over patching each call site.

## Settled — do not re-investigate without new evidence

These are verified correct against the real code paths and stay guarded by the tests and
invariants documented here — don't re-audit them absent a concrete new symptom: the PLMN
packed-BCD math (both directions, all five documented vectors), the canonical
rectangle/selection algebra, Kahn topological-ordering determinism, ZIP output determinism,
`atomic.rs` temp/rename semantics, the strict-vs-lenient PLMN duplicate polarity, LTE
class-letter bit decoding, and the clap argument relationships.

## Gotchas

- **`let`-chains require edition 2024** (`&& let` chains in `provision.rs` and `report/matrix.rs`). Down-editioning won't compile.
- **A shared NR anchor does not identify a model.** Pixel 10 Pro XL codes `GUL82`, `G45RY`, `GYPW4` all use `nr_anchor = 3616442437` but differ in `lte_id` (`GUL82` mmWave-US = `1254026417`; the sub-6 pair = `4017061044`).
- **`patch filter` rejects a bare band number** — it is ambiguous in an EN-DC patch. Use an `n`/`B` prefix (case-insensitive; `N77` → `n77`, `b66` → `B66`); `77`, `n`, `x5` all error. An empty filter result is valid (writes an empty patch + a stderr note).
- **Magisk path layout.** The standalone `magisk`/`provision` destination lands at `system/<dest-without-leading-slash>/<basename>`; `dest` must be absolute and not bare `/`. A `dest` of `/system/etc` yields a doubled `system/system/etc/…`. `--dest` (like input basenames) is rejected for `..`/`.`/empty path components or control/line-separator characters, and `--name` is rejected for control/newlines — otherwise a crafted value would escape the module tree on extraction or inject a `module.prop` line. The checks live in the shared `build_archive`, so `magisk`, `provision`, and the compiler `build` all get them. Folder-compiler `build` is different: its destination is fixed at `/vendor/firmware/uecapconfig` and it always adds `.replace`.
- **`self-test`** is the CLI name (kebab-case of the `SelfTest` variant); the string to grep for is `ALL TESTS PASSED`.
- **KDL test literals live in more than one file.** For any change to the `nr.kdl`/`lte.kdl` writers or readers, the goldens/inputs are spread across `compiler/kdl_source.rs` (unit goldens), `compiler/schema.rs` (reader-input strings), `compiler/test_support.rs` (`EXPECTED_NR_KDL`/`EXPECTED_LTE_KDL`, byte-compared in `compiler::decode`), and `report/inspect.rs` (assertions). And `inspect --kdl` **reuses the compiler writer** (`emit_nr_combo`), so a writer change flows into inspect automatically. `grep -rn` (without piping to `head`) to find every site.
- The crate is a lib + bin, so `pub` items are the library's API and reachable — `dead_code` won't fire on the exported surface, so `pub` items rarely need `#[allow(dead_code)]` (the source has none).
