mod materialize;
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
    /// Regenerate packaging while preserving maintainer overrides.
    Package {
        /// Source package directory; defaults to the nearest parent package.
        package: Option<PathBuf>,

        /// Report changes without writing them.
        #[arg(long)]
        check: bool,

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
            package,
            check,
            keep,
            replace,
        } => package::run(package.as_deref(), check, &keep, &replace),
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
