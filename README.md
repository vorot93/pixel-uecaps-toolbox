# pixel-uecaps-toolbox

Decode, inspect, and edit the Google Pixel **UE-capabilities** protobufs
(`<CARRIER>_<NUMBER>.binarypb`) that ship in Pixel carrier-config packages — see
exactly which LTE/5G bands a carrier profile unlocks, diff two carriers, or edit
the PLMN→carrier legend.

> Not affiliated with or endorsed by Google. The file format is observed, not
> documented; this tool is for research and personal use.

## What you can do with it

`pixel-uecaps-toolbox` reads the per-carrier capability files a Pixel uses to tell
the network what it supports. With it you can:

- **See what a carrier profile unlocks** — every LTE/5G band combination, and per
  band: bandwidth, MIMO, modulation, SCS, and 90 MHz support.
- **Diff two files** — which band combinations (and per-component capabilities)
  differ between two carriers or two SKU profiles.
- **Edit the PLMN→carrier legend** — decode it to TOML, edit, and re-encode
  bit-for-bit; or append a network to a carrier in one command.
- **Audit a whole dump** — scan a folder of capability files and flag anything that
  doesn't fit the expected scheme.
- **Get machine-readable output** — `--toml` for any analysis.

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

The files this tool reads ship inside Pixel **carrier-config packages**:

- `<CARRIER>_<NUMBER>.binarypb` — one per carrier × Pixel-SKU capability profile.
- `ap_plmn_mapping.binarypb` — the PLMN→carrier legend (which network maps to which
  `<CARRIER>` name).
- `lte_*.binarypb` — LTE-only fallback configs.

On a device they live in the carrier-config storage; pulling them off needs root
and `adb`, and the exact path varies by Android build — search for your build's
carrier-config path.

> **Getting an _edited_ file back onto a device is your responsibility.** The `magisk`
> command packages edited file(s) into a flashable Magisk module to help (see [Package
> an edited file into a flashable Magisk module](#package-an-edited-file-into-a-flashable-magisk-module)),
> but installing it still needs root, varies by build, and editing carrier configs can
> break service. Proceed at your own risk.

## Recipes

Commands below are shown with the bare name `pixel-uecaps-toolbox`; if you haven't
installed it on your `PATH`, use `./target/release/pixel-uecaps-toolbox` instead.

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
       n2    DL 40MHz 4x4 QAM256 · UL 40MHz cb:No nonCb:1 QAM256 · SCS 15kHz
  …
  g4     n77A
       n77   DL 100MHz 4x4 QAM256 · UL 100MHz cb:Yes nonCb:2 QAM256 · SCS 30kHz +90MHz
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
disabled). `--full` adds per-CC DL class·MIMO / UL class and the `bcs`; `--toml` emits every combo
structured. These files sit outside the 16/14 SKU-profile scheme (no anchor prime divides their
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

# Or edit freely: decode → edit the TOML → re-encode (bit-for-bit when unedited)
$ pixel-uecaps-toolbox mapping decode < ap_plmn_mapping.binarypb > mapping.toml
#   …edit mapping.toml…
$ pixel-uecaps-toolbox mapping encode < mapping.toml > new_mapping.binarypb
```

> **Note:** `… < f.binarypb > f.binarypb` truncates `f` before it is read. Write to
> a different file (or a temp file) when editing in place.

**Read:** `decode`/`encode` are a faithful round-trip; `inject-plmn` is the one-shot
"add network X to carrier Y". The `mapping` subcommands read stdin and write stdout.

### Transplant one carrier's band combos onto another

```console
# Build an A→B patch: applying it to A reproduces B's band combinations
$ pixel-uecaps-toolbox patch create ATT_100936302644210.binarypb VZW_193698151252893.binarypb \
    -o combos.patch.toml

# Apply it to A (keeps A's fingerprint/profile; only the combo set changes)
$ pixel-uecaps-toolbox patch apply ATT_100936302644210.binarypb \
    --in combos.patch.toml -o ATT_with_VZW_combos.binarypb

# Preview a patch before applying it (file or stdin; --full shows per-component caps)
$ pixel-uecaps-toolbox patch show combos.patch.toml --full

# Filter a patch to only certain bands (or exclude bands), then apply/show the result
$ pixel-uecaps-toolbox patch filter include n77 --in combos.patch.toml -o n77.patch.toml
```

**Read:** `patch create` writes a documented TOML [combo patch](docs/uecaps-combo-patch.md)
to `-o` (or stdout); `patch apply` reconstructs a `.binarypb` whose combos match the
patch's target, keeping the base's identity fields. Apply is best-effort — entries that
don't fit the base are warned and skipped (use `--strict` to fail instead). You can then
`magisk` the patched file onto a device (next recipe). Exit codes: create `0`/`2`; apply
`0` clean, `1` with skipped entries, `2` on error. `patch show [FILE]` (file or stdin) renders a patch's
`delete`/`set` entries — add `--full` for per-component capabilities, like `inspect --full`.
Patch `set` labels are derived from their combo payload, so set entries do not carry
duplicate `set.key` or `set.combo.bands` fields. In carrier/NR patches each flat
`[[set.combo.cc]]` has `kind = "lte"` or `kind = "nr"` plus a plain band number
(`band = 66`, `band = 78`); labels such as `B66` and `n78` are derived, and NR-only
capability fields are valid only on `kind = "nr"` components. `delete` entries keep
explicit keys because they have no combo payload.
`patch filter include`/`exclude <BANDS>…` (file or stdin) keeps or drops the patch's combos by band — labels like `n77`/`B66`, any-match (or `include --only` for combos whose *every* band is listed) — writing a filtered patch. `patch` also works on `lte_*.binarypb` fallback files — `patch create lteA lteB` writes an
`lte`-kind patch (the TOML leads with `kind = "lte"`) and `patch apply lteBASE` transplants the LTE
combos and re-encodes a new `lte_*.binarypb`. Both files of a `create`, and the base of an `apply`,
must be the same kind (you can't mix carrier and LTE).

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

### Build a complete package for your phone

`provision` assembles one flashable Magisk module for a specific Pixel in a single command,
pulling files from a folder of capability files (default `.`) and editing them in memory. Each
file is included **only** when you ask for it.

```console
# Rewrite the LTE fallback's combos for a Pixel 9 (US)
$ pixel-uecaps-toolbox provision G2YBB --lte-patch p9.lte.toml -o p9.zip

# Target Verizon: patch its NR combos and add a network to it in the legend
$ pixel-uecaps-toolbox provision G2YBB ~/uecaps --carrier VZW \
    --nr-patch vzw.nr.toml --add-plmn 250-99 -o vzw-p9.zip
```

**Read:** `provision <CODE>` builds a module for a known Pixel SKU named by its **Google 5-char model code** (e.g. `GUL82` = Pixel
10 Pro XL US, `G2YBB` = Pixel 9 mmWave US; run `provision --help` for the full list). The module holds the phone's **LTE fallback** (with `--lte-patch`), the
carrier's **NR file** (with `--nr-patch`), and the **PLMN legend** (with `--add-plmn`) — each present
only when its flag is given, so at least one is required. `--carrier` names the target for
`--add-plmn`/`--nr-patch` and must have files in the source folder; `--add-plmn` refuses a PLMN already
mapped to any carrier. Patch combos whose bands the model doesn't support (per the `pixel-bands`
table) are skipped with a warning. The output is the same kind of Magisk module `magisk` produces — flash it the
same way. Exit codes: `0` clean, `1` built but a patch skipped entries, `2` error.

### Audit a whole folder

```console
$ pixel-uecaps-toolbox check ~/uecaps
=== folder check: /home/you/uecaps ===
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
$ pixel-uecaps-toolbox matrix ~/uecaps > matrix.csv
$ pixel-uecaps-toolbox matrix ~/uecaps -o matrix.csv   # or write straight to a file
```

One row per carrier, one column per SKU capability profile; each cell is the
`<NUMBER>` of that carrier's file for that profile (empty when the carrier ships no
file for it — e.g. alt-tier carriers leave the last two profiles blank). Columns are
headed by the profile's **known Pixel model**, or its **anchor prime** when the model
isn't known, and are sorted by that header:

```console
$ pixel-uecaps-toolbox matrix ~/uecaps | head -1
carrier,1002739,196911437,2912407,3347,3539,688679,8969,Pixel 10 Pro Fold,Pixel 10 Pro XL,Pixel 9 (5G Sub-6 GHz),Pixel 9 (5G mmWave + Sub 6 GHz),Pixel 9 Pro (5G Sub-6 GHz),Pixel 9 Pro (5G mmWave + Sub 6 GHz),Pixel 9 Pro Fold,Pixel 9 Pro XL (5G Sub 6 GHz),Pixel 9 Pro XL (5G mmWave + Sub 6 GHz)
```

**Read:** a spreadsheet-friendly overview of the whole dump — at a glance, which
profiles each carrier provides and the exact selector numbers. Scans the same files
as `check`; non-carrier files (the legend, `lte_*`) are ignored.

## Command reference

| Command | What it does |
| --- | --- |
| `inspect <FILE> [--full] [--toml]` | Inspect one file. Adapts to the file type: a carrier file, the PLMN legend, or an `lte_*` fallback (whose LTE CA combinations it decodes). `--full` reveals the SKU-selection math and per-component capabilities; `--toml` emits the complete analysis as TOML. Exit `2` on an unrecognised filename. |
| `compare <A> <B> [--full] [--common]` | Diff two files' band combinations (set diff by default; `--full` adds per-component diffs; `--common` also lists the combos common to both — `=` identical, `~` caps differ). Exit `0` identical, `1` differ, `2` error. |
| `patch create <A> <B> [-o FILE]` | Diff two files (A→B) and emit a documented TOML combo patch to `-o` or stdout. Exit `0`/`2`. Both files must be the same kind (carrier or `lte_*`); the TOML's `kind` is `"nr"` or `"lte"`. |
| `patch apply <BASE> [--in FILE] [-o OUT] [--strict]` | Apply a combo patch to `BASE` → new `.binarypb` (`--in` stdin, `-o` stdout by default). Best-effort; `--strict` fails on the first non-applying entry. Exit `0` clean / `1` skipped / `2` error. Applies an `nr`/`lte` patch to a matching base. |
| `patch show [FILE] [--full]` | Render a combo patch (TOML; `FILE` or stdin) in human-readable form — its `delete` keys and `set` entries (`+` add, `~` change). `--full` adds per-component capabilities, like `inspect --full`. |
| `patch filter include <BANDS>… [--only] [--in FILE] [-o OUT]` | Keep only the patch's combos (and `delete`s) that involve any listed band; labels like `n77`/`B66`. With `--only`, keep a combo only when *every* band it uses is listed (else the whole combo is dropped). Patch in (`--in`, default stdin) → filtered patch out (`-o`, default stdout). |
| `patch filter exclude <BANDS>… [--in FILE] [-o OUT]` | Drop the patch's combos (and `delete`s) that involve any listed band; otherwise like `patch filter include`. |
| `magisk <FILE>… [--dest DIR] [-o OUT] [--name N]` | Package file(s) into a flashable Magisk module (`.zip` → `-o` or stdout). Overlays each onto `--dest` (default `/vendor/firmware/uecapconfig`) via Magisk's systemless mount. Inputs are packaged as opaque bytes. |
| `provision <CODE> [DIR] …` | Build a flashable Magisk module for one Pixel SKU named by its Google 5-char model code (e.g. `GUL82`) from a folder of capability files (default `.`). Includes the LTE file (`--lte-patch`), the carrier's NR file (`--nr-patch`), and/or the legend (`--add-plmn`) — each only when its flag is present; at least one required. `--carrier` targets `--add-plmn`/`--nr-patch` (and must have files in the folder). Patch combos using bands the model lacks (per `pixel-bands`) are skipped with a warning. `--dest`/`--name`/`-o`/`--strict` behave as elsewhere. Exit `0`/`1`/`2`. |
| `mapping decode` / `encode` | Decode the legend to editable TOML / re-encode TOML back to `.binarypb` (stdin → stdout). |
| `mapping inject-plmn <CARRIER> <PLMN…>` | Append one or more PLMNs (MCC-MNC) to a carrier (stdin → stdout). |
| `check [DIR]` | Scan a folder (default `.`) and report everything that doesn't fit the scheme. Exit `1` on a genuine anomaly. |
| `matrix [DIR] [-o FILE]` | Scan a folder (default `.`) and emit a carrier × profile matrix as CSV to `-o` or stdout. Columns are headed by Pixel model (or the profile's anchor prime when unknown), sorted by header. |
| `self-test` | Run built-in, data-independent sanity checks. |

Under `--toml`, each band component carries structured capability fields —
`dl_max_bw_mhz`, `dl_mimo`, `dl_scs_khz`, `dl_mod_order`, `dl_bw90mhz`, and the
`ul_*` equivalents. `--toml` also works on the legend (`type = "mapping"`) and on
`lte_*` files (`type = "lte"`).

## How the file naming works

The trailing `NUMBER` in `<CARRIER>_<NUMBER>.binarypb` is **not** a hash or version
— it is a selector key:

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

- `lte_*.binarypb` — LTE-only fallback configs (no profile).
- `ap_plmn_mapping.binarypb` — the PLMN→carrier legend.

## Development

- **Build internals:** `build.rs` runs protox → prost-build, compiling
  `proto/ue_caps.proto` to Rust types at build time (pure Rust — no system
  `protoc`).
- **Tests:** `cargo test` — covers the protobuf decoder, the factorizer (including
  the dataset's largest number), PLMN decoding, profile identification, filename
  parsing, and the TOML / band-combination rendering.
- **CI:** every push and PR runs `cargo fmt --all --check -- --config=imports_granularity=Crate`,
  `cargo hack clippy --workspace --each-feature -- -D warnings`, and
  `cargo hack test --workspace --each-feature`.

## License

Licensed under the [Apache License, Version 2.0](LICENSE); see [`NOTICE`](NOTICE).

Not affiliated with or endorsed by Google; the file format is observed, not
documented. For research and personal use — editing device configs is at your own
risk.
