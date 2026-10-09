//! Shared input notation for package generation and dependency inspection.
use anyhow::{Context, Result, bail};
use std::{
    fmt,
    path::{Path, PathBuf},
};

/// Distribution publishing an archive source package.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Distribution {
    /// Ubuntu Archive, with release and update pockets.
    Ubuntu,
    /// Debian archive, queried for an exact suite.
    Debian,
}

/// Distribution and suite identifying a source or dependency archive.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Suite {
    /// Distribution providing the repository.
    pub distribution: Distribution,
    /// Exact suite name, including an explicit Ubuntu pocket when selected.
    pub name: String,
}

impl fmt::Display for Suite {
    /// Formats the suite as a distribution-qualified selector.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let prefix = match self.distribution {
            Distribution::Ubuntu => "ubuntu",
            Distribution::Debian => "debian",
        };
        write!(formatter, "{prefix}:{}", self.name)
    }
}

/// Parses a qualified suite or an Ubuntu suite shorthand.
pub fn parse_suite(value: &str) -> Result<Suite> {
    let (prefix, name) = value.split_once(':').unwrap_or(("ubuntu", value));
    let distribution = match prefix {
        "ubuntu" => Distribution::Ubuntu,
        "debian" => Distribution::Debian,
        _ => bail!("unknown suite prefix {prefix:?}; expected ubuntu:SUITE or debian:SUITE"),
    };
    validate_name("suite", name)?;
    Ok(Suite {
        distribution,
        name: name.to_owned(),
    })
}

/// Input selected independently of a package command's destination.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Input {
    /// A crates.io crate.
    Crate(String),
    /// A published distribution source package in a suite.
    Archive {
        /// Input series or suite, independent of dependency checking overrides.
        suite: Suite,
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

/// Reports whether a directory contains the files identifying a Debian source package.
pub fn has_package_files(package_root: &Path) -> bool {
    package_root.join("debian/control").is_file() && package_root.join("debian/changelog").is_file()
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
            "ubuntu" | "debian" => parse_archive(value),
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
                if !has_package_files(&path) {
                    bail!(
                        "{} requires debian/control and debian/changelog to select a package",
                        path.display()
                    );
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
        if has_package_files(&path) {
            return Ok(Input::Package(path));
        }
        bail!(
            "{} requires debian/control and debian/changelog to select a package; use local:{value} to select a local Cargo crate",
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
    let suite = parse_suite(suite)?;
    validate_name("source", source)?;
    Ok(Input::Archive {
        suite,
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

/// Returns the input's checking suite, using the base series for Ubuntu pockets.
pub fn read_input_suite(input: &Input) -> Option<Suite> {
    match input {
        Input::Archive { suite, .. } => {
            let name = match suite.distribution {
                Distribution::Ubuntu => split_archive_suite(&suite.name).0,
                Distribution::Debian => &suite.name,
            };
            Some(Suite {
                distribution: suite.distribution,
                name: name.to_owned(),
            })
        }
        Input::Ppa { series, .. } => Some(Suite {
            distribution: Distribution::Ubuntu,
            name: series.clone(),
        }),
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
