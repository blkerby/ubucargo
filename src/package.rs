//! Orchestrates source-package creation and updating.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};

use crate::cargo::{MetadataPackage, read_root_package};

use self::{
    changelog::{TopChangelog, read_top_changelog, validate_top_changelog},
    generate::{
        CrateSelection, PackageConfig, cargo_to_debian_upstream_version, check_debcargo_version,
        generate_debcargo_package, get_crate_source_name, parse_exact_version,
        read_new_local_package_config, read_new_package_config, read_package_config,
        remove_generated_vcs_fields, select_release, update_staged_maintainer,
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

/// Create or reconcile a complete source package.
#[derive(clap::Args)]
pub struct PackageArgs {
    /// Crate name; defaults to the existing package's root Cargo identity.
    #[arg(value_name = "CRATE")]
    pub crate_name: Option<String>,

    /// Exact crate version; defaults to the latest release when a crate is named.
    #[arg(value_name = "VERSION", requires = "crate_name")]
    pub version: Option<String>,

    /// Debian source-package directory; defaults to the nearest parent package.
    #[arg(long = "package-dir", value_name = "DIR")]
    pub package_dir: Option<PathBuf>,

    /// Local crate used to create a new source package.
    #[arg(long, value_name = "DIR", conflicts_with_all = ["crate_name", "version"], requires = "package_dir")]
    pub local_crate: Option<PathBuf>,

    /// Report changes without writing them.
    #[arg(long)]
    pub check: bool,

    /// Resolve source-tree conflicts in favor of the selected crate release.
    #[arg(long)]
    pub force: bool,

    /// Retain the temporary debcargo staging directory for inspection.
    #[arg(long)]
    pub keep_staging: bool,

    /// Keep an ambiguous primary when baselines are missing or conflict.
    #[arg(long, value_name = "PATH")]
    pub keep: Vec<PathBuf>,

    /// Resolve an ambiguous primary by taking the generated state.
    #[arg(long, value_name = "PATH")]
    pub replace: Vec<PathBuf>,
}

/// Resolved source-package destination and whether it already exists.
#[derive(Debug, Eq, PartialEq)]
struct PackageTarget {
    /// Directory containing or intended to contain the Debian source package.
    source: PathBuf,
    /// Whether the directory is an existing source package.
    existing: bool,
}

/// Validated configuration and release selection for an existing package.
struct ExistingPackage {
    /// Top changelog entry describing the current upstream source.
    top_changelog: TopChangelog,
    /// Effective debcargo configuration.
    config: PackageConfig,
    /// Exact crate release selected for regeneration.
    crate_selection: CrateSelection,
    /// Debian upstream version for the selected crate release.
    upstream: String,
}

/// Creates or reconciles one source package, returning true when check mode finds changes.
pub fn run(args: PackageArgs) -> Result<bool> {
    if let Some(version) = args.version.as_deref() {
        parse_exact_version(version)?;
    }

    let current = std::env::current_dir()
        .context("get current directory")?
        .canonicalize()
        .context("resolve current directory")?;
    let target = resolve_package_target(
        &current,
        args.package_dir.as_deref(),
        args.crate_name.as_deref(),
        args.local_crate.as_deref(),
    )?;
    let (keep_paths, replace_paths) = collect_decisions(&args.keep, &args.replace)?;
    if target.existing {
        if args.local_crate.is_some() {
            bail!("--local-crate applies only when creating a package");
        }
        reconcile_existing(
            &target.source,
            args.crate_name.as_deref(),
            args.version.as_deref(),
            args.check,
            args.force,
            args.keep_staging,
            &keep_paths,
            &replace_paths,
        )
    } else {
        if !keep_paths.is_empty() || !replace_paths.is_empty() {
            bail!("--keep and --replace apply only to existing packages");
        }
        create_new(&target.source, &args)
    }
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

/// Generates a validated source package in the returned temporary directory's `output` tree.
pub fn stage_package(
    crate_name: Option<&str>,
    version: Option<&str>,
    package_dir: Option<&Path>,
) -> Result<tempfile::TempDir> {
    if let Some(version) = version {
        parse_exact_version(version)?;
    }

    let debcargo_version = check_debcargo_version()?;
    if let Some(crate_name) = crate_name {
        let config = read_new_package_config()?;
        let crate_selection = select_release(Some(crate_name), version, None, &config)?;
        let (source_name, upstream) = selected_debian_identity(&crate_selection, &config)?;
        let generated = generate_debcargo_package(
            &config,
            None,
            None,
            &source_name,
            &upstream,
            &crate_selection,
            &debcargo_version,
            false,
        )?;
        return Ok(generated.stage);
    }

    let current = std::env::current_dir()
        .context("get current directory")?
        .canonicalize()
        .context("resolve current directory")?;
    let target = resolve_package_target(&current, package_dir, None, None)?;
    if !target.existing {
        bail!("staging without CRATE requires an existing source package");
    }
    let root = target.source;

    let debian = root.join("debian");
    let package = prepare_existing_package(&root, None, None)?;
    check_patch_state(&root)?;
    let generated = generate_debcargo_package(
        &package.config,
        Some(&debian),
        Some(&package.top_changelog),
        &package.top_changelog.source,
        &package.upstream,
        &package.crate_selection,
        &debcargo_version,
        false,
    )?;
    Ok(generated.stage)
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
    let package = prepare_existing_package(root, requested_name, requested_version)?;

    let old_orig = acquire_old_orig(root, &package.top_changelog)?;
    let base = tempfile::tempdir().context("create old-source extraction directory")?;
    extract_tree(&old_orig.path, base.path())?;
    let patches_applied = check_patch_state(root)?;

    let generated = generate_debcargo_package(
        &package.config,
        Some(&debian),
        Some(&package.top_changelog),
        &package.top_changelog.source,
        &package.upstream,
        &package.crate_selection,
        &debcargo_version,
        keep_staging,
    )?;
    let raw_control = read_state(&generated.stage.path().join("output/debian/control"))?;
    remove_generated_vcs_fields(generated.stage.path())?;
    update_staged_maintainer(generated.stage.path())?;

    let base_tree = scan_tree(base.path())?;
    let old_tree = scan_tree(root)?;
    let new_tree = scan_tree(&generated.source)?;
    if !trees_match(&base_tree, &new_tree) && patches_applied {
        bail!("pop the complete quilt stack before reconciling changed upstream source");
    }
    let source_plan = build_source_plan(&base_tree, &old_tree, &new_tree, force)?;

    let generated_candidates = read_generated_candidates(generated.stage.path())?;
    let managed = collect_managed_paths(&debian, &generated_candidates)?;
    let control = PathBuf::from("debian/control");
    let mut inferred_bases = BTreeMap::new();
    if let Some(raw_control) = raw_control {
        inferred_bases.insert(control, raw_control);
    }
    let mut generated_plan = build_plan(
        &debian,
        &managed,
        &generated_candidates,
        &inferred_bases,
        keep,
        replace,
    )?;
    generated_plan
        .paths
        .push(build_patch_series_plan(&debian, generated.stage.path())?);
    let ambiguities = generated_plan.collect_ambiguities();
    if !ambiguities.is_empty() {
        for path in ambiguities {
            println!("ambiguous {} (use --keep or --replace)", path.display());
        }
        bail!("unresolved generated-file ambiguities");
    }

    let prepared_changelog = read_state(&generated.stage.path().join("overlay/changelog"))?
        .context("staged changelog is missing")?;
    let old_changelog = read_state(&debian.join("changelog"))?;
    let changelog_changed = old_changelog.as_ref() != Some(&prepared_changelog);
    let orig_destination = root.parent().context("package root has no parent")?.join(
        generated
            .orig
            .file_name()
            .context("candidate orig has no file name")?,
    );
    let orig_changed = files_differ(&generated.orig, &orig_destination)?;

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
        fs::copy(&generated.orig, &orig_destination)
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

/// Creates a clean source package at the resolved destination.
fn create_new(root: &Path, args: &PackageArgs) -> Result<bool> {
    let parent = root.parent().context("package root has no parent")?;
    let debcargo_version = check_debcargo_version()?;
    let (config, crate_selection) = if let Some(local_crate) = args.local_crate.as_deref() {
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
        (config, crate_selection)
    } else {
        let crate_name = args
            .crate_name
            .as_deref()
            .context("CRATE is required when creating a package")?;
        let config = read_new_package_config()?;
        let crate_selection =
            select_release(Some(crate_name), args.version.as_deref(), None, &config)?;
        (config, crate_selection)
    };
    let (source_name, upstream) = selected_debian_identity(&crate_selection, &config)?;
    let generated = generate_debcargo_package(
        &config,
        None,
        None,
        &source_name,
        &upstream,
        &crate_selection,
        &debcargo_version,
        args.keep_staging,
    )?;
    remove_generated_vcs_fields(generated.stage.path())?;
    update_staged_maintainer(generated.stage.path())?;
    initialize_package(&generated.source, &config)?;

    require_absent(root)?;
    let orig = parent.join(
        generated
            .orig
            .file_name()
            .context("candidate orig has no file name")?,
    );
    let orig_changed = files_differ(&generated.orig, &orig)?;
    println!("create {}", root.display());
    if orig_changed {
        println!("create {}", orig.display());
    }
    if args.check {
        return Ok(true);
    }

    fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    if orig_changed {
        fs::copy(&generated.orig, &orig).with_context(|| format!("install {}", orig.display()))?;
    }
    copy_tree(&generated.source, root)?;
    Ok(false)
}

/// Rejects local crate and source-package trees that overlap or contain one another.
fn validate_separate_trees(local_crate: &Path, package_root: &Path) -> Result<()> {
    if local_crate.starts_with(package_root) || package_root.starts_with(local_crate) {
        bail!("--local-crate and --package-dir must be separate, non-nested directory trees");
    }
    Ok(())
}

/// Reads and validates the generation inputs for an existing package.
fn prepare_existing_package(
    root: &Path,
    requested_name: Option<&str>,
    requested_version: Option<&str>,
) -> Result<ExistingPackage> {
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
    let (source_name, upstream) = selected_debian_identity(&crate_selection, &config)?;
    if source_name != top.source {
        bail!(
            "selected crate maps to Debian source {source_name}, not existing source {}",
            top.source
        );
    }
    Ok(ExistingPackage {
        top_changelog: top,
        config,
        crate_selection,
        upstream,
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

/// Computes the Debian source name and upstream version for a selected release.
fn selected_debian_identity(
    crate_selection: &CrateSelection,
    config: &PackageConfig,
) -> Result<(String, String)> {
    let version = parse_exact_version(&crate_selection.version)?;
    Ok((
        get_crate_source_name(
            &crate_selection.crate_name,
            if config.semver_suffix {
                Some(&version)
            } else {
                None
            },
        ),
        cargo_to_debian_upstream_version(&version, config.repack_suffix.as_deref()),
    ))
}

/// Normalizes Cargo crate spelling to Debian's dashed lowercase form.
pub fn normalize_crate_name(name: &str) -> String {
    name.replace('_', "-").to_lowercase()
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
