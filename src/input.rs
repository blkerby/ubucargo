//! Shared input notation for package generation and dependency inspection.
use crate::config::has_debcargo_config;
use anyhow::{Context, Result, bail};
use std::path::{Path, PathBuf};

/// Input selected independently of a package command's destination.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Input {
    /// A crates.io crate.
    Crate(String),
    /// A published Ubuntu source package in a series or explicit pocket.
    Archive {
        /// Input series or suite, independent of dependency checking overrides.
        suite: String,
        /// Exact Debian source-package name.
        source: String,
    },
    /// A published source package in a public PPA.
    Ppa {
        /// Public archive in ppa:OWNER/NAME notation.
        ppa: String,
        /// Ubuntu series in which the PPA source is published.
        series: String,
        /// Exact Debian source-package name.
        source: String,
    },
    /// An existing maintained source package.
    Package(PathBuf),
    /// Current local Cargo contents, ignoring packaging in the input tree.
    Local(PathBuf),
}

/// Parses explicit inputs and predictable automatic spellings against the working directory.
pub fn parse_input(value: &str, current: &Path) -> Result<Input> {
    if let Some((prefix, rest)) = value.split_once(':') {
        if rest.is_empty() {
            bail!("empty {prefix} input");
        }
        return match prefix {
            "crate" => {
                validate_name("crate", rest)?;
                Ok(Input::Crate(rest.to_owned()))
            }
            "archive" => parse_archive(rest),
            "ppa" => {
                let fields: Vec<_> = rest.split('/').collect();
                let [owner, name, series, source] = fields.as_slice() else {
                    bail!("expected ppa:OWNER/NAME/SERIES/SOURCE");
                };
                for field in &fields {
                    validate_name("PPA input field", field)?;
                }
                Ok(Input::Ppa {
                    ppa: format!("ppa:{owner}/{name}"),
                    series: series.to_string(),
                    source: source.to_string(),
                })
            }
            "pkg" => {
                let path = current
                    .join(rest)
                    .canonicalize()
                    .context("resolve package input")?;
                if !has_debcargo_config(&path) {
                    bail!("{} has no debian/debcargo.toml marker", path.display());
                }
                Ok(Input::Package(path))
            }
            "local" => {
                let path = current
                    .join(rest)
                    .canonicalize()
                    .context("resolve local input")?;
                if !path.join("Cargo.toml").is_file() {
                    bail!("{} has no root Cargo.toml", path.display());
                }
                Ok(Input::Local(path))
            }
            _ => bail!("unknown input prefix {prefix:?}"),
        };
    }
    if value == "."
        || value == ".."
        || value.starts_with("./")
        || value.starts_with("../")
        || value.starts_with('/')
    {
        let path = current
            .join(value)
            .canonicalize()
            .context("resolve directory input")?;
        if has_debcargo_config(&path) {
            return Ok(Input::Package(path));
        }
        bail!(
            "{} has no debian/debcargo.toml marker; use local:{value} to select a local Cargo crate",
            path.display()
        );
    }
    if value.contains('/') {
        return parse_archive(value);
    }
    validate_name("crate", value)?;
    Ok(Input::Crate(value.to_owned()))
}

/// Parses a series or suite and exact source-package name.
fn parse_archive(value: &str) -> Result<Input> {
    let (suite, source) = value.split_once('/').context("expected SUITE/SOURCE")?;
    validate_name("suite", suite)?;
    validate_name("source", source)?;
    Ok(Input::Archive {
        suite: suite.to_owned(),
        source: source.to_owned(),
    })
}

/// Splits an Archive selector's suite into its base series and optional pocket.
pub fn split_archive_suite(suite: &str) -> (&str, Option<&str>) {
    if let Some((series, pocket)) = suite.rsplit_once('-')
        && matches!(pocket, "updates" | "security" | "proposed" | "backports")
    {
        return (series, Some(pocket));
    }
    (suite, None)
}

/// Returns the input's base series when it selects a published source.
pub fn read_input_series(input: &Input) -> Option<&str> {
    match input {
        Input::Archive { suite, .. } => Some(split_archive_suite(suite).0),
        Input::Ppa { series, .. } => Some(series),
        _ => None,
    }
}

/// Rejects empty or unsafe input name fields.
fn validate_name(kind: &str, value: &str) -> Result<()> {
    if value.is_empty()
        || !value
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'-' | b'_' | b'+' | b'.'))
    {
        bail!("invalid {kind} {value:?}");
    }
    Ok(())
}

/// Validates a positional version according to the selected input kind.
pub fn validate_version(input: &Input, version: Option<&str>) -> Result<()> {
    if let Some(version) = version {
        match input {
            Input::Crate(_) => {
                crate::resolve::parse_exact_version(version)?;
            }
            Input::Archive { .. } | Input::Ppa { .. } => {
                version
                    .parse::<debversion::Version>()
                    .context("invalid Debian source version")?;
            }
            _ => bail!("VERSION cannot be used with a local input"),
        }
    }
    Ok(())
}
