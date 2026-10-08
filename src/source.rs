//! Shared acquisition and installation of maintained source packages.

use crate::{
    apt,
    changelog::read_top_changelog,
    config::{read_package_config, relocate_package_config, write_package_config},
    input::Input,
    resolve::validate_separate_trees,
    util::{copy_tree, files_differ, require_absent, resolve_path},
};
use anyhow::{Context, Result, bail};
use std::{
    fs,
    path::{Path, PathBuf},
};

mod orig;

pub use orig::acquire_old_orig;

/// Maintained source package and its intended destination.
pub struct SourcePackage {
    /// Owns staging when the package will be installed elsewhere.
    _stage: Option<tempfile::TempDir>,
    /// Original working tree or staged copy of the maintained package.
    pub root: PathBuf,
    /// Final source-package directory.
    pub destination: PathBuf,
}

/// Selects an in-place package or acquires a staged package for a new destination.
pub fn acquire_package(
    input: &Input,
    version: Option<&str>,
    destination: Option<&Path>,
    keep: bool,
) -> Result<SourcePackage> {
    let current = std::env::current_dir()?;
    if let Input::Package(root) = input {
        let destination = resolve_path(&current.join(destination.unwrap_or(root)))?;
        if destination == *root {
            return Ok(SourcePackage {
                root: root.clone(),
                destination,
                _stage: None,
            });
        }
        require_absent(&destination)?;
        validate_separate_trees(root, &destination)?;
        let mut config = read_package_config(root, None)?;
        if let Some(crate_root) = &config.resolved_crate_src_path {
            validate_separate_trees(crate_root, &destination)?;
        }
        let baseline =
            acquire_old_orig(root, &read_top_changelog(&root.join("debian/changelog"))?)?;
        let stage = tempfile::Builder::new().disable_cleanup(keep).tempdir()?;
        if keep {
            eprintln!("Source staging: {}", stage.path().display());
        }
        let staged_root = stage.path().join("source");
        copy_tree(root, &staged_root)?;
        fs::copy(
            &baseline.path,
            stage
                .path()
                .join(baseline.path.file_name().context("orig has no filename")?),
        )?;
        relocate_package_config(&mut config, &staged_root)?;
        write_package_config(&config, &staged_root)?;
        return Ok(SourcePackage {
            root: staged_root,
            destination,
            _stage: Some(stage),
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
    })
}

/// Installs a staged package and its orig archives without overwriting different archives.
pub fn install_package(package: &SourcePackage, check: bool) -> Result<bool> {
    require_absent(&package.destination)?;
    let parent = package
        .destination
        .parent()
        .context("package destination has no parent")?;
    let mut archives = Vec::new();
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
            if target.try_exists()? && files_differ(&entry.path(), &target)? {
                bail!(
                    "{} already exists with different contents",
                    target.display()
                );
            }
            archives.push((entry.path(), target));
        }
    }
    archives.sort();
    println!("create {}", package.destination.display());
    for (_, target) in &archives {
        println!("retain {}", target.display());
    }
    if check {
        return Ok(true);
    }
    fs::create_dir_all(parent)?;
    for (source, target) in archives {
        if !target.try_exists()? {
            fs::copy(source, target)?;
        }
    }
    copy_tree(&package.root, &package.destination)?;
    Ok(false)
}
