use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::OsStr,
    fs::{self, File},
    os::unix::fs as unix_fs,
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::{Context, Result, bail};
use fs2::FileExt;
use serde::Deserialize;
use tempfile::TempDir;
use toml_edit::{DocumentMut, value};

use crate::materialize::{FileState, build_plan, normalize_path, read_state};

const DEBCARGO_VERSION: &str = "debcargo 2.8.4";
const PACKAGE_MANAGED_PATHS: &[&str] = &[
    "debian/control",
    "debian/copyright",
    "debian/rules",
    "debian/source/format",
    "debian/tests/control",
];
const EXPECTED_UNMANAGED_OUTPUTS: &[&str] = &[
    "debian/cargo-checksum.json",
    "debian/watch",
    "debian/changelog",
];

/// Relevant package records returned by `cargo metadata`.
#[derive(Deserialize)]
struct Metadata {
    /// Cargo packages contained in the staged workspace.
    packages: Vec<MetadataPackage>,
}

/// Cargo metadata needed to identify the staged root package.
#[derive(Deserialize)]
struct MetadataPackage {
    /// Cargo package name passed to debcargo.
    name: String,
    /// Manifest used to distinguish the root package from workspace members.
    manifest_path: PathBuf,
}

/// Regenerates one source package, returning true when check mode finds changes.
pub fn run(
    package: Option<&Path>,
    check: bool,
    keep: &[PathBuf],
    replace: &[PathBuf],
) -> Result<bool> {
    let root = find_package_root(package)?;
    let debian_link = root.join("debian");
    let debian = debian_link
        .canonicalize()
        .with_context(|| format!("resolve {}", debian_link.display()))?;
    if !debian.is_dir() {
        bail!("{} must resolve to a directory", debian_link.display());
    }
    let lock = File::open(&debian).with_context(|| format!("open {}", debian.display()))?;
    if let Err(error) = lock.try_lock_exclusive() {
        if error.kind() == std::io::ErrorKind::WouldBlock {
            bail!("another process is using {}", root.display());
        }
        return Err(error).with_context(|| format!("lock {}", debian.display()));
    }

    let keep = normalize_paths(keep)?;
    let replace = normalize_paths(replace)?;
    if let Some(path) = keep.intersection(&replace).next() {
        bail!(
            "{} cannot be named by both --keep and --replace",
            path.display()
        );
    }

    check_debcargo_version()?;
    let stage = stage_package(&root, &debian)?;
    let crate_name = read_root_package(&stage.path().join("source"))?.name;
    run_debcargo(&stage, &crate_name)?;
    let generated = read_generated_candidates(stage.path())?;
    // "Managed" means the path participates in hint reconciliation. Its current
    // primary file may still be a maintainer override.
    let managed = collect_managed_paths(&debian, &generated)?;
    let plan = build_plan(&debian, &managed, &generated, &keep, &replace)?;
    plan.print_report();

    let ambiguities = plan.collect_ambiguities();
    if !ambiguities.is_empty() {
        let mut names = Vec::new();
        for path in ambiguities {
            names.push(path.display().to_string());
        }
        bail!(
            "unresolved generated-file ambiguities: {}",
            names.join(", ")
        );
    }

    let changed = plan.has_changes();
    if !check && changed {
        plan.apply()?;
    }
    if !changed {
        println!("clean");
    }

    Ok(check && changed)
}

/// Finds the nearest source-package root from an explicit path or the current directory.
fn find_package_root(package: Option<&Path>) -> Result<PathBuf> {
    let start = match package {
        Some(path) => path.to_path_buf(),
        None => std::env::current_dir().context("get current directory")?,
    };
    let start = start
        .canonicalize()
        .with_context(|| format!("resolve {}", start.display()))?;

    for candidate in start.ancestors() {
        if candidate.join("Cargo.toml").is_file()
            && candidate.join("debian/debcargo.toml").is_file()
        {
            if !candidate.join("debian/changelog").is_file() {
                bail!(
                    "{} has no debian/changelog; initial changelog creation is not implemented",
                    candidate.display()
                );
            }
            return Ok(candidate.to_path_buf());
        }
    }

    bail!(
        "{} is not inside a source package with Cargo.toml and debian/debcargo.toml",
        start.display()
    )
}

/// Validates and deduplicates package-relative command-line paths.
fn normalize_paths(paths: &[PathBuf]) -> Result<BTreeSet<PathBuf>> {
    let mut normalized = BTreeSet::new();
    for path in paths {
        normalized.insert(normalize_path(path)?);
    }
    Ok(normalized)
}

/// Uses Cargo to identify the package defined by the staged root manifest.
fn read_root_package(root: &Path) -> Result<MetadataPackage> {
    let manifest = root.join("Cargo.toml").canonicalize()?;
    let output = Command::new("cargo")
        .args([
            "metadata",
            "--offline",
            "--no-deps",
            "--format-version",
            "1",
            "--manifest-path",
        ])
        .arg(&manifest)
        .output()
        .context("run cargo metadata")?;
    if !output.status.success() {
        bail!(
            "cargo metadata failed:\n{}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }

    let metadata: Metadata =
        serde_json::from_slice(&output.stdout).context("parse cargo metadata")?;
    metadata
        .packages
        .into_iter()
        .find(|package| {
            package
                .manifest_path
                .canonicalize()
                .is_ok_and(|path| path == manifest)
        })
        .with_context(|| format!("{} does not contain a root [package]", manifest.display()))
}

/// Rejects debcargo versions not covered by the current compatibility target.
fn check_debcargo_version() -> Result<()> {
    let output = Command::new("debcargo")
        .arg("--version")
        .output()
        .context("run debcargo --version")?;
    if !output.status.success() {
        bail!("debcargo --version failed");
    }
    let actual = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    if actual != DEBCARGO_VERSION {
        bail!("unsupported {actual}; this release requires {DEBCARGO_VERSION}");
    }
    Ok(())
}

/// Copies and patches the source, then prepares the minimal debcargo overlay and config.
fn stage_package(root: &Path, debian: &Path) -> Result<TempDir> {
    let stage = tempfile::tempdir().context("create staging directory")?;
    let source = stage.path().join("source");
    copy_tree(root, &source, Some(debian))?;
    apply_patches(&source)?;

    let overlay = stage.path().join("overlay");
    fs::create_dir(&overlay)?;
    fs::copy(debian.join("changelog"), overlay.join("changelog"))?;
    adapt_config(root, debian, stage.path())?;
    Ok(stage)
}

/// Recursively copies a source tree while excluding top-level build and VCS state.
fn copy_tree(source: &Path, destination: &Path, top_debian: Option<&Path>) -> Result<()> {
    fs::create_dir(destination).with_context(|| format!("create {}", destination.display()))?;
    fs::set_permissions(destination, fs::symlink_metadata(source)?.permissions())?;

    for entry in fs::read_dir(source).with_context(|| format!("read {}", source.display()))? {
        let entry = entry?;
        let name = entry.file_name();
        if top_debian.is_some() && (name == OsStr::new(".git") || name == OsStr::new("target")) {
            continue;
        }
        let from = entry.path();
        let to = destination.join(&name);
        if name == OsStr::new("debian")
            && let Some(debian) = top_debian
        {
            // Keep quilt and debcargo writes inside staging even when the real
            // package uses a symlinked debian directory.
            copy_tree(debian, &to, None)?;
            continue;
        }
        let metadata = fs::symlink_metadata(&from)?;
        if metadata.file_type().is_dir() {
            copy_tree(&from, &to, None)?;
        } else if metadata.file_type().is_file() {
            fs::copy(&from, &to)
                .with_context(|| format!("copy {} to {}", from.display(), to.display()))?;
            fs::set_permissions(&to, metadata.permissions())?;
        } else if metadata.file_type().is_symlink() {
            unix_fs::symlink(fs::read_link(&from)?, &to)?;
        } else {
            bail!(
                "unsupported special file in source tree: {}",
                from.display()
            );
        }
    }
    Ok(())
}

/// Applies every remaining quilt patch to the staged source tree.
fn apply_patches(source: &Path) -> Result<()> {
    let series = source.join("debian/patches/series");
    if !series.is_file()
        || !fs::read_to_string(&series)?
            .lines()
            .any(|line| !line.trim().is_empty() && !line.trim_start().starts_with('#'))
    {
        return Ok(());
    }

    let output = Command::new("quilt")
        .args(["push", "--quiltrc=-", "-a"])
        .env("QUILT_PATCHES", "debian/patches")
        .current_dir(source)
        .output()
        .context("run quilt push -a")?;
    if !output.status.success() && output.status.code() != Some(2) {
        bail!(
            "could not apply complete patch series:\n{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    Ok(())
}

/// Validates the in-tree config and rewrites its paths for staged generation.
fn adapt_config(root: &Path, debian: &Path, stage: &Path) -> Result<()> {
    let config_path = debian.join("debcargo.toml");
    let config_directory = root.join("debian");
    let mut config: DocumentMut = fs::read_to_string(&config_path)
        .with_context(|| format!("read {}", config_path.display()))?
        .parse()
        .with_context(|| format!("parse {}", config_path.display()))?;

    if let Some(overlay) = config.get("overlay")
        && overlay.as_str() != Some(".")
    {
        bail!("overlay must be omitted or \".\"");
    }
    if let Some(crate_src_path) = config.get("crate_src_path") {
        let Some(crate_src_path) = crate_src_path.as_str() else {
            bail!("crate_src_path must be a path string");
        };
        let crate_src_path = Path::new(crate_src_path);
        let crate_src_path = if crate_src_path.is_absolute() {
            crate_src_path.to_path_buf()
        } else {
            config_directory.join(crate_src_path)
        };
        if crate_src_path.canonicalize().ok().as_deref() != Some(root) {
            bail!("crate_src_path must be omitted or point to the source package");
        }
    }
    if let Some(item) = config.get("ubucargo") {
        let Some(table) = item.as_table() else {
            bail!("ubucargo must be a table");
        };
        if !table.is_empty() {
            bail!("[ubucargo] settings are not implemented yet");
        }
    }
    config.remove("ubucargo");

    config["overlay"] = value(require_utf8_path(&stage.join("overlay"))?);
    config["crate_src_path"] = value(require_utf8_path(&stage.join("source"))?);
    fs::write(stage.join("debcargo.toml"), config.to_string())?;
    Ok(())
}

/// Returns a path as UTF-8 for insertion into TOML.
fn require_utf8_path(path: &Path) -> Result<&str> {
    path.to_str()
        .with_context(|| format!("path is not valid UTF-8: {}", path.display()))
}

/// Runs debcargo against the staged local crate with network access disabled.
fn run_debcargo(stage: &TempDir, crate_name: &str) -> Result<()> {
    let output = Command::new("debcargo")
        .arg("package")
        .arg("--config")
        .arg(stage.path().join("debcargo.toml"))
        .arg("--directory")
        .arg(stage.path().join("output"))
        .arg("--no-overlay-write-back")
        .arg("--changelog-ready")
        .arg(crate_name)
        .env("CARGO_NET_OFFLINE", "true")
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
    Ok(())
}

/// Reads fresh debcargo outputs proposed for reconciliation, before any are accepted or written.
fn read_generated_candidates(stage: &Path) -> Result<BTreeMap<PathBuf, FileState>> {
    let output_debian = stage.join("output/debian");
    if !output_debian.is_dir() {
        bail!("debcargo produced no debian directory");
    }

    let mut generated = BTreeMap::new();
    let mut output_paths = BTreeSet::new();
    collect_output_paths(&output_debian, &output_debian, &mut output_paths)?;
    for path in output_paths {
        if is_package_managed(&path) {
            let state = read_state(&stage.join("output").join(&path))?
                .with_context(|| format!("missing generated {}", path.display()))?;
            generated.insert(path, state);
        } else if !is_expected_unmanaged_output(&path) {
            eprintln!(
                "warning: ignoring unrecognized debcargo output {}",
                path.display()
            );
        }
    }

    Ok(generated)
}

/// Adds file-like debcargo output paths to a package-relative result set.
fn collect_output_paths(
    directory: &Path,
    root: &Path,
    paths: &mut BTreeSet<PathBuf>,
) -> Result<()> {
    for entry in fs::read_dir(directory).with_context(|| format!("read {}", directory.display()))? {
        let entry = entry?;
        let path = entry.path();
        if fs::symlink_metadata(&path)?.file_type().is_dir() {
            collect_output_paths(&path, root, paths)?;
        } else {
            paths.insert(Path::new("debian").join(path.strip_prefix(root)?));
        }
    }
    Ok(())
}

/// Collects paths that the package command reconciles, regardless of current override status.
fn collect_managed_paths(
    debian: &Path,
    generated: &BTreeMap<PathBuf, FileState>,
) -> Result<BTreeSet<PathBuf>> {
    // Fixed paths remain managed when a later generator stops emitting them.
    let mut managed = BTreeSet::new();
    for path in PACKAGE_MANAGED_PATHS {
        managed.insert(PathBuf::from(path));
    }

    // Feature-package overrides are the only dynamic managed filename space.
    for path in generated.keys() {
        managed.insert(path.clone());
    }
    for entry in fs::read_dir(debian).with_context(|| format!("read {}", debian.display()))? {
        let entry = entry?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        let primary = name.strip_suffix(".debcargo.hint").unwrap_or(&name);
        let primary = PathBuf::from("debian").join(primary);
        if is_feature_override(&primary) {
            managed.insert(primary);
        }
    }
    Ok(managed)
}

/// Reports whether the package command recognizes a staged path as managed output.
fn is_package_managed(path: &Path) -> bool {
    PACKAGE_MANAGED_PATHS
        .iter()
        .any(|managed| path == Path::new(managed))
        || is_feature_override(path)
}

/// Reports whether a path belongs to the dynamic feature-package override namespace.
fn is_feature_override(path: &Path) -> bool {
    path.parent() == Some(Path::new("debian"))
        && path.file_name().is_some_and(|name| {
            let name = name.to_string_lossy();
            name.starts_with("librust-") && name.ends_with(".lintian-overrides")
        })
}

/// Reports whether debcargo is expected to emit a path that `package` intentionally ignores.
fn is_expected_unmanaged_output(path: &Path) -> bool {
    EXPECTED_UNMANAGED_OUTPUTS
        .iter()
        .any(|expected| path == Path::new(expected))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    /// Verifies that names, rather than arbitrary hints, determine managed paths.
    fn ignores_arbitrary_hint_names() {
        let directory = tempfile::tempdir().unwrap();
        fs::write(directory.path().join("custom.debcargo.hint"), "generated").unwrap();
        fs::write(
            directory
                .path()
                .join("librust-example-dev.lintian-overrides.debcargo.hint"),
            "generated",
        )
        .unwrap();

        let managed = collect_managed_paths(directory.path(), &BTreeMap::new()).unwrap();

        assert!(!managed.contains(Path::new("debian/custom")));
        assert!(managed.contains(Path::new("debian/librust-example-dev.lintian-overrides")));
        assert!(is_expected_unmanaged_output(Path::new(
            "debian/cargo-checksum.json"
        )));
        assert!(!is_expected_unmanaged_output(Path::new(
            "debian/upstream/metadata"
        )));
    }

    #[test]
    /// Verifies that in-tree relative paths are interpreted from `debian/`.
    fn validates_config_paths_relative_to_debian() {
        let root = tempfile::tempdir().unwrap();
        let stage = tempfile::tempdir().unwrap();
        fs::create_dir(root.path().join("debian")).unwrap();
        fs::create_dir(stage.path().join("source")).unwrap();
        fs::create_dir(stage.path().join("overlay")).unwrap();
        fs::write(
            root.path().join("debian/debcargo.toml"),
            "overlay = \".\"\ncrate_src_path = \"..\"\n\n[ubucargo]\n",
        )
        .unwrap();

        adapt_config(root.path(), &root.path().join("debian"), stage.path()).unwrap();

        let config: DocumentMut = fs::read_to_string(stage.path().join("debcargo.toml"))
            .unwrap()
            .parse()
            .unwrap();
        assert!(config.get("ubucargo").is_none());
        assert_eq!(
            config["overlay"].as_str(),
            stage.path().join("overlay").to_str()
        );
        assert_eq!(
            config["crate_src_path"].as_str(),
            stage.path().join("source").to_str()
        );
    }
}
