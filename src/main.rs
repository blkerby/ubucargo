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
