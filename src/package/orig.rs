//! Acquires existing orig tarballs used as source-merge baselines.

use std::{
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::{Context, Result};

use crate::util::run_command;
use tempfile::TempDir;

use crate::changelog::TopChangelog;

/// An old orig path and any temporary directory that keeps it alive.
pub struct OrigBaseline {
    /// Existing or downloaded orig tarball.
    pub path: PathBuf,
    /// Download directory, retained until reconciliation is complete.
    _temporary: Option<TempDir>,
}

/// Finds the old orig locally or downloads the exact Launchpad source.
pub fn acquire_old_orig(root: &Path, top: &TopChangelog) -> Result<OrigBaseline> {
    let parent = root.parent().context("package root has no parent")?;
    if let Some(local) = find_orig(parent, top) {
        return Ok(OrigBaseline {
            path: local,
            _temporary: None,
        });
    }

    let download = tempfile::tempdir().context("create orig download directory")?;
    run_command(
        Command::new("pull-lp-source")
            .arg("--download-only")
            .arg(&top.source)
            .arg(&top.version)
            .current_dir(download.path()),
        "pull-lp-source",
    )?;

    // pull-lp-source already verifies downloaded source files against their `.dsc`.
    // So here we only check that the downloaded tarball exists.
    let orig = find_orig(download.path(), top).with_context(|| {
        format!(
            "pull-lp-source did not produce an orig tarball for {} {}",
            top.source, top.upstream
        )
    })?;
    Ok(OrigBaseline {
        path: orig,
        _temporary: Some(download),
    })
}

/// Finds the main orig tarball using the compression formats supported by dpkg-source.
fn find_orig(directory: &Path, top: &TopChangelog) -> Option<PathBuf> {
    for extension in ["gz", "xz", "bz2", "lzma"] {
        let path = directory.join(format!(
            "{}_{}.orig.tar.{extension}",
            top.source, top.upstream
        ));
        if path.is_file() {
            return Some(path);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    /// Verifies local orig discovery.
    fn finds_local_orig() {
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
    }
}
