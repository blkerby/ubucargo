//! Resolves and validates crate releases and configuration for staged generation.
//! Shared by the package and deps commands.

use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::{Context, Result, bail};
use semver::{Version, VersionReq};

use crate::{
    cargo::read_root_package,
    changelog::{TopChangelog, read_top_changelog, validate_top_changelog},
    config::{
        PackageConfig, get_package_config_path, get_staged_config_path, has_debcargo_config,
        write_staged_config,
    },
    input::has_package_files,
    util::run_command,
};

const DEBCARGO_VERSION_REQUIREMENT: &str = "^2.8.4";
/// Exact crate release selected for final generation.
#[derive(Clone)]
pub struct CrateSelection {
    /// Canonical crate name reported by Cargo.
    pub crate_name: String,
    /// Exact Cargo semver string reported by Cargo.
    pub version: String,
}

/// Upstream selection requested independently of the package destination.
pub enum CrateRequest<'a> {
    /// A crates.io crate and optional exact Cargo version.
    Registry {
        name: &'a str,
        version: Option<&'a str>,
    },
    /// An explicitly selected local Cargo tree.
    Local(&'a Path),
    /// The maintained package's current release or configured local source.
    Current,
}

/// Existing source and changelog used during generation and update.
pub struct ExistingPackage {
    /// Resolved directory containing the existing source package.
    pub root: PathBuf,
    /// Top changelog entry describing the current upstream source.
    pub top_changelog: TopChangelog,
    /// Current Cargo identity, captured before staging or changing quilt state.
    pub crate_selection: CrateSelection,
}

/// Validated inputs for generating one exact crate release.
pub struct ResolvedPackage {
    /// Effective debcargo configuration.
    pub config: PackageConfig,
    /// Exact crate release selected for regeneration.
    pub crate_selection: CrateSelection,
    /// Debian source name for the selected crate release.
    pub source_name: String,
    /// Debian upstream version for the selected crate release.
    pub upstream: String,
    /// Compatible debcargo version checked before any release resolution.
    pub debcargo_version: semver::Version,
    /// Existing packaging to preserve, or none for a fresh package.
    pub existing: Option<ExistingPackage>,
}

/// Reads the maintained Cargo and changelog identities required for regeneration.
pub fn read_existing_package(root: &Path) -> Result<ExistingPackage> {
    if !has_debcargo_config(root) {
        bail!(
            "package generation requires {} in {}",
            get_package_config_path(Path::new("")).display(),
            root.display()
        );
    }
    let current = read_root_package(root)?;
    let version = parse_exact_version(&current.version)?;
    let upstream = cargo_to_debian_upstream_version(&version, None);
    let top_changelog = read_top_changelog(&root.join("debian/changelog"))?;
    validate_top_changelog(&top_changelog, &current.version, &upstream)?;
    Ok(ExistingPackage {
        root: root.to_path_buf(),
        top_changelog,
        crate_selection: CrateSelection {
            crate_name: current.name,
            version: current.version,
        },
    })
}

/// Finds the nearest source-package root at or above a directory.
pub fn find_parent_package(start: &Path) -> Option<PathBuf> {
    for candidate in start.ancestors() {
        if has_package_files(candidate) {
            return Some(candidate.to_path_buf());
        }
    }
    None
}

/// Rejects input and destination trees that overlap or contain one another.
pub fn validate_separate_trees(input: &Path, destination: &Path) -> Result<()> {
    if input.starts_with(destination) || destination.starts_with(input) {
        bail!("input and --package-dir must be separate, non-nested directory trees");
    }
    Ok(())
}

/// Selects an exact upstream release and derives its Debian identity for generation.
/// Configuration paths are resolved against the maintained input before this call.
pub fn resolve_package(
    request: CrateRequest<'_>,
    config: PackageConfig,
    existing: Option<ExistingPackage>,
) -> Result<ResolvedPackage> {
    let debcargo_version = check_debcargo_version()?;
    let request = match (request, config.resolved_crate_src_path.as_deref()) {
        (CrateRequest::Current, Some(local)) => CrateRequest::Local(local),
        (request, _) => request,
    };
    let crate_selection = match request {
        CrateRequest::Registry { name, version } => {
            if config.resolved_crate_src_path.is_some() {
                bail!("CRATE and VERSION may not be used with crate_src_path");
            }
            if let Some(version) = version {
                CrateSelection {
                    crate_name: name.to_owned(),
                    version: version.to_owned(),
                }
            } else {
                resolve_latest(name, &config)?
            }
        }
        CrateRequest::Local(local) => {
            let current = read_root_package(local)?;
            CrateSelection {
                crate_name: current.name,
                version: current.version,
            }
        }
        CrateRequest::Current => existing
            .as_ref()
            .context("current release requires a maintained package")?
            .crate_selection
            .clone(),
    };

    if let Some(current) = &existing
        && normalize_crate_name(&crate_selection.crate_name)
            != normalize_crate_name(&current.crate_selection.crate_name)
    {
        bail!(
            "selected crate {} does not match existing crate {}",
            crate_selection.crate_name,
            current.crate_selection.crate_name
        );
    }

    let version = parse_exact_version(&crate_selection.version)?;
    let source_name = get_crate_source_name(
        &crate_selection.crate_name,
        if config.semver_suffix {
            Some(&version)
        } else {
            None
        },
    );
    let upstream =
        cargo_to_debian_upstream_version(&version, config.effective_repack_suffix.as_deref());
    Ok(ResolvedPackage {
        config,
        crate_selection,
        source_name,
        upstream,
        debcargo_version,
        existing,
    })
}

/// Normalizes Cargo crate spelling to Debian's dashed lowercase form.
pub fn normalize_crate_name(name: &str) -> String {
    name.replace('_', "-").to_lowercase()
}

/// Returns the installed debcargo version when it is compatible.
fn check_debcargo_version() -> Result<Version> {
    let output = run_command(
        Command::new("debcargo").arg("--version"),
        "debcargo --version",
    )?;
    parse_debcargo_version(String::from_utf8_lossy(&output.stdout).trim())
}

/// Parses and checks one `debcargo --version` response.
fn parse_debcargo_version(output: &str) -> Result<Version> {
    let version = output
        .strip_prefix("debcargo ")
        .with_context(|| format!("unrecognized debcargo version output {output:?}"))?;
    let version = Version::parse(version)
        .with_context(|| format!("unrecognized debcargo version output {output:?}"))?;
    let requirement = VersionReq::parse(DEBCARGO_VERSION_REQUIREMENT).unwrap();
    if !requirement.matches(&version) {
        bail!("unsupported debcargo {version}; this release requires {requirement}");
    }
    Ok(version)
}

/// Parses an exact Cargo semantic version and rejects requirement syntax.
pub fn parse_exact_version(version: &str) -> Result<Version> {
    Version::parse(version).with_context(|| format!("{version:?} is not an exact Cargo version"))
}

/// Converts Cargo semver to debcargo's Debian upstream-version syntax.
fn cargo_to_debian_upstream_version(version: &Version, repack_suffix: Option<&str>) -> String {
    let mut converted = format!("{}.{}.{}", version.major, version.minor, version.patch);
    if !version.pre.is_empty() {
        converted.push('~');
        converted.push_str(version.pre.as_str());
    }
    if let Some(repack_suffix) = repack_suffix {
        converted.push('+');
        converted.push_str(repack_suffix);
    }
    converted
}

/// Computes debcargo's Debian source name, optionally suffixed by a release's semver line.
pub fn get_crate_source_name(crate_name: &str, semver_suffix: Option<&Version>) -> String {
    let mut source = format!("rust-{}", normalize_crate_name(crate_name));
    if let Some(version) = semver_suffix {
        if version.major == 0 {
            source.push_str(&format!("-0.{}", version.minor));
        } else {
            source.push_str(&format!("-{}", version.major));
        }
    }
    source
}

/// Resolves the latest crate release using debcargo's version-selection logic.
/// This is done by using `debcargo extract`. This technically does more than needed
/// at this stage (it actually retrieves the crate), but it avoids us needing to
/// duplicate the version-selection logic.
fn resolve_latest(crate_name: &str, config: &PackageConfig) -> Result<CrateSelection> {
    let stage = tempfile::tempdir().context("create latest-version staging directory")?;
    fs::create_dir(stage.path().join("overlay"))?;
    write_staged_config(config, stage.path())?;
    run_command(
        Command::new("debcargo")
            .arg("extract")
            .arg("--config")
            .arg(get_staged_config_path(stage.path()))
            .arg("--directory")
            .arg(stage.path().join("output"))
            .arg(crate_name)
            .current_dir(stage.path()),
        "debcargo extract",
    )?;
    let package = read_root_package(&stage.path().join("output"))?;
    if normalize_crate_name(crate_name) != normalize_crate_name(&package.name) {
        bail!(
            "debcargo selected crate {} instead of {crate_name}",
            package.name
        );
    }
    Ok(CrateSelection {
        crate_name: package.name,
        version: package.version,
    })
}
