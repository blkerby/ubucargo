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
    command::run_command,
    config::{
        PackageConfig, get_package_config_path, get_staged_config_path, has_debcargo_config,
        read_new_local_package_config, read_new_package_config, read_package_config,
        write_staged_config,
    },
    tree::require_absent,
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
                        "{} is not a source-package root with {}",
                        root.display(),
                        get_package_config_path(Path::new("")).display()
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
        let config = read_package_config(root)?;
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

/// Resolves the latest crate release with `debcargo extract` and reads its Cargo identity.
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
        let config_path = get_package_config_path(&destination);
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
        assert_eq!(resolved.upstream, "0.4.1");
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
        assert_eq!(resolved.upstream, "0.4.0");
        // The old changelog identifies the baseline, not the next release's suffix.
        for (contents, expected) in [
            ("", "0.4.2"),
            ("excludes = [\"benches/**\"]\n", "0.4.2+ds"),
            ("repack_suffix = \"custom\"\n", "0.4.2+custom"),
        ] {
            fs::write(&config_path, contents).unwrap();
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
        fs::write(get_package_config_path(root.path()), "").unwrap();
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
        fs::write(get_package_config_path(&occupied), "").unwrap();
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
}
