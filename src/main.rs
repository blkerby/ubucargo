//! Command-line interface for creating and updating Ubuntu Rust source packages.

mod command;
mod deps;
mod package;

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
    use std::path::Path;

    use super::*;

    #[test]
    /// Verifies the consolidated package command's positional and flag parsing.
    fn parses_package_arguments() {
        let cli = Cli::try_parse_from([
            "ubucargo",
            "package",
            "serde",
            "1.0.220",
            "--package-dir",
            "rust-serde",
            "--check",
            "--force",
            "--keep-staging",
        ])
        .unwrap();

        let Command::Package(args) = cli.command else {
            panic!("expected package command");
        };
        assert_eq!(args.crate_name.as_deref(), Some("serde"));
        assert_eq!(args.version.as_deref(), Some("1.0.220"));
        assert_eq!(args.package_dir.as_deref(), Some(Path::new("rust-serde")));
        assert_eq!(args.local_crate, None);
        assert!(args.check);
        assert!(args.force);
        assert!(args.keep_staging);
    }

    #[test]
    /// Verifies local crate and package directory argument parsing.
    fn parses_local_package_arguments() {
        let cli = Cli::try_parse_from([
            "ubucargo",
            "package",
            "--local-crate",
            "../example",
            "--package-dir",
            "rust-example",
        ])
        .unwrap();

        let Command::Package(args) = cli.command else {
            panic!("expected package command");
        };
        assert_eq!(args.crate_name, None);
        assert_eq!(args.version, None);
        assert_eq!(args.package_dir.as_deref(), Some(Path::new("rust-example")));
        assert_eq!(args.local_crate.as_deref(), Some(Path::new("../example")));
        assert!(
            Cli::try_parse_from(["ubucargo", "package", "serde", "--directory", "rust-serde"])
                .is_err()
        );
    }

    #[test]
    /// Verifies dependency command target and Archive argument parsing.
    fn parses_dependency_arguments() {
        let cli = Cli::try_parse_from([
            "ubucargo",
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
        ])
        .unwrap();

        let Command::Deps(args) = cli.command else {
            panic!("expected deps command");
        };
        assert_eq!(args.crate_name.as_deref(), Some("serde"));
        assert_eq!(args.version.as_deref(), Some("1.0.220"));
        assert_eq!(args.package_dir, None);
        assert_eq!(args.series, "noble");
        assert!(args.proposed);
        assert_eq!(args.ppa, ["ppa:example/rust-staging"]);
        assert_eq!(args.architecture.as_deref(), Some("arm64"));
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
