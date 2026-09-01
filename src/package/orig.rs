use std::{
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::{Context, Result, bail};
use tempfile::TempDir;

use super::changelog::TopChangelog;

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

    // pull-lp-source already verifies downloaded source files against their `.dsc`.
    // So here we only check that the downloaded tarball exists.
    let orig = download
        .path()
        .join(format!("{}_{}.orig.tar.gz", top.source, top.upstream));
    if !orig.is_file() {
        bail!("pull-lp-source did not produce {}", orig.display());
    }
    Ok(OrigBaseline {
        path: orig,
        _temporary: Some(download),
    })
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
