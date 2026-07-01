//! pixel-uecaps-toolbox — decode and validate Google Pixel UE-capabilities files.

use pixel_uecaps_toolbox::{magisk, mapping, patch, provision, report};

use clap::{Parser, Subcommand};
use std::{path::PathBuf, process::ExitCode};

/// Decode/validate Pixel UE-capabilities `.binarypb` files.
#[derive(Parser)]
#[command(name = "pixel-uecaps-toolbox", version, about)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Inspect one <CARRIER>_<NUMBER>.binarypb file
    Inspect {
        file: PathBuf,
        /// Reveal per-component combo detail and the SKU-selection math
        #[arg(long)]
        full: bool,
        /// Emit the complete analysis as TOML
        #[arg(long)]
        toml: bool,
    },
    /// Scan a folder and report everything that does not match
    Check {
        #[arg(default_value = ".")]
        dir: PathBuf,
    },
    /// Emit a carrier × profile matrix (CSV) for a folder of capability files
    Matrix {
        #[arg(default_value = ".")]
        dir: PathBuf,
        /// Write the CSV here instead of stdout
        #[arg(short = 'o', long)]
        out: Option<PathBuf>,
    },
    /// Run built-in, data-independent sanity checks
    SelfTest,
    /// Decode, encode, or edit ap_plmn_mapping.binarypb (stdin -> stdout)
    Mapping {
        #[command(subcommand)]
        cmd: MappingCmd,
    },
    /// Compare the band combinations of two capability files
    Compare {
        file_a: PathBuf,
        file_b: PathBuf,
        #[arg(long)]
        full: bool,
        /// Also list the combos common to both files (= identical caps, ~ caps differ)
        #[arg(long)]
        common: bool,
    },
    /// Package UE-capability file(s) into a flashable Magisk module (.zip)
    Magisk {
        /// Files to overlay (each kept under its own name)
        #[arg(required = true)]
        files: Vec<PathBuf>,
        /// On-device destination directory (absolute)
        #[arg(long, default_value = DEFAULT_DEST)]
        dest: String,
        /// Write the .zip here instead of stdout
        #[arg(short = 'o', long)]
        out: Option<PathBuf>,
        /// Module name shown in the Magisk app
        #[arg(long, default_value = "Pixel UE-caps override")]
        name: String,
    },
    /// Create or apply a band-combination patch between capability files
    Patch {
        #[command(subcommand)]
        cmd: PatchCmd,
    },
    /// Build a flashable Magisk package for one phone
    #[command(group(
        clap::ArgGroup::new("modifier")
            .required(true)
            .multiple(true)
            .args(["lte_patch", "nr_patch", "add_plmn"])
    ))]
    Provision {
        /// Google 5-char model code, e.g. GUL82 (pass an unknown code to list all supported)
        model: String,
        /// Source folder of .binarypb files
        #[arg(default_value = ".")]
        dir: PathBuf,
        /// Carrier to target (required by --add-plmn/--nr-patch), e.g. VZW
        #[arg(long)]
        carrier: Option<String>,
        /// Apply an `lte` combo patch (TOML); includes the LTE file only when given
        #[arg(long)]
        lte_patch: Option<PathBuf>,
        /// Apply an `nr` combo patch (TOML) to the carrier's NR file (requires --carrier)
        #[arg(long, requires = "carrier")]
        nr_patch: Option<PathBuf>,
        /// Add a PLMN (MCC-MNC) to <CARRIER> in the legend; repeatable (requires --carrier)
        #[arg(long = "add-plmn", requires = "carrier")]
        add_plmn: Vec<String>,
        /// Write the .zip here instead of stdout
        #[arg(short = 'o', long)]
        out: Option<PathBuf>,
        /// On-device destination directory (absolute)
        #[arg(long, default_value = DEFAULT_DEST)]
        dest: String,
        /// Module name shown in the Magisk app
        #[arg(long)]
        name: Option<String>,
        /// Abort on the first patch entry that cannot be applied
        #[arg(long)]
        strict: bool,
    },
}

#[derive(Subcommand)]
enum MappingCmd {
    /// Decode a binarypb (stdin) to editable TOML (stdout)
    Decode,
    /// Encode editable TOML (stdin) to a binarypb (stdout)
    Encode,
    /// Append one or more PLMNs (MCC-MNC) to a named carrier; binarypb stdin -> stdout
    InjectPlmn {
        /// Target carrier name (the mapping's identifier)
        carrier: String,
        /// One or more PLMNs as MCC-MNC, e.g. 250-01 310-004
        #[arg(required = true)]
        plmns: Vec<String>,
    },
}

#[derive(Subcommand)]
enum PatchCmd {
    /// Diff two files (A -> B) and emit a combo patch (TOML) to -o or stdout
    Create {
        file_a: PathBuf,
        file_b: PathBuf,
        /// Write the patch here instead of stdout
        #[arg(short = 'o', long)]
        out: Option<PathBuf>,
    },
    /// Apply a combo patch to a base file -> new .binarypb (stdin/stdout by default)
    Apply {
        /// Base capability file to patch
        base: PathBuf,
        /// Patch TOML to read (default: stdin)
        #[arg(long = "in")]
        input: Option<PathBuf>,
        /// Write the .binarypb here instead of stdout
        #[arg(short = 'o', long)]
        out: Option<PathBuf>,
        /// Abort on the first entry that cannot be applied
        #[arg(long)]
        strict: bool,
    },
    /// View a combo patch (TOML) in human-readable form (FILE or stdin)
    Show {
        /// Patch TOML to read (default: stdin)
        file: Option<PathBuf>,
        /// Show per-component capabilities for each set entry
        #[arg(long)]
        full: bool,
    },
    /// Filter a combo patch by band — keep (include), keep-only (include --only), or drop (exclude) matching combos
    Filter {
        #[command(subcommand)]
        cmd: FilterCmd,
    },
}

#[derive(Subcommand)]
enum FilterCmd {
    /// Keep only combos that contain any of the given bands
    Include {
        /// Band labels, e.g. n77 B66 (NR n…, LTE B…)
        #[arg(required = true)]
        bands: Vec<String>,
        /// Keep a combo only when every band it uses is in the given set (else drop the whole combo)
        #[arg(long)]
        only: bool,
        /// Patch TOML to read (default: stdin)
        #[arg(long = "in")]
        input: Option<PathBuf>,
        /// Write the filtered patch here instead of stdout
        #[arg(short = 'o', long)]
        out: Option<PathBuf>,
    },
    /// Keep only combos that contain none of the given bands
    Exclude {
        /// Band labels, e.g. n258 (NR n…, LTE B…)
        #[arg(required = true)]
        bands: Vec<String>,
        /// Patch TOML to read (default: stdin)
        #[arg(long = "in")]
        input: Option<PathBuf>,
        /// Write the filtered patch here instead of stdout
        #[arg(short = 'o', long)]
        out: Option<PathBuf>,
    },
}

/// Default on-device destination for overlaid capability files.
const DEFAULT_DEST: &str = "/vendor/firmware/uecapconfig";

fn main() -> ExitCode {
    match run() {
        Ok(code) => ExitCode::from(u8::try_from(code).unwrap_or(2)),
        Err(e) => {
            eprintln!("error: {e:#}");
            ExitCode::from(2)
        }
    }
}

fn run() -> anyhow::Result<i32> {
    Cli::parse().cmd.run()
}

impl Cmd {
    fn run(self) -> anyhow::Result<i32> {
        match self {
            Self::Inspect { file, full, toml } => report::inspect(&file, full, toml),
            Self::Check { dir } => report::check_folder(&dir),
            Self::Matrix { dir, out } => report::matrix(&dir, out.as_deref()),
            Self::SelfTest => report::self_test(),
            Self::Mapping { cmd } => cmd.run(),
            Self::Compare {
                file_a,
                file_b,
                full,
                common,
            } => report::compare(&file_a, &file_b, full, common),
            Self::Magisk {
                files,
                dest,
                out,
                name,
            } => magisk::package(&files, &dest, out.as_deref(), &name),
            Self::Patch { cmd } => cmd.run(),
            Self::Provision {
                model,
                dir,
                carrier,
                lte_patch,
                nr_patch,
                add_plmn,
                out,
                dest,
                name,
                strict,
            } => provision::run(
                &model,
                &dir,
                carrier.as_deref(),
                lte_patch.as_deref(),
                nr_patch.as_deref(),
                &add_plmn,
                out.as_deref(),
                &dest,
                name.as_deref(),
                strict,
            ),
        }
    }
}

impl MappingCmd {
    fn run(self) -> anyhow::Result<i32> {
        match self {
            Self::Decode => mapping::decode(),
            Self::Encode => mapping::encode(),
            Self::InjectPlmn { carrier, plmns } => mapping::inject(&carrier, &plmns),
        }
    }
}

impl PatchCmd {
    fn run(self) -> anyhow::Result<i32> {
        match self {
            Self::Create {
                file_a,
                file_b,
                out,
            } => patch::create(&file_a, &file_b, out.as_deref()),
            Self::Apply {
                base,
                input,
                out,
                strict,
            } => patch::apply(&base, input.as_deref(), out.as_deref(), strict),
            Self::Show { file, full } => patch::show(file.as_deref(), full),
            Self::Filter { cmd } => cmd.run(),
        }
    }
}

impl FilterCmd {
    fn run(self) -> anyhow::Result<i32> {
        let (mode, bands, input, out) = match self {
            Self::Include {
                bands,
                only,
                input,
                out,
            } => (
                if only {
                    patch::FilterMode::IncludeOnly
                } else {
                    patch::FilterMode::Include
                },
                bands,
                input,
                out,
            ),
            Self::Exclude { bands, input, out } => (patch::FilterMode::Exclude, bands, input, out),
        };
        patch::filter(mode, &bands, input.as_deref(), out.as_deref())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[test]
    fn parses_decode_subcommand() {
        let cli = Cli::parse_from(["x", "mapping", "decode"]);
        assert!(matches!(
            cli.cmd,
            Cmd::Mapping {
                cmd: MappingCmd::Decode
            }
        ));
    }

    #[test]
    fn parses_inject_plmn_with_multiple_plmns() {
        let cli = Cli::parse_from(["x", "mapping", "inject-plmn", "VZW", "250-01", "310-004"]);
        let Cmd::Mapping {
            cmd: MappingCmd::InjectPlmn { carrier, plmns },
        } = cli.cmd
        else {
            panic!("expected mapping inject-plmn")
        };
        assert_eq!(carrier, "VZW");
        assert_eq!(plmns, vec!["250-01".to_string(), "310-004".to_string()]);
    }

    #[test]
    fn inject_plmn_requires_at_least_one_plmn() {
        assert!(Cli::try_parse_from(["x", "mapping", "inject-plmn", "VZW"]).is_err());
    }

    #[test]
    fn parses_compare() {
        let cli = Cli::parse_from(["x", "compare", "a.binarypb", "b.binarypb", "--full"]);
        let Cmd::Compare {
            file_a,
            file_b,
            full,
            common,
        } = cli.cmd
        else {
            panic!("expected compare")
        };
        assert_eq!(file_a, PathBuf::from("a.binarypb"));
        assert_eq!(file_b, PathBuf::from("b.binarypb"));
        assert!(full);
        assert!(!common);
    }

    #[test]
    fn parses_compare_common() {
        let cli = Cli::parse_from(["x", "compare", "a.binarypb", "b.binarypb", "--common"]);
        let Cmd::Compare {
            file_a,
            file_b,
            full,
            common,
        } = cli.cmd
        else {
            panic!("expected compare")
        };
        assert_eq!(file_a, PathBuf::from("a.binarypb"));
        assert_eq!(file_b, PathBuf::from("b.binarypb"));
        assert!(!full);
        assert!(common);
    }

    #[test]
    fn compare_requires_two_files() {
        assert!(Cli::try_parse_from(["x", "compare", "only-one.binarypb"]).is_err());
    }

    #[test]
    fn parses_magisk() {
        let cli = Cli::parse_from([
            "x",
            "magisk",
            "a.binarypb",
            "b.binarypb",
            "--dest",
            "/x",
            "-o",
            "out.zip",
            "--name",
            "Foo",
        ]);
        let Cmd::Magisk {
            files,
            dest,
            out,
            name,
        } = cli.cmd
        else {
            panic!("expected magisk")
        };
        assert_eq!(
            files,
            vec![PathBuf::from("a.binarypb"), PathBuf::from("b.binarypb")]
        );
        assert_eq!(dest, "/x");
        assert_eq!(out, Some(PathBuf::from("out.zip")));
        assert_eq!(name, "Foo");
    }

    #[test]
    fn magisk_requires_a_file() {
        assert!(Cli::try_parse_from(["x", "magisk"]).is_err());
    }

    #[test]
    fn parses_patch_create() {
        let cli = Cli::parse_from([
            "x",
            "patch",
            "create",
            "a.binarypb",
            "b.binarypb",
            "-o",
            "p.toml",
        ]);
        let Cmd::Patch {
            cmd:
                PatchCmd::Create {
                    file_a,
                    file_b,
                    out,
                },
        } = cli.cmd
        else {
            panic!("expected patch create")
        };
        assert_eq!(file_a, PathBuf::from("a.binarypb"));
        assert_eq!(file_b, PathBuf::from("b.binarypb"));
        assert_eq!(out, Some(PathBuf::from("p.toml")));
    }

    #[test]
    fn parses_patch_apply() {
        let cli = Cli::parse_from([
            "x",
            "patch",
            "apply",
            "VZW_1.binarypb",
            "--in",
            "p.toml",
            "-o",
            "out.binarypb",
            "--strict",
        ]);
        let Cmd::Patch {
            cmd:
                PatchCmd::Apply {
                    base,
                    input,
                    out,
                    strict,
                },
        } = cli.cmd
        else {
            panic!("expected patch apply")
        };
        assert_eq!(base, PathBuf::from("VZW_1.binarypb"));
        assert_eq!(input, Some(PathBuf::from("p.toml")));
        assert_eq!(out, Some(PathBuf::from("out.binarypb")));
        assert!(strict);
    }

    #[test]
    fn patch_apply_defaults() {
        let cli = Cli::parse_from(["x", "patch", "apply", "VZW_1.binarypb"]);
        let Cmd::Patch {
            cmd:
                PatchCmd::Apply {
                    base,
                    input,
                    out,
                    strict,
                },
        } = cli.cmd
        else {
            panic!("expected patch apply")
        };
        assert_eq!(base, PathBuf::from("VZW_1.binarypb"));
        assert_eq!(input, None);
        assert_eq!(out, None);
        assert!(!strict);
    }

    #[test]
    fn parses_matrix() {
        let cli = Cli::parse_from(["x", "matrix", "some/dir", "-o", "out.csv"]);
        let Cmd::Matrix { dir, out } = cli.cmd else {
            panic!("expected matrix")
        };
        assert_eq!(dir, PathBuf::from("some/dir"));
        assert_eq!(out, Some(PathBuf::from("out.csv")));
    }

    #[test]
    fn matrix_dir_defaults_to_cwd() {
        let cli = Cli::parse_from(["x", "matrix"]);
        let Cmd::Matrix { dir, out } = cli.cmd else {
            panic!("expected matrix")
        };
        assert_eq!(dir, PathBuf::from("."));
        assert_eq!(out, None);
    }

    #[test]
    fn magisk_dest_defaults() {
        let cli = Cli::parse_from(["x", "magisk", "a.binarypb"]);
        let Cmd::Magisk {
            files,
            dest,
            out,
            name,
        } = cli.cmd
        else {
            panic!("expected magisk")
        };
        assert_eq!(files, vec![PathBuf::from("a.binarypb")]);
        assert_eq!(dest, "/vendor/firmware/uecapconfig");
        assert_eq!(out, None);
        assert_eq!(name, "Pixel UE-caps override");
    }

    #[test]
    fn parses_provision_minimal() {
        let cli = Cli::parse_from(["x", "provision", "GUL82", "--lte-patch", "p.toml"]);
        let Cmd::Provision {
            model,
            dir,
            lte_patch,
            ..
        } = cli.cmd
        else {
            panic!("expected provision")
        };
        assert_eq!(model, "GUL82");
        assert_eq!(dir, PathBuf::from("."));
        assert_eq!(lte_patch, Some(PathBuf::from("p.toml")));
    }

    #[test]
    fn parses_provision_full() {
        let cli = Cli::parse_from([
            "x",
            "provision",
            "GUL82",
            "/dump",
            "--carrier",
            "VZW",
            "--nr-patch",
            "nr.toml",
            "--add-plmn",
            "250-99",
            "--add-plmn",
            "460-01",
            "-o",
            "out.zip",
            "--strict",
        ]);
        let Cmd::Provision {
            model,
            dir,
            carrier,
            nr_patch,
            add_plmn,
            out,
            strict,
            ..
        } = cli.cmd
        else {
            panic!("expected provision")
        };
        assert_eq!(model, "GUL82");
        assert_eq!(dir, PathBuf::from("/dump"));
        assert_eq!(carrier, Some("VZW".to_string()));
        assert_eq!(nr_patch, Some(PathBuf::from("nr.toml")));
        assert_eq!(add_plmn, vec!["250-99".to_string(), "460-01".to_string()]);
        assert_eq!(out, Some(PathBuf::from("out.zip")));
        assert!(strict);
    }

    #[test]
    fn provision_requires_a_modifier() {
        assert!(Cli::try_parse_from(["x", "provision", "GUL82"]).is_err());
    }

    #[test]
    fn provision_nr_patch_requires_carrier() {
        assert!(Cli::try_parse_from(["x", "provision", "GUL82", "--nr-patch", "n.toml"]).is_err());
    }

    #[test]
    fn provision_add_plmn_requires_carrier() {
        assert!(Cli::try_parse_from(["x", "provision", "GUL82", "--add-plmn", "250-99"]).is_err());
    }

    #[test]
    fn parses_patch_show() {
        let cli = Cli::parse_from(["x", "patch", "show", "p.toml", "--full"]);
        let Cmd::Patch {
            cmd: PatchCmd::Show { file, full },
        } = cli.cmd
        else {
            panic!("expected patch show")
        };
        assert_eq!(file, Some(PathBuf::from("p.toml")));
        assert!(full);
    }

    #[test]
    fn parses_patch_show_defaults_to_stdin() {
        let cli = Cli::parse_from(["x", "patch", "show"]);
        let Cmd::Patch {
            cmd: PatchCmd::Show { file, full },
        } = cli.cmd
        else {
            panic!("expected patch show")
        };
        assert_eq!(file, None);
        assert!(!full);
    }

    #[test]
    fn parses_patch_filter_include() {
        let cli = Cli::parse_from([
            "x", "patch", "filter", "include", "n77", "n78", "--in", "p.toml", "-o", "o.toml",
        ]);
        let Cmd::Patch {
            cmd:
                PatchCmd::Filter {
                    cmd:
                        FilterCmd::Include {
                            bands,
                            only,
                            input,
                            out,
                        },
                },
        } = cli.cmd
        else {
            panic!("expected patch filter include")
        };
        assert_eq!(bands, vec!["n77".to_string(), "n78".to_string()]);
        assert!(!only);
        assert_eq!(input, Some(PathBuf::from("p.toml")));
        assert_eq!(out, Some(PathBuf::from("o.toml")));
    }

    #[test]
    fn parses_patch_filter_exclude_defaults() {
        let cli = Cli::parse_from(["x", "patch", "filter", "exclude", "n258"]);
        let Cmd::Patch {
            cmd:
                PatchCmd::Filter {
                    cmd: FilterCmd::Exclude { bands, input, out },
                },
        } = cli.cmd
        else {
            panic!("expected patch filter exclude")
        };
        assert_eq!(bands, vec!["n258".to_string()]);
        assert_eq!(input, None);
        assert_eq!(out, None);
    }

    #[test]
    fn patch_filter_requires_bands() {
        assert!(Cli::try_parse_from(["x", "patch", "filter", "include"]).is_err());
    }

    #[test]
    fn parses_patch_filter_include_only() {
        let cli = Cli::parse_from(["x", "patch", "filter", "include", "n77", "--only"]);
        let Cmd::Patch {
            cmd:
                PatchCmd::Filter {
                    cmd:
                        FilterCmd::Include {
                            bands,
                            only,
                            input,
                            out,
                        },
                },
        } = cli.cmd
        else {
            panic!("expected patch filter include --only")
        };
        assert_eq!(bands, vec!["n77".to_string()]);
        assert!(only);
        assert_eq!(input, None);
        assert_eq!(out, None);
    }
}
