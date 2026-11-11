//! Command-line interface for creating and updating Ubuntu Rust source packages.

mod apt;
mod cargo;
mod changelog;
mod config;
mod crates_io;
mod debcargo;
mod debian_export;
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
    /// Crates.io metadata API base URL.
    #[arg(
        long,
        global = true,
        value_name = "URL",
        default_value = "https://crates.io/api/v1"
    )]
    crates_io_api: String,

    /// Operation to perform.
    #[command(subcommand)]
    command: Command,
}

/// Operations supported by the current Ubucargo release.
#[derive(Subcommand)]
enum Command {
    /// Export maintained packaging into a debcargo-conf repository.
    DebianExport(debian_export::ExportArgs),

    /// Inspect archive candidates for a crate's direct Rust dependencies.
    Deps(deps::DepArgs),

    /// Create or update a Debian source tree and orig tarball.
    Package(package::PackageArgs),
}

/// Parses the command line, runs the selected command, and maps its result to an exit status.
fn main() -> ExitCode {
    let cli = Cli::parse();
    let result = match cli.command {
        Command::DebianExport(args) => debian_export::run(args).map(|()| ExitCode::SUCCESS),
        Command::Deps(args) => deps::run(args, &cli.crates_io_api).map(|unsatisfied| {
            if unsatisfied {
                ExitCode::from(1)
            } else {
                ExitCode::SUCCESS
            }
        }),
        Command::Package(args) => {
            package::run(args, &cli.crates_io_api).map(|()| ExitCode::SUCCESS)
        }
    };

    match result {
        Ok(status) => status,
        Err(error) => {
            eprintln!("error: {error:#}");
            ExitCode::from(2)
        }
    }
}
