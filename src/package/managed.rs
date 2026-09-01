//! Plans and applies managed-file updates using generated-state hints.

use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::OsString,
    fs,
    io::Write,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};
use tempfile::NamedTempFile;

/// File content and Unix permission mode relevant to generated packaging.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FileState {
    /// Complete file contents.
    pub contents: Vec<u8>,
    /// Permission and special bits, excluding the file-type bits.
    pub mode: u32,
}

/// Planned reconciliation result for one managed primary file and its hint.
#[derive(Debug)]
pub struct PathPlan {
    /// Package-relative path of the primary file.
    pub path: PathBuf,
    /// Primary-file state observed in the working tree.
    pub old: Option<FileState>,
    /// Previous generator output stored in the hint.
    pub base: Option<FileState>,
    /// Primary-file state to leave after applying the plan.
    pub primary_after: Option<FileState>,
    /// Hint state to leave after applying the plan.
    pub hint_after: Option<FileState>,
    /// Whether this path uses a companion hint; false only for the patch series.
    pub tracks_hint: bool,
    /// Whether the working primary differs from its previous generated state.
    pub overridden: bool,
    /// Whether a missing hint requires an explicit keep-or-replace decision.
    pub ambiguous: bool,
}

impl PathPlan {
    /// Reports whether applying this path changes its primary file.
    fn has_primary_changed(&self) -> bool {
        self.old != self.primary_after
    }

    /// Reports whether applying this path changes its generated baseline.
    fn has_hint_changed(&self) -> bool {
        self.tracks_hint && self.base != self.hint_after
    }
}

/// Complete, validated set of filesystem changes for one package operation.
pub struct Plan {
    /// Resolved directory containing the package's Debian files.
    debian: PathBuf,
    /// Per-path reconciliation results in deterministic order.
    pub paths: Vec<PathPlan>,
}

impl Plan {
    /// Reports whether applying the plan performs any filesystem changes.
    pub fn has_changes(&self) -> bool {
        self.paths
            .iter()
            .any(|path| path.has_primary_changed() || path.has_hint_changed())
    }

    /// Collects paths that require an explicit keep-or-replace decision.
    pub fn collect_ambiguities(&self) -> Vec<&Path> {
        let mut ambiguities = Vec::new();
        for path in &self.paths {
            if path.ambiguous {
                ambiguities.push(path.path.as_path());
            }
        }
        ambiguities
    }

    /// Prints the deterministic, path-oriented summary of the plan.
    pub fn print_report(&self) {
        for path in &self.paths {
            if path.ambiguous {
                println!(
                    "ambiguous {} (use --keep or --replace)",
                    path.path.display()
                );
                continue;
            }

            if path.has_primary_changed() {
                println!(
                    "{} {}",
                    describe_change(&path.old, &path.primary_after),
                    path.path.display()
                );
            } else if path.overridden {
                println!("preserve override {}", path.path.display());
            }

            if path.has_hint_changed() {
                println!(
                    "{} {}",
                    describe_change(&path.base, &path.hint_after),
                    make_hint_path(&path.path).display()
                );
            }
        }
    }

    /// Applies primary changes first and writes generated baselines last.
    pub fn apply(&self) -> Result<()> {
        // Install new generated files before changing references such as the
        // patch series, then remove obsolete files and update hints last.
        for path in &self.paths {
            if path.has_primary_changed() && path.primary_after.is_some() {
                install_state(
                    &resolve_managed_path(&self.debian, &path.path)?,
                    path.primary_after.as_ref(),
                )
                .context("package may be partially updated; rerun `ubucargo package`")?;
            }
        }
        for path in &self.paths {
            if path.has_primary_changed() && path.primary_after.is_none() {
                install_state(&resolve_managed_path(&self.debian, &path.path)?, None)
                    .context("package may be partially updated; rerun `ubucargo package`")?;
            }
        }
        for path in &self.paths {
            if path.has_hint_changed() {
                install_state(
                    &resolve_managed_path(&self.debian, &make_hint_path(&path.path))?,
                    path.hint_after.as_ref(),
                )
                .context("package may be partially updated; rerun `ubucargo package`")?;
            }
        }
        Ok(())
    }
}

/// Compares previous output, working files, and new candidates without modifying the package.
pub fn build_plan(
    debian: &Path,
    managed: &BTreeSet<PathBuf>,
    generated: &BTreeMap<PathBuf, FileState>,
    keep: &BTreeSet<PathBuf>,
    replace: &BTreeSet<PathBuf>,
) -> Result<Plan> {
    let mut paths = Vec::new();
    let mut used_decisions = BTreeSet::new();

    for path in managed {
        let old = read_state(&resolve_managed_path(debian, path)?)?;
        let base = read_state(&resolve_managed_path(debian, &make_hint_path(path))?)?;
        let new = generated.get(path).cloned();
        let ambiguous =
            base.is_none() && matches!((&old, &new), (Some(old), Some(new)) if old != new);
        let decision_replace = if ambiguous {
            match (keep.contains(path), replace.contains(path)) {
                (true, false) => {
                    used_decisions.insert(path.clone());
                    Some(false)
                }
                (false, true) => {
                    used_decisions.insert(path.clone());
                    Some(true)
                }
                (false, false) => None,
                (true, true) => bail!(
                    "{} cannot be named by both --keep and --replace",
                    path.display()
                ),
            }
        } else {
            None
        };

        // A managed path is not necessarily generator-controlled: when the
        // primary differs from its hint, the primary is a maintainer override.
        let (primary_after, hint_after, overridden, unresolved) = match &base {
            // The primary still matches the last generated state, so accept the
            // new generated state as both the primary and the hint.
            Some(base) if old.as_ref() == Some(base) => (new.clone(), new.clone(), false, false),
            // The primary no longer matches the last generated state, so it is
            // a maintainer override: preserve it while tracking the new state.
            Some(_) => (old.clone(), new.clone(), true, false),
            None => match (&old, &new) {
                // Neither the working tree nor the generator owns this path.
                (None, None) => (None, None, false, false),
                // The generator introduced this path, so install it with a hint.
                (None, Some(_)) => (new.clone(), new.clone(), false, false),
                // Without a hint, an unchanged primary is already correct.
                (Some(old), Some(new)) if old == new => {
                    (old.clone().into(), new.clone().into(), false, false)
                }
                // Without a hint, a differing primary could be either an
                // override or stale generated output; ask the maintainer.
                (Some(_), Some(_)) => match decision_replace {
                    // Keep the current primary as an override while recording
                    // the new generated state in its hint.
                    Some(false) => (old.clone(), new.clone(), true, false),
                    // Replace the primary with the new generated state.
                    Some(true) => (new.clone(), new.clone(), false, false),
                    // Leave the ambiguity unresolved for the caller to report.
                    None => (old.clone(), None, false, true),
                },
                // The primary has no hint, so it predates hint tracking;
                // retain it because there is no generated replacement.
                (Some(_), None) => (old.clone(), None, false, false),
            },
        };

        paths.push(PathPlan {
            path: path.clone(),
            old,
            base,
            primary_after,
            hint_after,
            tracks_hint: true,
            overridden,
            ambiguous: unresolved,
        });
    }

    let mut unused = Vec::new();
    for path in keep.union(replace) {
        if !used_decisions.contains(path) {
            unused.push(path);
        }
    }
    if !unused.is_empty() {
        let mut names = Vec::new();
        for path in unused {
            names.push(path.display().to_string());
        }
        bail!(
            "--keep/--replace only accept ambiguous paths; not ambiguous: {}",
            names.join(", ")
        );
    }

    Ok(Plan {
        debian: debian.to_path_buf(),
        paths,
    })
}

/// Reads a regular file's contents and permission mode, preserving absence distinctly.
pub fn read_state(path: &Path) -> Result<Option<FileState>> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error).with_context(|| format!("inspect {}", path.display())),
    };

    if !metadata.file_type().is_file() {
        bail!("generated path is not a regular file: {}", path.display());
    }

    Ok(Some(FileState {
        contents: fs::read(path).with_context(|| format!("read {}", path.display()))?,
        mode: metadata.permissions().mode() & 0o7777,
    }))
}

/// Resolves a package-relative managed path beneath the selected Debian directory.
fn resolve_managed_path(debian: &Path, path: &Path) -> Result<PathBuf> {
    Ok(debian.join(
        path.strip_prefix("debian")
            .with_context(|| format!("generated path is outside debian/: {}", path.display()))?,
    ))
}

/// Atomically replaces one file with the requested state, or removes it.
pub(crate) fn install_state(path: &Path, state: Option<&FileState>) -> Result<()> {
    let parent = path
        .parent()
        .with_context(|| format!("{} has no parent directory", path.display()))?;

    match state {
        Some(state) => {
            fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
            let mut temporary = NamedTempFile::new_in(parent)
                .with_context(|| format!("create temporary file in {}", parent.display()))?;
            temporary
                .write_all(&state.contents)
                .with_context(|| format!("write temporary file for {}", path.display()))?;
            temporary
                .as_file()
                .set_permissions(fs::Permissions::from_mode(state.mode))
                .with_context(|| format!("set mode for {}", path.display()))?;
            temporary
                .persist(path)
                .map_err(|error| error.error)
                .with_context(|| format!("replace {}", path.display()))?;
        }
        None => match fs::remove_file(path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error).with_context(|| format!("remove {}", path.display())),
        },
    }

    Ok(())
}

/// Derives the companion `.debcargo.hint` path for a managed primary file.
pub fn make_hint_path(path: &Path) -> PathBuf {
    let mut name: OsString = path.file_name().expect("generated path has a name").into();
    name.push(".debcargo.hint");
    path.with_file_name(name)
}

/// Selects the user-facing verb for a file-state transition.
fn describe_change(before: &Option<FileState>, after: &Option<FileState>) -> &'static str {
    match (before, after) {
        (None, Some(_)) => "create",
        (Some(_), None) => "remove",
        _ => "update",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Creates a non-executable text state for planner tests.
    fn make_state(value: &str) -> FileState {
        FileState {
            contents: value.as_bytes().to_vec(),
            mode: 0o644,
        }
    }

    #[test]
    /// Verifies override preservation and explicit resolution of missing baselines.
    fn preserves_overrides_and_requires_ambiguous_decisions() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path();
        let debian = root.join("debian");
        fs::create_dir(&debian).unwrap();
        let path = PathBuf::from("debian/control");
        let managed = BTreeSet::from([path.clone()]);

        install_state(&root.join(&path), Some(&make_state("maintainer"))).unwrap();
        install_state(&root.join(make_hint_path(&path)), Some(&make_state("base"))).unwrap();
        let generated = BTreeMap::from([(path.clone(), make_state("new"))]);
        let plan = build_plan(
            &debian,
            &managed,
            &generated,
            &BTreeSet::new(),
            &BTreeSet::new(),
        )
        .unwrap();
        assert_eq!(plan.paths[0].primary_after, Some(make_state("maintainer")));
        assert_eq!(plan.paths[0].hint_after, Some(make_state("new")));

        fs::remove_file(root.join(make_hint_path(&path))).unwrap();
        let plan = build_plan(
            &debian,
            &managed,
            &generated,
            &BTreeSet::new(),
            &BTreeSet::new(),
        )
        .unwrap();
        assert_eq!(plan.collect_ambiguities(), vec![path.as_path()]);

        let plan = build_plan(
            &debian,
            &managed,
            &generated,
            &BTreeSet::from([path.clone()]),
            &BTreeSet::new(),
        )
        .unwrap();
        assert!(!plan.paths[0].ambiguous);
        assert_eq!(plan.paths[0].primary_after, Some(make_state("maintainer")));
        assert_eq!(plan.paths[0].hint_after, Some(make_state("new")));

        let plan = build_plan(
            &debian,
            &managed,
            &generated,
            &BTreeSet::new(),
            &BTreeSet::from([path.clone()]),
        )
        .unwrap();
        assert_eq!(plan.paths[0].primary_after, Some(make_state("new")));
        assert_eq!(plan.paths[0].hint_after, Some(make_state("new")));
    }

    #[test]
    /// Verifies creation, generator deletion, and maintainer deletion behavior.
    fn handles_creation_and_deletion() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path();
        let debian = root.join("debian");
        fs::create_dir(&debian).unwrap();
        let path = PathBuf::from("debian/control");
        let managed = BTreeSet::from([path.clone()]);

        let generated = BTreeMap::from([(path.clone(), make_state("new"))]);
        let plan = build_plan(
            &debian,
            &managed,
            &generated,
            &BTreeSet::new(),
            &BTreeSet::new(),
        )
        .unwrap();
        assert_eq!(plan.paths[0].primary_after, Some(make_state("new")));
        assert_eq!(plan.paths[0].hint_after, Some(make_state("new")));

        install_state(&root.join(&path), Some(&make_state("base"))).unwrap();
        install_state(&root.join(make_hint_path(&path)), Some(&make_state("base"))).unwrap();
        let plan = build_plan(
            &debian,
            &managed,
            &BTreeMap::new(),
            &BTreeSet::new(),
            &BTreeSet::new(),
        )
        .unwrap();
        assert_eq!(plan.paths[0].primary_after, None);
        assert_eq!(plan.paths[0].hint_after, None);

        fs::remove_file(root.join(&path)).unwrap();
        let plan = build_plan(
            &debian,
            &managed,
            &BTreeMap::new(),
            &BTreeSet::new(),
            &BTreeSet::new(),
        )
        .unwrap();
        assert_eq!(plan.paths[0].primary_after, None);
        assert_eq!(plan.paths[0].hint_after, None);
        assert!(plan.paths[0].overridden);
    }

    #[test]
    /// Verifies installation of matching primary and hint contents and modes.
    fn writes_primary_and_hint_with_the_generated_mode() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path();
        let debian = root.join("debian");
        fs::create_dir(&debian).unwrap();
        let path = PathBuf::from("debian/rules");
        let generated = FileState {
            contents: b"#!/usr/bin/make -f\n".to_vec(),
            mode: 0o750,
        };
        let plan = build_plan(
            &debian,
            &BTreeSet::from([path.clone()]),
            &BTreeMap::from([(path.clone(), generated.clone())]),
            &BTreeSet::new(),
            &BTreeSet::new(),
        )
        .unwrap();

        plan.apply().unwrap();

        assert_eq!(
            read_state(&root.join(&path)).unwrap(),
            Some(generated.clone())
        );
        assert_eq!(
            read_state(&root.join(make_hint_path(&path))).unwrap(),
            Some(generated)
        );

        fs::set_permissions(root.join(&path), fs::Permissions::from_mode(0o700)).unwrap();
        let plan = build_plan(
            &debian,
            &BTreeSet::from([path.clone()]),
            &BTreeMap::from([(
                path,
                FileState {
                    contents: b"#!/usr/bin/make -f\n".to_vec(),
                    mode: 0o750,
                },
            )]),
            &BTreeSet::new(),
            &BTreeSet::new(),
        )
        .unwrap();
        assert!(plan.paths[0].overridden);
        assert_eq!(plan.paths[0].primary_after.as_ref().unwrap().mode, 0o700);
    }
}
