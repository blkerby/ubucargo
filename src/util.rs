//! Provides shared command and filesystem utilities.

use std::{
    fs,
    io::Write,
    os::unix::fs::PermissionsExt,
    path::Path,
    process::{Command, Output},
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
    use std::io::Read;

    use indoc::indoc;

    use super::*;

    #[test]
    /// Replaces the destination while existing readers retain the original contents.
    fn replaces_file_without_changing_open_readers() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("file");
        fs::write(&path, "old").unwrap();
        let mut reader = fs::File::open(&path).unwrap();
        write_file(&path, b"new", None).unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"new");
        let mut original = String::new();
        reader.read_to_string(&mut original).unwrap();
        assert_eq!(original, "old");
    }

    #[test]
    /// Uses ordinary creation permissions unless an exact mode is supplied.
    fn sets_file_permissions() {
        let directory = tempfile::tempdir().unwrap();
        let reference = directory.path().join("reference");
        fs::write(&reference, "").unwrap();
        let creation_mode = fs::metadata(&reference).unwrap().permissions().mode() & 0o7777;
        let path = directory.path().join("file");
        write_file(&path, b"new", None).unwrap();
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o7777,
            creation_mode
        );
        write_file(&path, b"executable", Some(0o751)).unwrap();
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o7777,
            0o751
        );
    }

    #[test]
    /// Leaves the destination intact and removes temporary files after a failed replacement.
    fn cleans_up_failed_replacement() {
        let directory = tempfile::tempdir().unwrap();
        let blocked = directory.path().join("directory");
        fs::create_dir(&blocked).unwrap();
        fs::write(blocked.join("keep"), "original").unwrap();
        assert!(write_file(&blocked, b"replacement", None).is_err());
        assert_eq!(fs::read(blocked.join("keep")).unwrap(), b"original");
        assert_eq!(fs::read_dir(directory.path()).unwrap().count(), 1);
    }

    #[test]
    /// Requires the caller to create the destination's parent directory.
    fn rejects_missing_parent() {
        let directory = tempfile::tempdir().unwrap();
        assert!(write_file(&directory.path().join("missing/file"), b"new", None).is_err());
        assert!(!directory.path().join("missing").exists());
    }

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
