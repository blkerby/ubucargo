//! Creates and reconciles Debian source packages from staged output.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};

use crate::{
    prepare::{
        ExistingPackage, GeneratedPackage, PackageConfig, generate_package, parse_exact_version,
        prepare_package, resolve_package_target,
    },
    tree::{copy_tree, extract_tree, files_differ, require_absent},
};

use self::{
    managed::{build_plan, install_state, read_state},
    orig::acquire_old_orig,
    output::{
        build_patch_series_plan, collect_managed_paths, generated_patch_changes,
        initialize_package, read_generated_candidates, remove_generated_vcs_fields,
        update_staged_maintainer,
    },
    source::{build_source_plan, scan_tree, trees_match},
};

mod managed;
mod orig;
mod output;
mod source;

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
    } else if !keep_paths.is_empty() || !replace_paths.is_empty() {
        bail!("--keep and --replace apply only to existing packages");
    }
    let prepared = prepare_package(
        Some(&target),
        args.crate_name.as_deref(),
        args.version.as_deref(),
        args.local_crate.as_deref(),
    )?;
    if let Some(existing) = &prepared.existing {
        let old_orig = acquire_old_orig(&existing.root, &existing.top_changelog)?;
        let base = tempfile::tempdir().context("create old-source extraction directory")?;
        extract_tree(&old_orig.path, base.path())?;
        let generated = generate_package(&prepared, args.keep_staging)?;
        reconcile_existing(
            existing,
            base.path(),
            &generated,
            &args,
            &keep_paths,
            &replace_paths,
        )
    } else {
        let generated = generate_package(&prepared, args.keep_staging)?;
        create_new(&target.source, &prepared.config, &generated, args.check)
    }
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

/// Reconciles an existing package against the generated source and old orig baseline.
fn reconcile_existing(
    existing: &ExistingPackage,
    base: &Path,
    generated: &GeneratedPackage,
    args: &PackageArgs,
    keep: &BTreeSet<PathBuf>,
    replace: &BTreeSet<PathBuf>,
) -> Result<bool> {
    let root = &existing.root;
    let debian = root.join("debian");
    let raw_control = read_state(&generated.stage.path().join("output/debian/control"))?;
    remove_generated_vcs_fields(generated.stage.path())?;
    update_staged_maintainer(generated.stage.path())?;

    let base_tree = scan_tree(base)?;
    let old_tree = scan_tree(root)?;
    let new_tree = scan_tree(&generated.source)?;
    if !trees_match(&base_tree, &new_tree) && existing.patches_applied {
        bail!("pop the complete quilt stack before reconciling changed upstream source");
    }
    let source_plan = build_source_plan(&base_tree, &old_tree, &new_tree, args.force)?;

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
    if existing.patches_applied && generated_patch_changes(&generated_plan) && !args.check {
        bail!("pop the real quilt stack before applying generated patch changes");
    }
    let changed =
        orig_changed || source_plan.has_changes() || generated_changed || changelog_changed;
    if args.check {
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

/// Initializes and installs the generated package at the resolved destination.
fn create_new(
    root: &Path,
    config: &PackageConfig,
    generated: &GeneratedPackage,
    check: bool,
) -> Result<bool> {
    let parent = root.parent().context("package root has no parent")?;
    remove_generated_vcs_fields(generated.stage.path())?;
    update_staged_maintainer(generated.stage.path())?;
    initialize_package(&generated.source, config)?;

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
    if check {
        return Ok(true);
    }

    fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    if orig_changed {
        fs::copy(&generated.orig, &orig).with_context(|| format!("install {}", orig.display()))?;
    }
    copy_tree(&generated.source, root)?;
    Ok(false)
}
