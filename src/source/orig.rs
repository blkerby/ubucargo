//! Acquires existing orig tarballs used as source-merge baselines.

use std::{
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::{Context, Result};

use crate::util::run_streaming_command;
use tempfile::TempDir;

use crate::changelog::TopChangelog;

/// An old orig path and any temporary directory that keeps it alive.
pub struct OrigBaseline {
    /// Existing or downloaded orig tarball.
    pub path: PathBuf,
    /// Download directory, retained until update is complete.
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
    eprintln!("Downloading {} {} ...", top.source, top.version);
    run_streaming_command(
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
