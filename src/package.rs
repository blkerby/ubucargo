use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::OsStr,
    fs::{self, File},
    os::unix::fs as unix_fs,
    path::{Component, Path, PathBuf},
    process::Command,
};

use anyhow::{Context, Result, bail};
use fs2::FileExt;
use serde::Deserialize;
use tempfile::TempDir;
use toml_edit::{DocumentMut, value};

use crate::materialize::{FileState, PathPlan, build_plan, read_state};

const DEBCARGO_VERSION: &str = "debcargo 2.8.4";
const PACKAGE_MANAGED_PATHS: &[&str] = &[
    "debian/cargo-checksum.json",
    "debian/control",
    "debian/copyright",
    "debian/rules",
    "debian/source/format",
    "debian/tests/control",
    "debian/upstream/metadata",
    "debian/watch",
];
const EXPECTED_UNMANAGED_OUTPUTS: &[&str] = &["debian/changelog"];

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
    /// Exact Cargo package version passed to debcargo.
    version: String,
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
    let debian = root.join("debian");
    let lock = File::open(&debian).with_context(|| format!("open {}", debian.display()))?;
    if let Err(error) = lock.try_lock_exclusive() {
        if error.kind() == std::io::ErrorKind::WouldBlock {
            bail!("another process is using {}", root.display());
        }
        return Err(error).with_context(|| format!("lock {}", debian.display()));
    }

    let mut keep_paths = BTreeSet::new();
    for path in keep {
        keep_paths.insert(path.clone());
    }
    let mut replace_paths = BTreeSet::new();
    for path in replace {
        replace_paths.insert(path.clone());
    }
    if let Some(path) = keep_paths.intersection(&replace_paths).next() {
        bail!(
            "{} cannot be named by both --keep and --replace",
            path.display()
        );
    }

    check_debcargo_version()?;
    let patches_applied = check_patch_state(&root)?;
    let package = read_root_package(&root)?;
    let stage = stage_package(&root, &debian)?;
    run_debcargo(&stage, &package)?;
    let generated = read_generated_candidates(stage.path())?;
    // "Managed" means the path participates in hint reconciliation. Its current
    // primary file may still be a maintainer override.
    let managed = collect_managed_paths(&debian, &generated)?;
    let mut plan = build_plan(&debian, &managed, &generated, &keep_paths, &replace_paths)?;
    plan.paths
        .push(build_patch_series_plan(&debian, stage.path())?);
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
    if !check && patches_applied {
        for path in &plan.paths {
            let generated_patch_changed =
                path.path == Path::new("debian/patches/series") || is_auto_patch(&path.path);
            if generated_patch_changed && path.old != path.primary_after {
                bail!(
                    "pop the real quilt stack before applying generated patch changes; use --check to preview them"
                );
            }
        }
    }
    if !check && changed {
        plan.apply()?;
    }
    if !changed {
        println!("clean");
    }

    Ok(check && changed)
}

/// Uses an explicit package root or searches upward from the current directory.
fn find_package_root(package: Option<&Path>) -> Result<PathBuf> {
    let root = if let Some(path) = package {
        let root = path
            .canonicalize()
            .with_context(|| format!("resolve {}", path.display()))?;
        if !has_debcargo_config(&root) {
            bail!(
                "{} is not a source-package root with debian/debcargo.toml",
                root.display()
            );
        }
        root
    } else {
        let start = std::env::current_dir()
            .context("get current directory")?
            .canonicalize()
            .context("resolve current directory")?;
        let mut root = None;
        for candidate in start.ancestors() {
            if has_debcargo_config(candidate) {
                root = Some(candidate.to_path_buf());
                break;
            }
        }
        let Some(root) = root else {
            bail!(
                "{} is not inside a source package with debian/debcargo.toml",
                start.display()
            );
        };
        root
    };

    Ok(root)
}

/// Reports whether a directory contains Ubucargo's source-package marker.
fn has_debcargo_config(path: &Path) -> bool {
    path.join("debian/debcargo.toml").is_file()
}

/// Uses Cargo to identify the package defined by the root manifest.
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

/// Prepares the debcargo overlay and adapted registry-backed configuration.
fn stage_package(root: &Path, debian: &Path) -> Result<TempDir> {
    let changelog = debian.join("changelog");
    if !changelog.is_file() {
        bail!(
            "{} has no debian/changelog; initial changelog creation is not implemented",
            root.display()
        );
    }

    let stage = tempfile::tempdir().context("create staging directory")?;
    let overlay = stage.path().join("overlay");
    fs::create_dir(&overlay)?;
    fs::copy(changelog, overlay.join("changelog"))?;
    prepare_patch_overlay(debian, &overlay)?;
    adapt_config(debian, stage.path())?;
    Ok(stage)
}

/// Recursively copies a source tree while keeping symlink targets inside staging.
fn copy_tree(
    source: &Path,
    destination: &Path,
    source_root: &Path,
    destination_root: &Path,
) -> Result<()> {
    fs::create_dir(destination).with_context(|| format!("create {}", destination.display()))?;
    fs::set_permissions(destination, fs::symlink_metadata(source)?.permissions())?;

    for entry in fs::read_dir(source).with_context(|| format!("read {}", source.display()))? {
        let entry = entry?;
        let name = entry.file_name();
        if name == OsStr::new(".git") || name == OsStr::new("target") {
            continue;
        }
        let from = entry.path();
        let to = destination.join(&name);
        let metadata = fs::symlink_metadata(&from)?;
        if metadata.file_type().is_dir() {
            copy_tree(&from, &to, source_root, destination_root)?;
        } else if metadata.file_type().is_file() {
            fs::copy(&from, &to)
                .with_context(|| format!("copy {} to {}", from.display(), to.display()))?;
            fs::set_permissions(&to, metadata.permissions())?;
        } else if metadata.file_type().is_symlink() {
            let link_target = fs::read_link(&from)?;
            let source_target = if link_target.is_absolute() {
                link_target
            } else {
                from.parent().unwrap().join(link_target)
            };
            let source_target = source_target
                .canonicalize()
                .with_context(|| format!("resolve symlink {}", from.display()))?;
            let relative_target = source_target.strip_prefix(source_root).with_context(|| {
                format!("symlink {} points outside the source tree", from.display())
            })?;
            if relative_target.components().any(|component| {
                matches!(component, Component::Normal(name) if name == ".git" || name == "target")
            }) {
                bail!("symlink {} points to excluded source content", from.display());
            }
            let staged_target = destination_root.join(relative_target);
            let staged_link_target =
                make_relative_link_target(&to, &staged_target, destination_root)?;
            unix_fs::symlink(staged_link_target, &to)?;
        } else {
            bail!(
                "unsupported special file in source tree: {}",
                from.display()
            );
        }
    }
    Ok(())
}

/// Calculates a relative symlink target between two paths inside the staging root.
fn make_relative_link_target(link: &Path, target: &Path, root: &Path) -> Result<PathBuf> {
    let link_parent = link
        .parent()
        .with_context(|| format!("{} has no parent directory", link.display()))?;
    let from = link_parent.strip_prefix(root)?;
    let to = target.strip_prefix(root)?;
    let mut from_parts = Vec::new();
    let mut to_parts = Vec::new();

    for component in from.components() {
        if let Component::Normal(part) = component {
            from_parts.push(part);
        }
    }
    for component in to.components() {
        if let Component::Normal(part) = component {
            to_parts.push(part);
        }
    }

    let mut common = 0;
    while common < from_parts.len()
        && common < to_parts.len()
        && from_parts[common] == to_parts[common]
    {
        common += 1;
    }

    let mut relative = PathBuf::new();
    for _ in common..from_parts.len() {
        relative.push("..");
    }
    for part in &to_parts[common..] {
        relative.push(part);
    }
    if relative.as_os_str().is_empty() {
        relative.push(".");
    }
    Ok(relative)
}

/// Copies the complete patch set into the registry-backed debcargo overlay.
fn prepare_patch_overlay(debian: &Path, overlay: &Path) -> Result<()> {
    let patches = debian.join("patches");
    if !patches.is_dir() {
        return Ok(());
    }

    let overlay_patches = overlay.join("patches");
    let patches_root = patches.canonicalize()?;
    copy_tree(
        &patches_root,
        &overlay_patches,
        &patches_root,
        &overlay_patches,
    )?;

    let series = overlay_patches.join("series");
    if series.is_file()
        && !fs::read_to_string(&series)?
            .lines()
            .any(|line| !line.trim().is_empty() && !line.trim_start().starts_with('#'))
    {
        fs::remove_file(series)?;
    }
    Ok(())
}

/// Rejects unrefreshed top-patch edits and reports whether any patches are applied.
fn check_patch_state(source: &Path) -> Result<bool> {
    let path = source.join(".pc/applied-patches");
    let contents = match fs::read_to_string(&path) {
        Ok(contents) => contents,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error).with_context(|| format!("read {}", path.display())),
    };
    for line in contents.lines() {
        if !line.trim().is_empty() {
            let output = Command::new("quilt")
                .args(["diff", "--quiltrc=-", "-z", "--no-timestamps", "--no-index"])
                .env("QUILT_PATCHES", "debian/patches")
                .current_dir(source)
                .output()
                .context("run quilt diff -z")?;
            if !output.status.success() {
                bail!(
                    "could not inspect the current quilt patch:\n{}{}",
                    String::from_utf8_lossy(&output.stdout),
                    String::from_utf8_lossy(&output.stderr)
                );
            }
            if !output.stdout.is_empty() {
                bail!("the current quilt patch has unrefreshed changes; run `quilt refresh`");
            }
            return Ok(true);
        }
    }
    Ok(false)
}

/// Validates the in-tree config and rewrites its overlay for registry-backed generation.
fn adapt_config(debian: &Path, stage: &Path) -> Result<()> {
    let config_path = debian.join("debcargo.toml");
    let mut config: DocumentMut = fs::read_to_string(&config_path)
        .with_context(|| format!("read {}", config_path.display()))?
        .parse()
        .with_context(|| format!("parse {}", config_path.display()))?;

    if let Some(overlay) = config.get("overlay")
        && overlay.as_str() != Some(".")
    {
        bail!("overlay must be omitted or \".\"");
    }
    if config.get("crate_src_path").is_some() {
        bail!("crate_src_path is not supported by ubucargo package");
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
    fs::write(stage.join("debcargo.toml"), config.to_string())?;
    Ok(())
}

/// Returns a path as UTF-8 for insertion into TOML.
fn require_utf8_path(path: &Path) -> Result<&str> {
    path.to_str()
        .with_context(|| format!("path is not valid UTF-8: {}", path.display()))
}

/// Runs debcargo against the staged local crate with network access disabled.
fn run_debcargo(stage: &TempDir, package: &MetadataPackage) -> Result<()> {
    let output = Command::new("debcargo")
        .arg("package")
        .arg("--config")
        .arg(stage.path().join("debcargo.toml"))
        .arg("--directory")
        .arg(stage.path().join("output"))
        .arg("--no-overlay-write-back")
        .arg("--changelog-ready")
        .arg(&package.name)
        .arg(&package.version)
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

    let auto_dir = debian.join("patches/auto");
    if auto_dir.is_dir() {
        let mut auto_paths = BTreeSet::new();
        collect_output_paths(&auto_dir, debian, &mut auto_paths)?;
        for mut path in auto_paths {
            let name = path.file_name().unwrap().to_string_lossy();
            let primary = name.strip_suffix(".debcargo.hint").map(str::to_owned);
            if let Some(primary) = primary {
                path.set_file_name(primary);
            }
            if is_auto_patch(&path) {
                managed.insert(path);
            }
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
        || is_auto_patch(path)
}

/// Reports whether a path belongs to the dynamic feature-package override namespace.
fn is_feature_override(path: &Path) -> bool {
    path.parent() == Some(Path::new("debian"))
        && path.file_name().is_some_and(|name| {
            let name = name.to_string_lossy();
            name.starts_with("librust-") && name.ends_with(".lintian-overrides")
        })
}

/// Reports whether a path belongs to debcargo's generated auto-patch namespace.
fn is_auto_patch(path: &Path) -> bool {
    path.starts_with("debian/patches/auto")
        && !path
            .file_name()
            .is_some_and(|name| name.to_string_lossy().ends_with(".debcargo.hint"))
}

/// Reports whether debcargo is expected to emit a path that `package` intentionally ignores.
fn is_expected_unmanaged_output(path: &Path) -> bool {
    path.starts_with("debian/patches")
        || EXPECTED_UNMANAGED_OUTPUTS
            .iter()
            .any(|expected| path == Path::new(expected))
}

/// Builds the mixed-ownership patch-series update from generated auto entries and real manual entries.
fn build_patch_series_plan(debian: &Path, stage: &Path) -> Result<PathPlan> {
    let path = PathBuf::from("debian/patches/series");
    let old = read_state(&debian.join("patches/series"))?;
    let generated = read_state(&stage.join("output/debian/patches/series"))?;
    let primary_after = merge_patch_series(old.as_ref(), generated.as_ref())?;

    Ok(PathPlan {
        path,
        old,
        base: None,
        primary_after,
        hint_after: None,
        tracks_hint: false,
        overridden: false,
        ambiguous: false,
    })
}

/// Merges generated `auto/` entries with the maintainer-owned portion of a patch series.
fn merge_patch_series(
    current: Option<&FileState>,
    generated: Option<&FileState>,
) -> Result<Option<FileState>> {
    let mut contents = Vec::new();

    if let Some(generated) = generated {
        let text = std::str::from_utf8(&generated.contents)
            .context("generated patch series is not UTF-8")?;
        for line in text.split_inclusive('\n') {
            if is_auto_series_line(line) {
                contents.extend_from_slice(line.as_bytes());
            }
        }
    }

    if let Some(current) = current {
        let text =
            std::str::from_utf8(&current.contents).context("existing patch series is not UTF-8")?;
        for line in text.split_inclusive('\n') {
            if !is_auto_series_line(line) {
                if !contents.is_empty() && !contents.ends_with(b"\n") {
                    contents.push(b'\n');
                }
                contents.extend_from_slice(line.as_bytes());
            }
        }
    }

    if contents.is_empty() && current.is_none() {
        return Ok(None);
    }
    let mode = if let Some(current) = current {
        current.mode
    } else if let Some(generated) = generated {
        generated.mode
    } else {
        0o644
    };
    Ok(Some(FileState { contents, mode }))
}

/// Reports whether a patch-series line belongs to debcargo's generated auto namespace.
fn is_auto_series_line(line: &str) -> bool {
    let line = line.trim_start();
    if line.is_empty() || line.starts_with('#') {
        return false;
    }
    line.split_whitespace()
        .next()
        .is_some_and(|name| name.starts_with("auto/"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    /// Verifies that generated auto entries replace only the generated portion of a series.
    fn merges_auto_patch_entries_into_manual_series() {
        let current = FileState {
            contents: b"# manual patches\nfix.patch -p0\nauto/old.patch\n".to_vec(),
            mode: 0o640,
        };
        let generated = FileState {
            contents: b"auto/new.patch\nauto/second.patch\nfix.patch -p0\n".to_vec(),
            mode: 0o644,
        };

        let merged = merge_patch_series(Some(&current), Some(&generated))
            .unwrap()
            .unwrap();

        assert_eq!(
            merged.contents,
            b"auto/new.patch\nauto/second.patch\n# manual patches\nfix.patch -p0\n"
        );
        assert_eq!(merged.mode, 0o640);
    }

    #[test]
    /// Verifies that staged symlinks stay inside staging and escaping links fail.
    fn validates_symlink_targets_while_copying() {
        let source = tempfile::tempdir().unwrap();
        let destination = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        fs::write(source.path().join("payload"), "inside").unwrap();
        unix_fs::symlink(
            source.path().join("payload"),
            source.path().join("inside-link"),
        )
        .unwrap();

        let staged = destination.path().join("source");
        copy_tree(source.path(), &staged, source.path(), &staged).unwrap();
        let staged_link = fs::read_link(staged.join("inside-link")).unwrap();
        assert!(!staged_link.is_absolute());
        assert_eq!(
            staged.join(staged_link).canonicalize().unwrap(),
            staged.join("payload").canonicalize().unwrap()
        );

        unix_fs::symlink(outside.path(), source.path().join("outside-link")).unwrap();
        let rejected = destination.path().join("rejected");
        assert!(copy_tree(source.path(), &rejected, source.path(), &rejected).is_err());
    }

    #[test]
    /// Verifies that an explicit path is validated directly instead of searched upward.
    fn treats_explicit_package_path_as_root() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir_all(root.path().join("debian")).unwrap();
        fs::create_dir(root.path().join("nested")).unwrap();
        fs::write(root.path().join("debian/debcargo.toml"), "").unwrap();
        assert_eq!(
            find_package_root(Some(root.path())).unwrap(),
            root.path().canonicalize().unwrap()
        );
        assert!(find_package_root(Some(&root.path().join("nested"))).is_err());
    }

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
        assert!(is_package_managed(Path::new("debian/cargo-checksum.json")));
        assert!(!is_expected_unmanaged_output(Path::new(
            "debian/upstream/metadata"
        )));
    }

    #[test]
    /// Verifies that registry-backed staging rewrites only the overlay path.
    fn adapts_config_for_registry_generation() {
        let root = tempfile::tempdir().unwrap();
        let stage = tempfile::tempdir().unwrap();
        fs::create_dir(root.path().join("debian")).unwrap();
        fs::create_dir(stage.path().join("source")).unwrap();
        fs::create_dir(stage.path().join("overlay")).unwrap();
        fs::write(
            root.path().join("debian/debcargo.toml"),
            "overlay = \".\"\n\n[ubucargo]\n",
        )
        .unwrap();

        adapt_config(&root.path().join("debian"), stage.path()).unwrap();

        let config: DocumentMut = fs::read_to_string(stage.path().join("debcargo.toml"))
            .unwrap()
            .parse()
            .unwrap();
        assert!(config.get("ubucargo").is_none());
        assert_eq!(
            config["overlay"].as_str(),
            stage.path().join("overlay").to_str()
        );
        assert!(config.get("crate_src_path").is_none());
    }
}
