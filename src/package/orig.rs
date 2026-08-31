use std::{
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::{Context, Result, bail};
use tempfile::TempDir;

use super::changelog::TopChangelog;

/// An old orig path and any temporary directory that keeps it alive.
pub(super) struct OrigBaseline {
    /// Existing or downloaded orig tarball.
    pub(super) path: PathBuf,
    /// Download directory, retained until reconciliation is complete.
    _temporary: Option<TempDir>,
}

/// Finds the old orig locally or downloads and verifies the exact Launchpad source.
pub(super) fn acquire_old_orig(root: &Path, top: &TopChangelog) -> Result<OrigBaseline> {
    let parent = root.parent().context("package root has no parent")?;
    let expected_name = format!("{}_{}.orig.tar.gz", top.source, top.upstream);
    let local = parent.join(&expected_name);
    if local.is_file() {
        return Ok(OrigBaseline {
            path: local,
            _temporary: None,
        });
    }

    let download = tempfile::tempdir().context("create orig download directory")?;
    let output = Command::new("pull-lp-source")
        .arg("--download-only")
        .arg(&top.source)
        .arg(&top.version)
        .current_dir(download.path())
        .output()
        .context("run pull-lp-source")?;
    if !output.status.success() {
        bail!(
            "pull-lp-source failed:\n{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    // pull-lp-source verifies downloaded source files against their `.dsc`.
    let orig = locate_downloaded_orig(download.path(), top)?;
    Ok(OrigBaseline {
        path: orig,
        _temporary: Some(download),
    })
}

/// Locates the orig tarball downloaded for the old source version.
fn locate_downloaded_orig(directory: &Path, top: &TopChangelog) -> Result<PathBuf> {
    let expected_name = format!("{}_{}.orig.tar.gz", top.source, top.upstream);
    let orig = directory.join(expected_name);
    if !orig.is_file() {
        bail!("pull-lp-source did not produce {}", orig.display());
    }
    Ok(orig)
}

/// Extracts an orig tarball as the pristine merge baseline.
pub(super) fn extract_orig(orig: &Path, destination: &Path) -> Result<()> {
    let output = Command::new("tar")
        .arg("--extract")
        .arg("--file")
        .arg(orig)
        .arg("--directory")
        .arg(destination)
        .arg("--strip-components=1")
        .output()
        .context("run tar")?;
    if !output.status.success() {
        bail!(
            "could not extract {}:\n{}{}",
            orig.display(),
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    /// Verifies local orig discovery and destination creation reporting.
    fn finds_local_orig_and_reports_destination_creation() {
        let parent = tempfile::tempdir().unwrap();
        let root = parent.path().join("rust-example");
        fs::create_dir(&root).unwrap();
        let orig = parent.path().join("rust-example_1.0.0.orig.tar.gz");
        fs::write(&orig, "orig").unwrap();
        let top = TopChangelog {
            source: "rust-example".to_owned(),
            version: "1.0.0-0ubuntu1".to_owned(),
            upstream: "1.0.0".to_owned(),
            distribution: "noble".to_owned(),
        };
        assert_eq!(acquire_old_orig(&root, &top).unwrap().path, orig);

        let candidate = parent.path().join("candidate");
        fs::write(&candidate, "different").unwrap();
        assert!(files_differ(&candidate, &orig).unwrap());
    }
}
