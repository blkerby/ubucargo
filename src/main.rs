//! Command-line interface for creating and updating Ubuntu Rust source packages.

mod deps;
mod package;

use std::{path::PathBuf, process::ExitCode};

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
    Deps {
        /// Crate name from crates.io; conflicts with --package-dir.
        #[arg(value_name = "CRATE")]
        crate_name: Option<String>,

        /// Exact crate version; defaults to the latest release when a crate is named.
        #[arg(value_name = "VERSION")]
        version: Option<String>,

        /// Existing source package directory; defaults to the nearest parent package.
        #[arg(long = "package-dir", value_name = "DIR")]
        package_dir: Option<PathBuf>,

        /// Ubuntu series to query.
        #[arg(long, value_name = "SERIES")]
        series: String,

        /// Include the Ubuntu proposed pocket.
        #[arg(long)]
        proposed: bool,

        /// Public Launchpad PPA to include.
        #[arg(long, value_name = "ppa:OWNER/NAME")]
        ppa: Vec<String>,

        /// Debian architecture; defaults to dpkg --print-architecture.
        #[arg(long, value_name = "ARCH")]
        architecture: Option<String>,
    },

    /// Create or reconcile a complete source package.
    Package {
        /// Crate name; defaults to the existing package's root Cargo identity.
        #[arg(value_name = "CRATE")]
        crate_name: Option<String>,

        /// Exact crate version; defaults to the latest release when a crate is named.
        #[arg(value_name = "VERSION")]
        version: Option<String>,

        /// Debian source-package directory; defaults to the nearest parent package.
        #[arg(long = "package-dir", value_name = "DIR")]
        package_dir: Option<PathBuf>,

        /// Local crate used to create a new source package.
        #[arg(long, value_name = "DIR")]
        local_crate: Option<PathBuf>,

        /// Report changes without writing them.
        #[arg(long)]
        check: bool,

        /// Resolve source-tree conflicts in favor of the selected crate release.
        #[arg(long)]
        force: bool,

        /// Retain the temporary debcargo staging directory for inspection.
        #[arg(long)]
        keep_staging: bool,

        /// Preserve an ambiguous existing file and establish it as an override.
        #[arg(long, value_name = "PATH")]
        keep: Vec<PathBuf>,

        /// Replace an ambiguous existing file with generated output.
        #[arg(long, value_name = "PATH")]
        replace: Vec<PathBuf>,
    },
}

/// Parses the command line, runs the selected command, and maps its result to an exit status.
fn main() -> ExitCode {
    let result = match Cli::parse().command {
        Command::Deps {
            crate_name,
            version,
            package_dir,
            series,
            proposed,
            ppa,
            architecture,
        } => deps::run(
            crate_name.as_deref(),
            version.as_deref(),
            package_dir.as_deref(),
            &series,
            proposed,
            &ppa,
            architecture.as_deref(),
        ),
        Command::Package {
            crate_name,
            version,
            package_dir,
            local_crate,
            check,
            force,
            keep_staging,
            keep,
            replace,
        } => package::run(
            package::PackageSource {
                crate_name: crate_name.as_deref(),
                version: version.as_deref(),
                local_crate: local_crate.as_deref(),
            },
            package_dir.as_deref(),
            check,
            force,
            keep_staging,
            &keep,
            &replace,
        ),
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

        let Command::Package {
            crate_name,
            version,
            package_dir,
            local_crate,
            check,
            force,
            keep_staging,
            ..
        } = cli.command
        else {
            panic!("expected package command");
        };
        assert_eq!(crate_name.as_deref(), Some("serde"));
        assert_eq!(version.as_deref(), Some("1.0.220"));
        assert_eq!(package_dir.as_deref(), Some(Path::new("rust-serde")));
        assert_eq!(local_crate, None);
        assert!(check);
        assert!(force);
        assert!(keep_staging);
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

        let Command::Package {
            crate_name,
            version,
            package_dir,
            local_crate,
            ..
        } = cli.command
        else {
            panic!("expected package command");
        };
        assert_eq!(crate_name, None);
        assert_eq!(version, None);
        assert_eq!(package_dir.as_deref(), Some(Path::new("rust-example")));
        assert_eq!(local_crate.as_deref(), Some(Path::new("../example")));
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

        let Command::Deps {
            crate_name,
            version,
            package_dir,
            series,
            proposed,
            ppa,
            architecture,
        } = cli.command
        else {
            panic!("expected deps command");
        };
        assert_eq!(crate_name.as_deref(), Some("serde"));
        assert_eq!(version.as_deref(), Some("1.0.220"));
        assert_eq!(package_dir, None);
        assert_eq!(series, "noble");
        assert!(proposed);
        assert_eq!(ppa, ["ppa:example/rust-staging"]);
        assert_eq!(architecture.as_deref(), Some("arm64"));
    }

    #[test]
    /// Verifies that the removed import command is no longer accepted.
    fn rejects_import_command() {
        assert!(Cli::try_parse_from(["ubucargo", "import", "serde"]).is_err());
    }
}
