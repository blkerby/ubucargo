use std::{
    ffi::OsStr,
    fs::{self, File},
    io::Read,
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::{Context, Result, bail};
use tempfile::{NamedTempFile, TempDir};

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

    let orig = validate_download_directory(download.path(), top)?;
    Ok(OrigBaseline {
        path: orig,
        _temporary: Some(download),
    })
}

/// Locates and verifies the single downloaded source description and expected orig.
fn validate_download_directory(directory: &Path, top: &TopChangelog) -> Result<PathBuf> {
    let mut dscs = Vec::new();
    for entry in fs::read_dir(directory)? {
        let path = entry?.path();
        if path.extension() == Some(OsStr::new("dsc")) {
            dscs.push(path);
        }
    }
    if dscs.len() != 1 {
        bail!("pull-lp-source produced {} .dsc files", dscs.len());
    }
    let expected_name = format!("{}_{}.orig.tar.gz", top.source, top.upstream);
    let orig = directory.join(expected_name);
    verify_downloaded_orig(&dscs[0], &orig, &top.source, &top.version)?;
    Ok(orig)
}

/// Verifies downloaded source identity, orig filename, size, and SHA-256 from its `.dsc`.
fn verify_downloaded_orig(
    dsc: &Path,
    orig: &Path,
    expected_source: &str,
    expected_version: &str,
) -> Result<()> {
    let contents = fs::read_to_string(dsc).with_context(|| format!("read {}", dsc.display()))?;
    if read_dsc_field(&contents, "Source") != Some(expected_source)
        || read_dsc_field(&contents, "Version") != Some(expected_version)
    {
        bail!("{} has unexpected source identity", dsc.display());
    }
    let expected_name = orig.file_name().context("orig path has no file name")?;
    let expected_name = expected_name.to_string_lossy();
    let mut in_checksums = false;
    let mut checksum = None;
    for line in contents.lines() {
        if line == "Checksums-Sha256:" {
            in_checksums = true;
            continue;
        }
        if in_checksums && !line.starts_with(' ') {
            break;
        }
        if !in_checksums {
            continue;
        }
        let fields: Vec<&str> = line.split_whitespace().collect();
        if fields.len() == 3 && fields[2] == expected_name {
            checksum = Some((fields[0], fields[1]));
        }
    }
    let (expected_hash, expected_size) =
        checksum.context(".dsc does not name the expected orig")?;
    let size = fs::metadata(orig)
        .with_context(|| format!("inspect {}", orig.display()))?
        .len();
    if expected_size
        .parse::<u64>()
        .context("parse .dsc orig size")?
        != size
    {
        bail!("downloaded orig size does not match .dsc");
    }
    let output = Command::new("sha256sum")
        .arg(orig)
        .output()
        .context("run sha256sum")?;
    if !output.status.success() {
        bail!("sha256sum failed for {}", orig.display());
    }
    let actual = String::from_utf8_lossy(&output.stdout);
    let actual = actual.split_whitespace().next().unwrap_or_default();
    if actual != expected_hash {
        bail!("downloaded orig checksum does not match .dsc");
    }
    Ok(())
}

/// Reads one simple RFC822 field from a `.dsc` file.
fn read_dsc_field<'a>(contents: &'a str, field: &str) -> Option<&'a str> {
    let prefix = format!("{field}:");
    for line in contents.lines() {
        if let Some(value) = line.strip_prefix(&prefix) {
            return Some(value.trim());
        }
    }
    None
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

/// Reports whether a candidate orig must be installed and rejects mismatching collisions.
pub(super) fn validate_candidate_orig(candidate: &Path, destination: &Path) -> Result<bool> {
    match fs::symlink_metadata(destination) {
        Ok(metadata) if metadata.is_file() => {
            if files_equal(candidate, destination)? {
                Ok(false)
            } else {
                bail!(
                    "existing orig {} does not match the candidate",
                    destination.display()
                )
            }
        }
        Ok(_) => bail!(
            "orig destination is not a regular file: {}",
            destination.display()
        ),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(true),
        Err(error) => Err(error).with_context(|| format!("inspect {}", destination.display())),
    }
}

/// Installs an orig tarball atomically without replacing an existing path.
pub(super) fn install_orig(candidate: &Path, destination: &Path) -> Result<()> {
    let parent = destination
        .parent()
        .context("orig destination has no parent")?;
    fs::create_dir_all(parent)?;
    let mut temporary = NamedTempFile::new_in(parent)?;
    let mut source = File::open(candidate)?;
    std::io::copy(&mut source, &mut temporary)?;
    temporary
        .as_file()
        .set_permissions(fs::metadata(candidate)?.permissions())?;
    temporary
        .persist_noclobber(destination)
        .map_err(|error| error.error)
        .with_context(|| format!("install {}", destination.display()))?;
    Ok(())
}

/// Compares two files without loading tarballs into memory.
fn files_equal(first: &Path, second: &Path) -> Result<bool> {
    if fs::metadata(first)?.len() != fs::metadata(second)?.len() {
        return Ok(false);
    }
    let mut first = File::open(first)?;
    let mut second = File::open(second)?;
    let mut first_buffer = [0_u8; 64 * 1024];
    let mut second_buffer = [0_u8; 64 * 1024];
    loop {
        let first_read = first.read(&mut first_buffer)?;
        let second_read = second.read(&mut second_buffer)?;
        if first_read != second_read || first_buffer[..first_read] != second_buffer[..second_read] {
            return Ok(false);
        }
        if first_read == 0 {
            return Ok(true);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    /// Verifies local orig discovery and mismatching candidate refusal.
    fn finds_local_orig_and_rejects_candidate_collision() {
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
        assert!(validate_candidate_orig(&candidate, &orig).is_err());
    }

    #[test]
    /// Verifies downloaded orig discovery, checksum validation, and missing-base rejection.
    fn validates_downloaded_orig_fixture() {
        let directory = tempfile::tempdir().unwrap();
        let top = TopChangelog {
            source: "rust-example".to_owned(),
            version: "1.0.0-0ubuntu1".to_owned(),
            upstream: "1.0.0".to_owned(),
            distribution: "noble".to_owned(),
        };
        assert!(validate_download_directory(directory.path(), &top).is_err());

        let orig = directory.path().join("rust-example_1.0.0.orig.tar.gz");
        fs::write(&orig, "orig").unwrap();
        let dsc = directory.path().join("rust-example_1.0.0-0ubuntu1.dsc");
        fs::write(
            &dsc,
            concat!(
                "Source: rust-example\n",
                "Version: 1.0.0-0ubuntu1\n",
                "Checksums-Sha256:\n",
                " 14e0ffdc8215c81da0cde40f581237ee35177ddac4f1fc7613cad3004798d25f 4 rust-example_1.0.0.orig.tar.gz\n",
            ),
        )
        .unwrap();
        assert_eq!(
            validate_download_directory(directory.path(), &top).unwrap(),
            orig
        );

        fs::write(
            dsc,
            concat!(
                "Source: rust-example\n",
                "Version: 1.0.0-0ubuntu1\n",
                "Checksums-Sha256:\n",
                " deadbeef 4 rust-example_1.0.0.orig.tar.gz\n",
            ),
        )
        .unwrap();
        assert!(validate_download_directory(directory.path(), &top).is_err());
    }
}
