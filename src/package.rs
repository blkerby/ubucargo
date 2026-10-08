//! Creates and updates Debian source packages from staged output.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};
use debian_control::lossless::control::Control;

use crate::{
    changelog::read_top_changelog,
    config::{PackageConfig, relocate_package_config, write_package_config},
    generate::{GeneratedPackage, generate_package},
    resolve::{ExistingPackage, resolve_package},
    source::{acquire_old_orig, acquire_package, install_package},
    util::{copy_tree, extract_tree, files_differ, require_absent},
};

use self::{
    managed::{FileState, ManagedPlan, build_plan, install_state, read_state},
    output::{
        build_patch_series_plan, collect_managed_paths, generated_patch_changes,
        initialize_package, read_generated_candidates, remove_generated_vcs_fields,
        update_staged_maintainer,
    },
    source::{SourcePlan, build_source_plan, scan_tree, trees_match},
};

mod managed;
mod output;
mod source;

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

/// Creates or updates one source package, returning true when check mode finds changes.
pub fn run(mut args: PackageArgs) -> Result<bool> {
    let current = std::env::current_dir()
        .context("get current directory")?
        .canonicalize()
        .context("resolve current directory")?;
    let (keep_paths, replace_paths) = collect_decisions(&args.keep, &args.replace)?;
    let input = if let Some(value) = &args.input {
        crate::input::parse_input(value, &current)?
    } else {
        crate::input::Input::Package(
            crate::resolve::find_parent_package(&current)
                .context("not inside a source package; supply INPUT")?,
        )
    };
    crate::input::validate_version(&input, args.version.as_deref())?;
    let acquired = match &input {
        crate::input::Input::Package(_)
        | crate::input::Input::Archive { .. }
        | crate::input::Input::Ppa { .. } => Some(acquire_package(
            &input,
            args.version.as_deref(),
            args.package_dir.as_deref(),
            args.keep_staging,
        )?),
        _ => None,
    };
    let check = args.check;
    if let Some(package) = &acquired {
        args.version = None;
        if package.root != package.destination {
            // Extracted or copied packages regenerate against unpatched upstream
            // source. Only staging is changed, including during --check.
            let applied = package.root.join(".pc/applied-patches");
            if applied.is_file() && !fs::read_to_string(&applied)?.trim().is_empty() {
                crate::util::run_command(
                    std::process::Command::new("quilt")
                        .args(["pop", "--quiltrc=-", "-a"])
                        .env("QUILT_PATCHES", "debian/patches")
                        .current_dir(&package.root),
                    "pop staged quilt patches",
                )?;
            }
            args.check = false;
        }
    }
    let destination = acquired
        .as_ref()
        .map(|package| package.root.as_path())
        .or(args.package_dir.as_deref());
    let mut name = None;
    let mut local = None;
    match &input {
        crate::input::Input::Crate(value) => name = Some(value.as_str()),
        crate::input::Input::Local(path) => {
            if destination.is_none() {
                bail!("local: inputs require --package-dir");
            }
            local = Some(path.as_path());
        }
        _ => {}
    }
    let mut resolved = resolve_package(
        Some(&current),
        destination,
        name,
        args.version.as_deref(),
        local,
    )?;
    let destination = acquired
        .as_ref()
        .map(|package| package.destination.as_path())
        .or(resolved.destination.as_deref())
        .context("package destination is missing")?;
    let action = if resolved.existing.is_none() {
        "Create new package"
    } else if acquired
        .as_ref()
        .is_some_and(|package| package.root != package.destination)
    {
        "Create package from existing packaging"
    } else {
        "Update existing package"
    };
    println!(
        "{}{action}: {}",
        if check { "Check: " } else { "" },
        destination.display()
    );
    if resolved.existing.is_none() && (!keep_paths.is_empty() || !replace_paths.is_empty()) {
        bail!("--keep and --replace apply only to existing packages");
    }
    let baseline = if let Some(existing) = &resolved.existing {
        let old_orig = acquire_old_orig(&existing.root, &existing.top_changelog)?;
        let base = tempfile::tempdir().context("create old-source extraction directory")?;
        extract_tree(&old_orig.path, base.path())?;
        Some((existing, base))
    } else {
        None
    };
    let generated = generate_package(&resolved, args.keep_staging)?;
    // Capture debcargo's raw control before Ubuntu adjustments so an unchanged
    // existing control can establish ownership without a manifest entry or hint.
    let raw_control = read_state(&generated.source.join("debian/control"))?;
    remove_generated_vcs_fields(generated.stage.path())?;
    update_staged_maintainer(generated.stage.path())?;

    let changed = if let Some((existing, base)) = &baseline {
        update_existing(
            existing,
            base.path(),
            &generated,
            &resolved.config,
            raw_control,
            &args,
            &keep_paths,
            &replace_paths,
        )
    } else {
        create_new(
            resolved
                .destination
                .as_deref()
                .context("package destination is missing")?,
            &resolved.config,
            &generated,
            args.check,
        )
    }?;
    if let Some(package) = &acquired
        && package.root != package.destination
    {
        relocate_package_config(&mut resolved.config, &package.destination)?;
        write_package_config(&resolved.config, &package.root)?;
        return install_package(package, check);
    }
    Ok(changed)
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

/// Updates an existing package using the generated source and old orig baseline.
fn update_existing(
    existing: &ExistingPackage,
    base: &Path,
    generated: &GeneratedPackage,
    config: &PackageConfig,
    raw_control: Option<FileState>,
    args: &PackageArgs,
    keep: &BTreeSet<PathBuf>,
    replace: &BTreeSet<PathBuf>,
) -> Result<bool> {
    let plan = build_update_plan(
        existing,
        base,
        generated,
        config,
        raw_control,
        args.force,
        keep,
        replace,
    )?;
    plan.print_report();
    if existing.patches_applied && generated_patch_changes(&plan.managed) && !args.check {
        bail!("pop the real quilt stack before applying generated patch changes");
    }
    let changed = plan.has_changes();
    if args.check || !changed {
        return Ok(changed);
    }
    plan.apply()?;
    Ok(false)
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
    let base_tree = scan_tree(base)?;
    let old_tree = scan_tree(root)?;
    let new_tree = scan_tree(&generated.source)?;
    if !trees_match(&base_tree, &new_tree) && existing.patches_applied {
        bail!("pop the complete quilt stack before updating changed upstream source");
    }
    let source_plan = build_source_plan(&base_tree, &old_tree, &new_tree, force)?;

    let generated_candidates = read_generated_candidates(&generated.source)?;
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
    source: SourcePlan,
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

    /// Prints package changes or reports a clean package.
    fn print_report(&self) {
        if let Some((_, destination)) = &self.orig {
            println!("create {}", destination.display());
        }
        self.source.print_report();
        self.managed.print_report();
        if self.changelog.is_some() {
            println!("update debian/changelog");
        }
        if self.config.is_some() {
            println!("update debian/debcargo.toml");
        }
        if !self.has_changes() {
            println!("clean");
        }
    }

    /// Installs the orig, source, managed files, changelog, and configuration.
    fn apply(&self) -> Result<()> {
        if let Some((source, destination)) = &self.orig {
            fs::copy(source, destination)
                .with_context(|| format!("install {}", destination.display()))?;
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
            install_state(&self.root.join("debian/changelog"), Some(changelog))?;
        }
        if let Some(config) = &self.config {
            install_state(&self.root.join("debian/debcargo.toml"), Some(config))?;
        }
        Ok(())
    }
}

/// Initializes and installs the generated package at the resolved destination.
fn create_new(
    root: &Path,
    config: &PackageConfig,
    generated: &GeneratedPackage,
    check: bool,
) -> Result<bool> {
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
