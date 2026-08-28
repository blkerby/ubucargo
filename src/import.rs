use std::{
    collections::BTreeSet,
    ffi::OsStr,
    fs::{self, File},
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::{Context, Result, bail};
use fs2::FileExt;
use tempfile::TempDir;

use crate::{
    materialize::make_hint_path,
    package::{
        check_debcargo_version, collect_output_paths, is_package_managed, read_root_package,
    },
};

/// Validated files and identities produced by debcargo for one import.
struct GeneratedOutput {
    /// Staged source package tree.
    source: PathBuf,
    /// Staged Debian orig tarball.
    orig: PathBuf,
    /// Debian source package name used for the default destination.
    debian_source: String,
    /// Exact Cargo package version selected by debcargo.
    crate_version: String,
}

/// Imports one crates.io release as a new Debian source package.
pub fn run(crate_name: &str, version: Option<&str>, directory: Option<&Path>) -> Result<()> {
    if version.is_some_and(|version| {
        version.is_empty() || !version.starts_with(|character: char| character.is_ascii_digit())
    }) {
        bail!("--version must be an exact numeric Cargo version");
    }

    let current = std::env::current_dir()
        .context("get current directory")?
        .canonicalize()
        .context("resolve current directory")?;
    let requested_source = if let Some(directory) = directory {
        let requested = current.join(directory);
        let name = requested
            .file_name()
            .with_context(|| format!("{} has no directory name", requested.display()))?
            .to_owned();
        let parent = requested
            .parent()
            .with_context(|| format!("{} has no parent", requested.display()))?;
        fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
        let parent = parent
            .canonicalize()
            .with_context(|| format!("resolve {}", parent.display()))?;
        Some(parent.join(name))
    } else {
        None
    };
    let parent = requested_source
        .as_deref()
        .and_then(Path::parent)
        .unwrap_or(&current);
    let lock = File::open(parent).with_context(|| format!("open {}", parent.display()))?;
    if let Err(error) = lock.try_lock_exclusive() {
        if error.kind() == std::io::ErrorKind::WouldBlock {
            bail!("another import is using {}", parent.display());
        }
        return Err(error).with_context(|| format!("lock {}", parent.display()));
    }
    if let Some(path) = &requested_source {
        ensure_destination_absent(path)?;
    }

    check_debcargo_version()?;
    let stage = stage_package(parent, crate_name, version)?;
    let output = validate_output(stage.path(), crate_name, version)?;
    initialize_package(&output.source)?;

    println!("Importing {crate_name} {}", output.crate_version);
    let (source, orig) = install_output(&output, requested_source.as_deref())?;
    println!("created {}", source.display());
    println!("created {}", orig.display());
    Ok(())
}

/// Runs debcargo in a temporary directory on the destination filesystem.
fn stage_package(parent: &Path, crate_name: &str, version: Option<&str>) -> Result<TempDir> {
    let stage = tempfile::Builder::new()
        .prefix(".ubucargo-import-")
        .tempdir_in(parent)
        .with_context(|| format!("create staging directory in {}", parent.display()))?;
    let config = stage.path().join("debcargo.toml");
    fs::write(&config, "").with_context(|| format!("write {}", config.display()))?;

    let mut command = Command::new("debcargo");
    command
        .arg("package")
        .arg("--config")
        .arg(&config)
        .arg("--directory")
        .arg(stage.path().join("output"))
        .arg(crate_name);
    if let Some(version) = version {
        command.arg(version);
    }
    let output = command
        .current_dir(stage.path())
        .output()
        .context("run debcargo package")?;
    if !output.status.success() {
        bail!(
            "debcargo package failed:\n{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    Ok(stage)
}

/// Validates the staged crate identity and Debian output names.
fn validate_output(
    stage: &Path,
    requested_name: &str,
    requested_version: Option<&str>,
) -> Result<GeneratedOutput> {
    let source = stage.join("output");
    for path in [
        source.join("Cargo.toml"),
        source.join("debian/changelog"),
        source.join("debian/control"),
        source.join("debian/rules"),
        source.join("debian/source/format"),
    ] {
        if !path.is_file() {
            bail!("debcargo produced no {}", path.display());
        }
    }

    let package = read_root_package(&source)?;
    if requested_name.replace('_', "-") != package.name.replace('_', "-") {
        bail!(
            "debcargo selected crate {} instead of {requested_name}",
            package.name
        );
    }
    if requested_version.is_some_and(|version| version != package.version) {
        bail!(
            "debcargo selected {} {} instead of requested version {}",
            package.name,
            package.version,
            requested_version.unwrap()
        );
    }

    let control = fs::read_to_string(source.join("debian/control"))
        .context("read generated debian/control")?;
    let mut debian_source = None;
    for line in control.lines() {
        if let Some(value) = line.strip_prefix("Source:") {
            let value = value.trim();
            if value.is_empty() {
                bail!("generated debian/control has an empty Source field");
            }
            debian_source = Some(value.to_owned());
            break;
        }
    }
    let debian_source = debian_source.context("generated debian/control has no Source field")?;

    let mut origs = Vec::new();
    for entry in fs::read_dir(stage).with_context(|| format!("read {}", stage.display()))? {
        let path = entry?.path();
        if path
            .file_name()
            .and_then(OsStr::to_str)
            .is_some_and(|name| name.ends_with(".orig.tar.gz"))
        {
            origs.push(path);
        }
    }
    if origs.len() != 1 {
        bail!(
            "debcargo produced {} orig tarballs; expected one",
            origs.len()
        );
    }
    let orig = origs.pop().unwrap();
    let orig_name = orig.file_name().and_then(OsStr::to_str).unwrap();
    if !orig_name.starts_with(&format!("{debian_source}_")) {
        bail!(
            "orig tarball {} does not match Debian source {debian_source}",
            orig.display()
        );
    }

    Ok(GeneratedOutput {
        source,
        orig,
        debian_source,
        crate_version: package.version,
    })
}

/// Adds Ubucargo's in-tree configuration and generated-file baselines.
fn initialize_package(source: &Path) -> Result<()> {
    let debian = source.join("debian");
    fs::write(debian.join("debcargo.toml"), "[ubucargo]\n")?;

    let mut paths = BTreeSet::new();
    collect_output_paths(&debian, &debian, &mut paths)?;
    for path in paths {
        if !is_package_managed(&path) {
            continue;
        }
        let primary = debian.join(path.strip_prefix("debian")?);
        let hint = make_hint_path(&primary);
        fs::copy(&primary, &hint)
            .with_context(|| format!("initialize {} from {}", hint.display(), primary.display()))?;
    }
    Ok(())
}

/// Installs the staged source tree and orig tarball without replacing existing paths.
fn install_output(
    output: &GeneratedOutput,
    requested_source: Option<&Path>,
) -> Result<(PathBuf, PathBuf)> {
    let parent = requested_source
        .and_then(Path::parent)
        .or_else(|| output.source.parent().and_then(Path::parent))
        .context("find import destination directory")?;
    let source = requested_source
        .map(Path::to_path_buf)
        .unwrap_or_else(|| parent.join(&output.debian_source));
    let orig_name = output
        .orig
        .file_name()
        .with_context(|| format!("{} has no file name", output.orig.display()))?;
    let orig = parent.join(orig_name);
    ensure_destination_absent(&source)?;
    ensure_destination_absent(&orig)?;

    fs::rename(&output.orig, &orig).with_context(|| format!("install {}", orig.display()))?;
    fs::rename(&output.source, &source).with_context(|| format!("install {}", source.display()))?;

    Ok((source, orig))
}

/// Rejects any existing filesystem entry, including a broken symbolic link.
fn ensure_destination_absent(path: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(_) => bail!("destination already exists: {}", path.display()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).with_context(|| format!("inspect {}", path.display())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Creates the minimum generated package tree needed by initialization tests.
    fn make_generated_package(root: &Path) {
        fs::create_dir_all(root.join("debian/patches/auto")).unwrap();
        fs::write(root.join("debian/control"), "control").unwrap();
        fs::write(root.join("debian/changelog"), "changelog").unwrap();
        fs::write(root.join("debian/patches/auto/change.patch"), "patch").unwrap();
    }

    #[test]
    /// Verifies that imports establish baselines only for managed generated files.
    fn initializes_config_and_managed_hints() {
        let root = tempfile::tempdir().unwrap();
        make_generated_package(root.path());

        initialize_package(root.path()).unwrap();

        assert_eq!(
            fs::read_to_string(root.path().join("debian/debcargo.toml")).unwrap(),
            "[ubucargo]\n"
        );
        assert_eq!(
            fs::read_to_string(root.path().join("debian/control.debcargo.hint")).unwrap(),
            "control"
        );
        assert_eq!(
            fs::read_to_string(
                root.path()
                    .join("debian/patches/auto/change.patch.debcargo.hint")
            )
            .unwrap(),
            "patch"
        );
        assert!(!root.path().join("debian/changelog.debcargo.hint").exists());
    }

    #[test]
    /// Verifies successful installation and collision refusal.
    fn installs_without_replacing_existing_paths() {
        let parent = tempfile::tempdir().unwrap();
        let stage = tempfile::Builder::new().tempdir_in(parent.path()).unwrap();
        let generated_source = stage.path().join("output");
        let generated_orig = stage.path().join("rust-example_1.0.0.orig.tar.gz");
        fs::create_dir(&generated_source).unwrap();
        fs::write(generated_source.join("file"), "source").unwrap();
        fs::write(&generated_orig, "orig").unwrap();
        let output = GeneratedOutput {
            source: generated_source,
            orig: generated_orig,
            debian_source: "rust-example".to_owned(),
            crate_version: "1.0.0".to_owned(),
        };

        let (source, orig) = install_output(&output, None).unwrap();

        assert_eq!(fs::read_to_string(source.join("file")).unwrap(), "source");
        assert_eq!(fs::read_to_string(orig).unwrap(), "orig");

        let existing = parent.path().join("existing");
        fs::create_dir(&existing).unwrap();
        assert!(install_output(&output, Some(&existing)).is_err());
    }
}
