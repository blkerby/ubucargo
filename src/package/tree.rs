use std::{fs, path::Path, process::Command};

use anyhow::{Context, Result, bail};

/// Rejects a path that already exists.
pub(super) fn require_absent(path: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(_) => bail!("destination already exists: {}", path.display()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).with_context(|| format!("inspect {}", path.display())),
    }
}

/// Copies a directory tree, preserving file attributes and metadata.
pub(super) fn copy_tree(source: &Path, destination: &Path) -> Result<()> {
    let output = Command::new("cp")
        .arg("-a")
        .arg("--reflink=auto")
        .arg(source)
        .arg(destination)
        .output()
        .context("run cp -a --reflink=auto")?;
    if !output.status.success() {
        bail!(
            "cp -a --reflink=auto failed:\n{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    Ok(())
}

/// Extracts a tarball, removing its top-level directory component.
pub(super) fn extract_tree(archive: &Path, destination: &Path) -> Result<()> {
    let output = Command::new("tar")
        .arg("--extract")
        .arg("--file")
        .arg(archive)
        .arg("--directory")
        .arg(destination)
        .arg("--strip-components=1")
        .output()
        .context("run tar")?;
    if !output.status.success() {
        bail!(
            "could not extract {}:\n{}{}",
            archive.display(),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    Ok(())
}

/// Reports whether two files differ; a missing second file counts as differing.
pub(super) fn files_differ(first: &Path, second: &Path) -> Result<bool> {
    let output = Command::new("cmp")
        .arg("--silent")
        .arg(first)
        .arg(second)
        .output()
        .context("run cmp")?;
    Ok(!output.status.success())
}
