//! Generates and validates staged Debian source packages from resolved inputs.

use std::{
    ffi::OsStr,
    fs,
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::{Context, Result, bail};
use debian_control::lossless::control::Control;
use tempfile::TempDir;

use crate::{
    cargo::read_root_package,
    changelog::prepare_changelog,
    command::run_command,
    resolve::{CrateSelection, ResolvedPackage, normalize_crate_name, write_staged_config},
    tree::copy_tree,
};

/// Validated files and identities produced by final debcargo generation.
pub struct GeneratedPackage {
    /// Temporary directory containing all staged generation state.
    pub stage: TempDir,
    /// Staged source package tree.
    pub source: PathBuf,
    /// Staged Debian orig tarball.
    pub orig: PathBuf,
}

/// Generates and validates a staged source package using the resolved release and configuration.
pub fn generate_package(package: &ResolvedPackage, keep_staging: bool) -> Result<GeneratedPackage> {
    let stage = tempfile::Builder::new()
        .disable_cleanup(keep_staging)
        .tempdir()
        .context("create package staging directory")?;
    if keep_staging {
        eprintln!("package staging directory: {}", stage.path().display());
    }
    let overlay = stage.path().join("overlay");
    fs::create_dir(&overlay)?;
    if let Some(existing) = &package.existing {
        prepare_patch_overlay(&existing.root.join("debian"), &overlay)?;
    }
    write_staged_config(&package.config, stage.path())?;
    let source = if package.config.crate_src_path.is_some() {
        "local source"
    } else {
        "crates.io"
    };
    let provenance = format!(
        "Package {} {} from {source}.\n  Generated with debcargo {} and ubucargo {}.",
        package.crate_selection.crate_name,
        package.crate_selection.version,
        package.debcargo_version,
        env!("CARGO_PKG_VERSION")
    );
    prepare_changelog(
        package
            .existing
            .as_ref()
            .map(|existing| existing.root.join("debian/changelog")),
        &overlay.join("changelog"),
        package
            .existing
            .as_ref()
            .map(|existing| &existing.top_changelog),
        &package.source_name,
        &package.upstream,
        &provenance,
    )?;
    run_debcargo(stage.path(), &package.crate_selection)?;
    validate_debcargo_output(
        stage,
        &package.source_name,
        &package.upstream,
        &package.crate_selection.crate_name,
        &package.crate_selection.version,
    )
}

/// Validates staged source identity, Cargo identity, essential packaging, and orig naming.
fn validate_debcargo_output(
    stage: TempDir,
    expected_source: &str,
    expected_upstream: &str,
    requested_name: &str,
    requested_version: &str,
) -> Result<GeneratedPackage> {
    let source = stage.path().join("output");
    for path in [
        source.join("Cargo.toml"),
        source.join("debian/changelog"),
        source.join("debian/control"),
        source.join("debian/rules"),
        source.join("debian/source/format"),
    ] {
        if !path.is_file() {
            bail!("debcargo produced no {}", path.display());
        }
    }

    let package = read_root_package(&source)?;
    if normalize_crate_name(requested_name) != normalize_crate_name(&package.name) {
        bail!(
            "debcargo selected crate {} instead of {requested_name}",
            package.name
        );
    }
    if package.version != requested_version {
        bail!(
            "debcargo selected {} {} instead of requested version {requested_version}",
            package.name,
            package.version
        );
    }

    let control = Control::from_file(source.join("debian/control"))
        .context("parse generated debian/control")?;
    let debian_source = control
        .source()
        .context("generated debian/control has no source paragraph")?
        .name()
        .context("generated debian/control has no Source field")?;
    if debian_source != expected_source {
        bail!("debcargo produced Debian source {debian_source}, expected {expected_source}");
    }
    if fs::read(source.join("debian/changelog"))?
        != fs::read(stage.path().join("overlay/changelog"))?
    {
        bail!("debcargo changed the prepared changelog despite --changelog-ready");
    }

    let expected_orig = format!("{expected_source}_{expected_upstream}.orig.tar.gz");
    let mut origs = Vec::new();
    for entry in fs::read_dir(stage.path())? {
        let path = entry?.path();
        if path
            .file_name()
            .and_then(OsStr::to_str)
            .is_some_and(|name| name.ends_with(".orig.tar.gz"))
        {
            origs.push(path);
        }
    }
    if origs.len() != 1 {
        bail!(
            "debcargo produced {} orig tarballs; expected one",
            origs.len()
        );
    }
    let orig = origs.pop().unwrap();
    if orig.file_name() != Some(OsStr::new(&expected_orig)) {
        bail!(
            "debcargo produced orig {}, expected {expected_orig}",
            orig.display()
        );
    }
    Ok(GeneratedPackage {
        stage,
        source,
        orig,
    })
}

/// Copies the complete patch set into the debcargo overlay.
fn prepare_patch_overlay(debian: &Path, overlay: &Path) -> Result<()> {
    let patches = debian.join("patches");
    if !patches.is_dir() {
        return Ok(());
    }
    copy_tree(&patches, &overlay.join("patches"))
}

/// Runs final debcargo generation for one exact selected release.
fn run_debcargo(stage: &Path, crate_selection: &CrateSelection) -> Result<()> {
    let mut command = Command::new("debcargo");
    command
        .arg("package")
        .arg("--config")
        .arg(stage.join("debcargo.toml"))
        .arg("--directory")
        .arg(stage.join("output"))
        .arg("--no-overlay-write-back")
        .arg("--changelog-ready")
        .arg(&crate_selection.crate_name)
        .arg(&crate_selection.version)
        .current_dir(stage)
        // Set CARGO_TARGET_DIR to a unique staging directory, to work around
        // github.com/rust-lang/cargo/issues/16683:
        .env("CARGO_TARGET_DIR", stage.join("cargo-target"));
    run_command(&mut command, "debcargo package")?;
    Ok(())
}
