# pixel-uecaps-toolbox

Inspect, audit, and compile the Google Pixel **UE-capabilities** protobufs that ship
in Pixel carrier-config packages — see exactly which LTE/5G bands a carrier
profile unlocks, diff two carriers, audit a folder, or rebuild a complete
model-specific `uecapconfig` folder.

> Not affiliated with or endorsed by Google. The file format is observed, not
> documented; this tool is for research and personal use.

## What you can do with it

`pixel-uecaps-toolbox` reads the per-carrier capability files a Pixel uses to tell
the network what it supports. With it you can:

- **See what a carrier profile unlocks** — every LTE/5G band combination, and per
  band: bandwidth, MIMO, modulation, SCS, and 90 MHz support.
- **Diff two files** — which band combinations (and per-component capabilities)
  differ between two carriers or two SKU profiles.
- **Audit a whole dump** — scan a folder of capability files and flag anything that
  doesn't fit the expected scheme, and export the carrier × profile matrix as CSV.
- **Edit a complete offline folder** — normalize the legacy and Exynos 5400 layouts
  into `nr.kdl` + `lte.kdl` with `decompose`, edit the KDL, then `provision` a
  deterministic full-replacement Magisk module for a real Pixel model code.

Editing goes through the folder compiler and nothing else: there is no single-file
edit, patch, or repackage command.

## Install

Build from source with a stable Rust toolchain (edition 2024):

```sh
cargo build --release
# binary at target/release/pixel-uecaps-toolbox
```

There is no build step or codegen: the protobuf message types are hand-written in
`src/proto.rs` with `#[derive(prost::Message)]`, and no external protobuf toolchain is needed.

Prebuilt binaries aren't published yet; build from source for now.

## Get your capability files

The files this tool reads ship inside Pixel **carrier-config packages** in two
different layouts:

- Older Tensor Pixels use one unnumbered `<CARRIER>.binarypb` per carrier, with
  per-combination model bitmasks.
- Exynos 5400 Pixels use numbered `<CARRIER>_<NUMBER>.binarypb` files — one per
  carrier × Pixel-SKU capability profile — plus `ap_plmn_mapping.binarypb` and
  hardware-selected `lte_*.binarypb` fallbacks.

On a device they live in the carrier-config storage; pulling them off needs root
and `adb`, and the exact path varies by Android build — search for your build's
carrier-config path.

> **Getting edited files back onto a device is your responsibility.** `provision`
> packages a complete replacement folder as a Magisk module. Installing it still needs
> root, varies by build, and editing carrier configs can break service. Proceed at your
> own risk.

## Recipes

Commands below are shown with the bare name `pixel-uecaps-toolbox`; if you haven't
installed it on your `PATH`, use `./target/release/pixel-uecaps-toolbox` instead.

### Decompose, edit, and provision a complete offline `uecapconfig` folder

The folder compiler consumes **both** generations together, writes exactly two
canonical source files, and builds one complete replacement module for a real phone:

```console
$ pixel-uecaps-toolbox decompose \
    --bitmask bitmask-uecapconfig/ \
    --profiled profiled-uecapconfig/ \
    -o source/
$ ls source/
lte.kdl  nr.kdl

# Edit source/nr.kdl and source/lte.kdl, then choose a registered phone model.
$ pixel-uecaps-toolbox provision G2YBB source/ -o pixel-uecaps-G2YBB.zip
```

`decompose` requires both directories. The bitmask input may contain only unnumbered
`<CARRIER>.binarypb` capability files and must contain at least one. The profiled
input must contain exactly one `ap_plmn_mapping.binarypb`, at least one numbered
carrier file, and at least one `lte_<id>.binarypb`. Unsupported or cross-layout
`.binarypb` names are errors; unrelated non-`.binarypb` files are ignored. Every
protobuf is recursively checked for fields and wire types the observed schema does
not model.

The generated documents are strict, versioned KDL: unknown keys, unsupported
versions, malformed references, and lossy values are rejected. A small valid
`nr.kdl` has this shape:

```kdl
version 2
bc VZW

bf 715188856 {
    c VZW
}

cr VZW bi=1 pi=0 mi=1 sg=1 t=main {
    p mcc=311 mnc=480
    pf "66813533" x=66813533 u=0
}

df s=3 m=2 b=100
```

Keys are abbreviated — see the table below. `bc` (bitmask-carriers) is the exact legacy output whitelist, and the fingerprint
groups must form a disjoint, complete partition of it. A carrier's `profile`
children are the exact whitelist for that carrier/anchor output. A profile's key
(the anchor number) is a quoted string argument — KDL requires quoting because it's
numeric-leading — while `multiplier` and `unknown` are native KDL integers;
`signature * multiplier` reconstructs the numbered filename. Profile family plus
`tier` derives the modern fingerprint, while field 9 remains stored as `unknown`
because it is opaque.

The three carrier IDs are independent: `bitmask-id` is legacy NR protobuf field 2;
`profiled-id` is optional signed `int32` profiled NR protobuf field 2, including
absence and present-zero, and may repeat across carriers; `mapping-id` is the unique,
full-width `u64` index into the **legend** (`ap_plmn_mapping.binarypb`, which maps each
network's PLMN to a carrier-config name), stored as a native KDL integer (KDL integers are
i128-backed, so the full `u64` range fits without string-encoding). `mapping-id` and
the carrier's PLMNs must either both be present or both be absent. Omitting them
excludes that carrier from the rebuilt profiled legend, while a bare, childless
`plmns` marker node (distinct from the per-entry `plmn mcc=… mnc=…` nodes shown
above) deliberately emits an entry with no PLMNs. PLMN order and duplicates are
significant and preserved.

Top-level `dl-feature` and `ul-feature` nodes are canonical global catalogs for
compiler source. A band+CA-bandwidth-class entry (an `nr`/`lte` node) can carry more than
one component carrier (CC) — the count is fixed by its bandwidth class, e.g. `n78` class C
= 2 CCs — and each CC has its own feature reference, so a sub-block's `d=`/`u=` value carries a
comma-separated list, one 1-based catalog position per CC in CC order (a single-CC sub-block
carries exactly one, e.g. `d=A3`). Decompose
prunes unreferenced wire records, then sorts and deduplicates the retained records by their
complete raw values. Provision filters those catalogs into a compact, independently numbered
subset for each generated carrier/SKU file, so a file never contains a record used only by
another target. A source catalog may exceed 255 records; each generated file may use at
most 255 DL records and, independently, at most 255 UL records.

The raw `dl-cc-id`/`ul-cc-id` selector fallback for unresolved bytes was removed: a component
with no resolved feature set surfaces no per-CC property at all (the all-zero placeholder is
re-derived from `bw-class`/`cc_count` on read). Old inline compiler
`dl-max-*`/`ul-max-*` properties are rejected in `nr.kdl`; regenerate canonical
source with `decompose` instead of hand-migrating feature indexes.

Compiler `nr.kdl` stores no feature index for NR components at all: that value is derived from
the component's per-CC feature set on provision (DL from the subcarrier-spacing band FR1/FR2, UL from
MIMO presence). The old `dl-feature-index`/`ul-feature-index` override was removed — a decoded NR
index that contradicts the derivation is now a hard decode error rather than a carried override
(the proto field is still materialized on provision/decompose). LTE components keep the value explicit
but spell it `dl-feature`/`ul-feature` (dropping the `-index` suffix; the LTE MIMO × CC-count
encoding, which is not derivable); `ul-feature` is omitted when it is `0` — the common "no UL"
default — and re-defaults to `0` on read.

A combo header's `bcs-intra-endc=0` is likewise omitted from `nr.kdl` when it is derivable: an
absent value re-derives to `0` when the same combo's `intra-band-en-dc-support=1`, so a surviving
explicit `bcs-intra-endc=0` marks one of the exceptional combos where `intra-band-en-dc-support`
is not `1`. Every nonzero `bcs-intra-endc` stays explicit.

The matching `lte.kdl` stores the exact LTE file whitelist and byte-preserving
payloads:

```kdl
version 2

f "400907661" fp=862505271 bm=1645725906

c b=0 u1=0 u2=0 {
    s {
        m G2YBB
    }
    B1 dm=A2 um=off
}
```

An `f` (file) node's quoted key argument is the modem firmware's exact `lte_file_id`
value (quoted because it's numeric-leading), not a hash. File-level `fingerprint`
and `bitmask` remain stored because the compiler has no independent derivation for
them. For optional protobuf fields, omission means absent and an explicit `0` means
present-zero. LTE component order is significant.

In either document, a combo's `selection` is zero or more child `selection { … }`
nodes, each with an optional `carriers` child list-node and an optional `skus` child
list-node. Each `selection` node must constrain at least one nonempty axis; LTE
selections may use only `skus`. Omitting one axis means unrestricted on that axis,
the nodes are unioned as a set of eligible carrier/SKU pairs, and omitting
`selection` entirely means the payload applies everywhere. Decompose canonicalizes that
relation, so its output does not depend on rectangle order, overlap, duplicates, or
input file order. `legacy`, `prime:<anchor>`, and `lte:<id>` may appear as internal
applicability tokens, but none is accepted as a `provision` model argument.

`provision` requires a registered Google five-character hardware model code (lookup is
case-insensitive after trimming). The in-code registry is authoritative for both
validation and layout choice:

- Registered cellular Tensor SKUs through the Pixel 8 series, the original Pixel
  Fold, and Pixel 9a use the legacy bitmask layout. The module contains every
  unnumbered carrier in `bitmask-carriers`, no PLMN legend or LTE file, and writes
  bitmask `65535` on every NR combo.
- Registered Exynos 5400 SKUs use their exact `(NR anchor, LTE file ID)` pair. The
  module contains one numbered carrier file for every carrier that has that anchor,
  the complete legend selected by `plmns` key presence, exactly one LTE fallback,
  and an explicit NR combo bitmask `0`.

The registry currently contains every legacy code in the pinned band table and the
18 evidence-backed profiled mappings used by `provision`. Some Pixel 10, Pixel 10
Pro, and Pixel 10a codes are known to use the profiled layout but are intentionally
not provision targets until their NR anchors are verified; an unknown-model error lists
the accepted codes. Decompose still preserves profiles and LTE files without a real
target as `prime:<anchor>` and `lte:<id>` applicability tokens.

The resulting ZIP always targets
`system/vendor/firmware/uecapconfig/`, includes a zero-length `.replace`, and has no
destination override. This is a **full directory replacement**: stock files absent
from the generated set are hidden. Entry order, timestamps, permissions, Deflate
settings, and the default module name (`Pixel UE-caps: <CODE>`) are deterministic;
`--name` changes only the display name. Identical source bytes, resolved model, and
effective module name (the same default or the same `--name`) produce an identical
ZIP.

All source validation, protobuf generation, re-decoding, and ZIP assembly finish in
memory before the requested ZIP is atomically replaced. Normal decompose validation or
encoding failures leave existing `nr.kdl`/`lte.kdl` files unchanged. Fidelity is
format-specific: NR preserves canonical modeled values rather than original bytes
(with the deliberate `65535` legacy / explicit-zero modern normalization), while an
unedited LTE file and PLMN legend rebuild bit-for-bit, including optional-zero
presence, ordering, and duplicate PLMNs.

### See what a carrier profile supports

```console
$ pixel-uecaps-toolbox inspect VZW_193698151252893.binarypb
Carrier UE-capability profile

Carrier      : VZW
  PLMNs (12) : 310-004, 310-005, 310-006, 310-012, 310-590, 310-890, 311-480, 311-270, 312-770, 311-489, ...
  countries  : USA

SKU profile  : 167  [family A, main tier] — Pixel 10 Pro Fold
  in-file fp : 874888686  [OK]

Band combinations (1235)
  g1     n2A
  g2     n5A
  g3     n66A
  g4     n77A
  g5     n2A↓ + n2A
  …       (1235 combos total — trimmed)
```

**Read:** the carrier and the networks it serves, which SKU profile this file is
for, that the in-file fingerprint matches the profile, and the supported band
combinations (`↓` marks a downlink-only component).

> When a file's SKU profile maps to a known Pixel model, `inspect` appends it inline — e.g.
> `SKU profile  : 3616442437  [family A, main tier] — Pixel 10 Pro XL`.

### The full picture — SKU math + per-band 5G capabilities

```console
$ pixel-uecaps-toolbox inspect --full VZW_193698151252893.binarypb
Carrier UE-capability profile

Carrier      : VZW
  mapping idx: 1
  PLMNs (12) : 310-004, 310-005, 310-006, 310-012, 310-590, 310-890, 311-480, 311-270, 312-770, 311-489, ...
  countries  : USA

Trailing number
  value      : 193698151252893
  factored   : 3^5 · 7^2 · 17 · 67 · 167 · 85523
  meaning    : carrier-identity  x  SKU-profile tag

Carrier signature (common factor of all of this carrier's files)
  value      : 85523   = 85523
  derived from: 16 sibling file(s) in this directory
  SKU portion : 193698151252893 / 85523 = 2264866191

SKU profile  : 167  [family A, main tier] — Pixel 10 Pro Fold
  anchor prime: 167  (193698151252893 mod 167 == 0  OK)
  full tag   : 67 · 167
  in-file fp : 874888686  [OK]

Selection rule
  A Pixel whose SKU maps to profile 167 loads THIS file, because it is
  the unique VZW file whose number is divisible by 167.

Band combinations (1235)
  g1     n2A
       n2    DL 40MHz 4x4 QAM256 SCS 15kHz · UL 40MHz cb:No nonCb:1 QAM256 SCS 15kHz
  …
  g4     n77A
       n77   DL 100MHz 4x4 QAM256 SCS 30kHz +90MHz · UL 100MHz cb:Yes nonCb:2 QAM256 SCS 30kHz +90MHz
  …       (per-component detail for all 1235 combos — trimmed)
```

**Read:** `--full` adds the SKU-selection math — *why* your Pixel loads this exact
file — and expands every combo into per-component 5G capabilities. The math is
explained in prose under [How the file naming works](#how-the-file-naming-works).

### Inspect an LTE-only fallback file

```console
$ pixel-uecaps-toolbox inspect lte_844857560.binarypb
LTE-only fallback config

in-file fp : 874888686  [family A, main tier]
LTE config : sta5_na
             modem-selected by hardware category 0x812 (Shannon g5400), not SIM/MCC

LTE band combinations (1053)
  g1     B1A↓ + B1A
  g2     B1A↓ + B5A
  g3     B1A↓ + B8A
  …
```

**Read:** `lte_*.binarypb` files carry LTE-only carrier aggregation combinations (no NR). Each
line is one combination — band + CA bandwidth class, `↓` marks a downlink-only component (UL
disabled). `--full` adds per-CC DL class·MIMO / UL class and the `bcs`. These files sit
outside the 16/14 SKU-profile scheme (no anchor prime divides their
number). The `LTE config` line names the modem's selection-table family (and the Pixel model where
confirmed); the modem picks the file by hardware/SKU category — burned into the Shannon firmware —
not by SIM or MCC.

### Compare two carriers or profiles

```console
$ pixel-uecaps-toolbox compare VZW_193698151252893.binarypb ATT_100936302644210.binarypb
A: VZW_193698151252893.binarypb   167   fp 874888686 (main/A)
B: ATT_100936302644210.binarypb   154921957   fp 862505271 (main/B)
  279 common (8 caps-changed) · 956 only in A · 1194 only in B

only in A (956):
  - B13A + B2A↓ + B2A↓ + B66A↓ + B66A↓ + n77A
  …       (trimmed)
```

Identical files report and exit cleanly, so it scripts like `diff`:

```console
$ pixel-uecaps-toolbox compare A.binarypb B.binarypb && echo same
  1235 common · no differences
same
```

**Read:** one header line per file (carrier · profile · fingerprint · tier/family),
a summary, then the set difference. Add `--full` for per-component diffs of the
common combos. Exit codes: `0` identical, `1` differ, `2` error.

Add `--common` to also list the combos both files share (`=` identical caps,
`~` caps differ) — `compare` stays a one-line summary without it.

### Audit a whole folder

```console
$ pixel-uecaps-toolbox check uecaps/
=== folder check: uecaps/ ===
files: 1398  |  carriers: 89  |  legend entries: 80

## genuine anomalies (do not fit the 16/14-profile, 4-fingerprint model)
   none

## reference stubs (profile + fingerprint, but NO capability payload)
   224 files
   carriers: AIRTEL(14), DT_NL(14), …

## alt-tier carriers (14 profiles, fingerprints 707802847/627223094)
   AIRTEL, DT_NL, EU_COMMON1, …

## carriers with files but ABSENT from the legend
   DT_NL
   …

## incomplete profile sets (fewer files than the tier expects)
   GOOGLE_COMCAST_  15/16 profiles (main tier)

## non-capability files
   ap_plmn_mapping.binarypb : 1 (the legend)
   lte_*.binarypb           : 8 (LTE-only fallback)
   unparseable names        : none
```

**Read:** `check` exits non-zero **only** on a genuine anomaly (unknown fingerprint,
wrong anchor count, or a family/fingerprint contradiction). Reference stubs,
alt-tier carriers, legend gaps, and incomplete sets are informational. For
data-independent sanity checks, `pixel-uecaps-toolbox self-test` runs the built-in
suite and prints `ALL TESTS PASSED`.

### Export the carrier × profile matrix as CSV

```console
$ pixel-uecaps-toolbox matrix <uecapconfig folder> > matrix.csv
$ pixel-uecaps-toolbox matrix <uecapconfig folder> -o matrix.csv   # or write straight to a file
```

One row per carrier, one column per SKU capability profile; each cell is the
`<NUMBER>` of that carrier's file for that profile (empty when the carrier ships no
file for it — e.g. alt-tier carriers leave the last two profiles blank). Columns are
headed by the profile's **known Pixel model**, or its **anchor prime** when the model
isn't known, and are sorted by that header:

```console
$ pixel-uecaps-toolbox matrix <uecapconfig folder> | head -1
carrier,1002739,196911437,2912407,3347,3539,688679,8969,Pixel 10 Pro Fold,Pixel 10 Pro XL,Pixel 9 (5G Sub-6 GHz),Pixel 9 (5G mmWave + Sub 6 GHz),Pixel 9 Pro (5G Sub-6 GHz),Pixel 9 Pro (5G mmWave + Sub 6 GHz),Pixel 9 Pro Fold,Pixel 9 Pro XL (5G Sub 6 GHz),Pixel 9 Pro XL (5G mmWave + Sub 6 GHz)
```

**Read:** a spreadsheet-friendly overview of the whole dump — at a glance, which
profiles each carrier provides and the exact selector numbers. Scans the same files
as `check`; non-carrier files (the legend, `lte_*`) are ignored.

## Reading the source format

`decompose` writes a compact vocabulary. Keys are abbreviated because the per-combo lines repeat
tens of thousands of times — the real corpus is 7.6 MB of KDL, down from 12.7 MB with the long
names. Per-carrier keys that appear a handful of times (`mcc`, `mnc`) are left spelled out.

```kdl
c bn=1 be=0 ie=1 {
    s { c VZW; m legacy GUL82 }
    n257 d=G30,30 u=A1
    B66  d=C2
}
```

A sub-block's **node name is its 3GPP band**: `n257` is NR band n257, `B66` is E-UTRA band 66.

`d` and `u` are the DL and UL directions. Each is a **CA bandwidth-class letter** followed by an
optional comma-separated list of per-CC feature references — one per component carrier, pointing
1-based into the `df`/`uf` catalogs at the top of the file.

- `d=G30,30` — class G, two CCs, both using catalog entry 30
- `d=A3` — class A, one CC, entry 3
- `d=A` — class A with no features (the common placeholder)
- no `u` at all — UL disabled

The letter is worth learning: it is the 3GPP class, so it tells you the aggregation directly.
`A` is 1 CC, `B` and `C` are 2, and `G` through `M` are the FR2 (mmWave) classes running 2 to 8
CCs. So `n257 d=G30,30` reads as "mmWave band n257, two aggregated carriers".

`lte.kdl` sub-blocks use `dm`/`um` instead, which pack the class *and* the MIMO width: `dm=A4`
is class A with 4x4 MIMO, `dm=A2` is 2x2, and `um=off` means UL disabled.

Other keys, in rough order of how often you will meet them: `c` combo (and, inside `s`,
carriers), `s` selection, `m` skus, `cr` carrier, `pf` profile, `bc` bitmask-carriers,
`bf` bitmask-fingerprint, `p`/`ps` plmn/plmns, `f` file, `df`/`uf` the feature catalogs.
`version` is never abbreviated, so a file from an older build fails with a message telling you
to re-run `decompose`.

## A note on what the tools will refuse

The reader is deliberately fail-closed, and the report commands use the same one the compiler
does. A capability file is rejected — with the reason named — if it carries a field the schema
does not model, a wrong wire type, a packed PLMN list, a value too wide for its declared type,
a repeated singular field, fields out of tag order, or a non-minimally encoded integer. Each of
those decodes "fine" with an ordinary protobuf library and yields either different values or
different bytes, which is exactly what a tool that regenerates flashable modem configuration
must not do quietly. None of it rejects any real Pixel file: the whole 1487-file reference
corpus is clean on every one of those properties.

Two smaller consequences you may notice. A PLMN value that does not fit the 24 bits a PLMN has
renders as `<invalid PLMN N>` rather than being masked into a different, plausible-looking
carrier. And `provision` refuses to build a module from an empty file set, because the
`.replace` marker would tell Magisk to wipe the device's `uecapconfig` directory and put
nothing back.

## Command reference

| Command | What it does |
| --- | --- |
| `decompose --bitmask DIR --profiled DIR -o SOURCE` | Decompose both complete folder layouts into canonical `SOURCE/nr.kdl` and `SOURCE/lte.kdl`. Both directories and `-o` are required; unsupported `.binarypb` files or any lossy/failed self-check exit `2`. |
| `provision <CODE> <SOURCE> -o ZIP [--name N]` | Strictly load both compiler documents and build a complete, deterministic `.replace` Magisk ZIP for a registered real model code. The destination is fixed at `/vendor/firmware/uecapconfig`; there is no `--dest`. Exit `0`/`2`. |
| `inspect <FILE> [--full]` | Inspect one file. Adapts to the file type: a carrier file, the PLMN legend, or an `lte_*` fallback (whose LTE CA combinations it decodes). `--full` reveals the SKU-selection math and per-component capabilities. Exit `2` on an unrecognised filename. A file that fails strict wire validation prints the reason instead of its combinations, rather than reporting "not readable" or silently showing normalized data. |
| `compare <A> <B> [--full] [--common]` | Diff two files' band combinations (set diff by default; `--full` adds per-component diffs; `--common` also lists the combos common to both — `=` identical, `~` caps differ). Exit `0` identical, `1` differ, `2` error. |
| `check [DIR]` | Scan a folder (default `.`) and report everything that doesn't fit the scheme. Exit `1` on a genuine anomaly. Every file is decoded with the same fail-closed reader the compiler uses, so a file with an unknown field, a wrong wire type or a packed PLMN list is reported rather than accepted; the scan continues past it. Legend anomalies now include duplicate carrier indices, which `provision`/`decompose` reject. |
| `matrix [DIR] [-o FILE]` | Scan a folder (default `.`) and emit a carrier × profile matrix as CSV to `-o` or stdout. Columns are headed by Pixel model (or the profile's anchor prime when unknown), sorted by header. |
| `self-test` | Run built-in, data-independent sanity checks. |

**Migration note:** this branch renamed one command and removed the following six, with no
aliases:

```
build MODEL SOURCE -o ZIP [--name N]                      →  provision MODEL SOURCE -o ZIP [--name N]  (renamed)
decode FILE [--kind KIND]                                 →  removed
patch {create, apply, show, filter {include, exclude}}    →  removed
provision MODEL [DIR] --carrier/--lte-patch/--nr-patch/…  →  removed (name reused — see note below)
magisk FILES... [--dest PATH] [-o ZIP] [--name N]         →  removed
mapping encode                                            →  removed
mapping inject-plmn CARRIER PLMNS...                      →  removed
```

Each is a hard rename or removal, not a deprecation — there is no back-compat reader or CLI
shim for any of them. Two things to know if you had scripted one of the removed spellings:

- **`provision` changed meaning, not just name.** The pre-branch `provision` patched a single
  carrier/LTE file in place (`--carrier`, `--lte-patch`, `--nr-patch`, `--add-plmn`, `--dest`,
  `--strict`) and is gone with no replacement. The current `provision MODEL SOURCE -o ZIP` is
  the renamed former `build` — a full-folder compile — and accepts none of those flags, so an
  old `provision` invocation now fails to parse instead of silently doing the old thing.
- **The other five removed commands each edited or packaged a single file, and none has a
  direct substitute.** Editing goes through the folder compiler and nothing else: there is no
  single-file edit, patch, or repackage command to move `patch`, the old `provision`, `magisk`,
  `mapping encode`, or `mapping inject-plmn` onto — the replacement workflow for all of them is
  `decompose` → hand-edit `nr.kdl`/`lte.kdl` → `provision`.

An older, unrelated rename (already in place before this branch) is still worth knowing if
you're carrying very old scripts or library calls:

```
decode --bitmask … -o SRC   →  decompose --bitmask … -o SRC
compiler::decode (library)  →  compiler::decompose
```

## How the file naming works

For the profiled Exynos 5400 layout, the trailing `NUMBER` in
`<CARRIER>_<NUMBER>.binarypb` is **not** a hash or version — it is a selector key:

```
NUMBER  =  carrier-identity  ×  SKU-profile tag
```

- Every carrier ships one file per **Pixel-SKU capability profile**.
- Each profile is identified by a unique **anchor prime** that divides `NUMBER`.
- A Pixel loads the file whose `NUMBER` is divisible by its own SKU's profile tag —
  so *which* numbered file gets picked depends on the exact Pixel SKU.
- All of a carrier's files share a common factor: the **carrier signature**
  (`NUMBER ÷ carrier-signature` is the SKU portion).

### Two capability tiers

There are 16 profiles, in two tiers distinguished by the in-file fingerprint
(protobuf field 1):

| Tier | Fingerprints (family A / B) | Profiles | Carriers |
|------|-----------------------------|----------|----------|
| main | `874888686` / `862505271`   | 16       | US / EU / APAC majors |
| alt  | `707802847` / `627223094`   | 14 (no 2912407/3539) | India + emerging markets |

Alt-tier *operators* ship tiny **reference stubs** (fingerprint + a `field 9`
reference, no capability payload); the real alt-tier data lives in `EU_COMMON1`.

### Files that don't follow the scheme

- `<CARRIER>.binarypb` — older Tensor bitmask-layout carrier configs (no profile
  suffix).
- `lte_*.binarypb` — LTE-only fallback configs (no profile).
- `ap_plmn_mapping.binarypb` — the PLMN→carrier legend.

## Contributing

Contributions welcome. The build/test workflow, conventions, and gotchas live in
[CONTRIBUTING.md](CONTRIBUTING.md); the architecture, reverse-engineered `.binarypb`
formats, and implementation invariants live in [DESIGN.md](DESIGN.md).

## License

Licensed under the [Apache License, Version 2.0](LICENSE); see [`NOTICE`](NOTICE).

Not affiliated with or endorsed by Google; the file format is observed, not
documented. For research and personal use — editing device configs is at your own
risk.
