use std::{ffi::OsStr, fs, path::Path, path::PathBuf, process::Command};

use anyhow::{Context, Result, bail};
use semver::{Version, VersionReq};
use serde::Deserialize;
use tempfile::TempDir;
use toml_edit::{DocumentMut, value};

use super::changelog::{TopChangelog, prepare_changelog};
use super::tree::copy_tree;

const DEBCARGO_VERSION_REQUIREMENT: &str = "^2.8.4";

/// Relevant package records returned by `cargo metadata`.
#[derive(Deserialize)]
struct Metadata {
    /// Cargo packages contained in the staged workspace.
    packages: Vec<MetadataPackage>,
}

/// Cargo metadata needed to identify the staged root package.
#[derive(Clone, Debug, Deserialize)]
pub struct MetadataPackage {
    /// Cargo package name passed to debcargo.
    pub name: String,
    /// Exact Cargo package version passed to debcargo.
    pub version: String,
    /// Manifest used to distinguish the root package from workspace members.
    manifest_path: PathBuf,
}

/// Debcargo configuration values that affect package identity.
pub struct PackageConfig {
    /// Complete in-tree configuration text.
    pub contents: String,
    /// Whether the Debian source name includes the crate's semver line.
    pub semver_suffix: bool,
    /// Effective repack suffix, including debcargo's default for exclusions.
    pub repack_suffix: Option<String>,
}

/// Exact crate release selected for final generation.
pub struct CrateSelection {
    /// Canonical crate name reported by Cargo.
    pub crate_name: String,
    /// Exact Cargo semver string reported by Cargo.
    pub version: String,
}

/// Validated files and identities produced by final debcargo generation.
pub struct GeneratedOutput {
    /// Staged source package tree.
    pub source: PathBuf,
    /// Staged Debian orig tarball.
    pub orig: PathBuf,
    /// Debian source package name.
    pub debian_source: String,
}

/// Returns the installed debcargo version when it is compatible.
pub fn check_debcargo_version() -> Result<Version> {
    let output = Command::new("debcargo")
        .arg("--version")
        .output()
        .context("run debcargo --version")?;
    if !output.status.success() {
        bail!("debcargo --version failed");
    }
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

/// Uses Cargo to identify the package defined by the root manifest.
pub fn read_root_package(root: &Path) -> Result<MetadataPackage> {
    let manifest = root.join("Cargo.toml").canonicalize()?;
    let output = Command::new("cargo")
        .args([
            "metadata",
            "--offline",
            "--no-deps",
            "--format-version",
            "1",
            "--manifest-path",
        ])
        .arg(&manifest)
        .output()
        .context("run cargo metadata")?;
    if !output.status.success() {
        bail!(
            "cargo metadata failed:\n{}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    let metadata: Metadata = serde_json::from_slice(&output.stdout)?;
    metadata
        .packages
        .into_iter()
        .find(|package| {
            package
                .manifest_path
                .canonicalize()
                .is_ok_and(|path| path == manifest)
        })
        .with_context(|| format!("{} does not contain a root [package]", manifest.display()))
}

/// Selects an exact release, using preliminary extraction only for latest-version resolution.
pub fn select_release(
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
        (Some(name), Some(version), _) => {
            parse_exact_version(version)?;
            Ok(CrateSelection {
                crate_name: name.to_owned(),
                version: version.to_owned(),
            })
        }
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
pub fn cargo_to_debian_upstream_version(version: &Version, repack_suffix: Option<&str>) -> String {
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

/// Computes debcargo's Debian source name for a crate release.
pub fn get_crate_source_name(crate_name: &str, version: &Version, semver_suffix: bool) -> String {
    let mut source = format!("rust-{}", normalize_crate_name(crate_name));
    if semver_suffix {
        if version.major == 0 {
            source.push_str(&format!("-0.{}", version.minor));
        } else {
            source.push_str(&format!("-{}", version.major));
        }
    }
    source
}

/// Reads and validates the in-tree debcargo configuration.
pub fn read_package_config(path: &Path) -> Result<PackageConfig> {
    let contents = fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    read_package_config_text(&contents).with_context(|| format!("parse {}", path.display()))
}

/// Reads the default configuration used for a new package.
pub fn read_new_package_config() -> Result<PackageConfig> {
    read_package_config_text("[ubucargo]\n")
}

/// Builds a debcargo source tree with a prepared Ubuntu changelog.
pub fn build_debcargo_tree(
    config: &PackageConfig,
    existing_debian: Option<&Path>,
    old_top: Option<&TopChangelog>,
    source_name: &str,
    upstream: &str,
    crate_selection: &CrateSelection,
    debcargo_version: &Version,
) -> Result<TempDir> {
    let stage = tempfile::tempdir().context("create package staging directory")?;
    let overlay = stage.path().join("overlay");
    fs::create_dir(&overlay)?;
    if let Some(debian) = existing_debian {
        prepare_patch_overlay(debian, &overlay)?;
    }
    write_staged_config(config, stage.path())?;
    let provenance = format!(
        "Package {} {} from crates.io using debcargo {} and ubucargo {}.",
        crate_selection.crate_name,
        crate_selection.version,
        debcargo_version,
        env!("CARGO_PKG_VERSION")
    );
    prepare_changelog(
        existing_debian.map(|debian| debian.join("changelog")),
        &overlay.join("changelog"),
        old_top,
        source_name,
        upstream,
        &provenance,
    )?;
    run_debcargo(stage.path(), crate_selection)?;
    Ok(stage)
}

/// Validates staged source identity, Cargo identity, essential packaging, and orig naming.
pub fn validate_debcargo_output(
    stage: &Path,
    expected_source: &str,
    expected_upstream: &str,
    requested_name: &str,
    requested_version: &str,
) -> Result<GeneratedOutput> {
    let source = stage.join("output");
    for path in [
        source.join("Cargo.toml"),
        source.join("debian/changelog"),
        source.join("debian/control"),
        source.join("debian/rules"),
        source.join("debian/source/format"),
    ] {
        if !path.is_file() {
            bail!("debcargo produced no {}", path.display());
        }
    }

    let package = read_root_package(&source)?;
    if normalize_crate_name(requested_name) != normalize_crate_name(&package.name) {
        bail!(
            "debcargo selected crate {} instead of {requested_name}",
            package.name
        );
    }
    if package.version != requested_version {
        bail!(
            "debcargo selected {} {} instead of requested version {requested_version}",
            package.name,
            package.version
        );
    }

    let control = fs::read_to_string(source.join("debian/control"))?;
    let mut debian_source = None;
    for line in control.lines() {
        if let Some(value) = line.strip_prefix("Source:") {
            debian_source = Some(value.trim().to_owned());
            break;
        }
    }
    let debian_source = debian_source.context("generated debian/control has no Source field")?;
    if debian_source != expected_source {
        bail!("debcargo produced Debian source {debian_source}, expected {expected_source}");
    }
    if fs::read(source.join("debian/changelog"))? != fs::read(stage.join("overlay/changelog"))? {
        bail!("debcargo changed the prepared changelog despite --changelog-ready");
    }

    let expected_orig = format!("{expected_source}_{expected_upstream}.orig.tar.gz");
    let mut origs = Vec::new();
    for entry in fs::read_dir(stage)? {
        let path = entry?.path();
        if path
            .file_name()
            .and_then(OsStr::to_str)
            .is_some_and(|name| name.ends_with(".orig.tar.gz"))
        {
            origs.push(path);
        }
    }
    if origs.len() != 1 {
        bail!(
            "debcargo produced {} orig tarballs; expected one",
            origs.len()
        );
    }
    let orig = origs.pop().unwrap();
    if orig.file_name() != Some(OsStr::new(&expected_orig)) {
        bail!(
            "debcargo produced orig {}, expected {expected_orig}",
            orig.display()
        );
    }
    Ok(GeneratedOutput {
        source,
        orig,
        debian_source,
    })
}

/// Resolves the latest crate release with `debcargo extract` and reads its Cargo identity.
fn resolve_latest(crate_name: &str, config: &PackageConfig) -> Result<CrateSelection> {
    let stage = tempfile::tempdir().context("create latest-version staging directory")?;
    fs::create_dir(stage.path().join("overlay"))?;
    write_staged_config(config, stage.path())?;
    let output = Command::new("debcargo")
        .arg("extract")
        .arg("--config")
        .arg(stage.path().join("debcargo.toml"))
        .arg("--directory")
        .arg(stage.path().join("output"))
        .arg(crate_name)
        .current_dir(stage.path())
        .output()
        .context("run debcargo extract")?;
    if !output.status.success() {
        bail!(
            "debcargo extract failed:\n{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
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

/// Reads the package-identity subset of a debcargo configuration.
fn read_package_config_text(contents: &str) -> Result<PackageConfig> {
    let config: DocumentMut = contents.parse().context("parse debcargo configuration")?;
    if let Some(overlay) = config.get("overlay")
        && overlay.as_str() != Some(".")
    {
        bail!("overlay must be omitted or \".\"");
    }
    if config.get("crate_src_path").is_some() {
        bail!("crate_src_path is not supported by ubucargo package");
    }
    if let Some(item) = config.get("ubucargo") {
        let Some(table) = item.as_table() else {
            bail!("ubucargo must be a table");
        };
        if !table.is_empty() {
            bail!("[ubucargo] settings are not implemented yet");
        }
    }
    let semver_suffix = config
        .get("semver_suffix")
        .and_then(|item| item.as_bool())
        .unwrap_or(false);
    let repack_suffix = if let Some(item) = config.get("repack_suffix") {
        Some(
            item.as_str()
                .context("repack_suffix must be a string")?
                .to_owned(),
        )
    } else if config.get("excludes").is_some() {
        Some("ds".to_owned())
    } else {
        None
    };
    Ok(PackageConfig {
        contents: contents.to_owned(),
        semver_suffix,
        repack_suffix,
    })
}

/// Writes registry-backed staged configuration with a temporary overlay.
fn write_staged_config(config: &PackageConfig, stage: &Path) -> Result<()> {
    let mut document: DocumentMut = config.contents.parse()?;
    document.remove("ubucargo");
    document["overlay"] = value(require_utf8_path(&stage.join("overlay"))?);
    fs::write(stage.join("debcargo.toml"), document.to_string())?;
    Ok(())
}

/// Returns a path as UTF-8 for insertion into TOML.
fn require_utf8_path(path: &Path) -> Result<&str> {
    path.to_str()
        .with_context(|| format!("path is not valid UTF-8: {}", path.display()))
}

/// Copies the complete patch set into the debcargo overlay.
fn prepare_patch_overlay(debian: &Path, overlay: &Path) -> Result<()> {
    let patches = debian.join("patches");
    if !patches.is_dir() {
        return Ok(());
    }
    copy_tree(&patches, &overlay.join("patches"))
}

/// Runs final debcargo generation for one exact selected release.
fn run_debcargo(stage: &Path, crate_selection: &CrateSelection) -> Result<()> {
    let output = Command::new("debcargo")
        .arg("package")
        .arg("--config")
        .arg(stage.join("debcargo.toml"))
        .arg("--directory")
        .arg(stage.join("output"))
        .arg("--no-overlay-write-back")
        .arg("--changelog-ready")
        .arg(&crate_selection.crate_name)
        .arg(&crate_selection.version)
        .current_dir(stage)
        .output()
        .context("run debcargo package")?;
    if !output.status.success() {
        bail!(
            "debcargo package failed:\n{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    Ok(())
}

/// Normalizes Cargo crate spelling to debcargo's dashed lowercase form.
fn normalize_crate_name(name: &str) -> String {
    name.replace('_', "-").to_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    /// Verifies exact Cargo-to-Debian upstream conversion.
    fn converts_cargo_versions() {
        assert_eq!(
            cargo_to_debian_upstream_version(&Version::parse("1.2.3-alpha.1+build").unwrap(), None),
            "1.2.3~alpha.1"
        );
        assert_eq!(
            cargo_to_debian_upstream_version(&Version::parse("0.4.0").unwrap(), Some("ds")),
            "0.4.0+ds"
        );
        assert!(parse_exact_version("^1.2").is_err());
    }

    #[test]
    /// Accepts compatible debcargo releases and rejects other major versions.
    fn checks_debcargo_versions() {
        assert_eq!(
            parse_debcargo_version("debcargo 2.8.4").unwrap(),
            Version::new(2, 8, 4)
        );
        assert!(parse_debcargo_version("debcargo 2.9.0").is_ok());
        assert!(parse_debcargo_version("debcargo 2.8.3").is_err());
        assert!(parse_debcargo_version("debcargo 3.0.0").is_err());
        assert!(parse_debcargo_version("2.8.4").is_err());
    }
}
