//! Creates and updates Debian source packages from staged output.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::{Context, Result, bail};
use debian_control::lossless::control::Control;

use crate::{
    changelog::read_top_changelog,
    config::{
        PackageConfig, get_new_local_package_config, get_new_package_config, read_package_config,
        relocate_package_config,
    },
    generate::{GeneratedPackage, generate_package},
    input::Input,
    resolve::{
        CrateRequest, ExistingPackage, find_parent_package, get_crate_source_name,
        read_existing_package, resolve_package, validate_separate_trees,
    },
    source::{
        acquire_old_orig, acquire_package, read_top_patch,
        tree::{TreePlan, build_source_plan, scan_tree},
        write_package,
    },
    util::{copy_tree, extract_tree, files_differ, require_absent, resolve_path, run_command},
};

use self::{
    managed::{FileState, ManagedPlan, build_plan, read_state, write_state},
    output::{
        build_patch_series_plan, collect_managed_paths, initialize_package,
        prepare_generated_candidates, remove_generated_vcs_fields, update_staged_maintainer,
    },
};

mod managed;
mod output;

/// Create or update a complete source package.
#[derive(clap::Args)]
pub struct PackageArgs {
    /// Crate, published source, package, or local input; defaults to the nearest package.
    #[arg(value_name = "INPUT")]
    pub input: Option<String>,

    /// Exact Cargo or Debian source version; defaults to the latest selected release.
    #[arg(value_name = "VERSION", requires = "input")]
    pub version: Option<String>,

    /// Destination directory; package inputs default to their existing directory.
    #[arg(long = "package-dir", value_name = "DIR")]
    pub package_dir: Option<PathBuf>,

    /// Resolve source-tree conflicts in favor of the selected crate release.
    #[arg(long)]
    pub force: bool,

    /// Retain the temporary debcargo staging directory for inspection.
    #[arg(long)]
    pub keep_staging: bool,

    /// Keep an ambiguous primary when baselines are missing or conflict.
    #[arg(long, value_name = "PATH")]
    pub keep: Vec<PathBuf>,

    /// Adopt the generated state for a managed path, including deletion.
    #[arg(long, value_name = "PATH")]
    pub replace: Vec<PathBuf>,
}

/// Creates or updates one source package.
pub fn run(args: PackageArgs) -> Result<()> {
    let current = std::env::current_dir()
        .context("get current directory")?
        .canonicalize()
        .context("resolve current directory")?;
    let (keep_paths, replace_paths) = collect_decisions(&args.keep, &args.replace)?;
    let input = if let Some(value) = &args.input {
        crate::input::parse_input(value, &current)?
    } else {
        Input::Package(
            find_parent_package(&current).context("not inside a source package; supply INPUT")?,
        )
    };
    crate::input::validate_version(&input, args.version.as_deref())?;
    let destination = resolve_package_destination(&current, &input, args.package_dir.as_deref())?;
    let maintained_input = match &input {
        Input::Crate(_) | Input::Local(_) => {
            if destination.try_exists()? {
                Some(Input::Package(destination.clone()))
            } else {
                None
            }
        }
        Input::Package(_) | Input::Archive { .. } | Input::Ppa { .. } => Some(input.clone()),
    };
    let source_version = match &input {
        Input::Archive { .. } | Input::Ppa { .. } => args.version.as_deref(),
        _ => None,
    };
    let mut acquired = match &maintained_input {
        Some(input) => Some(acquire_package(
            input,
            source_version,
            &destination,
            args.keep_staging,
        )?),
        None => None,
    };
    let local = match &input {
        Input::Local(path) => Some(path.as_path()),
        _ => None,
    };
    let (mut config, existing) = if let Some(package) = &acquired {
        let existing = read_existing_package(&package.root)?;
        let config = read_package_config(&package.root, local)?;
        (config, Some(existing))
    } else if let Some(local) = local {
        (
            get_new_local_package_config(local, Some(&destination))?,
            None,
        )
    } else {
        (get_new_package_config()?, None)
    };
    if let Some(local) = &config.resolved_crate_src_path {
        validate_separate_trees(local, &destination)?;
        if let Some(existing) = &existing
            && existing.root != destination
        {
            validate_separate_trees(local, &existing.root)?;
        }
    }
    if acquired.as_ref().is_some_and(|package| !package.update) {
        relocate_package_config(&mut config, &destination)?;
    }
    let request = match &input {
        Input::Crate(name) => CrateRequest::Registry {
            name,
            version: args.version.as_deref(),
        },
        Input::Local(path) => CrateRequest::Local(path),
        _ => CrateRequest::Current,
    };
    let mut resolved = resolve_package(request, config, existing)?;
    let action = if resolved.existing.is_none() {
        "Create new package"
    } else if acquired.as_ref().is_some_and(|package| !package.update) {
        "Create package from existing packaging"
    } else {
        "Update existing package"
    };
    println!("{action}: {}", destination.display());
    if resolved.existing.is_none() && (!keep_paths.is_empty() || !replace_paths.is_empty()) {
        bail!("--keep and --replace apply only to existing packages");
    }
    let mut top_patch = None;
    let baseline = if let Some(package) = &mut acquired {
        let existing = resolved.existing.as_mut().unwrap();
        package.stage(args.keep_staging)?;
        existing.root = package.root.clone();
        top_patch = read_top_patch(&existing.root)?;
        if top_patch.is_some() {
            run_command(
                Command::new("quilt")
                    .args(["pop", "--quiltrc=-", "-a"])
                    .env("QUILT_PATCHES", "debian/patches")
                    .current_dir(&existing.root),
                "pop staged quilt patches",
            )?;
        }
        let old_orig = acquire_old_orig(&existing.root, &existing.top_changelog)?;
        let base = tempfile::tempdir().context("create old-source extraction directory")?;
        extract_tree(&old_orig.path, base.path())?;
        Some(base)
    } else {
        None
    };
    let generated = generate_package(&resolved, args.keep_staging)?;
    // Capture debcargo's raw control before Ubuntu adjustments so an unchanged
    // existing control can establish ownership without a manifest entry or hint.
    let raw_control = read_state(&generated.source.join("debian/control"))?;
    remove_generated_vcs_fields(generated.stage.path())?;
    update_staged_maintainer(generated.stage.path())?;

    // Reconcile unpatched source and packaging in staging, then restore the
    // original quilt position there. This validates patch reapplication and
    // rebuilds .pc against the updated upstream before touching the destination.
    // The final write compares the completed, patched tree with the destination
    // so unchanged files retain their modification times.
    if let (Some(existing), Some(base)) = (&resolved.existing, &baseline) {
        let plan = build_update_plan(
            existing,
            base.path(),
            &generated,
            &resolved.config,
            raw_control,
            args.force,
            &keep_paths,
            &replace_paths,
        )?;
        plan.managed.print_overrides();
        if plan.has_changes() {
            plan.apply()?;
        }
    } else {
        create_new(&destination, &resolved.config, &generated)?;
    }
    if let Some(package) = &acquired {
        if let Some(top) = top_patch {
            run_command(
                Command::new("quilt")
                    .args(["push", "--quiltrc=-", "--"])
                    .arg(Path::new("debian/patches").join(&top))
                    .env("QUILT_PATCHES", "debian/patches")
                    .current_dir(&package.root),
                "restore staged quilt patches",
            )?;
            if read_top_patch(&package.root)?.as_deref() != Some(top.as_str()) {
                bail!("could not restore the original top quilt patch {top}");
            }
        }
        write_package(package)?;
    }
    Ok(())
}

/// Resolves the final destination and validates restrictions for the selected input kind.
fn resolve_package_destination(
    current: &Path,
    input: &Input,
    package_dir: Option<&Path>,
) -> Result<PathBuf> {
    let destination = if let Some(package_dir) = package_dir {
        current.join(package_dir)
    } else {
        match input {
            Input::Crate(name) => find_parent_package(current)
                .unwrap_or_else(|| current.join(get_crate_source_name(name, None))),
            Input::Local(_) => bail!("local: inputs require --package-dir"),
            Input::Package(root) => root.clone(),
            Input::Archive { source, .. } | Input::Ppa { source, .. } => current.join(source),
        }
    };
    let destination = resolve_path(&destination)?;
    match input {
        Input::Package(root) if destination != *root => {
            require_absent(&destination)?;
            validate_separate_trees(root, &destination)?;
        }
        Input::Archive { .. } | Input::Ppa { .. } => require_absent(&destination)?,
        Input::Crate(_) | Input::Local(_) => {
            if let Input::Local(root) = input {
                validate_separate_trees(root, &destination)?;
            }
            if !destination.try_exists()? {
                require_absent(&destination)?;
            }
        }
        _ => {}
    }
    Ok(destination)
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

/// Builds source and packaging changes against the old orig without modifying the package.
fn build_update_plan(
    existing: &ExistingPackage,
    base: &Path,
    generated: &GeneratedPackage,
    config: &PackageConfig,
    raw_control: Option<FileState>,
    force: bool,
    keep: &BTreeSet<PathBuf>,
    replace: &BTreeSet<PathBuf>,
) -> Result<UpdatePlan> {
    let root = &existing.root;
    let debian = root.join("debian");
    let exclude = Some(Path::new("debian"));
    let base_tree = scan_tree(base, exclude)?;
    let old_tree = scan_tree(root, exclude)?;
    let new_tree = scan_tree(&generated.source, exclude)?;
    let source_plan = build_source_plan(&base_tree, &old_tree, &new_tree, force)?;

    let generated_candidates = prepare_generated_candidates(&generated.source)?;
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
            println!("Ambiguous {} (use --keep or --replace)", path.display());
        }
        bail!("unresolved generated-file ambiguities");
    }

    // Preserve control overrides, but flag identities that will prevent a build.
    let prepared_top = read_top_changelog(&generated.stage.path().join("overlay/changelog"))?;
    for path in &generated_plan.paths {
        if path.path == Path::new("debian/control") {
            let state = path
                .primary_after
                .as_ref()
                .context("planned debian/control is missing")?;
            let control: Control = std::str::from_utf8(&state.contents)?
                .parse()
                .context("parse planned debian/control")?;
            let source = control.source().and_then(|source| source.name());
            if source.as_deref() != Some(prepared_top.source.as_str()) {
                eprintln!(
                    "warning: debian/control Source does not match {}; the package will not build until updated to match debian/control.debcargo.hint",
                    prepared_top.source
                );
            }
        }
    }

    let prepared_changelog = read_state(&generated.stage.path().join("overlay/changelog"))?
        .context("staged changelog is missing")?;
    let old_changelog = read_state(&debian.join("changelog"))?;
    let orig_destination = root.parent().context("package root has no parent")?.join(
        generated
            .orig
            .file_name()
            .context("candidate orig has no file name")?,
    );
    let old_config =
        read_state(&debian.join("debcargo.toml"))?.context("package configuration is missing")?;
    let mut new_config = old_config.clone();
    new_config.contents = config.original_contents.as_bytes().to_vec();
    Ok(UpdatePlan {
        root: root.clone(),
        orig: if files_differ(&generated.orig, &orig_destination)? {
            Some((generated.orig.clone(), orig_destination))
        } else {
            None
        },
        source: source_plan,
        managed: generated_plan,
        config: if new_config != old_config {
            Some(new_config)
        } else {
            None
        },
        changelog: if old_changelog.as_ref() != Some(&prepared_changelog) {
            Some(prepared_changelog)
        } else {
            None
        },
    })
}

/// Complete changes to an existing package; staged source files must remain until applied.
struct UpdatePlan {
    root: PathBuf,
    /// Staged orig and destination paths, or None when the tarball already matches.
    orig: Option<(PathBuf, PathBuf)>,
    source: TreePlan,
    managed: ManagedPlan,
    /// Updated changelog, or None when the current changelog already matches.
    changelog: Option<FileState>,
    /// Updated configuration, including an explicitly selected local source path.
    config: Option<FileState>,
}

impl UpdatePlan {
    /// Reports whether any part of the package needs updating.
    fn has_changes(&self) -> bool {
        self.orig.is_some()
            || self.source.has_changes()
            || self.managed.has_changes()
            || self.changelog.is_some()
            || self.config.is_some()
    }

    /// Writes the orig, source, managed files, changelog, and configuration.
    fn apply(&self) -> Result<()> {
        if let Some((source, destination)) = &self.orig {
            fs::copy(source, destination)
                .with_context(|| format!("write {}", destination.display()))?;
        }
        self.source
            .apply(&self.root)
            .context("package may be partially updated; rerun `ubucargo package`")?;
        if self.managed.has_changes() {
            self.managed
                .apply()
                .context("package may be partially updated; rerun `ubucargo package`")?;
        }
        if let Some(changelog) = &self.changelog {
            write_state(&self.root.join("debian/changelog"), Some(changelog))?;
        }
        if let Some(config) = &self.config {
            write_state(&self.root.join("debian/debcargo.toml"), Some(config))?;
        }
        Ok(())
    }
}

/// Initializes and writes the generated package at the resolved destination.
fn create_new(root: &Path, config: &PackageConfig, generated: &GeneratedPackage) -> Result<()> {
    let parent = root.parent().context("package root has no parent")?;
    initialize_package(&generated.source, config)?;

    require_absent(root)?;
    let orig = parent.join(
        generated
            .orig
            .file_name()
            .context("candidate orig has no file name")?,
    );
    let orig_changed = files_differ(&generated.orig, &orig)?;
    if orig_changed && orig.try_exists()? {
        bail!("{} already exists with different contents", orig.display());
    }
    println!("Create {}", root.display());
    if orig_changed {
        println!("Create {}", orig.display());
    }
    fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    if orig_changed {
        fs::copy(&generated.orig, &orig).with_context(|| format!("write {}", orig.display()))?;
    }
    copy_tree(&generated.source, root)?;
    Ok(())
}
