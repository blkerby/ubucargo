//! Command-line interface for creating and updating Ubuntu Rust source packages.

mod apt;
mod cargo;
mod changelog;
mod config;
mod deps;
mod generate;
mod input;
mod package;
mod resolve;
mod source;
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
    /// Inspect archive candidates for a crate's direct Rust dependencies.
    Deps(deps::DepArgs),

    /// Create or update a complete source package.
    Package(package::PackageArgs),
}

/// Parses the command line, runs the selected command, and maps its result to an exit status.
fn main() -> ExitCode {
    let result = match Cli::parse().command {
        Command::Deps(args) => deps::run(args).map(|unsatisfied| {
            if unsatisfied {
                ExitCode::from(1)
            } else {
                ExitCode::SUCCESS
            }
        }),
        Command::Package(args) => package::run(args).map(|()| ExitCode::SUCCESS),
    };

    match result {
        Ok(status) => status,
        Err(error) => {
            eprintln!("error: {error:#}");
            ExitCode::from(2)
        }
    }
}
