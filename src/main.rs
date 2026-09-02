//! Command-line interface for creating and updating Ubuntu Rust source packages.

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
    /// Create or reconcile a complete source package.
    Package {
        /// Crate name; defaults to the existing package's root Cargo identity.
        #[arg(value_name = "CRATE")]
        crate_name: Option<String>,

        /// Exact crate version; defaults to the latest release when a crate is named.
        #[arg(value_name = "VERSION")]
        version: Option<String>,

        /// Source package directory; defaults to the nearest parent package.
        #[arg(long, value_name = "DIR")]
        directory: Option<PathBuf>,

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
        Command::Package {
            crate_name,
            version,
            directory,
            check,
            force,
            keep_staging,
            keep,
            replace,
        } => package::run(
            crate_name.as_deref(),
            version.as_deref(),
            directory.as_deref(),
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
            "--directory",
            "rust-serde",
            "--check",
            "--force",
            "--keep-staging",
        ])
        .unwrap();

        let Command::Package {
            crate_name,
            version,
            directory,
            check,
            force,
            keep_staging,
            ..
        } = cli.command;
        assert_eq!(crate_name.as_deref(), Some("serde"));
        assert_eq!(version.as_deref(), Some("1.0.220"));
        assert_eq!(directory.as_deref(), Some(Path::new("rust-serde")));
        assert!(check);
        assert!(force);
        assert!(keep_staging);
    }

    #[test]
    /// Verifies that the removed import command is no longer accepted.
    fn rejects_import_command() {
        assert!(Cli::try_parse_from(["ubucargo", "import", "serde"]).is_err());
    }
}
