# pixel-uecaps-toolbox

Decode, inspect, and edit the Google Pixel **UE-capabilities** protobufs that ship
in Pixel carrier-config packages — see exactly which LTE/5G bands a carrier
profile unlocks, diff two carriers, edit the PLMN→carrier legend, or rebuild a
complete model-specific `uecapconfig` folder.

> Not affiliated with or endorsed by Google. The file format is observed, not
> documented; this tool is for research and personal use.

## What you can do with it

`pixel-uecaps-toolbox` reads the per-carrier capability files a Pixel uses to tell
the network what it supports. With it you can:

- **See what a carrier profile unlocks** — every LTE/5G band combination, and per
  band: bandwidth, MIMO, modulation, SCS, and 90 MHz support.
- **Diff two files** — which band combinations (and per-component capabilities)
  differ between two carriers or two SKU profiles.
- **Edit the PLMN→carrier legend** — decode it to KDL, edit, and re-encode
  bit-for-bit; or append a network to a carrier in one command.
- **Edit a complete offline folder** — normalize the legacy and Exynos 5400
  layouts into `nr.kdl` + `lte.kdl`, then build a deterministic full-replacement
  Magisk module for a real Pixel model code.
- **Audit a whole dump** — scan a folder of capability files and flag anything that
  doesn't fit the expected scheme.
- **Get a one-file KDL slice** — `--kdl` emits the same combo/sub-block spelling `decode` produces, for one file.

## Install

Build from source with a stable Rust toolchain (edition 2024):

```sh
cargo build --release
# binary at target/release/pixel-uecaps-toolbox
```

No system `protoc` is needed — protobuf codegen is pure Rust: `build.rs` compiles
`proto/ue_caps.proto` via protox at build time.

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

> **Getting edited files back onto a device is your responsibility.** The `magisk`
> command packages individual edited files, while folder-compiler `build` packages a
> complete replacement. Installing either module still needs root, varies by build, and
> editing carrier configs can break service. Proceed at your own risk.

## Recipes

Commands below are shown with the bare name `pixel-uecaps-toolbox`; if you haven't
installed it on your `PATH`, use `./target/release/pixel-uecaps-toolbox` instead.

### Edit and rebuild a complete offline `uecapconfig` folder

The folder compiler consumes **both** generations together, writes exactly two
canonical source files, and builds one complete replacement module for a real phone:

```console
$ pixel-uecaps-toolbox decode \
    --bitmask bitmask-uecapconfig/ \
    --profiled profiled-uecapconfig/ \
    -o source/
$ ls source/
lte.kdl  nr.kdl

# Edit source/nr.kdl and source/lte.kdl, then choose a registered phone model.
$ pixel-uecaps-toolbox build G2YBB source/ -o pixel-uecaps-G2YBB.zip
```

`decode` requires both directories. The bitmask input may contain only unnumbered
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
version 1
bitmask-carriers VZW

bitmask-fingerprint 715188856 {
    carriers VZW
}

carrier VZW bitmask-id=1 profiled-id=0 mapping-id=1 signature=1 tier=main {
    plmn mcc=311 mnc=480
    profile "66813533" multiplier=66813533 unknown=0
}

dl-feature max-scs=3 max-mimo=2 max-bw=100
```

`bitmask-carriers` is the exact legacy output whitelist, and the fingerprint
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
full-width `u64` legend index, stored as a native KDL integer (KDL integers are
i128-backed, so the full `u64` range fits without string-encoding). `mapping-id` and
the carrier's PLMNs must either both be present or both be absent. Omitting them
excludes that carrier from the rebuilt profiled legend, while a bare, childless
`plmns` marker node (distinct from the per-entry `plmn mcc=… mnc=…` nodes shown
above) deliberately emits an entry with no PLMNs. PLMN order and duplicates are
significant and preserved.

Top-level `dl-feature` and `ul-feature` nodes are canonical global catalogs for
compiler source. A band+CA-bandwidth-class entry (an `nr`/`lte` node) can carry more than
one component carrier (CC) — the count is fixed by its bandwidth class, e.g. `n78` class C
= 2 CCs — and each CC has its own feature reference, so a sub-block's `dl-feature` and
`ul-feature` properties are **repeated**, one 1-based catalog position per CC in CC order
(a single-CC sub-block still emits exactly one `dl-feature=N`, identical to before). Decode
prunes unreferenced wire records, then sorts and deduplicates the retained records by their
complete raw values. Build filters those catalogs into a compact, independently numbered
subset for each generated carrier/SKU file, so a file never contains a record used only by
another target. A source catalog may exceed 255 records; each generated file may use at
most 255 DL records and, independently, at most 255 UL records.

The raw `dl-cc-id`/`ul-cc-id` selector fallback for unresolved bytes was removed: a component
with no resolved feature set surfaces no per-CC property at all (the all-zero placeholder is
re-derived from `bw-class`/`cc_count` on read). Old inline compiler
`dl-max-*`/`ul-max-*` properties are rejected in `nr.kdl`; regenerate canonical
source with `decode` instead of hand-migrating feature indexes. Patch KDL remains a
separate format with its own raw-value fields (per-CC feature values there are child
nodes, not properties — see "Patch KDL reference" below).

Compiler `nr.kdl` stores no feature index for NR components at all: that value is derived from
the component's per-CC feature set on build (DL from the subcarrier-spacing band FR1/FR2, UL from
MIMO presence). The old `dl-feature-index`/`ul-feature-index` override was removed — a decoded NR
index that contradicts the derivation is now a hard decode error rather than a carried override
(the proto field is still materialized on build/decode). LTE components keep the value explicit
but spell it `dl-feature`/`ul-feature` (dropping the `-index` suffix; the LTE MIMO × CC-count
encoding, which is not derivable); `ul-feature` is omitted when it is `0` — the common "no UL"
default — and re-defaults to `0` on read.

A combo header's `bcs-intra-endc=0` is likewise omitted from `nr.kdl` when it is derivable: an
absent value re-derives to `0` when the same combo's `intra-band-en-dc-support=1`, so a surviving
explicit `bcs-intra-endc=0` marks one of the exceptional combos where `intra-band-en-dc-support`
is not `1`. Every nonzero `bcs-intra-endc` stays explicit. The patch reference's explicit
`bcs-intra-endc=` (below) is unaffected — that format always keeps it stored.

The matching `lte.kdl` stores the exact LTE file whitelist and byte-preserving
payloads:

```kdl
version 1

file "400907661" fingerprint=862505271 bitmask=1645725906

combo bcs=0 unknown1=0 unknown2=0 {
    selection {
        skus G2YBB
    }
    subblock 1 dl-bw-class-mimo=32768 ul-bw-class-mimo=0
}
```

A `file` node's quoted key argument is the modem firmware's exact `lte_file_id`
value (quoted because it's numeric-leading), not a hash. File-level `fingerprint`
and `bitmask` remain stored because the compiler has no independent derivation for
them. For optional protobuf fields, omission means absent and an explicit `0` means
present-zero. LTE component order is significant.

In either document, a combo's `selection` is zero or more child `selection { … }`
nodes, each with an optional `carriers` child list-node and an optional `skus` child
list-node. Each `selection` node must constrain at least one nonempty axis; LTE
selections may use only `skus`. Omitting one axis means unrestricted on that axis,
the nodes are unioned as a set of eligible carrier/SKU pairs, and omitting
`selection` entirely means the payload applies everywhere. Decode canonicalizes that
relation, so its output does not depend on rectangle order, overlap, duplicates, or
input file order. `legacy`, `prime:<anchor>`, and `lte:<id>` may appear as internal
applicability tokens, but none is accepted as a `build` model argument.

`build` requires a registered Google five-character hardware model code (lookup is
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
not build targets until their NR anchors are verified; an unknown-model error lists
the accepted codes. Decode still preserves profiles and LTE files without a real
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
memory before the requested ZIP is atomically replaced. Normal decode validation or
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
disabled). `--full` adds per-CC DL class·MIMO / UL class and the `bcs`; `--kdl` emits a one-file
slice of `decode`'s `nr.kdl`/`lte.kdl` format (combo/sub-block spelling identical; no diagnostic envelope
— use the text report for that). These files sit outside the 16/14 SKU-profile scheme (no anchor prime divides their
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

### Add a network to a carrier (and edit the legend)

```console
# Append a PLMN (MCC-MNC) to a carrier; legend in → new legend out
$ pixel-uecaps-toolbox mapping inject-plmn VZW 250-99 \
    < ap_plmn_mapping.binarypb > new_mapping.binarypb

# Or edit freely: decode → edit the KDL → re-encode (bit-for-bit when unedited)
$ pixel-uecaps-toolbox mapping decode < ap_plmn_mapping.binarypb > mapping.kdl
#   …edit mapping.kdl…
$ pixel-uecaps-toolbox mapping encode < mapping.kdl > new_mapping.binarypb
```

`mapping.kdl` opens with a `version 1` header, then one `mapping id=… name=…{ … }`
node per carrier, each holding one `plmn mcc=… mnc=…` child node per network (a real
decoded legend starts like this):

```kdl
version 1

mapping id=1 name=VZW {
    plmn mcc=310 mnc=4 mnc-digits=3
    plmn mcc=310 mnc=5 mnc-digits=3
    plmn mcc=311 mnc=480
    …
}
mapping id=2 name=TMO {
    plmn mcc=310 mnc=160
    …
}
```

`mcc`/`mnc` are plain decimal integers; `mnc-digits=3` marks a 3-digit MNC that would
otherwise look 2-digit due to a leading zero (`310-004` needs it, `311-480` doesn't).
An MNC-wildcard entry (any network for that MCC) omits `mnc=` entirely.

> **Note:** `… < f.binarypb > f.binarypb` truncates `f` before it is read. Write to
> a different file (or a temp file) when editing in place.

**Read:** `decode`/`encode` are a faithful round-trip; `inject-plmn` is the one-shot
"add network X to carrier Y". The `mapping` subcommands read stdin and write stdout.

### Transplant one carrier's band combos onto another

```console
# Build an A→B patch: applying it to A reproduces B's band combinations
$ pixel-uecaps-toolbox patch create ATT_100936302644210.binarypb VZW_193698151252893.binarypb \
    -o combos.patch.kdl

# Apply it to A (keeps A's fingerprint/profile; only the combo set changes)
$ pixel-uecaps-toolbox patch apply ATT_100936302644210.binarypb \
    --in combos.patch.kdl -o ATT_with_VZW_combos.binarypb

# Preview a patch before applying it (file or stdin; --full shows per-component caps)
$ pixel-uecaps-toolbox patch show combos.patch.kdl --full

# Filter a patch to only certain bands (or exclude bands), then apply/show the result
$ pixel-uecaps-toolbox patch filter include n77 --in combos.patch.kdl -o n77.patch.kdl
```

**Read:** `patch create` writes a strict, versioned KDL combo patch to `-o` (or
stdout); `patch apply` reconstructs a `.binarypb` whose combos match the
patch's target, keeping the base's identity fields. Apply is best-effort — entries that
don't fit the base are warned and skipped (use `--strict` to fail instead). You can then
`magisk` the patched file onto a device (next recipe). Exit codes: create `0`/`2`; apply
`0` clean, `1` with skipped entries, `2` on error. `patch show [FILE]` (file or stdin) renders a patch's
`delete`/`add`/`change` entries — add `--full` for per-component capabilities, like `inspect --full`.
Patch entry keys are derived from their combo payload, so `add`/`change` nodes do not carry a
duplicate stored key or band string. In carrier/NR patches each combo's components are child nodes
literally named `nr` or `lte` — the node name **is** the radio kind — with a plain band number as
the leading positional argument (`66`, `78`); labels such as `B66` and `n78` are derived, and NR-only
capability properties are valid only on `nr` components. `delete` entries keep their derived key
as a bare argument (e.g. `delete n41A`) because they have no combo payload.
`patch filter include`/`exclude <BANDS>…` (file or stdin) keeps or drops the patch's combos by band — labels like `n77`/`B66`, any-match (or `include --only` for combos whose *every* band is listed) — writing a filtered patch. `patch` also works on `lte_*.binarypb` fallback files — `patch create lteA lteB` writes an
`lte`-kind patch (it opens with `kind lte`) and `patch apply lteBASE` transplants the LTE
combos and re-encodes a new `lte_*.binarypb`. Both files of a `create`, and the base of an `apply`,
must be the same kind (you can't mix carrier and LTE).

#### Patch KDL reference

Every patch opens with `kind nr` or `kind lte`, then `version 1`. Unknown nodes or
properties, unsupported versions, empty `add`/`change` entries, and mixed derived keys in
one entry are rejected.

An NR/carrier patch uses protobuf-shaped numeric values. Each component carrier is a child
node literally named `nr` or `lte` — the node name **is** the radio kind (the one place a
single combo can mix both, for an EN-DC combo). A sub-block's resolved feature sets are
**per-CC child nodes** — one `dl-cc`/`ul-cc` per component carrier, in CC order — since a
band+bandwidth-class entry can carry more than one CC (e.g. `n48` class B = 2 CCs) and those
CCs can reference *different* feature records:

```kdl
kind nr
version 1
delete n41A

add {
    combo bit-mask=0 {
        nr 78 dl-bw-class=1 ul-bw-class=1 {
            dl-cc max-bw=40 max-mimo=2
        }
    }
}
change {
    combo bit-mask=0 {
        lte 66 dl-bw-class=1 ul-bw-class=1
        nr 48 dl-bw-class=2 {
            dl-cc max-bw=40 max-scs=1
            dl-cc max-bw=100 max-scs=2
        }
    }
}
```

`combo` also accepts optional `group=`, `index=`, `power-class=`, `bcs-nr=`,
`bcs-intra-endc=`, `bcs-eutra=`, and `intra-band-en-dc-support=` properties (omitted when
absent, like every optional property below). Each component also accepts `srs-tx-switch=`. Only
LTE (`lte`) components carry a feature index in source, spelled `dl-feature=`/`ul-feature=` and
kept explicit — except `ul-feature=0`, which is omitted and re-defaults to `0` on read. NR (`nr`)
components carry no feature index in source: it is derived from the component's feature set on
build (the old `dl-feature-index=`/`ul-feature-index=` override was removed — the proto field is
still materialized on build/decode, just not surfaced).

Each `dl-cc` child accepts `max-scs=`, `max-mimo=`, `max-bw=`, `max-mod-order=`, and
`bw-90mhz-supported=` (`#true`/`#false`); each `ul-cc` child accepts the same plus
`max-mimo-cb=` (instead of `max-mimo=`) and `max-mimo-non-cb=`. The feature values use the
raw modem vocabulary: `max-scs` codes `1`–`5` mean 15/30/60/120/240 kHz; DL `max-mimo` codes
`1`–`3` mean 2×2/4×4/8×8; UL `max-mimo-cb` uses `1` for No and `2` for Yes; `max-mod-order`
uses `1` for QAM64 and `2` for QAM256; `max-bw` is MHz. `srs-tx-switch` and the `dl-cc`/`ul-cc`
children are valid only on `nr` components; the `dl-feature=`/`ul-feature=` feature index is
valid only on `lte` components.

A patch represents each per-CC direction only in resolved `dl-cc`/`ul-cc` child form; the raw
`dl-cc-id=`/`ul-cc-id=` selector fallback for unresolved directions was removed (real combos
always resolve their selectors into feature records). A component whose selector doesn't
resolve — a non-placeholder selector that points at no feature set — is rejected rather than
silently dropped: both the proto-decode boundary and `patch create` fail closed on it (symmetric;
corpus-verified impossible on real files). A resolved direction's selectors are reassigned on
apply, so they are excluded from the `create` diff.

An LTE-fallback patch uses the raw Shannon class/MIMO values. The file is already all-LTE,
so each component is just a `subblock` child node (no per-component kind needed):

```kdl
kind lte
version 1

change {
    combo bcs=3221225472 unknown1=0 unknown2=0 {
        subblock 1 dl-bw-class-mimo=32768 ul-bw-class-mimo=0
    }
}
```

Unlike the NR side, `bcs`/`unknown1`/`unknown2` and each `subblock`'s `band`/`dl-bw-class-mimo`/
`ul-bw-class-mimo` are always present (never omitted). For LTE class/MIMO values, `0`
disables that direction; the high bits map `32768/16384/8192/4096/2048/1024` to classes
A–F, and the low bit selects 4×4 when set or 2×2 when clear.

The key for an `add`/`change` entry is the sorted component labels joined with `" + "`, such as
`B66A + n77A`; each label includes its band and CA-class letter, with `↓` for DL-only,
`↑` for UL-only, and `A/B` for asymmetric classes. The key is never stored as a separate
property — it's derived from the combo payload every time. MIMO, SCS, modulation, feature
bandwidth, BCS values, bitmask, selectors, and group/index provenance do not change the key. Of
those, differences in MIMO, SCS, modulation, feature bandwidth (on ANY CC, not just the
first), BCS values, and bitmask **do** produce a `change`; group/index provenance is
discarded entirely, so a pure regrouping produces no diff; and a selector-byte difference
produces a `change` only for a **selector-only** component (for a feature-resolved component
the selectors are reassigned on apply and are excluded from the comparison). `delete` keeps
its derived key as a bare argument (e.g. `delete n41A`) because it has no combo payload.

`inspect --kdl` shares field names and value encodings with `decode`'s `nr.kdl`/`lte.kdl`
(it's a one-file slice of the same source shape); it's not accepted as a patch. Old patch
spellings such as `kind 5g`, decoded-string capability fields, hex-string selector IDs, and
stored derived keys are deliberately rejected.

**Migration note:** the per-CC grammar above (repeated `dl-feature=`/`ul-feature=` in
`nr.kdl`/`lte.kdl`; per-CC `dl-cc`/`ul-cc` patch child nodes) is a hard cutover with no
back-compat reader for the old single-value/flat-scalar shape. `nr.kdl`/`lte.kdl` are always
regenerated by `decode`, never hand-migrated, so just re-run `decode` over your source
`.binarypb` files. Any patch `.kdl` saved before this change must be re-created with
`patch create` — an old flat-scalar `nr`/`lte` component (feature values such as
`dl-max-scs=` as properties directly on the component node, instead of `dl-cc`/`ul-cc`
children) is rejected, not auto-upgraded.

### Package an edited file into a flashable Magisk module

```console
# Bundle one or more edited files into a flashable module
$ pixel-uecaps-toolbox magisk VZW_193698151252893.binarypb -o uecaps-override.zip

# Several at once (e.g. an edited carrier file + the edited legend) → one module
$ pixel-uecaps-toolbox magisk VZW_193698151252893.binarypb ap_plmn_mapping.binarypb \
    -o uecaps-override.zip
```

Flash `uecaps-override.zip` in the Magisk app (Modules → Install from storage) and
reboot. The module overlays each file onto `/vendor/firmware/uecapconfig` (the default;
override with `--dest`) using Magisk's systemless mount, so the stock partition is left
untouched. With no `-o`, the `.zip` is written to stdout (`> uecaps-override.zip`).

**Read:** `magisk` packages files as opaque bytes — it works for carrier files, the
legend, and `lte_*` fallbacks alike. Installing the module is still root-only and at
your own risk; a wrong capability set can break service.

### Build a targeted package for a profiled phone

`provision` assembles one flashable Magisk module for a registered profiled Exynos 5400
Pixel in a single command, pulling files from a folder of capability files (default `.`)
and editing them in memory. Each file is included **only** when you ask for it.

```console
# Rewrite the LTE fallback's combos for a Pixel 9 (US)
$ pixel-uecaps-toolbox provision G2YBB --lte-patch p9.lte.kdl -o p9.zip

# Target Verizon: patch its NR combos and add a network to it in the legend
$ pixel-uecaps-toolbox provision G2YBB uecaps/ --carrier VZW \
    --nr-patch vzw.nr.kdl --add-plmn 250-99 -o vzw-p9.zip
```

**Read:** `provision <CODE>` builds a module for a supported profiled SKU named by its
**Google 5-char model code** (e.g. `GUL82` = Pixel 10 Pro XL US, `G2YBB` = Pixel 9
mmWave US). Legacy bitmask-layout models are rejected here; use the complete folder
compiler's `build` command when targeting either layout. An unknown-model error lists
every accepted profiled code; the CLI still requires at least one modifier to reach
that error. The module holds the phone's **LTE fallback** (with `--lte-patch`), the
carrier's **NR file** (with `--nr-patch`), and the **PLMN legend** (with `--add-plmn`) — each present
only when its flag is given, so at least one is required. `--carrier` names the target for
`--add-plmn`/`--nr-patch` and must have files in the source folder; `--add-plmn` refuses a PLMN already
mapped to any carrier. Patch combos whose bands the model doesn't support (per the `pixel-bands`
table) are skipped with a warning. The output is the same kind of Magisk module `magisk` produces — flash it the
same way. Exit codes: `0` clean, `1` built but a patch skipped entries, `2` error.

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

## Command reference

| Command | What it does |
| --- | --- |
| `decode --bitmask DIR --profiled DIR -o SOURCE` | Decode both complete folder layouts into canonical `SOURCE/nr.kdl` and `SOURCE/lte.kdl`. Both directories and `-o` are required; unsupported `.binarypb` files or any lossy/failed self-check exit `2`. |
| `build <CODE> <SOURCE> -o ZIP [--name N]` | Strictly load both compiler documents and build a complete, deterministic `.replace` Magisk ZIP for a registered real model code. The destination is fixed at `/vendor/firmware/uecapconfig`; there is no `--dest`. Exit `0`/`2`. |
| `inspect <FILE> [--full] [--kdl]` | Inspect one file. Adapts to the file type: a carrier file, the PLMN legend, or an `lte_*` fallback (whose LTE CA combinations it decodes). `--full` reveals the SKU-selection math and per-component capabilities; `--kdl` emits a one-file slice of `decode`'s `nr.kdl`/`lte.kdl` format (combo/sub-block spelling identical; no diagnostic envelope). Exit `2` on an unrecognised filename. |
| `compare <A> <B> [--full] [--common]` | Diff two files' band combinations (set diff by default; `--full` adds per-component diffs; `--common` also lists the combos common to both — `=` identical, `~` caps differ). Exit `0` identical, `1` differ, `2` error. |
| `patch create <A> <B> [-o FILE]` | Diff two files (A→B) and emit a documented KDL combo patch to `-o` or stdout. Exit `0`/`2`. Both files must be the same kind (carrier or `lte_*`); the patch's top-level `kind` node is `nr` or `lte`. |
| `patch apply <BASE> [--in FILE] [-o OUT] [--strict]` | Apply a combo patch to `BASE` → new `.binarypb` (`--in` stdin, `-o` stdout by default). Best-effort; `--strict` fails on the first non-applying entry. Exit `0` clean / `1` skipped / `2` error. Applies an `nr`/`lte` patch to a matching base. |
| `patch show [FILE] [--full]` | Render a combo patch (KDL; `FILE` or stdin) in human-readable form — its `delete` keys and `add`/`change` entries (`+` add, `~` change). `--full` adds per-component capabilities, like `inspect --full`. |
| `patch filter include <BANDS>… [--only] [--in FILE] [-o OUT]` | Keep only the patch's combos (and `delete`s) that involve any listed band; labels like `n77`/`B66`. With `--only`, keep a combo only when *every* band it uses is listed (else the whole combo is dropped). Patch in (`--in`, default stdin) → filtered patch out (`-o`, default stdout). |
| `patch filter exclude <BANDS>… [--in FILE] [-o OUT]` | Drop the patch's combos (and `delete`s) that involve any listed band; otherwise like `patch filter include`. |
| `magisk <FILE>… [--dest DIR] [-o OUT] [--name N]` | Package file(s) into a flashable Magisk module (`.zip` → `-o` or stdout). Overlays each onto `--dest` (default `/vendor/firmware/uecapconfig`) via Magisk's systemless mount. Inputs are packaged as opaque bytes. |
| `provision <CODE> [DIR] …` | Build a partial flashable Magisk module for one registered profiled Exynos 5400 SKU. Legacy bitmask models are rejected. Includes the LTE file (`--lte-patch`), the carrier's NR file (`--nr-patch`), and/or the legend (`--add-plmn`) — each only when its flag is present; at least one required. `--carrier` targets `--add-plmn`/`--nr-patch` (and must have files in the folder). Patch combos using bands the model lacks (per `pixel-bands`) are skipped with a warning. `--dest`/`--name`/`-o`/`--strict` behave as elsewhere. Exit `0`/`1`/`2`. |
| `mapping decode` / `encode` | Decode the legend to editable KDL / re-encode KDL back to `.binarypb` (stdin → stdout). |
| `mapping inject-plmn <CARRIER> <PLMN…>` | Append one or more PLMNs (MCC-MNC) to a carrier (stdin → stdout). |
| `check [DIR]` | Scan a folder (default `.`) and report everything that doesn't fit the scheme. Exit `1` on a genuine anomaly. |
| `matrix [DIR] [-o FILE]` | Scan a folder (default `.`) and emit a carrier × profile matrix as CSV to `-o` or stdout. Columns are headed by Pixel model (or the profile's anchor prime when unknown), sorted by header. |
| `self-test` | Run built-in, data-independent sanity checks. |

Under `--kdl`, a carrier file emits a one-file slice of `nr.kdl` (`version 1`, per-file
`dl-feature`/`ul-feature` catalogs, `combo`/`nr`/`lte` nodes matching `decode`'s output exactly).
An `lte_*` file emits a one-file slice of `lte.kdl`. The mapping legend emits its own
`type=mapping` view. Diagnostic fields (file path, fingerprint status, profile model, mapping
info, etc.) live in the text report, not under `--kdl`.

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
