//! Provides shared command and filesystem utilities.

use std::{
    fs,
    path::Path,
    process::{Command, Output},
};

use anyhow::{Context, Result, bail};

/// Captures command output, retaining both output streams in failure diagnostics.
pub fn run_command(command: &mut Command, operation: &str) -> Result<Output> {
    let output = command
        .output()
        .with_context(|| format!("run {operation}"))?;
    if !output.status.success() {
        bail!(
            "{operation} failed:\n{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    Ok(output)
}

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
    use indoc::indoc;

    use super::*;

    #[test]
    /// Returns successful output and reports both streams when a command fails.
    fn captures_command_output_and_errors() {
        let output = run_command(Command::new("printf").arg("hello"), "print greeting").unwrap();
        assert_eq!(output.stdout, b"hello");
        let error = run_command(
            Command::new("sh").args(["-c", "printf stdout; printf stderr >&2; exit 1"]),
            "example command",
        )
        .unwrap_err();
        assert_eq!(
            error.to_string(),
            indoc! {r"
                example command failed:
                stdoutstderr"}
        );
    }
}
