//! Command-line interface for creating and updating Ubuntu Rust source packages.

mod apt;
mod cargo;
mod changelog;
mod config;
mod deps;
mod generate;
mod import;
mod input;
mod package;
mod published;
mod resolve;
mod util;

use std::process::ExitCode;

use clap::{Parser, Subcommand};

/// Top-level command-line arguments.
#[derive(Parser)]
#[command(version, about = "Maintain Ubuntu Rust source packages with debcargo")]
struct Cli {
    /// Operation to perform.
    #[command(subcommand)]
    command: Command,
}

/// Operations supported by the current Ubucargo release.
#[derive(Subcommand)]
enum Command {
    /// Inspect Ubuntu candidates for a crate's direct Rust dependencies.
    Deps(deps::DepArgs),

    /// Import a published source package without regenerating it.
    Import(import::ImportArgs),

    /// Create or reconcile a complete source package.
    Package(package::PackageArgs),
}

/// Parses the command line, runs the selected command, and maps its result to an exit status.
fn main() -> ExitCode {
    let result = match Cli::parse().command {
        Command::Deps(args) => deps::run(args),
        Command::Import(args) => import::run(args),
        Command::Package(args) => package::run(args),
    };

    match result {
        Ok(changed) if changed => ExitCode::from(1),
        Ok(_) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("error: {error:#}");
            ExitCode::from(2)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    /// Accepts registry, local, and dependency command forms.
    fn parses_command_arguments() {
        for arguments in [
            vec!["deps", "local:../example", "--series", "noble"],
            vec![
                "package",
                "serde",
                "1.0.220",
                "--package-dir",
                "rust-serde",
                "--check",
                "--force",
                "--keep-staging",
            ],
            vec![
                "package",
                "local:../example",
                "--package-dir",
                "rust-example",
            ],
            vec![
                "deps",
                "serde",
                "1.0.220",
                "--series",
                "noble",
                "--proposed",
                "--ppa",
                "ppa:example/rust-staging",
                "--architecture",
                "arm64",
                "--keep-staging",
            ],
        ] {
            Cli::try_parse_from(std::iter::once("ubucargo").chain(arguments)).unwrap();
        }
    }

    #[test]
    /// Rejects removed flags and extra positional arguments during CLI parsing.
    fn rejects_removed_flags() {
        for arguments in [
            vec!["deps", "--package-dir", "./example", "--series", "noble"],
            vec!["deps", "--local-crate", "./example", "--series", "noble"],
            vec![
                "package",
                "--local-crate",
                "./example",
                "--package-dir",
                "./package",
            ],
            vec!["deps", "serde", "1.0.0", "extra", "--series", "noble"],
        ] {
            assert!(Cli::try_parse_from(std::iter::once("ubucargo").chain(arguments)).is_err());
        }
        for arguments in [
            vec!["deps", "archive:noble/rust-serde"],
            vec!["package", "pkg:./example"],
        ] {
            Cli::try_parse_from(std::iter::once("ubucargo").chain(arguments)).unwrap();
        }
    }
}
