//! Resolves and validates crate releases and configuration for staged generation.
//! Shared by the package and deps commands.

use std::{
    fs,
    path::{Component, Path, PathBuf},
    process::Command,
};

use anyhow::{Context, Result, bail};
use semver::{Version, VersionReq};
use toml_edit::{DocumentMut, value};

use crate::{
    cargo::{MetadataPackage, read_root_package},
    changelog::{TopChangelog, read_top_changelog, validate_top_changelog},
    command::run_command,
    tree::require_absent,
};

const DEBCARGO_VERSION_REQUIREMENT: &str = "^2.8.4";
const UBUNTU_MAINTAINER: &str = "Ubuntu Developers <ubuntu-devel-discuss@lists.ubuntu.com>";

/// Debcargo configuration values that affect package identity.
pub struct PackageConfig {
    /// Complete in-tree configuration text.
    pub contents: String,
    /// Whether the Debian source name includes the crate's semver line.
    pub semver_suffix: bool,
    /// Effective repack suffix, including debcargo's default for exclusions.
    pub repack_suffix: Option<String>,
    /// Resolved local crate source, or none for crates.io.
    pub crate_src_path: Option<PathBuf>,
    /// Whether the repack suffix was explicitly configured by the maintainer.
    repack_suffix_explicit: bool,
}

impl PackageConfig {
    /// Preserves an existing package's repack suffix when none is configured explicitly.
    pub fn preserve_repack_suffix(&mut self, cargo_upstream: &str, existing_upstream: &str) {
        if self.repack_suffix_explicit {
            return;
        }
        if let Some(suffix) = existing_upstream
            .strip_prefix(cargo_upstream)
            .and_then(|suffix| suffix.strip_prefix('+'))
            .filter(|suffix| !suffix.is_empty())
        {
            self.repack_suffix = Some(suffix.to_owned());
        }
    }
}

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
    source: PathBuf,
    /// Whether the directory is an existing source package.
    existing: bool,
}

/// Existing source and validated quilt state used during generation and reconciliation.
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
    /// Source-package destination, or none when only inspecting a registry crate.
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

/// Resolves an existing source package or an absent destination for a new one.
fn resolve_package_target(
    start: &Path,
    package_dir: Option<&Path>,
    crate_name: Option<&str>,
    local_crate: Option<&Path>,
) -> Result<PackageTarget> {
    if let Some(package_dir) = package_dir {
        let requested_dir = start.join(package_dir);
        match fs::symlink_metadata(&requested_dir) {
            Ok(metadata) if metadata.is_dir() => {
                let root = requested_dir
                    .canonicalize()
                    .with_context(|| format!("resolve {}", requested_dir.display()))?;
                if !has_debcargo_config(&root) {
                    bail!(
                        "{} is not a source-package root with debian/debcargo.toml",
                        root.display()
                    );
                }
                return Ok(PackageTarget {
                    source: root,
                    existing: true,
                });
            }
            Ok(_) => bail!("{} is not a directory", requested_dir.display()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                if crate_name.is_none() && local_crate.is_none() {
                    bail!(
                        "CRATE or --local-crate is required when creating {}",
                        requested_dir.display()
                    );
                }
                return Ok(PackageTarget {
                    source: requested_dir,
                    existing: false,
                });
            }
            Err(error) => {
                return Err(error).with_context(|| format!("inspect {}", requested_dir.display()));
            }
        }
    }

    if let Some(root) = find_parent_package(start) {
        return Ok(PackageTarget {
            source: root,
            existing: true,
        });
    }
    let Some(crate_name) = crate_name else {
        bail!(
            "{} is not inside a source package; CRATE is required to create one",
            start.display()
        );
    };
    // New registry packages use the default configuration, without a semver suffix.
    let source = start.join(get_crate_source_name(crate_name, None));
    require_absent(&source)?;
    Ok(PackageTarget {
        source,
        existing: false,
    })
}

/// Finds the nearest source-package root at or above a directory.
fn find_parent_package(start: &Path) -> Option<PathBuf> {
    for candidate in start.ancestors() {
        if has_debcargo_config(candidate) {
            return Some(candidate.to_path_buf());
        }
    }
    None
}

/// Reports whether a directory contains Ubucargo's source-package marker.
fn has_debcargo_config(path: &Path) -> bool {
    path.join("debian/debcargo.toml").is_file()
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

/// Rejects local crate and source-package trees that overlap or contain one another.
fn validate_separate_trees(local_crate: &Path, package_root: &Path) -> Result<()> {
    if local_crate.starts_with(package_root) || package_root.starts_with(local_crate) {
        bail!("--local-crate and --package-dir must be separate, non-nested directory trees");
    }
    Ok(())
}

/// Resolves a destination, configuration, and exact release for staged generation.
/// A starting directory enables package discovery and destination selection.
/// Omit it to inspect a registry crate independently of the working directory.
/// Selecting the latest release runs `debcargo extract` and may download the crate.
pub fn resolve_package(
    start: Option<&Path>,
    package_dir: Option<&Path>,
    requested_name: Option<&str>,
    requested_version: Option<&str>,
    local_crate: Option<&Path>,
) -> Result<ResolvedPackage> {
    if let Some(version) = requested_version {
        parse_exact_version(version)?;
    }
    let target = if let Some(start) = start {
        Some(resolve_package_target(
            start,
            package_dir,
            requested_name,
            local_crate,
        )?)
    } else {
        if package_dir.is_some() || local_crate.is_some() {
            bail!("package directories and local crates require a starting directory");
        }
        None
    };
    if local_crate.is_some() && target.as_ref().is_some_and(|target| target.existing) {
        bail!("--local-crate applies only when creating a package");
    }
    let debcargo_version = check_debcargo_version()?;
    let existing_root = match &target {
        Some(target) if target.existing => Some(target.source.as_path()),
        _ => None,
    };
    let (config, crate_selection, existing) = if let Some(root) = existing_root {
        let debian = root.join("debian");
        let current_package = read_root_package(root)?;
        let current_version = parse_exact_version(&current_package.version)?;
        let current_upstream = cargo_to_debian_upstream_version(&current_version, None);
        let top = read_top_changelog(&debian.join("changelog"))?;
        validate_top_changelog(&top, &current_package.version, &current_upstream)?;
        let mut config = read_package_config(&debian.join("debcargo.toml"))?;
        config.preserve_repack_suffix(&current_upstream, &top.upstream);
        let crate_selection = select_existing_release(
            root,
            requested_name,
            requested_version,
            &current_package,
            &config,
        )?;
        let existing = ExistingPackage {
            root: root.to_path_buf(),
            top_changelog: top,
            patches_applied: check_patch_state(root)?,
        };
        (config, crate_selection, Some(existing))
    } else if let Some(local_crate) = local_crate {
        let root = &target
            .as_ref()
            .context("local crate resolution requires a package destination")?
            .source;
        let local_crate = if local_crate.is_absolute() {
            local_crate.to_path_buf()
        } else {
            std::env::current_dir()
                .context("get current directory")?
                .join(local_crate)
        };
        validate_separate_trees(&local_crate, root)?;
        let source = local_crate
            .canonicalize()
            .with_context(|| format!("resolve local crate {}", local_crate.display()))?;
        let config = read_new_local_package_config(&source, root)?;
        let package = read_root_package(&source)?;
        let crate_selection = select_release(None, None, Some(&package), &config)?;
        (config, crate_selection, None)
    } else {
        let config = read_new_package_config()?;
        let crate_selection = select_release(requested_name, requested_version, None, &config)?;
        (config, crate_selection, None)
    };
    let version = parse_exact_version(&crate_selection.version)?;
    let source_name = get_crate_source_name(
        &crate_selection.crate_name,
        if config.semver_suffix {
            Some(&version)
        } else {
            None
        },
    );
    let upstream = cargo_to_debian_upstream_version(&version, config.repack_suffix.as_deref());
    if let Some(existing) = &existing
        && source_name != existing.top_changelog.source
    {
        bail!(
            "selected crate maps to Debian source {source_name}, not existing source {}",
            existing.top_changelog.source
        );
    }
    Ok(ResolvedPackage {
        destination: target.map(|target| target.source),
        config,
        crate_selection,
        source_name,
        upstream,
        debcargo_version,
        existing,
    })
}

/// Selects registry or configured local input for an existing source package.
fn select_existing_release(
    root: &Path,
    requested_name: Option<&str>,
    requested_version: Option<&str>,
    current_package: &MetadataPackage,
    config: &PackageConfig,
) -> Result<CrateSelection> {
    let Some(local_crate) = &config.crate_src_path else {
        return select_release(
            requested_name,
            requested_version,
            Some(current_package),
            config,
        );
    };
    if requested_name.is_some() || requested_version.is_some() {
        bail!("CRATE and VERSION may not be used with crate_src_path");
    }
    validate_separate_trees(local_crate, root)?;
    let local_package = read_root_package(local_crate)?;
    select_release(None, None, Some(&local_package), config)
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

/// Selects an exact release, using preliminary extraction only for latest-version resolution.
fn select_release(
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

/// Reads and validates the in-tree debcargo configuration.
fn read_package_config(path: &Path) -> Result<PackageConfig> {
    let contents = fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    let mut config =
        read_package_config_text(&contents).with_context(|| format!("parse {}", path.display()))?;
    if let Some(crate_src_path) = &config.crate_src_path {
        let config_dir = path
            .parent()
            .context("debcargo.toml has no parent directory")?;
        config.crate_src_path = Some(
            config_dir
                .join(crate_src_path)
                .canonicalize()
                .with_context(|| format!("resolve crate_src_path from {}", path.display()))?,
        );
    }
    Ok(config)
}

/// Creates the persisted Ubuntu configuration used for a new package.
pub fn read_new_package_config() -> Result<PackageConfig> {
    let mut document = DocumentMut::new();
    document["maintainer"] = value(UBUNTU_MAINTAINER);
    read_package_config_text(&document.to_string())
}

/// Creates the persisted configuration for a new package built from a local crate.
fn read_new_local_package_config(crate_root: &Path, package_root: &Path) -> Result<PackageConfig> {
    let relative = make_relative_path(crate_root, &package_root.join("debian"))?;
    let mut config = read_new_package_config()?;
    let mut document: DocumentMut = config.contents.parse()?;
    document["crate_src_path"] = value(require_utf8_path(&relative)?);
    config.contents = document.to_string();
    config.crate_src_path = Some(crate_root.to_path_buf());
    Ok(config)
}

/// Resolves the latest crate release with `debcargo extract` and reads its Cargo identity.
fn resolve_latest(crate_name: &str, config: &PackageConfig) -> Result<CrateSelection> {
    let stage = tempfile::tempdir().context("create latest-version staging directory")?;
    fs::create_dir(stage.path().join("overlay"))?;
    write_staged_config(config, stage.path())?;
    run_command(
        Command::new("debcargo")
            .arg("extract")
            .arg("--config")
            .arg(stage.path().join("debcargo.toml"))
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

/// Reads the package-identity subset of a debcargo configuration.
fn read_package_config_text(contents: &str) -> Result<PackageConfig> {
    let config: DocumentMut = contents.parse().context("parse debcargo configuration")?;
    if let Some(overlay) = config.get("overlay")
        && overlay.as_str() != Some(".")
    {
        bail!("overlay must be omitted or \".\"");
    }
    let crate_src_path = config
        .get("crate_src_path")
        .map(|item| {
            item.as_str()
                .context("crate_src_path must be a string")
                .map(PathBuf::from)
        })
        .transpose()?;
    let semver_suffix = config
        .get("semver_suffix")
        .and_then(|item| item.as_bool())
        .unwrap_or(false);
    let repack_suffix_explicit = config.get("repack_suffix").is_some();
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
        crate_src_path,
        repack_suffix_explicit,
    })
}

/// Writes staged configuration with resolved local source and temporary overlay paths.
pub fn write_staged_config(config: &PackageConfig, stage: &Path) -> Result<()> {
    let mut document: DocumentMut = config.contents.parse()?;
    if let Some(repack_suffix) = &config.repack_suffix {
        document["repack_suffix"] = value(repack_suffix);
    }
    if let Some(crate_src_path) = &config.crate_src_path {
        document["crate_src_path"] = value(require_utf8_path(crate_src_path)?);
    }
    document["overlay"] = value(require_utf8_path(&stage.join("overlay"))?);
    fs::write(stage.join("debcargo.toml"), document.to_string())?;
    Ok(())
}

/// Returns a path as UTF-8 for insertion into TOML.
fn require_utf8_path(path: &Path) -> Result<&str> {
    path.to_str()
        .with_context(|| format!("path is not valid UTF-8: {}", path.display()))
}

/// Expresses one absolute path relative to another absolute directory.
fn make_relative_path(path: &Path, base: &Path) -> Result<PathBuf> {
    let path_components: Vec<Component<'_>> = path.components().collect();
    let base_components: Vec<Component<'_>> = base.components().collect();
    let common = path_components
        .iter()
        .zip(&base_components)
        .take_while(|(path, base)| path == base)
        .count();
    if common == 0 {
        bail!(
            "{} and {} have no common filesystem root",
            path.display(),
            base.display()
        );
    }
    let mut relative = PathBuf::new();
    for _ in common..base_components.len() {
        relative.push("..");
    }
    for component in &path_components[common..] {
        relative.push(component.as_os_str());
    }
    Ok(relative)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    /// Resolves registry, local, and existing inputs while preserving existing package identity.
    fn resolves_package_inputs() {
        let parent = tempfile::tempdir().unwrap();
        let destination = parent.path().join("rust-example");
        for start in [None, Some(parent.path())] {
            let resolved = resolve_package(
                start,
                None,
                Some("Example_Crate"),
                Some("1.2.3-alpha.1"),
                None,
            )
            .unwrap();
            assert_eq!(resolved.source_name, "rust-example-crate");
            assert_eq!(resolved.upstream, "1.2.3~alpha.1");
            assert_eq!(resolved.crate_selection.version, "1.2.3-alpha.1");
            assert!(resolved.existing.is_none());
            assert_eq!(
                resolved.destination,
                start.map(|path| path.join("rust-example-crate"))
            );
        }
        assert!(!destination.exists());

        let local = parent.path().join("local");
        fs::create_dir_all(local.join("src")).unwrap();
        fs::write(local.join("src/lib.rs"), "").unwrap();
        let manifest = "[package]\nname = \"example\"\nversion = \"0.4.0\"\nedition = \"2024\"\n";
        fs::write(local.join("Cargo.toml"), manifest).unwrap();
        let resolved = resolve_package(
            Some(parent.path()),
            Some(&destination),
            None,
            None,
            Some(&local),
        )
        .unwrap();
        assert_eq!(resolved.destination.as_deref(), Some(destination.as_path()));
        assert_eq!(resolved.source_name, "rust-example");
        assert_eq!(resolved.upstream, "0.4.0");
        assert_eq!(
            resolved.config.crate_src_path.as_deref(),
            Some(local.as_path())
        );
        assert!(
            resolved
                .config
                .contents
                .contains("crate_src_path = \"../../local\"")
        );
        assert!(resolved.existing.is_none());
        assert!(!destination.exists());

        fs::create_dir_all(destination.join("debian")).unwrap();
        fs::create_dir(destination.join("src")).unwrap();
        fs::write(destination.join("src/lib.rs"), "").unwrap();
        fs::write(destination.join("Cargo.toml"), manifest).unwrap();
        let config_path = destination.join("debian/debcargo.toml");
        fs::write(&config_path, &resolved.config.contents).unwrap();
        let changelog = "rust-example (0.4.0+dfsg-1) UNRELEASED; urgency=medium\n\n  * Initial release.\n\n -- Example <example@example.com>  Thu, 17 Sep 2026 12:00:00 +0000\n";
        fs::write(destination.join("debian/changelog"), changelog).unwrap();

        assert!(
            resolve_package(
                Some(parent.path()),
                Some(&destination),
                None,
                None,
                Some(&local)
            )
            .err()
            .unwrap()
            .to_string()
            .contains("--local-crate applies only")
        );
        let nested = destination.join("nested");
        fs::create_dir(&nested).unwrap();
        let resolved = resolve_package(Some(&nested), None, None, None, None).unwrap();
        assert_eq!(resolved.destination.as_deref(), Some(destination.as_path()));
        assert_eq!(resolved.existing.unwrap().root, destination);
        // Registry inspection needs no package destination or existing-package context.
        let resolved = resolve_package(None, None, Some("example"), Some("9.0.0"), None).unwrap();
        assert!(resolved.destination.is_none());
        assert!(resolved.existing.is_none());
        assert_eq!(resolved.crate_selection.version, "9.0.0");

        fs::write(local.join("Cargo.toml"), manifest.replace("0.4.0", "0.4.1")).unwrap();
        let resolved =
            resolve_package(Some(parent.path()), Some(&destination), None, None, None).unwrap();
        assert_eq!(resolved.crate_selection.version, "0.4.1");
        assert_eq!(resolved.upstream, "0.4.1+dfsg");
        let existing = resolved.existing.as_ref().unwrap();
        assert_eq!(existing.root, destination);
        assert_eq!(existing.top_changelog.upstream, "0.4.0+dfsg");
        assert!(!existing.patches_applied);
        assert!(
            resolve_package(
                Some(parent.path()),
                Some(&destination),
                Some("example"),
                Some("0.4.2"),
                None
            )
            .is_err()
        );

        fs::write(&config_path, "").unwrap();
        let resolved =
            resolve_package(Some(parent.path()), Some(&destination), None, None, None).unwrap();
        assert_eq!(resolved.crate_selection.version, "0.4.0");
        let resolved = resolve_package(
            Some(parent.path()),
            Some(&destination),
            Some("example"),
            Some("0.4.2"),
            None,
        )
        .unwrap();
        assert_eq!(resolved.upstream, "0.4.2+dfsg");
        assert!(
            resolve_package(
                Some(parent.path()),
                Some(&destination),
                Some("different"),
                Some("0.4.2"),
                None
            )
            .is_err()
        );
        fs::write(&config_path, "semver_suffix = true\n").unwrap();
        assert!(
            resolve_package(Some(parent.path()), Some(&destination), None, None, None).is_err()
        );
        fs::write(&config_path, "").unwrap();
        fs::write(
            destination.join("debian/changelog"),
            changelog.replace("0.4.0", "9.0.0"),
        )
        .unwrap();
        assert!(
            resolve_package(Some(parent.path()), Some(&destination), None, None, None).is_err()
        );
    }

    #[test]
    /// Verifies explicit nonexistent directories select clean creation even inside a package.
    fn selects_clean_explicit_destination() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir(root.path().join("debian")).unwrap();
        fs::write(root.path().join("debian/debcargo.toml"), "").unwrap();
        let target = resolve_package_target(
            root.path(),
            Some(Path::new("new-package")),
            Some("example"),
            None,
        )
        .unwrap();
        assert_eq!(
            target,
            PackageTarget {
                source: root.path().join("new-package"),
                existing: false,
            }
        );
        assert_eq!(
            resolve_package_target(
                root.path(),
                Some(Path::new("new-package")),
                None,
                Some(Path::new("../crate")),
            )
            .unwrap(),
            target
        );
        assert_eq!(
            resolve_package_target(root.path(), Some(root.path()), None, None).unwrap(),
            PackageTarget {
                source: root.path().to_path_buf(),
                existing: true,
            }
        );
    }

    #[test]
    /// Finds a parent package or resolves an unoccupied default destination from the crate name.
    fn resolves_implicit_package_target() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir_all(root.path().join("debian")).unwrap();
        fs::write(root.path().join("debian/debcargo.toml"), "").unwrap();
        let nested = root.path().join("a/b");
        fs::create_dir_all(&nested).unwrap();
        assert_eq!(
            resolve_package_target(&nested, None, None, None).unwrap(),
            PackageTarget {
                source: root.path().to_path_buf(),
                existing: true,
            }
        );

        let clean = tempfile::tempdir().unwrap();
        assert_eq!(
            resolve_package_target(clean.path(), None, Some("Example_Crate"), None).unwrap(),
            PackageTarget {
                source: clean.path().join("rust-example-crate"),
                existing: false,
            }
        );
        assert!(resolve_package_target(clean.path(), None, None, None).is_err());

        let occupied = clean.path().join("rust-example-crate");
        fs::create_dir_all(occupied.join("debian")).unwrap();
        fs::write(occupied.join("debian/debcargo.toml"), "").unwrap();
        // An occupied default destination must not silently turn creation into reconciliation.
        assert!(resolve_package_target(clean.path(), None, Some("Example_Crate"), None).is_err());
    }

    #[test]
    /// Rejects invalid explicit targets, including symlinks and creation without a crate.
    fn rejects_invalid_package_targets() {
        let root = tempfile::tempdir().unwrap();
        let plain = root.path().join("plain");
        fs::create_dir(&plain).unwrap();
        let file = root.path().join("file");
        fs::write(&file, "").unwrap();
        let link = root.path().join("link");
        std::os::unix::fs::symlink(root.path().join("absent"), &link).unwrap();
        for path in [&plain, &file, &link] {
            assert!(
                resolve_package_target(root.path(), Some(path), Some("example"), None).is_err()
            );
        }
        assert!(
            resolve_package_target(root.path(), Some(Path::new("absent")), None, None).is_err()
        );
    }

    #[test]
    /// Rejects either nesting direction for local crate and package trees.
    fn rejects_nested_local_package_trees() {
        let root = tempfile::tempdir().unwrap();
        let crate_root = root.path().join("crate");
        let package_root = root.path().join("package");
        assert!(validate_separate_trees(&crate_root, &package_root).is_ok());
        assert!(validate_separate_trees(&crate_root, &crate_root).is_err());
        assert!(validate_separate_trees(&crate_root, &crate_root.join("package")).is_err());
        assert!(validate_separate_trees(&package_root.join("crate"), &package_root).is_err());
    }

    #[test]
    /// Preserves normalized source names with and without a semver suffix.
    fn computes_source_names() {
        assert_eq!(
            get_crate_source_name("Example_Crate", None),
            "rust-example-crate"
        );
        assert_eq!(
            get_crate_source_name("example", Some(&Version::new(0, 4, 1))),
            "rust-example-0.4"
        );
        assert_eq!(
            get_crate_source_name("example", Some(&Version::new(2, 3, 1))),
            "rust-example-2"
        );
    }

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

    #[test]
    /// Preserves the new-package maintainer through configuration reload and staging.
    fn preserves_new_package_maintainer() {
        let stage = tempfile::tempdir().unwrap();
        let crate_root = stage.path().join("crate");
        let package_root = stage.path().join("output");
        fs::create_dir(&crate_root).unwrap();
        fs::create_dir_all(package_root.join("debian")).unwrap();
        for config in [
            read_new_package_config().unwrap(),
            read_new_local_package_config(&crate_root, &package_root).unwrap(),
        ] {
            write_staged_config(&config, stage.path()).unwrap();
            let initial = fs::read_to_string(stage.path().join("debcargo.toml")).unwrap();
            fs::write(package_root.join("debian/debcargo.toml"), &config.contents).unwrap();
            let reloaded = read_package_config(&package_root.join("debian/debcargo.toml")).unwrap();
            let document: DocumentMut = reloaded.contents.parse().unwrap();
            assert_eq!(document["maintainer"].as_str(), Some(UBUNTU_MAINTAINER));
            assert!(!document.contains_key("overlay"));

            write_staged_config(&reloaded, stage.path()).unwrap();
            assert_eq!(
                fs::read_to_string(stage.path().join("debcargo.toml")).unwrap(),
                initial
            );
        }
    }

    #[test]
    /// Leaves existing maintainer settings and debcargo's implicit default unchanged.
    fn preserves_existing_maintainer_config() {
        let stage = tempfile::tempdir().unwrap();
        let path = stage.path().join("existing.toml");
        for contents in [
            "# Use debcargo's default maintainer.\n",
            "maintainer = \"Debian Rust Maintainers <pkg-rust-maintainers@alioth-lists.debian.net>\"\n",
            "maintainer = \"Example Developer <example@ubuntu.com>\"\n",
        ] {
            fs::write(&path, contents).unwrap();
            let config = read_package_config(&path).unwrap();
            write_staged_config(&config, stage.path()).unwrap();
            let mut staged: DocumentMut = fs::read_to_string(stage.path().join("debcargo.toml"))
                .unwrap()
                .parse()
                .unwrap();
            staged.remove("overlay");
            assert_eq!(staged.to_string(), contents);
            assert_eq!(fs::read_to_string(&path).unwrap(), contents);
        }
    }

    #[test]
    /// Persists a relative local source while staging its resolved absolute path.
    fn configures_local_crate_source() {
        let parent = tempfile::tempdir().unwrap();
        let crate_root = parent.path().join("example");
        let package_root = parent.path().join("rust-example");
        fs::create_dir(&crate_root).unwrap();
        let config = read_new_local_package_config(&crate_root, &package_root).unwrap();
        assert!(
            config
                .contents
                .contains("crate_src_path = \"../../example\"")
        );

        fs::create_dir_all(package_root.join("debian")).unwrap();
        fs::write(package_root.join("debian/debcargo.toml"), &config.contents).unwrap();
        assert_eq!(
            read_package_config(&package_root.join("debian/debcargo.toml"))
                .unwrap()
                .crate_src_path
                .as_deref(),
            Some(crate_root.as_path())
        );

        let stage = tempfile::tempdir().unwrap();
        write_staged_config(&config, stage.path()).unwrap();
        let staged: DocumentMut = fs::read_to_string(stage.path().join("debcargo.toml"))
            .unwrap()
            .parse()
            .unwrap();
        assert_eq!(staged["crate_src_path"].as_str(), crate_root.to_str());
    }

    #[test]
    /// Preserves an existing implicit suffix while respecting an explicit suffix.
    fn selects_repack_suffix() {
        let mut inferred = read_package_config_text("excludes = [\"benches/**\"]").unwrap();
        inferred.preserve_repack_suffix("1.0.0", "1.0.0+dfsg");
        assert_eq!(inferred.repack_suffix.as_deref(), Some("dfsg"));

        let stage = tempfile::tempdir().unwrap();
        write_staged_config(&inferred, stage.path()).unwrap();
        let staged: DocumentMut = fs::read_to_string(stage.path().join("debcargo.toml"))
            .unwrap()
            .parse()
            .unwrap();
        assert_eq!(staged["repack_suffix"].as_str(), Some("dfsg"));

        let mut explicit =
            read_package_config_text("excludes = [\"benches/**\"]\nrepack_suffix = \"custom\"")
                .unwrap();
        explicit.preserve_repack_suffix("1.0.0", "1.0.0+dfsg");
        assert_eq!(explicit.repack_suffix.as_deref(), Some("custom"));
    }
}
