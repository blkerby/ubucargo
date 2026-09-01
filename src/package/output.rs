//! Classifies debcargo output and manages generated packaging metadata.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::{Context, Result, bail};

use super::{
    generate::PackageConfig,
    managed::{FileState, PathPlan, make_hint_path, read_state},
};

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

/// Rejects unrefreshed top-patch edits and reports whether any patches are applied.
pub fn check_patch_state(source: &Path) -> Result<bool> {
    let path = source.join(".pc/applied-patches");
    let contents = match fs::read_to_string(&path) {
        Ok(contents) => contents,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error).with_context(|| format!("read {}", path.display())),
    };
    if !contents.lines().any(|line| !line.trim().is_empty()) {
        return Ok(false);
    }
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
    Ok(true)
}

/// Reports whether generated automatic patches or their series change.
pub fn generated_patch_changes(plan: &super::managed::Plan) -> bool {
    for path in &plan.paths {
        let generated_patch_changed =
            path.path == Path::new("debian/patches/series") || is_auto_patch(&path.path);
        if generated_patch_changed && path.old != path.primary_after {
            return true;
        }
    }
    false
}

/// Reads fresh debcargo outputs proposed for reconciliation.
pub fn read_generated_candidates(stage: &Path) -> Result<BTreeMap<PathBuf, FileState>> {
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

/// Collects paths reconciled by generated-file hint rules.
pub fn collect_managed_paths(
    debian: &Path,
    generated: &BTreeMap<PathBuf, FileState>,
) -> Result<BTreeSet<PathBuf>> {
    let mut managed = BTreeSet::new();
    for path in PACKAGE_MANAGED_PATHS {
        managed.insert(PathBuf::from(path));
    }
    for path in generated.keys() {
        managed.insert(path.clone());
    }
    for entry in fs::read_dir(debian)? {
        let name = entry?.file_name();
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
            let primary = path
                .file_name()
                .unwrap()
                .to_string_lossy()
                .strip_suffix(".debcargo.hint")
                .map(str::to_owned);
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

/// Builds the patch-series update from debcargo's merged output.
pub fn build_patch_series_plan(debian: &Path, stage: &Path) -> Result<PathPlan> {
    Ok(PathPlan {
        path: PathBuf::from("debian/patches/series"),
        old: read_state(&debian.join("patches/series"))?,
        base: None,
        primary_after: read_state(&stage.join("output/debian/patches/series"))?,
        hint_after: None,
        tracks_hint: false,
        overridden: false,
        ambiguous: false,
    })
}

/// Adds the used Ubucargo configuration and generated-file baselines to a new staged package.
pub fn initialize_package(source: &Path, config: &PackageConfig) -> Result<()> {
    let debian = source.join("debian");
    fs::write(debian.join("debcargo.toml"), &config.contents)?;
    let mut paths = BTreeSet::new();
    collect_output_paths(&debian, &debian, &mut paths)?;
    for path in paths {
        if !is_package_managed(&path) {
            continue;
        }
        let primary = debian.join(path.strip_prefix("debian")?);
        let hint = make_hint_path(&primary);
        fs::copy(&primary, &hint)?;
    }
    Ok(())
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

/// Reports whether debcargo is expected to emit a path that Ubucargo intentionally ignores.
fn is_expected_unmanaged_output(path: &Path) -> bool {
    path.starts_with("debian/patches")
        || EXPECTED_UNMANAGED_OUTPUTS
            .iter()
            .any(|expected| path == Path::new(expected))
}

#[cfg(test)]
mod tests {
    use super::super::generate::read_new_package_config;
    use super::*;

    #[test]
    /// Verifies new-package initialization creates hints only for managed output.
    fn initializes_config_and_managed_hints() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir_all(root.path().join("debian/patches/auto")).unwrap();
        fs::write(root.path().join("debian/control"), "control").unwrap();
        fs::write(root.path().join("debian/changelog"), "changelog").unwrap();
        fs::write(
            root.path().join("debian/patches/auto/change.patch"),
            "patch",
        )
        .unwrap();
        initialize_package(root.path(), &read_new_package_config().unwrap()).unwrap();
        assert_eq!(
            fs::read_to_string(root.path().join("debian/debcargo.toml")).unwrap(),
            "[ubucargo]\n"
        );
        assert!(root.path().join("debian/control.debcargo.hint").is_file());
        assert!(!root.path().join("debian/changelog.debcargo.hint").exists());
    }
}
