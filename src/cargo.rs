//! Reads Cargo package identities.

use std::{
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::{Context, Result, bail};
use serde::Deserialize;

use crate::util::run_command;

/// Relevant package records returned by `cargo metadata`.
#[derive(Deserialize)]
struct Metadata {
    /// Cargo packages contained in the workspace.
    packages: Vec<MetadataPackage>,
}

/// Cargo package identity.
#[derive(Clone, Debug, Deserialize)]
pub struct MetadataPackage {
    /// Cargo package name.
    pub name: String,
    /// Exact Cargo package version.
    pub version: String,
    /// Manifest used to distinguish the root package from workspace members.
    manifest_path: PathBuf,
}

/// Checks that a crate name uses the ASCII characters accepted for registry lookups.
pub fn validate_crate_name(crate_name: &str) -> Result<()> {
    if crate_name.is_empty()
        || !crate_name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        bail!("invalid crate name {crate_name:?}");
    }
    Ok(())
}

/// Uses Cargo to identify the package defined by the root manifest.
pub fn read_root_package(root: &Path) -> Result<MetadataPackage> {
    let manifest = root.join("Cargo.toml").canonicalize()?;
    let output = run_command(
        Command::new("cargo")
            .args([
                "metadata",
                "--offline",
                "--no-deps",
                "--format-version",
                "1",
                "--manifest-path",
            ])
            .arg(&manifest),
        "cargo metadata",
    )?;
    let metadata: Metadata = serde_json::from_slice(&output.stdout)?;
    for package in metadata.packages {
        if package
            .manifest_path
            .canonicalize()
            .is_ok_and(|path| path == manifest)
        {
            return Ok(package);
        }
    }
    bail!("{} does not contain a root [package]", manifest.display())
}

/// Reads the literal identity of an extracted or maintained crate without resolving Cargo metadata.
pub fn read_package_identity(root: &Path) -> Result<MetadataPackage> {
    let manifest_path = root.join("Cargo.toml");
    let contents = std::fs::read_to_string(&manifest_path)
        .with_context(|| format!("read {}", manifest_path.display()))?;
    let manifest: toml_edit::DocumentMut = contents
        .parse()
        .with_context(|| format!("parse {}", manifest_path.display()))?;
    let package = manifest
        .get("package")
        .context("Cargo.toml requires a [package] table")?;
    let name = package
        .get("name")
        .and_then(toml_edit::Item::as_str)
        .context("Cargo.toml requires a literal package.name")?;
    let version = package
        .get("version")
        .and_then(toml_edit::Item::as_str)
        .context("Cargo.toml requires a literal package.version")?;
    Ok(MetadataPackage {
        name: name.to_owned(),
        version: version.to_owned(),
        manifest_path,
    })
}
