//! Adjusts generated packaging and selects managed files and patch metadata.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::{Context, Result, bail};

use crate::{
    command::run_command,
    config::{PackageConfig, write_package_config},
};
use debian_control::lossless::control::Control;

use super::managed::{FileState, PathPlan, build_plan, read_state};

const PACKAGE_MANAGED_PATHS: &[&str] = &[
    "debian/cargo-checksum.json",
    "debian/control",
    "debian/copyright",
    "debian/rules",
    "debian/tests/control",
    "debian/upstream/metadata",
    "debian/watch",
];
const EXPECTED_UNMANAGED_OUTPUTS: &[&str] = &["debian/changelog", "debian/source/format"];

/// Applies Ubuntu maintainer fields to staged debcargo output.
pub fn update_staged_maintainer(stage: &Path) -> Result<()> {
    run_command(
        Command::new("update-maintainer")
            .arg("--quiet")
            .arg("--debian-directory")
            .arg(stage.join("output/debian")),
        "update-maintainer",
    )?;
    Ok(())
}

/// Removes Debian packaging repository fields from generated control output.
pub fn remove_generated_vcs_fields(stage: &Path) -> Result<()> {
    let path = stage.join("output/debian/control");
    let control = Control::from_file(&path).with_context(|| format!("parse {}", path.display()))?;
    let mut source = control
        .source()
        .context("generated control has no source paragraph")?;
    source.as_mut_deb822().remove("Vcs-Git");
    source.as_mut_deb822().remove("Vcs-Browser");
    fs::write(&path, control.to_string()).with_context(|| format!("write {}", path.display()))
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

/// Collects current managed paths; planning also includes paths retained in the manifest.
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
        hint_before: None,
        primary_after: read_state(&stage.join("output/debian/patches/series"))?,
        hint_after: None,
        overridden: false,
        ambiguous: false,
    })
}

/// Adds the used Ubucargo configuration and generated-file baselines to a new staged package.
pub fn initialize_package(source: &Path, config: &PackageConfig) -> Result<()> {
    let debian = source.join("debian");
    write_package_config(config, source)?;
    let mut paths = BTreeSet::new();
    collect_output_paths(&debian, &debian, &mut paths)?;
    let mut generated = BTreeMap::new();
    for path in paths {
        if is_package_managed(&path) {
            let primary = source.join(&path);
            generated.insert(
                path,
                read_state(&primary)?.context("generated file disappeared")?,
            );
        }
    }
    build_plan(
        &debian,
        &collect_managed_paths(&debian, &generated)?,
        &generated,
        &BTreeMap::new(),
        &BTreeSet::new(),
        &BTreeSet::new(),
    )?
    .apply()
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
pub fn is_package_managed(path: &Path) -> bool {
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
        && path != Path::new("debian/patches/auto")
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
    use super::*;
    use crate::config::{get_package_config_path, read_new_package_config};

    #[test]
    /// Removes generated VCS fields without changing adjacent control fields.
    fn removes_generated_vcs_fields() {
        let stage = tempfile::tempdir().unwrap();
        let debian = stage.path().join("output/debian");
        fs::create_dir_all(&debian).unwrap();
        fs::write(
            debian.join("control"),
            concat!(
                "Source: rust-example\n",
                "Vcs-Git: https://salsa.debian.org/rust-team/debcargo-conf.git\n",
                " [src/example]\n",
                "Vcs-Browser: https://salsa.debian.org/rust-team/debcargo-conf/src/example\n",
                "Homepage: https://example.com\n",
            ),
        )
        .unwrap();

        remove_generated_vcs_fields(stage.path()).unwrap();

        assert_eq!(
            fs::read_to_string(debian.join("control")).unwrap(),
            "Source: rust-example\nHomepage: https://example.com\n"
        );
    }

    #[test]
    /// Verifies new-package initialization records baselines without redundant hints.
    fn initializes_config_and_manifest() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir_all(root.path().join("debian/patches/auto")).unwrap();
        fs::create_dir_all(root.path().join("debian/source")).unwrap();
        fs::write(root.path().join("debian/control"), "control").unwrap();
        fs::write(root.path().join("debian/changelog"), "changelog").unwrap();
        fs::write(root.path().join("debian/source/format"), "3.0 (quilt)\n").unwrap();
        fs::write(
            root.path().join("debian/patches/auto/change.patch"),
            "patch",
        )
        .unwrap();
        let config = read_new_package_config().unwrap();
        initialize_package(root.path(), &config).unwrap();
        assert_eq!(
            fs::read_to_string(get_package_config_path(root.path())).unwrap(),
            config.contents
        );
        assert!(!root.path().join("debian/control.debcargo.hint").exists());
        assert!(root.path().join("debian/ubucargo-state.json").is_file());
        let before = fs::read(root.path().join("debian/ubucargo-state.json")).unwrap();
        initialize_package(root.path(), &config).unwrap();
        assert_eq!(
            before,
            fs::read(root.path().join("debian/ubucargo-state.json")).unwrap()
        );
        assert!(!root.path().join("debian/changelog.debcargo.hint").exists());
        assert!(root.path().join("debian/source/format").is_file());
        assert!(
            !root
                .path()
                .join("debian/source/format.debcargo.hint")
                .exists()
        );
    }
}
