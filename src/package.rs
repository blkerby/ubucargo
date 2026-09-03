//! Orchestrates source-package creation and updating.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};

use self::{
    changelog::{read_top_changelog, validate_top_changelog},
    generate::{
        CrateSelection, PackageConfig, build_debcargo_tree, cargo_to_debian_upstream_version,
        check_debcargo_version, get_crate_source_name, parse_exact_version,
        read_new_local_package_config, read_new_package_config, read_package_config,
        read_root_package, remove_generated_vcs_fields, select_release, update_staged_maintainer,
        validate_debcargo_output,
    },
    managed::{build_plan, install_state, read_state},
    orig::acquire_old_orig,
    output::{
        build_patch_series_plan, check_patch_state, collect_managed_paths, generated_patch_changes,
        initialize_package, read_generated_candidates,
    },
    source::{build_source_plan, scan_tree, trees_match},
    tree::{copy_tree, extract_tree, files_differ, require_absent},
};

mod changelog;
mod generate;
mod managed;
mod orig;
mod output;
mod source;
mod tree;

/// Existing-package reconciliation or clean-package creation mode.
#[derive(Debug, Eq, PartialEq)]
enum PackageMode {
    /// An existing Ubucargo source package.
    Existing(PathBuf),
    /// A clean destination that does not yet exist.
    New {
        /// Directory that will contain the source package and orig tarball.
        parent: PathBuf,
        /// Explicit source directory, or none when the Debian source name is used.
        requested_dir: Option<PathBuf>,
    },
}

/// Registry or local crate selection supplied to the package command.
pub struct PackageSource<'a> {
    /// Crate name from crates.io, or none for the current package.
    pub crate_name: Option<&'a str>,
    /// Exact crates.io version paired with an explicit crate name.
    pub version: Option<&'a str>,
    /// Local crate used only when creating a package.
    pub local_crate: Option<&'a Path>,
}

/// Creates or reconciles one source package, returning true when check mode finds changes.
pub fn run(
    source: PackageSource<'_>,
    package_dir: Option<&Path>,
    check: bool,
    force: bool,
    keep_staging: bool,
    keep: &[PathBuf],
    replace: &[PathBuf],
) -> Result<bool> {
    if source.local_crate.is_some() && (source.crate_name.is_some() || source.version.is_some()) {
        bail!("--local-crate may not be combined with CRATE or VERSION");
    }
    if source.local_crate.is_some() && package_dir.is_none() {
        bail!("--local-crate requires --package-dir");
    }
    if source.version.is_some() && source.crate_name.is_none() {
        bail!("VERSION requires CRATE");
    }
    if let Some(version) = source.version {
        parse_exact_version(version)?;
    }

    let current = std::env::current_dir()
        .context("get current directory")?
        .canonicalize()
        .context("resolve current directory")?;
    let mode = select_package_mode(
        &current,
        package_dir,
        source.crate_name.is_some() || source.local_crate.is_some(),
    )?;
    let (keep_paths, replace_paths) = collect_decisions(keep, replace)?;
    match mode {
        PackageMode::Existing(root) => {
            if source.local_crate.is_some() {
                bail!("--local-crate applies only when creating a package");
            }
            reconcile_existing(
                &root,
                source.crate_name,
                source.version,
                check,
                force,
                keep_staging,
                &keep_paths,
                &replace_paths,
            )
        }
        PackageMode::New {
            parent,
            requested_dir,
        } => {
            if !keep_paths.is_empty() || !replace_paths.is_empty() {
                bail!("--keep and --replace apply only to existing packages");
            }
            create_new(
                &parent,
                requested_dir.as_deref(),
                &source,
                check,
                keep_staging,
            )
        }
    }
}

/// Selects existing-package reconciliation or clean-package creation.
fn select_package_mode(
    start: &Path,
    package_dir: Option<&Path>,
    has_crate: bool,
) -> Result<PackageMode> {
    if let Some(package_dir) = package_dir {
        let requested_dir = if package_dir.is_absolute() {
            package_dir.to_path_buf()
        } else {
            start.join(package_dir)
        };
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
                return Ok(PackageMode::Existing(root));
            }
            Ok(_) => bail!("{} is not a directory", requested_dir.display()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                if !has_crate {
                    bail!(
                        "CRATE or --local-crate is required when creating {}",
                        requested_dir.display()
                    );
                }
                let parent = requested_dir
                    .parent()
                    .with_context(|| format!("{} has no parent", requested_dir.display()))?
                    .to_path_buf();
                return Ok(PackageMode::New {
                    parent,
                    requested_dir: Some(requested_dir),
                });
            }
            Err(error) => {
                return Err(error).with_context(|| format!("inspect {}", requested_dir.display()));
            }
        }
    }

    if let Some(root) = find_parent_package(start) {
        return Ok(PackageMode::Existing(root));
    }
    if !has_crate {
        bail!(
            "{} is not inside a source package; CRATE is required to create one",
            start.display()
        );
    }
    Ok(PackageMode::New {
        parent: start.to_path_buf(),
        requested_dir: None,
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

/// Stages one crate package for read-only dependency inspection.
pub(crate) fn stage_for_dependency_inspection(
    crate_name: Option<&str>,
    version: Option<&str>,
    package_dir: Option<&Path>,
) -> Result<tempfile::TempDir> {
    if version.is_some() && crate_name.is_none() {
        bail!("VERSION requires CRATE");
    }
    if crate_name.is_some() && package_dir.is_some() {
        bail!("CRATE and --package-dir may not be combined");
    }
    if let Some(version) = version {
        parse_exact_version(version)?;
    }

    let debcargo_version = check_debcargo_version()?;
    if let Some(crate_name) = crate_name {
        let config = read_new_package_config()?;
        let crate_selection = select_release(Some(crate_name), version, None, &config)?;
        let (source_name, upstream) = selected_debian_identity(&crate_selection, &config)?;
        let stage = build_debcargo_tree(
            &config,
            None,
            None,
            &source_name,
            &upstream,
            &crate_selection,
            &debcargo_version,
            false,
        )?;
        validate_debcargo_output(
            stage.path(),
            &source_name,
            &upstream,
            &crate_selection.crate_name,
            &crate_selection.version,
        )?;
        return Ok(stage);
    }

    let current = std::env::current_dir()
        .context("get current directory")?
        .canonicalize()
        .context("resolve current directory")?;
    let root = if let Some(package_dir) = package_dir {
        let requested = if package_dir.is_absolute() {
            package_dir.to_path_buf()
        } else {
            current.join(package_dir)
        };
        let root = requested
            .canonicalize()
            .with_context(|| format!("resolve {}", requested.display()))?;
        if !has_debcargo_config(&root) {
            bail!(
                "{} is not a source-package root with debian/debcargo.toml",
                root.display()
            );
        }
        root
    } else {
        find_parent_package(&current)
            .with_context(|| format!("{} is not inside a source package", current.display()))?
    };

    let debian = root.join("debian");
    let current_package = read_root_package(&root)?;
    let current_version = parse_exact_version(&current_package.version)?;
    let current_upstream = cargo_to_debian_upstream_version(&current_version, None);
    let top = read_top_changelog(&debian.join("changelog"))?;
    validate_top_changelog(&top, &current_package.version, &current_upstream)?;
    let mut config = read_package_config(&debian.join("debcargo.toml"))?;
    config.preserve_repack_suffix(&current_upstream, &top.upstream);
    check_patch_state(&root)?;
    let crate_selection = select_existing_release(&root, None, None, &current_package, &config)?;
    let (source_name, upstream) = selected_debian_identity(&crate_selection, &config)?;
    if source_name != top.source {
        bail!(
            "selected crate maps to Debian source {source_name}, not existing source {}",
            top.source
        );
    }
    let stage = build_debcargo_tree(
        &config,
        Some(&debian),
        Some(&top),
        &source_name,
        &upstream,
        &crate_selection,
        &debcargo_version,
        false,
    )?;
    validate_debcargo_output(
        stage.path(),
        &source_name,
        &upstream,
        &crate_selection.crate_name,
        &crate_selection.version,
    )?;
    Ok(stage)
}

/// Validates and deduplicates generated-file decisions.
fn collect_decisions(
    keep: &[PathBuf],
    replace: &[PathBuf],
) -> Result<(BTreeSet<PathBuf>, BTreeSet<PathBuf>)> {
    let mut keep_paths = BTreeSet::new();
    for path in keep {
        keep_paths.insert(path.clone());
    }
    let mut replace_paths = BTreeSet::new();
    for path in replace {
        replace_paths.insert(path.clone());
    }
    if let Some(path) = keep_paths.intersection(&replace_paths).next() {
        bail!(
            "{} cannot be named by both --keep and --replace",
            path.display()
        );
    }
    Ok((keep_paths, replace_paths))
}

/// Reconciles an existing package against one exact crate release.
fn reconcile_existing(
    root: &Path,
    requested_name: Option<&str>,
    requested_version: Option<&str>,
    check: bool,
    force: bool,
    keep_staging: bool,
    keep: &BTreeSet<PathBuf>,
    replace: &BTreeSet<PathBuf>,
) -> Result<bool> {
    let debcargo_version = check_debcargo_version()?;
    let debian = root.join("debian");
    let current_package = read_root_package(root)?;
    let top = read_top_changelog(&debian.join("changelog"))?;
    let current_version = parse_exact_version(&current_package.version)?;
    let current_upstream = cargo_to_debian_upstream_version(&current_version, None);
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
    let (source_name, upstream) = selected_debian_identity(&crate_selection, &config)?;
    if source_name != top.source {
        bail!(
            "selected crate maps to Debian source {source_name}, not existing source {}",
            top.source
        );
    }

    let old_orig = acquire_old_orig(root, &top)?;
    let base = tempfile::tempdir().context("create old-source extraction directory")?;
    extract_tree(&old_orig.path, base.path())?;
    let patches_applied = check_patch_state(root)?;

    let stage = build_debcargo_tree(
        &config,
        Some(&debian),
        Some(&top),
        &source_name,
        &upstream,
        &crate_selection,
        &debcargo_version,
        keep_staging,
    )?;
    let raw_control = read_state(&stage.path().join("output/debian/control"))?;
    remove_generated_vcs_fields(stage.path())?;
    update_staged_maintainer(stage.path())?;
    let output = validate_debcargo_output(
        stage.path(),
        &source_name,
        &upstream,
        &crate_selection.crate_name,
        &crate_selection.version,
    )?;

    let base_tree = scan_tree(base.path(), true)?;
    let old_tree = scan_tree(root, true)?;
    let new_tree = scan_tree(&output.source, true)?;
    if !trees_match(&base_tree, &new_tree) && patches_applied {
        bail!("pop the complete quilt stack before reconciling changed upstream source");
    }
    let source_plan = build_source_plan(&base_tree, &old_tree, &new_tree, force)?;

    let generated = read_generated_candidates(stage.path())?;
    let managed = collect_managed_paths(&debian, &generated)?;
    let control = PathBuf::from("debian/control");
    let mut inferred_bases = BTreeMap::new();
    if let Some(raw_control) = raw_control {
        inferred_bases.insert(control, raw_control);
    }
    let mut generated_plan = build_plan(
        &debian,
        &managed,
        &generated,
        &inferred_bases,
        keep,
        replace,
    )?;
    generated_plan
        .paths
        .push(build_patch_series_plan(&debian, stage.path())?);
    let ambiguities = generated_plan.collect_ambiguities();
    if !ambiguities.is_empty() {
        for path in ambiguities {
            println!("ambiguous {} (use --keep or --replace)", path.display());
        }
        bail!("unresolved generated-file ambiguities");
    }

    let prepared_changelog = read_state(&stage.path().join("overlay/changelog"))?
        .context("staged changelog is missing")?;
    let old_changelog = read_state(&debian.join("changelog"))?;
    let changelog_changed = old_changelog.as_ref() != Some(&prepared_changelog);
    let orig_destination = root.parent().context("package root has no parent")?.join(
        output
            .orig
            .file_name()
            .context("candidate orig has no file name")?,
    );
    let orig_changed = files_differ(&output.orig, &orig_destination)?;

    if orig_changed {
        println!("create {}", orig_destination.display());
    }
    source_plan.print_report();
    generated_plan.print_report();
    if changelog_changed {
        println!("update debian/changelog");
    }

    let generated_changed = generated_plan.has_changes();
    if patches_applied && generated_patch_changes(&generated_plan) && !check {
        bail!("pop the real quilt stack before applying generated patch changes");
    }
    let changed =
        orig_changed || source_plan.has_changes() || generated_changed || changelog_changed;
    if check {
        if !changed {
            println!("clean");
        }
        return Ok(changed);
    }
    if !changed {
        println!("clean");
        return Ok(false);
    }

    if orig_changed {
        fs::copy(&output.orig, &orig_destination)
            .with_context(|| format!("install {}", orig_destination.display()))?;
    }
    source_plan
        .apply(root)
        .context("package may be partially updated; rerun `ubucargo package`")?;
    if generated_changed {
        generated_plan.apply()?;
    }
    if changelog_changed {
        install_state(&debian.join("changelog"), Some(&prepared_changelog))?;
    }
    Ok(false)
}

/// Creates a clean source package
fn create_new(
    parent: &Path,
    requested_dir: Option<&Path>,
    source: &PackageSource<'_>,
    check: bool,
    keep_staging: bool,
) -> Result<bool> {
    let debcargo_version = check_debcargo_version()?;
    let (config, crate_selection) = if let Some(local_crate) = source.local_crate {
        let local_crate = if local_crate.is_absolute() {
            local_crate.to_path_buf()
        } else {
            std::env::current_dir()
                .context("get current directory")?
                .join(local_crate)
        };
        let package_root = requested_dir.context("--local-crate requires --package-dir")?;
        validate_separate_trees(&local_crate, package_root)?;
        let source = local_crate
            .canonicalize()
            .with_context(|| format!("resolve local crate {}", local_crate.display()))?;
        let config = read_new_local_package_config(&source, package_root)?;
        let package = read_root_package(&source)?;
        let crate_selection = select_release(None, None, Some(&package), &config)?;
        (config, crate_selection)
    } else {
        let crate_name = source
            .crate_name
            .context("CRATE is required when creating a package")?;
        let config = read_new_package_config()?;
        let crate_selection = select_release(Some(crate_name), source.version, None, &config)?;
        (config, crate_selection)
    };
    let (source_name, upstream) = selected_debian_identity(&crate_selection, &config)?;
    let stage = build_debcargo_tree(
        &config,
        None,
        None,
        &source_name,
        &upstream,
        &crate_selection,
        &debcargo_version,
        keep_staging,
    )?;
    remove_generated_vcs_fields(stage.path())?;
    update_staged_maintainer(stage.path())?;
    let output = validate_debcargo_output(
        stage.path(),
        &source_name,
        &upstream,
        &crate_selection.crate_name,
        &crate_selection.version,
    )?;
    initialize_package(&output.source, &config)?;

    let source = requested_dir
        .map(Path::to_path_buf)
        .unwrap_or_else(|| parent.join(&output.debian_source));
    require_absent(&source)?;
    let orig = parent.join(
        output
            .orig
            .file_name()
            .context("candidate orig has no file name")?,
    );
    let orig_changed = files_differ(&output.orig, &orig)?;
    println!("create {}", source.display());
    if orig_changed {
        println!("create {}", orig.display());
    }
    if check {
        return Ok(true);
    }

    fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    if orig_changed {
        fs::copy(&output.orig, &orig).with_context(|| format!("install {}", orig.display()))?;
    }
    copy_tree(&output.source, &source)?;
    Ok(false)
}

/// Rejects local crate and source-package trees that overlap or contain one another.
fn validate_separate_trees(local_crate: &Path, package_root: &Path) -> Result<()> {
    if local_crate.starts_with(package_root) || package_root.starts_with(local_crate) {
        bail!("--local-crate and --package-dir must be separate, non-nested directory trees");
    }
    Ok(())
}

/// Selects registry or configured local input for an existing source package.
fn select_existing_release(
    root: &Path,
    requested_name: Option<&str>,
    requested_version: Option<&str>,
    current_package: &generate::MetadataPackage,
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

/// Computes the Debian source name and upstream version for a selected release.
fn selected_debian_identity(
    crate_selection: &CrateSelection,
    config: &PackageConfig,
) -> Result<(String, String)> {
    let version = parse_exact_version(&crate_selection.version)?;
    Ok((
        get_crate_source_name(&crate_selection.crate_name, &version, config.semver_suffix),
        cargo_to_debian_upstream_version(&version, config.repack_suffix.as_deref()),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    /// Verifies explicit nonexistent directories select clean creation even inside a package.
    fn selects_clean_explicit_destination() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir(root.path().join("debian")).unwrap();
        fs::write(root.path().join("debian/debcargo.toml"), "").unwrap();
        let mode = select_package_mode(root.path(), Some(Path::new("new-package")), true).unwrap();
        assert_eq!(
            mode,
            PackageMode::New {
                parent: root.path().to_path_buf(),
                requested_dir: Some(root.path().join("new-package")),
            }
        );
    }

    #[test]
    /// Verifies implicit mode finds a parent package or creates in the start directory.
    fn selects_implicit_target_mode() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir_all(root.path().join("debian")).unwrap();
        fs::write(root.path().join("debian/debcargo.toml"), "").unwrap();
        let nested = root.path().join("a/b");
        fs::create_dir_all(&nested).unwrap();
        assert_eq!(
            select_package_mode(&nested, None, false).unwrap(),
            PackageMode::Existing(root.path().to_path_buf())
        );

        let clean = tempfile::tempdir().unwrap();
        assert_eq!(
            select_package_mode(clean.path(), None, true).unwrap(),
            PackageMode::New {
                parent: clean.path().to_path_buf(),
                requested_dir: None,
            }
        );
        assert!(select_package_mode(clean.path(), None, false).is_err());
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
    /// Rejects mixing explicit registry and source-package dependency targets.
    fn rejects_ambiguous_dependency_target() {
        assert!(
            stage_for_dependency_inspection(Some("serde"), None, Some(Path::new("rust-serde")))
                .is_err()
        );
    }
}
