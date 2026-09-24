//! Creates and reconciles Debian source packages from staged output.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};
use debian_control::lossless::control::Control;

use crate::{
    changelog::read_top_changelog,
    config::PackageConfig,
    generate::{GeneratedPackage, generate_package},
    resolve::{ExistingPackage, resolve_package},
    util::{copy_tree, extract_tree, files_differ, require_absent},
};

use self::{
    managed::{FileState, build_plan, install_state, read_state},
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

    /// Debian source-package directory. If omitted, uses the nearest parent package;
    /// if none exists, CRATE creates one under the current directory, or the command fails.
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
    let current = std::env::current_dir()
        .context("get current directory")?
        .canonicalize()
        .context("resolve current directory")?;
    let (keep_paths, replace_paths) = collect_decisions(&args.keep, &args.replace)?;
    let resolved = resolve_package(
        Some(&current),
        args.package_dir.as_deref(),
        args.crate_name.as_deref(),
        args.version.as_deref(),
        args.local_crate.as_deref(),
    )?;
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

    if let Some((existing, base)) = &baseline {
        reconcile_existing(
            existing,
            base.path(),
            &generated,
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
    raw_control: Option<FileState>,
    args: &PackageArgs,
    keep: &BTreeSet<PathBuf>,
    replace: &BTreeSet<PathBuf>,
) -> Result<bool> {
    let root = &existing.root;
    let debian = root.join("debian");
    let base_tree = scan_tree(base)?;
    let old_tree = scan_tree(root)?;
    let new_tree = scan_tree(&generated.source)?;
    if !trees_match(&base_tree, &new_tree) && existing.patches_applied {
        bail!("pop the complete quilt stack before reconciling changed upstream source");
    }
    let source_plan = build_source_plan(&base_tree, &old_tree, &new_tree, args.force)?;

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
                    "warning: debian/control Source does not match {}; the package will not build until reconciled with debian/control.debcargo.hint",
                    prepared_top.source
                );
            }
        }
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
    if !changed {
        println!("clean");
    }
    if args.check || !changed {
        return Ok(changed);
    }

    if orig_changed {
        fs::copy(&generated.orig, &orig_destination)
            .with_context(|| format!("install {}", orig_destination.display()))?;
    }
    source_plan
        .apply(root)
        .context("package may be partially updated; rerun `ubucargo package`")?;
    if generated_changed {
        generated_plan
            .apply()
            .context("package may be partially updated; rerun `ubucargo package`")?;
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    /// Checks creation, upgrades, dry-run isolation, and convergence using a local crate.
    fn creates_and_reconciles_local_package() {
        let temporary = tempfile::tempdir().unwrap();
        let local = temporary.path().join("local");
        let root = temporary.path().join("packages/rust-example");
        fs::create_dir_all(local.join("src")).unwrap();
        fs::write(local.join("src/lib.rs"), "// Example library.\n").unwrap();
        let manifest = local.join("Cargo.toml");
        for (version, check, expected_change) in [
            ("1.0.0", true, true),
            ("1.0.0", false, false),
            ("1.0.0", true, false),
            ("1.0.1", true, true),
            ("1.0.1", false, false),
            ("1.0.1", true, false),
        ] {
            let contents = indoc::formatdoc! {r#"
                [package]
                name = "example"
                version = "{version}"
                edition = "2021"
                license = "MIT"
                description = "Example library"
            "#};
            fs::write(&manifest, contents).unwrap();
            let creating = !root.exists();
            let orig = root
                .parent()
                .unwrap()
                .join(format!("rust-example_{version}.orig.tar.gz"));
            let tracked = [
                root.join("Cargo.toml"),
                root.join("debian/control"),
                root.join("debian/changelog"),
                root.join("debian/ubucargo-state.json"),
                orig.clone(),
            ];
            let mut before = Vec::new();
            for path in &tracked {
                before.push(fs::read(path).ok());
            }
            let changed = run(PackageArgs {
                crate_name: None,
                version: None,
                package_dir: Some(root.clone()),
                local_crate: if creating { Some(local.clone()) } else { None },
                check,
                force: false,
                keep_staging: false,
                keep: Vec::new(),
                replace: Vec::new(),
            })
            .unwrap();
            assert_eq!(changed, expected_change, "{version}, check={check}");
            if check {
                for (path, contents) in tracked.iter().zip(before) {
                    assert_eq!(fs::read(path).ok(), contents, "{}", path.display());
                }
                if creating {
                    assert!(!root.parent().unwrap().exists());
                }
            } else {
                assert!(orig.is_file());
                assert_eq!(
                    read_top_changelog(&root.join("debian/changelog"))
                        .unwrap()
                        .upstream,
                    version
                );
                assert!(root.join("debian/debcargo.toml").is_file());
                assert!(root.join("debian/source/format").is_file());
                let control = fs::read_to_string(root.join("debian/control")).unwrap();
                assert!(control.contains("Maintainer: Ubuntu Developers"));
                assert!(!control.contains("Vcs-Git:"));
                assert!(!control.contains("Vcs-Browser:"));
            }
        }
    }
}
