//! Shared acquisition and installation of published source packages.

use crate::{
    apt,
    input::Input,
    util::{copy_tree, files_differ, require_absent},
};
use anyhow::{Context, Result, bail};
use std::{
    fs,
    path::{Path, PathBuf},
};

/// Authenticated source extracted in staging, with its intended destination.
pub struct PublishedPackage {
    /// Keeps the extracted source and downloaded archives alive.
    pub stage: tempfile::TempDir,
    /// Extracted maintained source package.
    pub root: PathBuf,
    /// New source-package directory to install.
    pub destination: PathBuf,
}

/// Selects and downloads an exact published source into staging.
pub fn acquire_package(
    input: &Input,
    version: Option<&str>,
    destination: Option<&Path>,
    keep: bool,
) -> Result<PublishedPackage> {
    let source = match input {
        Input::Archive { source, .. } | Input::Ppa { source, .. } => source,
        _ => bail!("expected an Archive or PPA source input"),
    };
    let current = std::env::current_dir()?;
    let destination = current.join(destination.unwrap_or(Path::new(source)));
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
    Ok(PublishedPackage {
        stage,
        root,
        destination,
    })
}

/// Installs a staged package and its orig archives without overwriting different archives.
pub fn install_package(package: &PublishedPackage, check: bool) -> Result<bool> {
    require_absent(&package.destination)?;
    let parent = package
        .destination
        .parent()
        .context("package destination has no parent")?;
    let mut archives = Vec::new();
    for entry in fs::read_dir(package.stage.path())? {
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
