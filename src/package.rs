use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};

use crate::materialize::{build_plan, install_state, read_state};

use self::{
    changelog::{read_top_changelog, validate_top_changelog},
    generate::{
        CrateSelection, PackageConfig, cargo_to_debian_version, check_debcargo_version,
        get_crate_source_name, parse_exact_version, read_new_package_config, read_package_config,
        read_root_package, select_release, stage_candidate, validate_output,
    },
    orig::{acquire_old_orig, extract_orig, validate_candidate_orig},
    output::{
        build_patch_series_plan, check_patch_state, collect_managed_paths,
        ensure_destination_absent, generated_patch_changes, initialize_package, install_new_tree,
        read_generated_candidates,
    },
    source::{build_source_plan, scan_tree},
};

mod changelog;
mod generate;
mod orig;
mod output;
mod source;

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

/// Creates or reconciles one source package, returning true when check mode finds changes.
pub fn run(
    crate_name: Option<&str>,
    version: Option<&str>,
    directory: Option<&Path>,
    check: bool,
    force: bool,
    keep: &[PathBuf],
    replace: &[PathBuf],
) -> Result<bool> {
    if version.is_some() && crate_name.is_none() {
        bail!("VERSION requires CRATE");
    }
    if let Some(version) = version {
        parse_exact_version(version)?;
    }

    let current = std::env::current_dir()
        .context("get current directory")?
        .canonicalize()
        .context("resolve current directory")?;
    let mode = select_package_mode(&current, directory, crate_name.is_some())?;
    let (keep_paths, replace_paths) = collect_decisions(keep, replace)?;
    check_debcargo_version()?;
    match mode {
        PackageMode::Existing(root) => reconcile_existing(
            &root,
            crate_name,
            version,
            check,
            force,
            &keep_paths,
            &replace_paths,
        ),
        PackageMode::New {
            parent,
            requested_dir,
        } => {
            if !keep_paths.is_empty() || !replace_paths.is_empty() {
                bail!("--keep and --replace apply only to existing packages");
            }
            let crate_name = crate_name.context("CRATE is required when creating a package")?;
            create_new(
                &parent,
                requested_dir.as_deref(),
                crate_name,
                version,
                check,
            )
        }
    }
}

/// Selects existing-package reconciliation or clean-package creation.
fn select_package_mode(
    start: &Path,
    directory: Option<&Path>,
    has_crate: bool,
) -> Result<PackageMode> {
    if let Some(directory) = directory {
        let requested_dir = if directory.is_absolute() {
            directory.to_path_buf()
        } else {
            start.join(directory)
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
                        "CRATE is required when creating {}",
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

    for candidate in start.ancestors() {
        if has_debcargo_config(candidate) {
            return Ok(PackageMode::Existing(candidate.to_path_buf()));
        }
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

/// Reports whether a directory contains Ubucargo's source-package marker.
fn has_debcargo_config(path: &Path) -> bool {
    path.join("debian/debcargo.toml").is_file()
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
    keep: &BTreeSet<PathBuf>,
    replace: &BTreeSet<PathBuf>,
) -> Result<bool> {
    let debian = root.join("debian");
    let current_package = read_root_package(root)?;
    let top = read_top_changelog(&debian.join("changelog"))?;
    let current_version = parse_exact_version(&current_package.version)?;
    let current_upstream = cargo_to_debian_version(&current_version, None);
    validate_top_changelog(&top, &current_package.version, &current_upstream)?;
    let config = read_package_config(&debian.join("debcargo.toml"))?;
    let crate_selection = select_release(
        requested_name,
        requested_version,
        Some(&current_package),
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
    extract_orig(&old_orig.path, base.path())?;
    let patches_applied = check_patch_state(root)?;

    let stage = stage_candidate(
        &config,
        Some(&debian),
        Some(&top),
        &source_name,
        &upstream,
        &crate_selection,
    )?;
    let output = validate_output(
        stage.path(),
        &source_name,
        &upstream,
        &crate_selection.crate_name,
        &crate_selection.version,
    )?;

    let base_tree = scan_tree(base.path(), false)?;
    let old_tree = scan_tree(root, true)?;
    let new_tree = scan_tree(&output.source, true)?;
    if base_tree != new_tree && patches_applied {
        bail!("pop the complete quilt stack before reconciling changed upstream source");
    }
    let source_plan = build_source_plan(&base_tree, &old_tree, &new_tree, force)?;

    let generated = read_generated_candidates(stage.path())?;
    let managed = collect_managed_paths(&debian, &generated)?;
    let mut generated_plan = build_plan(&debian, &managed, &generated, keep, replace)?;
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
    let orig_changed = validate_candidate_orig(&output.orig, &orig_destination)?;

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
    crate_name: &str,
    requested_version: Option<&str>,
    check: bool,
) -> Result<bool> {
    let config = read_new_package_config()?;
    let crate_selection = select_release(Some(crate_name), requested_version, None, &config)?;
    let (source_name, upstream) = selected_debian_identity(&crate_selection, &config)?;
    let stage = stage_candidate(
        &config,
        None,
        None,
        &source_name,
        &upstream,
        &crate_selection,
    )?;
    let output = validate_output(
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
    ensure_destination_absent(&source)?;
    let orig = parent.join(
        output
            .orig
            .file_name()
            .context("candidate orig has no file name")?,
    );
    let orig_changed = validate_candidate_orig(&output.orig, &orig)?;
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
    install_new_tree(&output.source, &source)?;
    Ok(false)
}

/// Computes the Debian source name and upstream version for a selected release.
fn selected_debian_identity(
    crate_selection: &CrateSelection,
    config: &PackageConfig,
) -> Result<(String, String)> {
    let version = parse_exact_version(&crate_selection.version)?;
    Ok((
        get_crate_source_name(&crate_selection.crate_name, &version, config.semver_suffix),
        cargo_to_debian_version(&version, config.repack_suffix.as_deref()),
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
}
