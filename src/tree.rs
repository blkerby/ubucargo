//! Provides filesystem operations for package trees and archives.

use std::{fs, path::Path, process::Command};

use anyhow::{Context, Result, bail};

use crate::command::run_command;

/// Rejects a path that already exists.
pub fn require_absent(path: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(_) => bail!("destination already exists: {}", path.display()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).with_context(|| format!("inspect {}", path.display())),
    }
}

/// Copies a directory tree, preserving file attributes and metadata.
pub fn copy_tree(source: &Path, destination: &Path) -> Result<()> {
    run_command(
        Command::new("cp")
            .arg("-a")
            .arg("--reflink=auto")
            .arg(source)
            .arg(destination),
        "cp -a --reflink=auto",
    )?;
    Ok(())
}

/// Extracts a tarball, removing its top-level directory component.
pub fn extract_tree(archive: &Path, destination: &Path) -> Result<()> {
    run_command(
        Command::new("tar")
            .arg("--extract")
            .arg("--file")
            .arg(archive)
            .arg("--directory")
            .arg(destination)
            .arg("--strip-components=1"),
        &format!("extract {}", archive.display()),
    )?;
    Ok(())
}

/// Reports whether two files differ; a missing second file counts as differing.
pub fn files_differ(first: &Path, second: &Path) -> Result<bool> {
    let output = Command::new("cmp")
        .arg("--silent")
        .arg(first)
        .arg(second)
        .output()
        .context("run cmp")?;
    Ok(!output.status.success())
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::{PermissionsExt, symlink};

    use super::*;

    #[test]
    /// Preserves copied modes and symlinks without changing their targets.
    fn preserves_copied_permissions() {
        let temporary = tempfile::tempdir().unwrap();
        let source = temporary.path().join("source");
        fs::create_dir(&source).unwrap();
        fs::set_permissions(&source, fs::Permissions::from_mode(0o750)).unwrap();
        let file = source.join("file");
        fs::write(&file, "contents").unwrap();
        fs::set_permissions(&file, fs::Permissions::from_mode(0o710)).unwrap();
        symlink(&file, source.join("link")).unwrap();
        let destination = temporary.path().join("copy");
        copy_tree(&source, &destination).unwrap();
        assert_eq!(
            fs::metadata(&destination).unwrap().permissions().mode() & 0o7777,
            0o750
        );
        assert_eq!(
            fs::metadata(destination.join("file"))
                .unwrap()
                .permissions()
                .mode()
                & 0o7777,
            0o710
        );
        assert_eq!(fs::read_link(destination.join("link")).unwrap(), file);
        assert_eq!(
            fs::read_to_string(destination.join("file")).unwrap(),
            "contents"
        );
    }
}
