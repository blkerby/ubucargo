//! Reads Cargo package identities and direct dependency metadata.

use std::{
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::{Result, bail};
use serde::Deserialize;

use crate::command::run_command;

/// Relevant package records returned by `cargo metadata`.
#[derive(Deserialize)]
struct Metadata {
    /// Cargo packages contained in the workspace.
    packages: Vec<MetadataPackage>,
}

/// Cargo package identity and direct dependencies.
#[derive(Clone, Debug, Deserialize)]
pub struct MetadataPackage {
    /// Cargo package name.
    pub name: String,
    /// Exact Cargo package version.
    pub version: String,
    /// Direct Cargo dependencies declared by the package.
    #[serde(default)]
    pub dependencies: Vec<MetadataDependency>,
    /// Manifest used to distinguish the root package from workspace members.
    manifest_path: PathBuf,
}

/// Direct dependency fields used by dependency inspection.
#[derive(Clone, Debug, Deserialize)]
pub struct MetadataDependency {
    /// Canonical package name, independent of any local rename.
    pub name: String,
    /// Cargo semantic-version requirement.
    pub req: String,
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

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    #[test]
    /// Selects the requested manifest within a workspace and rejects a virtual root.
    fn reads_workspace_root_package() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir(root.path().join("src")).unwrap();
        fs::create_dir_all(root.path().join("member/src")).unwrap();
        fs::write(root.path().join("src/lib.rs"), "").unwrap();
        fs::write(root.path().join("member/src/lib.rs"), "").unwrap();
        fs::write(
            root.path().join("Cargo.toml"),
            "[package]\nname = \"z-root\"\nversion = \"1.0.0\"\nedition = \"2024\"\n[workspace]\nmembers = [\"member\"]\n",
        )
        .unwrap();
        fs::write(
            root.path().join("member/Cargo.toml"),
            "[package]\nname = \"a-member\"\nversion = \"2.0.0\"\nedition = \"2024\"\n",
        )
        .unwrap();

        let package = read_root_package(root.path()).unwrap();
        assert_eq!(package.name, "z-root");
        assert_eq!(package.version, "1.0.0");
        assert!(package.dependencies.is_empty());
        assert_eq!(
            read_root_package(&root.path().join("member")).unwrap().name,
            "a-member"
        );

        fs::write(
            root.path().join("Cargo.toml"),
            "[workspace]\nmembers = [\"member\"]\n",
        )
        .unwrap();
        let error = read_root_package(root.path()).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("does not contain a root [package]")
        );
    }
}
