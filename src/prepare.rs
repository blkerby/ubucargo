//! Prepares crate releases and generates staged Debian source packages.
//! This is used in both the `package` and `deps` commands.

use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::{Context, Result, bail};

use crate::{
    cargo::{MetadataPackage, read_root_package},
    command::run_command,
    tree::require_absent,
};

use self::{
    changelog::{read_top_changelog, validate_top_changelog},
    generate::{
        CrateSelection, cargo_to_debian_upstream_version, check_debcargo_version,
        get_crate_source_name, read_new_local_package_config, read_new_package_config,
        read_package_config, select_release,
    },
};

mod changelog;
pub mod generate;

pub use changelog::TopChangelog;
pub use generate::{GeneratedPackage, PackageConfig, generate_package, parse_exact_version};

/// Resolved source-package destination and whether it already exists.
#[derive(Debug, Eq, PartialEq)]
pub struct PackageTarget {
    /// Directory containing or intended to contain the Debian source package.
    pub source: PathBuf,
    /// Whether the directory is an existing source package.
    pub existing: bool,
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
pub struct PreparedPackage {
    /// Effective debcargo configuration.
    pub config: PackageConfig,
    /// Exact crate release selected for regeneration.
    crate_selection: CrateSelection,
    /// Debian source name for the selected crate release.
    source_name: String,
    /// Debian upstream version for the selected crate release.
    upstream: String,
    /// Compatible debcargo version checked before any release resolution.
    debcargo_version: semver::Version,
    /// Existing packaging to preserve, or none for a fresh package.
    pub existing: Option<ExistingPackage>,
}

/// Resolves an existing source package or an absent destination for a new one.
pub fn resolve_package_target(
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

/// Resolves configuration and an exact release for staged generation.
/// The target provides existing-package context or a destination for a local crate;
/// it may be omitted if the crate will only be inspected rather than packaged.
/// Selecting the latest release runs `debcargo extract` and may download the crate.
pub fn prepare_package(
    target: Option<&PackageTarget>,
    requested_name: Option<&str>,
    requested_version: Option<&str>,
    local_crate: Option<&Path>,
) -> Result<PreparedPackage> {
    if let Some(version) = requested_version {
        parse_exact_version(version)?;
    }
    let debcargo_version = check_debcargo_version()?;
    let existing_root = match target {
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
            .context("local crate preparation requires a package destination")?
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
    Ok(PreparedPackage {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    /// Prepares registry, local, and existing inputs while preserving existing package identity.
    fn prepares_package_inputs() {
        let parent = tempfile::tempdir().unwrap();
        let mut target = PackageTarget {
            source: parent.path().join("rust-example"),
            existing: false,
        };
        for destination in [None, Some(&target)] {
            let prepared = prepare_package(
                destination,
                Some("Example_Crate"),
                Some("1.2.3-alpha.1"),
                None,
            )
            .unwrap();
            assert_eq!(prepared.source_name, "rust-example-crate");
            assert_eq!(prepared.upstream, "1.2.3~alpha.1");
            assert_eq!(prepared.crate_selection.version, "1.2.3-alpha.1");
            assert!(prepared.existing.is_none());
        }
        assert!(!target.source.exists());

        let local = parent.path().join("local");
        fs::create_dir_all(local.join("src")).unwrap();
        fs::write(local.join("src/lib.rs"), "").unwrap();
        let manifest = "[package]\nname = \"example\"\nversion = \"0.4.0\"\nedition = \"2024\"\n";
        fs::write(local.join("Cargo.toml"), manifest).unwrap();
        let prepared = prepare_package(Some(&target), None, None, Some(&local)).unwrap();
        assert_eq!(prepared.source_name, "rust-example");
        assert_eq!(prepared.upstream, "0.4.0");
        assert_eq!(
            prepared.config.crate_src_path.as_deref(),
            Some(local.as_path())
        );
        assert!(
            prepared
                .config
                .contents
                .contains("crate_src_path = \"../../local\"")
        );
        assert!(prepared.existing.is_none());
        assert!(!target.source.exists());

        fs::create_dir_all(target.source.join("debian")).unwrap();
        fs::create_dir(target.source.join("src")).unwrap();
        fs::write(target.source.join("src/lib.rs"), "").unwrap();
        fs::write(target.source.join("Cargo.toml"), manifest).unwrap();
        let config_path = target.source.join("debian/debcargo.toml");
        fs::write(&config_path, &prepared.config.contents).unwrap();
        let changelog = "rust-example (0.4.0+dfsg-1) UNRELEASED; urgency=medium\n\n  * Initial release.\n\n -- Example <example@example.com>  Thu, 17 Sep 2026 12:00:00 +0000\n";
        fs::write(target.source.join("debian/changelog"), changelog).unwrap();
        target.existing = true;

        fs::write(local.join("Cargo.toml"), manifest.replace("0.4.0", "0.4.1")).unwrap();
        let prepared = prepare_package(Some(&target), None, None, None).unwrap();
        assert_eq!(prepared.crate_selection.version, "0.4.1");
        assert_eq!(prepared.upstream, "0.4.1+dfsg");
        let existing = prepared.existing.as_ref().unwrap();
        assert_eq!(existing.root, target.source);
        assert_eq!(existing.top_changelog.upstream, "0.4.0+dfsg");
        assert!(!existing.patches_applied);
        assert!(prepare_package(Some(&target), Some("example"), Some("0.4.2"), None).is_err());

        fs::write(&config_path, "").unwrap();
        let prepared = prepare_package(Some(&target), None, None, None).unwrap();
        assert_eq!(prepared.crate_selection.version, "0.4.0");
        let prepared =
            prepare_package(Some(&target), Some("example"), Some("0.4.2"), None).unwrap();
        assert_eq!(prepared.upstream, "0.4.2+dfsg");
        assert!(prepare_package(Some(&target), Some("different"), Some("0.4.2"), None).is_err());
        fs::write(&config_path, "semver_suffix = true\n").unwrap();
        assert!(prepare_package(Some(&target), None, None, None).is_err());
        fs::write(&config_path, "").unwrap();
        fs::write(
            target.source.join("debian/changelog"),
            changelog.replace("0.4.0", "9.0.0"),
        )
        .unwrap();
        assert!(prepare_package(Some(&target), None, None, None).is_err());
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
}
