//! pixel-uecaps-toolbox — inspect and audit Google Pixel UE-capabilities files, and compile a
//! complete offline `uecapconfig` folder into a flashable replacement module.

use pixel_uecaps_toolbox::{compiler, report};

use clap::{Parser, Subcommand};
use std::{path::PathBuf, process::ExitCode};

/// Inspect, audit, and compile Pixel UE-capabilities `.binarypb` files.
#[derive(Parser)]
#[command(name = "pixel-uecaps-toolbox", version, about)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Decompose complete bitmask and profiled folders into canonical compiler sources
    Decompose {
        /// Legacy bitmask-based uecapconfig folder
        #[arg(long)]
        bitmask: PathBuf,
        /// Profile-based Exynos 5400 uecapconfig folder
        #[arg(long)]
        profiled: PathBuf,
        /// Write nr.kdl and lte.kdl into this directory
        #[arg(short = 'o', long)]
        out: PathBuf,
    },
    /// Provision a complete model-specific replacement Magisk module from compiler sources
    Provision {
        /// Registered Google 5-character hardware model code
        model: String,
        /// Directory containing nr.kdl and lte.kdl
        source: PathBuf,
        /// Write the replacement Magisk ZIP here
        #[arg(short = 'o', long)]
        out: PathBuf,
        /// Module name shown in the Magisk app
        #[arg(long)]
        name: Option<String>,
    },
    /// Inspect one <CARRIER>_<NUMBER>.binarypb file
    Inspect {
        file: PathBuf,
        /// Reveal per-component combo detail and the SKU-selection math
        #[arg(long)]
        full: bool,
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
}

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
            Self::Decompose {
                bitmask,
                profiled,
                out,
            } => compiler::decompose(&bitmask, &profiled, &out),
            Self::Provision {
                model,
                source,
                out,
                name,
            } => compiler::provision(&model, &source, &out, name.as_deref()),
            Self::Inspect { file, full } => report::inspect(&file, full),
            Self::Check { dir } => report::check_folder(&dir),
            Self::Matrix { dir, out } => report::matrix(&dir, out.as_deref()),
            Self::SelfTest => report::self_test(),
            Self::Compare {
                file_a,
                file_b,
                full,
                common,
            } => report::compare(&file_a, &file_b, full, common),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[test]
    fn parses_compiler_decompose() {
        let cli = Cli::parse_from([
            "x",
            "decompose",
            "--bitmask",
            "OLD",
            "--profiled",
            "NEW",
            "-o",
            "SOURCE",
        ]);
        let Cmd::Decompose {
            bitmask,
            profiled,
            out,
        } = cli.cmd
        else {
            panic!("expected compiler decompose")
        };
        assert_eq!(bitmask, PathBuf::from("OLD"));
        assert_eq!(profiled, PathBuf::from("NEW"));
        assert_eq!(out, PathBuf::from("SOURCE"));
    }

    #[test]
    fn parses_compiler_decompose_requires_both_folders_and_output() {
        for args in [
            vec!["x", "decompose", "--profiled", "NEW", "-o", "SOURCE"],
            vec!["x", "decompose", "--bitmask", "OLD", "-o", "SOURCE"],
            vec!["x", "decompose", "--bitmask", "OLD", "--profiled", "NEW"],
        ] {
            assert!(Cli::try_parse_from(args).is_err());
        }
    }

    #[test]
    fn parses_provision() {
        let cli = Cli::parse_from(["x", "provision", "GUL82", "SOURCE", "-o", "module.zip"]);
        let Cmd::Provision {
            model,
            source,
            out,
            name,
        } = cli.cmd
        else {
            panic!("expected provision")
        };
        assert_eq!(model, "GUL82");
        assert_eq!(source, PathBuf::from("SOURCE"));
        assert_eq!(out, PathBuf::from("module.zip"));
        assert_eq!(name, None);
    }

    #[test]
    fn parses_provision_with_name() {
        let cli = Cli::parse_from([
            "x",
            "provision",
            "GUL82",
            "SOURCE",
            "-o",
            "module.zip",
            "--name",
            "My module",
        ]);
        let Cmd::Provision { name, .. } = cli.cmd else {
            panic!("expected provision")
        };
        assert_eq!(name.as_deref(), Some("My module"));
    }

    #[test]
    fn parses_provision_requires_model_source_and_output() {
        for args in [
            vec!["x", "provision"],
            vec!["x", "provision", "GUL82"],
            vec!["x", "provision", "GUL82", "SOURCE"],
        ] {
            assert!(Cli::try_parse_from(args).is_err());
        }
    }

    #[test]
    fn parses_provision_rejects_destination_override() {
        assert!(
            Cli::try_parse_from([
                "x",
                "provision",
                "GUL82",
                "SOURCE",
                "-o",
                "module.zip",
                "--dest",
                "/vendor/elsewhere",
            ])
            .is_err()
        );
    }

    #[test]
    fn parses_provision_has_no_legacy_layout_positional() {
        assert!(
            Cli::try_parse_from([
                "x",
                "provision",
                "legacy",
                "GUL82",
                "SOURCE",
                "-o",
                "module.zip",
            ])
            .is_err()
        );
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
    fn parses_decompose() {
        let cli = Cli::parse_from([
            "x",
            "decompose",
            "--bitmask",
            "b",
            "--profiled",
            "p",
            "-o",
            "src",
        ]);
        let Cmd::Decompose {
            bitmask,
            profiled,
            out,
        } = cli.cmd
        else {
            panic!("expected decompose");
        };
        assert_eq!(bitmask, PathBuf::from("b"));
        assert_eq!(profiled, PathBuf::from("p"));
        assert_eq!(out, PathBuf::from("src"));
    }
}
