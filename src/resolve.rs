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
    util::{require_absent, run_command},
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

/// Resolves an existing source package (either specified explicitly, or located within
/// the current directory or ancestor), or determine a default destination for a new one.
fn resolve_package_target(
    current_dir: &Path,
    package_dir: Option<&Path>,
    crate_name: Option<&str>,
    local_crate: Option<&Path>,
) -> Result<PackageTarget> {
    if let Some(package_dir) = package_dir {
        let requested_dir = current_dir.join(package_dir);
        match fs::symlink_metadata(&requested_dir) {
            Ok(metadata) if metadata.is_dir() => {
                let root = requested_dir
                    .canonicalize()
                    .with_context(|| format!("resolve {}", requested_dir.display()))?;
                if !has_debcargo_config(&root) {
                    bail!(
                        "{} is not a source-package root with {}",
                        root.display(),
                        get_package_config_path(Path::new("")).display()
                    );
                }
                return Ok(PackageTarget {
                    destination: root,
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
                    destination: requested_dir,
                    existing: false,
                });
            }
            Err(error) => {
                return Err(error).with_context(|| format!("inspect {}", requested_dir.display()));
            }
        }
    }

    if let Some(root) = find_parent_package(current_dir) {
        return Ok(PackageTarget {
            destination: root,
            existing: true,
        });
    }
    let Some(crate_name) = crate_name else {
        bail!(
            "{} is not inside a source package; CRATE is required to create one",
            current_dir.display()
        );
    };
    // New registry packages use the default configuration, without a semver suffix.
    let destination = current_dir.join(get_crate_source_name(crate_name, None));
    require_absent(&destination)?;
    Ok(PackageTarget {
        destination,
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

/// Resolves a destination, configuration, and exact version to use for a package.
/// `current_dir` is the canonical working directory used for relative paths and
/// parent-package discovery. `None` selects registry inspection without a destination.
/// - Local sources (`local_crate` for new packages or configured `crate_src_path`
///   for existing packages) use the local source's Cargo.toml version.
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
        if package_dir.is_some() || local_crate.is_some() {
            bail!("package directories and local crates require a current directory");
        }
        None
    };
    if local_crate.is_some() && target.as_ref().is_some_and(|target| target.existing) {
        bail!("--local-crate applies only when creating a package");
    }
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
        let config = read_package_config(root)?;
        let existing = ExistingPackage {
            root: root.to_path_buf(),
            top_changelog: top,
            patches_applied: check_patch_state(root)?,
        };
        (config, Some(current_package), Some(existing))
    } else if let Some(local_crate) = local_crate {
        let root = &target
            .as_ref()
            .context("local crate resolution requires a package destination")?
            .destination;
        let local_crate = current_dir
            .context("local crate resolution requires a current directory")?
            .join(local_crate);
        validate_separate_trees(&local_crate, root)?;
        let source = local_crate
            .canonicalize()
            .with_context(|| format!("resolve local crate {}", local_crate.display()))?;
        let config = get_new_local_package_config(&source, root)?;
        (config, None, None)
    } else {
        (get_new_package_config()?, None, None)
    };

    // The effective configuration selects local input for both new and existing packages.
    let crate_selection = if let Some(local_crate) = &config.resolved_crate_src_path {
        if let Some(root) = existing_root {
            if requested_name.is_some() || requested_version.is_some() {
                bail!("CRATE and VERSION may not be used with crate_src_path");
            }
            validate_separate_trees(local_crate, root)?;
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

#[cfg(test)]
mod tests {
    use indoc::{formatdoc, indoc};

    use super::*;

    /// Creates a minimal example crate at the requested version.
    fn create_test_crate(root: &Path, version: &str) {
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join("src/lib.rs"), "").unwrap();
        fs::write(
            root.join("Cargo.toml"),
            formatdoc! {r#"
                [package]
                name = "example"
                version = "{version}"
                edition = "2024"
            "#},
        )
        .unwrap();
    }

    /// Creates existing packaging for an example package with the supplied configuration.
    fn create_test_package(root: &Path, config: &str) {
        create_test_crate(root, "0.4.0");
        fs::create_dir(root.join("debian")).unwrap();
        fs::write(get_package_config_path(root), config).unwrap();
        fs::write(
            root.join("debian/changelog"),
            indoc! {r"
                rust-example (0.4.0+dfsg-1) UNRELEASED; urgency=medium

                  * Initial release.

                 -- Example <example@example.com>  Thu, 17 Sep 2026 12:00:00 +0000
            "},
        )
        .unwrap();
    }

    #[test]
    /// Resolves an exact registry release with or without a package destination.
    fn resolves_registry_package_inputs() {
        let parent = tempfile::tempdir().unwrap();
        for current_dir in [None, Some(parent.path())] {
            let resolved = resolve_package(
                current_dir,
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
                current_dir.map(|path| path.join("rust-example-crate"))
            );
        }
    }

    #[test]
    /// Resolves a new local package without creating its destination.
    fn resolves_new_local_package() {
        let parent = tempfile::tempdir().unwrap();
        let destination = parent.path().join("rust-example");
        let local = parent.path().join("local");
        create_test_crate(&local, "0.4.0");
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
            resolved.config.resolved_crate_src_path.as_deref(),
            Some(local.as_path())
        );
        assert!(
            resolved
                .config
                .original_contents
                .contains("crate_src_path = \"../../local\"")
        );
        assert!(resolved.existing.is_none());
        assert!(!destination.exists());
    }

    #[test]
    /// Resolves relative local inputs against the supplied directory, independently of cwd.
    fn resolves_relative_local_package_paths() {
        let parent = tempfile::tempdir().unwrap();
        let destination = parent.path().join("rust-example");
        let local = parent.path().join("local");
        create_test_crate(&local, "0.4.0");
        let relative = resolve_package(
            Some(parent.path()),
            Some(Path::new("rust-example")),
            None,
            None,
            Some(Path::new("local")),
        )
        .unwrap();
        assert_eq!(relative.destination.as_deref(), Some(destination.as_path()));
        assert_eq!(
            relative.config.resolved_crate_src_path.as_deref(),
            Some(local.as_path())
        );
    }

    #[test]
    /// Rejects --local-crate when the destination already contains a source package.
    fn rejects_local_crate_for_existing_package() {
        let parent = tempfile::tempdir().unwrap();
        let destination = parent.path().join("rust-example");
        create_test_package(&destination, "");
        assert!(
            resolve_package(
                Some(parent.path()),
                Some(&destination),
                None,
                None,
                Some(Path::new("local"))
            )
            .err()
            .unwrap()
            .to_string()
            .contains("--local-crate applies only")
        );
    }

    #[test]
    /// Inspects a registry release without a destination or existing-package context.
    fn inspects_registry_without_package_context() {
        let resolved = resolve_package(None, None, Some("example"), Some("9.0.0"), None).unwrap();
        assert!(resolved.destination.is_none());
        assert!(resolved.existing.is_none());
        assert_eq!(resolved.crate_selection.version, "9.0.0");
    }

    #[test]
    /// Selects the local crate's version while retaining the existing package baseline.
    fn resolves_existing_local_package_version() {
        let parent = tempfile::tempdir().unwrap();
        let destination = parent.path().join("rust-example");
        create_test_crate(&parent.path().join("local"), "0.4.1");
        create_test_package(
            &destination,
            indoc! {r#"
                crate_src_path = "../../local"
            "#},
        );
        let resolved =
            resolve_package(Some(parent.path()), Some(&destination), None, None, None).unwrap();
        assert_eq!(resolved.crate_selection.version, "0.4.1");
        assert_eq!(resolved.upstream, "0.4.1");
        let existing = resolved.existing.as_ref().unwrap();
        assert_eq!(existing.root, destination);
        assert_eq!(existing.top_changelog.upstream, "0.4.0+dfsg");
        assert!(!existing.patches_applied);
    }

    #[test]
    /// Rejects a registry release request for a package configured to use local sources.
    fn rejects_registry_release_for_local_package() {
        let parent = tempfile::tempdir().unwrap();
        let destination = parent.path().join("rust-example");
        create_test_crate(&parent.path().join("local"), "0.4.1");
        create_test_package(
            &destination,
            indoc! {r#"
                crate_src_path = "../../local"
            "#},
        );
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
    }

    #[test]
    /// Defaults an existing registry package to the version in its Cargo manifest.
    fn resolves_existing_registry_package_version() {
        let parent = tempfile::tempdir().unwrap();
        let destination = parent.path().join("rust-example");
        create_test_package(&destination, "");
        let resolved =
            resolve_package(Some(parent.path()), Some(&destination), None, None, None).unwrap();
        assert_eq!(resolved.crate_selection.version, "0.4.0");
        assert_eq!(resolved.upstream, "0.4.0");
    }

    #[test]
    /// Uses the current configuration's repack suffix while retaining the changelog baseline.
    fn resolves_configured_repack_suffix() {
        let parent = tempfile::tempdir().unwrap();
        let destination = parent.path().join("rust-example");
        create_test_package(&destination, "");
        for (contents, expected) in [
            ("", "0.4.2"),
            (
                indoc! {r#"
                    excludes = ["benches/**"]
                "#},
                "0.4.2+ds",
            ),
            (
                indoc! {r#"
                    repack_suffix = "custom"
                "#},
                "0.4.2+custom",
            ),
        ] {
            fs::write(get_package_config_path(&destination), contents).unwrap();
            let resolved = resolve_package(
                Some(parent.path()),
                Some(&destination),
                Some("example"),
                Some("0.4.2"),
                None,
            )
            .unwrap();
            assert_eq!(resolved.upstream, expected);
            assert_eq!(
                resolved.existing.unwrap().top_changelog.upstream,
                "0.4.0+dfsg"
            );
        }
    }

    #[test]
    /// Rejects a different Cargo crate, even if its Debian name matches the old source.
    fn rejects_existing_package_crate_change() {
        let parent = tempfile::tempdir().unwrap();
        let destination = parent.path().join("rust-example");
        create_test_package(&destination, "");
        let changelog_path = destination.join("debian/changelog");
        let changelog = fs::read_to_string(&changelog_path).unwrap();
        fs::write(
            &changelog_path,
            changelog.replace("rust-example", "rust-example-0.4"),
        )
        .unwrap();
        for name in ["different", "example-0.4"] {
            let error = resolve_package(
                Some(parent.path()),
                Some(&destination),
                Some(name),
                Some("0.4.2"),
                None,
            )
            .err()
            .unwrap();
            assert!(
                error
                    .to_string()
                    .contains("does not match existing crate example")
            );
        }
    }

    #[test]
    /// Allows a new source name while retaining the old changelog identity.
    fn resolves_existing_package_source_name_changes() {
        let parent = tempfile::tempdir().unwrap();
        let destination = parent.path().join("rust-example");
        create_test_package(&destination, "semver_suffix = true");
        let resolved =
            resolve_package(Some(parent.path()), Some(&destination), None, None, None).unwrap();
        assert_eq!(resolved.source_name, "rust-example-0.4");
        assert_eq!(
            resolved.existing.unwrap().top_changelog.source,
            "rust-example"
        );
    }

    #[test]
    /// Rejects a changelog baseline that disagrees with the package's Cargo version.
    fn rejects_existing_package_changelog_mismatch() {
        let parent = tempfile::tempdir().unwrap();
        let destination = parent.path().join("rust-example");
        create_test_package(&destination, "");
        let changelog_path = destination.join("debian/changelog");
        let changelog = fs::read_to_string(&changelog_path).unwrap();
        fs::write(&changelog_path, changelog.replace("0.4.0", "9.0.0")).unwrap();
        assert!(
            resolve_package(Some(parent.path()), Some(&destination), None, None, None).is_err()
        );
    }

    #[test]
    /// Verifies explicit nonexistent directories select clean creation even inside a package.
    fn selects_clean_explicit_destination() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir(root.path().join("debian")).unwrap();
        fs::write(get_package_config_path(root.path()), "").unwrap();
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
                destination: root.path().join("new-package"),
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
                destination: root.path().to_path_buf(),
                existing: true,
            }
        );
    }

    #[test]
    /// Finds a parent package or resolves an unoccupied default destination from the crate name.
    fn resolves_implicit_package_target() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir_all(root.path().join("debian")).unwrap();
        fs::write(get_package_config_path(root.path()), "").unwrap();
        let nested = root.path().join("a/b");
        fs::create_dir_all(&nested).unwrap();
        assert_eq!(
            resolve_package_target(&nested, None, None, None).unwrap(),
            PackageTarget {
                destination: root.path().to_path_buf(),
                existing: true,
            }
        );

        let clean = tempfile::tempdir().unwrap();
        assert_eq!(
            resolve_package_target(clean.path(), None, Some("Example_Crate"), None).unwrap(),
            PackageTarget {
                destination: clean.path().join("rust-example-crate"),
                existing: false,
            }
        );
        assert!(resolve_package_target(clean.path(), None, None, None).is_err());

        let occupied = clean.path().join("rust-example-crate");
        fs::create_dir_all(occupied.join("debian")).unwrap();
        fs::write(get_package_config_path(&occupied), "").unwrap();
        // An occupied default destination errors, to avoid unintended overwriting of existing data.
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
}
