//! Command-line interface for creating and updating Ubuntu Rust source packages.

mod cargo;
mod changelog;
mod command;
mod config;
mod deps;
mod generate;
mod package;
mod resolve;
mod tree;

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

    /// Create or reconcile a complete source package.
    Package(package::PackageArgs),
}

/// Parses the command line, runs the selected command, and maps its result to an exit status.
fn main() -> ExitCode {
    let result = match Cli::parse().command {
        Command::Deps(args) => deps::run(args),
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
                "--local-crate",
                "../example",
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
            ],
        ] {
            Cli::try_parse_from(std::iter::once("ubucargo").chain(arguments)).unwrap();
        }
    }

    #[test]
    /// Rejects argument combinations before attempting filesystem or network work.
    fn rejects_conflicting_targets() {
        for arguments in [
            vec!["package", "--local-crate", "../example"],
            vec![
                "package",
                "serde",
                "--local-crate",
                "../example",
                "--package-dir",
                "rust-example",
            ],
            vec![
                "package",
                "serde",
                "1.0.0",
                "--local-crate",
                "../example",
                "--package-dir",
                "rust-example",
            ],
            vec![
                "deps",
                "serde",
                "--package-dir",
                "rust-serde",
                "--series",
                "noble",
            ],
        ] {
            let error = Cli::try_parse_from(std::iter::once("ubucargo").chain(arguments))
                .err()
                .unwrap();
            assert!(matches!(
                error.kind(),
                clap::error::ErrorKind::ArgumentConflict
                    | clap::error::ErrorKind::MissingRequiredArgument
            ));
            assert_eq!(error.exit_code(), 2);
        }
    }
}
