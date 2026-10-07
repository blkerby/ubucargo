//! Provides shared command and filesystem utilities.

use std::{
    fs,
    io::{self, Write},
    os::{fd::AsFd, unix::fs::PermissionsExt},
    path::Path,
    process::{Command, Output, Stdio},
};

use anyhow::{Context, Result, bail};

/// Atomically replaces a file using a temporary file in its existing parent directory.
/// An explicit mode is applied exactly; otherwise creation permissions follow umask.
/// Replaces destination symlinks rather than following them; does not preserve ownership
/// or other metadata, or synchronize the write for power-loss durability.
pub fn write_file(path: &Path, contents: &[u8], mode: Option<u32>) -> Result<()> {
    let parent = path
        .parent()
        .with_context(|| format!("{} has no parent directory", path.display()))?;
    let parent = if parent.as_os_str().is_empty() {
        Path::new(".")
    } else {
        parent
    };
    let mut builder = tempfile::Builder::new();
    if mode.is_none() {
        // Let the OS apply umask as it would for an ordinary new file.
        builder.permissions(fs::Permissions::from_mode(0o666));
    }
    let mut temporary = builder
        .tempfile_in(parent)
        .with_context(|| format!("create temporary file in {}", parent.display()))?;
    temporary
        .write_all(contents)
        .with_context(|| format!("write temporary file for {}", path.display()))?;
    if let Some(mode) = mode {
        temporary
            .as_file()
            .set_permissions(fs::Permissions::from_mode(mode))
            .with_context(|| format!("set mode for {}", path.display()))?;
    }
    temporary
        .persist(path)
        .map_err(|error| error.error)
        .with_context(|| format!("replace {}", path.display()))?;
    Ok(())
}

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

/// Streams both command output streams to stderr, preserving terminal progress displays.
pub fn run_streaming_command(command: &mut Command, operation: &str) -> Result<()> {
    let stderr = io::stderr()
        .as_fd()
        .try_clone_to_owned()
        .with_context(|| format!("duplicate stderr for {operation}"))?;
    let status = command
        .stdin(Stdio::null())
        .stdout(stderr)
        .stderr(Stdio::inherit())
        .status()
        .with_context(|| format!("run {operation}"))?;
    eprintln!();
    if !status.success() {
        bail!("{operation} failed ({status})");
    }
    Ok(())
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
