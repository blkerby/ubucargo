//! Reads Cargo package identities.

use std::{
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::{Result, bail};
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
