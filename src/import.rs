//! Imports published Debian source packages without regeneration.

use crate::{
    input::{Input, parse_input, validate_version},
    source::{acquire_package, write_package},
};
use anyhow::{Result, bail};
use std::path::PathBuf;

/// Import a published source package unchanged.
#[derive(clap::Args)]
pub struct ImportArgs {
    /// Published input: archive:SUITE/SOURCE or ppa:OWNER/NAME/SERIES/SOURCE.
    #[arg(value_name = "INPUT")]
    pub input: String,
    /// Exact Debian source version; defaults to the highest published version.
    #[arg(value_name = "VERSION")]
    pub version: Option<String>,
    /// New source-package directory; defaults to SOURCE in the current directory.
    #[arg(long, value_name = "DIR")]
    pub package_dir: Option<PathBuf>,
    /// Retain downloaded source staging, including on failure.
    #[arg(long)]
    pub keep_staging: bool,
}

/// Downloads and writes the selected source without modifying its packaging.
pub fn run(args: ImportArgs) -> Result<()> {
    let input = parse_input(&args.input, &std::env::current_dir()?)?;
    validate_version(&input, args.version.as_deref())?;
    if !matches!(input, Input::Archive { .. } | Input::Ppa { .. }) {
        bail!("expected an Archive or PPA source input");
    }
    let package = acquire_package(
        &input,
        args.version.as_deref(),
        args.package_dir.as_deref(),
        args.keep_staging,
    )?;
    write_package(&package)
}
