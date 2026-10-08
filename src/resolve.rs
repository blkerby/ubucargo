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
    cargo::{MetadataPackage, read_root_package},
    changelog::{TopChangelog, read_top_changelog, validate_top_changelog},
    config::{
        PackageConfig, get_new_local_package_config, get_new_package_config,
        get_package_config_path, get_staged_config_path, has_debcargo_config, read_package_config,
        write_staged_config,
    },
    util::{require_absent, resolve_path, run_command},
};

const DEBCARGO_VERSION_REQUIREMENT: &str = "^2.8.4";
/// Exact crate release selected for final generation.
pub struct CrateSelection {
    /// Canonical crate name reported by Cargo.
    pub crate_name: String,
    /// Exact Cargo semver string reported by Cargo.
    pub version: String,
}

/// Resolved source-package destination and whether it already exists.
#[derive(Debug, Eq, PartialEq)]
struct PackageTarget {
    /// Directory containing or intended to contain the Debian source package.
    destination: PathBuf,
    /// Whether the directory is an existing source package.
    existing: bool,
}

/// Existing source and validated quilt state used during generation and update.
pub struct ExistingPackage {
    /// Resolved directory containing the existing source package.
    pub root: PathBuf,
    /// Top changelog entry describing the current upstream source.
    pub top_changelog: TopChangelog,
    /// Whether the working source has refreshed quilt patches applied.
    pub patches_applied: bool,
}

/// Validated inputs for generating one exact crate release.
pub struct ResolvedPackage {
    /// Source-package destination, or none when only inspecting a crate.
    pub destination: Option<PathBuf>,
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

/// Resolves an existing source package (either specified explicitly, or located within
/// the current directory or ancestor), or determine a default destination for a new one.
fn resolve_package_target(
    current_dir: &Path,
    package_dir: Option<&Path>,
    crate_name: Option<&str>,
    local_crate: Option<&Path>,
) -> Result<PackageTarget> {
    let destination = if let Some(package_dir) = package_dir {
        current_dir.join(package_dir)
    } else if let Some(root) = find_parent_package(current_dir) {
        root
    } else {
        let crate_name = crate_name.context("CRATE is required to select a default destination")?;
        // Default source names have no semver suffix until configuration is read.
        current_dir.join(get_crate_source_name(crate_name, None))
    };
    let destination = resolve_path(&destination)?;
    let existing = destination.try_exists()?;
    if existing {
        if !has_debcargo_config(&destination) {
            bail!(
                "{} is not a source-package root with {}",
                destination.display(),
                get_package_config_path(Path::new("")).display()
            );
        }
    } else {
        require_absent(&destination)?;
        if crate_name.is_none() && local_crate.is_none() {
            bail!(
                "CRATE or local: input is required when creating {}",
                destination.display()
            );
        }
    }
    Ok(PackageTarget {
        destination,
        existing,
    })
}

/// Finds the nearest source-package root at or above a directory.
pub fn find_parent_package(start: &Path) -> Option<PathBuf> {
    for candidate in start.ancestors() {
        if has_debcargo_config(candidate) {
            return Some(candidate.to_path_buf());
        }
    }
    None
}

/// Rejects unrefreshed top-patch edits and reports whether any patches are applied.
fn check_patch_state(source: &Path) -> Result<bool> {
    let path = source.join(".pc/applied-patches");
    let contents = match fs::read_to_string(&path) {
        Ok(contents) => contents,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error).with_context(|| format!("read {}", path.display())),
    };
    if !contents.lines().any(|line| !line.trim().is_empty()) {
        return Ok(false);
    }
    let output = run_command(
        Command::new("quilt")
            .args(["diff", "--quiltrc=-", "-z", "--no-timestamps", "--no-index"])
            .env("QUILT_PATCHES", "debian/patches")
            .current_dir(source),
        "quilt diff -z",
    )?;
    if !output.stdout.is_empty() {
        bail!("the current quilt patch has unrefreshed changes; run `quilt refresh`");
    }
    Ok(true)
}

/// Rejects input and destination trees that overlap or contain one another.
pub fn validate_separate_trees(input: &Path, destination: &Path) -> Result<()> {
    if input.starts_with(destination) || destination.starts_with(input) {
        bail!("input and --package-dir must be separate, non-nested directory trees");
    }
    Ok(())
}

/// Resolves a destination, configuration, and exact version to use for a package.
/// `current_dir` is the canonical working directory used for relative paths and
/// parent-package discovery. `None` selects crate inspection without a destination;
/// relative local paths then resolve against the process working directory.
/// - Local sources (explicit `local_crate` or configured `crate_src_path`) use the local source's Cargo.toml version.
/// - Without `local_crate` or a configured `crate_src_path`, the source is
///   crates.io. A requested crate name selects the requested exact version,
///   or the latest release if no version is requested.
/// - If neither a local source nor a crate name is supplied, an existing
///   package is required. Its Cargo.toml supplies the crate name and version
///   to select from crates.io.
pub fn resolve_package(
    current_dir: Option<&Path>,
    package_dir: Option<&Path>,
    requested_name: Option<&str>,
    requested_version: Option<&str>,
    local_crate: Option<&Path>,
) -> Result<ResolvedPackage> {
    if local_crate.is_some() && (requested_name.is_some() || requested_version.is_some()) {
        bail!("CRATE and VERSION may not be used with local: input");
    }
    if let Some(version) = requested_version {
        parse_exact_version(version)?;
    }
    let target = if let Some(current_dir) = current_dir {
        Some(resolve_package_target(
            current_dir,
            package_dir,
            requested_name,
            local_crate,
        )?)
    } else {
        if package_dir.is_some() {
            bail!("package directories require a current directory");
        }
        None
    };
    let debcargo_version = check_debcargo_version()?;

    let existing_root = match &target {
        Some(target) if target.existing => Some(target.destination.as_path()),
        _ => None,
    };
    let (config, current_package, existing) = if let Some(root) = existing_root {
        let debian = root.join("debian");
        let current_package = read_root_package(root)?;
        let current_version = parse_exact_version(&current_package.version)?;
        let current_upstream = cargo_to_debian_upstream_version(&current_version, None);
        let top = read_top_changelog(&debian.join("changelog"))?;
        validate_top_changelog(&top, &current_package.version, &current_upstream)?;
        let config = read_package_config(root, local_crate)?;
        let existing = ExistingPackage {
            root: root.to_path_buf(),
            top_changelog: top,
            patches_applied: check_patch_state(root)?,
        };
        (config, Some(current_package), Some(existing))
    } else if let Some(local_crate) = local_crate {
        let root = target.as_ref().map(|target| target.destination.as_path());
        let local_crate = current_dir.unwrap_or(Path::new("")).join(local_crate);
        let config = get_new_local_package_config(&local_crate, root)?;
        (config, None, None)
    } else {
        (get_new_package_config()?, None, None)
    };

    // The effective configuration selects local input for both new and existing packages.
    let crate_selection = if let Some(local_crate) = &config.resolved_crate_src_path {
        if requested_name.is_some() || requested_version.is_some() {
            bail!("CRATE and VERSION may not be used with crate_src_path");
        }
        if let Some(target) = &target {
            validate_separate_trees(local_crate, &target.destination)?;
        }
        let package = read_root_package(local_crate)?;
        CrateSelection {
            crate_name: package.name,
            version: package.version,
        }
    } else {
        select_registry_release(
            requested_name,
            requested_version,
            current_package.as_ref(),
            &config,
        )?
    };

    if let Some(current) = &current_package
        && normalize_crate_name(&crate_selection.crate_name) != normalize_crate_name(&current.name)
    {
        bail!(
            "selected crate {} does not match existing crate {}",
            crate_selection.crate_name,
            current.name
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
        destination: target.map(|target| target.destination),
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

/// Selects an exact registry release, extracting the crate only to resolve the latest version.
fn select_registry_release(
    requested_name: Option<&str>,
    requested_version: Option<&str>,
    current: Option<&MetadataPackage>,
    config: &PackageConfig,
) -> Result<CrateSelection> {
    match (requested_name, requested_version, current) {
        (None, None, Some(current)) => Ok(CrateSelection {
            crate_name: current.name.clone(),
            version: current.version.clone(),
        }),
        (Some(name), Some(version), _) => Ok(CrateSelection {
            crate_name: name.to_owned(),
            version: version.to_owned(),
        }),
        (Some(name), None, _) => resolve_latest(name, config),
        (None, _, None) => bail!("CRATE is required when creating a package"),
        (None, Some(_), Some(_)) => bail!("VERSION requires CRATE"),
    }
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
fn get_crate_source_name(crate_name: &str, semver_suffix: Option<&Version>) -> String {
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
