//! Shared acquisition and writing of maintained source packages.

use crate::{
    apt,
    changelog::read_top_changelog,
    input::Input,
    resolve::validate_separate_trees,
    util::{copy_tree, files_differ, require_absent, resolve_path, run_command},
};
use anyhow::{Context, Result, bail};
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    process::Command,
};

mod orig;

pub use orig::acquire_old_orig;
pub mod tree;

use tree::{TreePlan, build_tree_plan, scan_tree};

/// Maintained source package and its intended destination.
pub struct SourcePackage {
    /// Owns the staged package until writing is complete.
    _stage: Option<tempfile::TempDir>,
    /// Original working tree or staged copy of the maintained package.
    pub root: PathBuf,
    /// Final source-package directory.
    pub destination: PathBuf,
    /// Whether writing updates the selected existing package in place.
    pub update: bool,
}

impl SourcePackage {
    /// Copies a local package and its orig baseline into staging; published inputs are already staged.
    pub fn stage(&mut self, keep: bool) -> Result<()> {
        if self._stage.is_some() {
            return Ok(());
        }
        let baseline = acquire_old_orig(
            &self.root,
            &read_top_changelog(&self.root.join("debian/changelog"))?,
        )?;
        let stage = tempfile::Builder::new().disable_cleanup(keep).tempdir()?;
        if keep {
            eprintln!("Source staging: {}", stage.path().display());
        }
        let root = stage.path().join("source");
        copy_tree(&self.root, &root)?;
        fs::copy(
            &baseline.path,
            stage
                .path()
                .join(baseline.path.file_name().context("orig has no filename")?),
        )?;
        self.root = root;
        self._stage = Some(stage);
        Ok(())
    }
}

/// Reads the top applied quilt patch and rejects unrefreshed changes.
pub fn read_top_patch(source: &Path) -> Result<Option<String>> {
    let path = source.join(".pc/applied-patches");
    let contents = match fs::read_to_string(&path) {
        Ok(contents) => contents,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error).with_context(|| format!("read {}", path.display())),
    };
    let mut top = None;
    for patch in contents.lines().rev() {
        if !patch.trim().is_empty() {
            top = Some(patch.to_owned());
            break;
        }
    }
    let Some(top) = top else {
        return Ok(None);
    };
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
    Ok(Some(top))
}

/// Selects a local package or acquires a published package in staging.
pub fn acquire_package(
    input: &Input,
    version: Option<&str>,
    destination: Option<&Path>,
    keep: bool,
) -> Result<SourcePackage> {
    let current = std::env::current_dir()?;
    if let Input::Package(root) = input {
        let destination = resolve_path(&current.join(destination.unwrap_or(root)))?;
        let update = destination == *root;
        if !update {
            require_absent(&destination)?;
            validate_separate_trees(root, &destination)?;
        }
        return Ok(SourcePackage {
            root: root.clone(),
            destination,
            update,
            _stage: None,
        });
    }
    let source = match input {
        Input::Archive { source, .. } | Input::Ppa { source, .. } => source,
        _ => bail!("expected a maintained source-package input"),
    };
    let destination = resolve_path(&current.join(destination.unwrap_or(Path::new(source))))?;
    require_absent(&destination)?;
    let records = apt::load_source_records(input, &apt::read_architecture()?)?;
    let ppa = match input {
        Input::Ppa { ppa, .. } => Some(ppa.as_str()),
        _ => None,
    };
    let selected = apt::select_source(&records, source, ppa, version)?;
    println!(
        "Input: {} {} from {}",
        selected.source, selected.version, selected.location
    );
    let (stage, root) = apt::retrieve_source(selected, keep)?;
    Ok(SourcePackage {
        _stage: Some(stage),
        root,
        destination,
        update: false,
    })
}

/// Writes a completed staged package, updating existing destinations by their tree differences.
pub fn write_package(package: &SourcePackage) -> Result<()> {
    if !package.update {
        require_absent(&package.destination)?;
    }
    let parent = package
        .destination
        .parent()
        .context("package destination has no parent")?;
    let mut orig_tarballs = Vec::new();
    for entry in fs::read_dir(
        package
            .root
            .parent()
            .context("package root has no parent")?,
    )? {
        let entry = entry?;
        let name = entry.file_name();
        let name_text = name.to_string_lossy();
        if entry.file_type()?.is_file()
            && (name_text.contains(".orig.tar.")
                || (name_text.contains(".orig-") && name_text.contains(".tar.")))
        {
            let target = parent.join(&name);
            if !files_differ(&entry.path(), &target)? {
                continue;
            }
            if target.try_exists()? {
                bail!(
                    "{} already exists with different contents",
                    target.display()
                );
            }
            orig_tarballs.push((entry.path(), target));
        }
    }
    orig_tarballs.sort();
    let staged_tree = scan_tree(&package.root, None)?;
    let old_tree = if package.update {
        scan_tree(&package.destination, None)?
    } else {
        Default::default()
    };
    let mut plan = build_tree_plan(&old_tree, &staged_tree);
    // Commit generated ownership state only after source, packaging, hints,
    // and quilt backups have all been written.
    let manifest_path = PathBuf::from("debian/ubucargo-state.json");
    let mut final_paths = BTreeMap::new();
    if let Some(change) = plan.paths.remove(&manifest_path) {
        final_paths.insert(manifest_path, change);
    }
    let final_plan = TreePlan { paths: final_paths };
    if package.update {
        plan.print_report();
        final_plan.print_report();
    } else {
        println!("Create {}", package.destination.display());
    }
    for (_, target) in &orig_tarballs {
        println!("Write {}", target.display());
    }
    let changed = !package.update
        || plan.has_changes()
        || final_plan.has_changes()
        || !orig_tarballs.is_empty();
    if !changed {
        println!("Clean");
        return Ok(());
    }
    fs::create_dir_all(parent)?;
    for (source, target) in orig_tarballs {
        fs::copy(source, target)?;
    }
    if package.update {
        plan.apply(&package.destination)
            .context("package may be partially updated; rerun `ubucargo package`")?;
        // Quilt's backup timestamps are part of its state even though ordinary
        // tree comparisons use contents and executable status only.
        let quilt_state = package.root.join(".pc");
        if quilt_state.is_dir() {
            copy_tree(&quilt_state.join("."), &package.destination.join(".pc"))?;
        }
        final_plan
            .apply(&package.destination)
            .context("package may be partially updated; rerun `ubucargo package`")?;
    } else {
        copy_tree(&package.root, &package.destination)?;
    }
    Ok(())
}
