//! Tracks generated fingerprints and preserves maintainer overrides.

use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::OsString,
    fs,
    io::Write,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

use crate::util::write_file;
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use super::output::is_package_managed;

const MANIFEST_NAME: &str = "ubucargo-state.json";

/// Content digest and executable status of one generated regular file.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct Fingerprint {
    sha256: String,
    executable: bool,
}

/// Latest generated state; null entries record absence, missing entries are unknown.
#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    version: u32,
    files: BTreeMap<String, Option<Fingerprint>>,
}

/// Hashes captured bytes through sha256sum, without rereading the file.
fn compute_fingerprint(state: &FileState) -> Result<Fingerprint> {
    let mut child = Command::new("sha256sum")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("run sha256sum")?;
    let written = child.stdin.take().unwrap().write_all(&state.contents);
    let output = child.wait_with_output().context("wait for sha256sum")?;
    if !output.status.success() {
        bail!(
            "sha256sum failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    written.context("write contents to sha256sum")?;
    let stdout = String::from_utf8(output.stdout).context("read sha256sum output")?;
    let sha256 = stdout
        .strip_suffix("  -\n")
        .context("invalid sha256sum output")?;
    if sha256.len() != 64
        || !sha256
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        bail!("invalid sha256sum digest");
    }
    Ok(Fingerprint {
        sha256: sha256.to_owned(),
        executable: state.is_executable(),
    })
}

/// Reads and validates the ownership manifest before any package changes.
fn read_manifest(debian: &Path) -> Result<(Manifest, Option<FileState>)> {
    let state = read_state(&debian.join(MANIFEST_NAME))?;
    let Some(file) = &state else {
        return Ok((
            Manifest {
                version: 1,
                files: BTreeMap::new(),
            },
            None,
        ));
    };
    let value: serde_json::Value = serde_json::from_slice(&file.contents)
        .with_context(|| format!("parse {}", debian.join(MANIFEST_NAME).display()))?;
    if let Some(version) = value.get("version").and_then(serde_json::Value::as_u64)
        && version != 1
    {
        bail!("unsupported {MANIFEST_NAME} version: {version}");
    }
    let manifest: Manifest = serde_json::from_value(value)
        .with_context(|| format!("parse {}", debian.join(MANIFEST_NAME).display()))?;
    for (path, fingerprint) in &manifest.files {
        if path.split('/').any(|part| matches!(part, "" | "." | ".."))
            || !is_package_managed(Path::new(path))
        {
            bail!("invalid managed path in {MANIFEST_NAME}: {path}");
        }
        if let Some(fingerprint) = fingerprint
            && (fingerprint.sha256.len() != 64
                || !fingerprint
                    .sha256
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)))
        {
            bail!("invalid fingerprint in {MANIFEST_NAME}: {path}");
        }
    }
    Ok((manifest, state))
}

/// File content and installation mode, compared using only contents and executable status.
#[derive(Clone, Debug, Eq)]
pub struct FileState {
    /// Complete file contents.
    pub contents: Vec<u8>,
    /// Captured permission bits to preserve; None creates a new file according to umask.
    pub mode: Option<u32>,
}

impl FileState {
    /// Reports executable status; newly created content is non-executable.
    fn is_executable(&self) -> bool {
        self.mode.unwrap_or(0o666) & 0o111 != 0
    }
}

impl PartialEq for FileState {
    /// Ignores permission differences other than executable status.
    fn eq(&self, other: &Self) -> bool {
        self.contents == other.contents && self.is_executable() == other.is_executable()
    }
}

/// Planned reconciliation result for one managed primary file and its hint.
#[derive(Debug)]
pub struct PathPlan {
    /// Package-relative path of the primary file.
    pub path: PathBuf,
    /// Primary-file state observed in the working tree.
    pub old: Option<FileState>,
    /// Existing reference copy, separate from the manifest ownership baseline.
    pub hint_before: Option<FileState>,
    /// Primary-file state to leave after applying the plan.
    pub primary_after: Option<FileState>,
    /// Hint state to leave after applying the plan.
    pub hint_after: Option<FileState>,
    /// Whether the resulting primary differs from the latest generated state.
    pub overridden: bool,
    /// Whether this path requires an explicit decision that has not been supplied.
    pub unresolved: bool,
}

impl PathPlan {
    /// Reports whether applying this path changes its primary file.
    fn has_primary_changed(&self) -> bool {
        self.old != self.primary_after
    }

    /// Reports whether applying this path changes its generated reference copy.
    fn has_hint_changed(&self) -> bool {
        self.hint_before != self.hint_after
    }
}

/// Planned managed-file, hint, and ownership-manifest changes.
pub struct ManagedPlan {
    /// Resolved directory containing the package's Debian files.
    debian: PathBuf,
    /// Per-path reconciliation results in deterministic order.
    pub paths: Vec<PathPlan>,
    /// Manifest bytes observed before planning.
    manifest_before: Option<FileState>,
    /// Deterministically serialized latest generator state.
    manifest_after: FileState,
}

impl ManagedPlan {
    /// Reports whether the ownership manifest needs installation.
    fn has_manifest_changed(&self) -> bool {
        self.manifest_before.as_ref() != Some(&self.manifest_after)
    }

    /// Reports whether applying the plan performs any filesystem changes.
    pub fn has_changes(&self) -> bool {
        self.has_manifest_changed()
            || self
                .paths
                .iter()
                .any(|path| path.has_primary_changed() || path.has_hint_changed())
    }

    /// Collects paths that require an explicit keep-or-replace decision.
    pub fn collect_ambiguities(&self) -> Vec<&Path> {
        let mut ambiguities = Vec::new();
        for path in &self.paths {
            if path.unresolved {
                ambiguities.push(path.path.as_path());
            }
        }
        ambiguities
    }

    /// Prints the deterministic, path-oriented summary of the plan.
    pub fn print_report(&self) {
        for path in &self.paths {
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
                    describe_change(&path.hint_before, &path.hint_after),
                    make_hint_path(&path.path).display()
                );
            }
        }
        if self.has_manifest_changed() {
            let verb = if self.manifest_before.is_some() {
                "update"
            } else {
                "create"
            };
            println!("{verb} debian/{MANIFEST_NAME}");
        }
    }

    /// Applies primary changes first and writes generated baselines last.
    pub fn apply(&self) -> Result<()> {
        if !self.collect_ambiguities().is_empty() {
            bail!("unresolved ambiguities");
        }
        // Install files (including the new patch series) before removing obsolete
        // files, so an interruption cannot leave the series naming a missing patch.
        // Update hints after primaries, and write the manifest last: in case of
        // an interrupted run, a rerun must see the old baseline until the planned
        // file changes are complete.
        for path in &self.paths {
            if path.has_primary_changed() && path.primary_after.is_some() {
                install_state(
                    &resolve_managed_path(&self.debian, &path.path)?,
                    path.primary_after.as_ref(),
                )?;
            }
        }
        for path in &self.paths {
            if path.has_primary_changed() && path.primary_after.is_none() {
                install_state(&resolve_managed_path(&self.debian, &path.path)?, None)?;
            }
        }
        for path in &self.paths {
            if path.has_hint_changed() {
                install_state(
                    &resolve_managed_path(&self.debian, &make_hint_path(&path.path))?,
                    path.hint_after.as_ref(),
                )?;
            }
        }
        if self.has_manifest_changed() {
            install_state(&self.debian.join(MANIFEST_NAME), Some(&self.manifest_after))?;
        }
        Ok(())
    }
}

/// Compares previous output, working files, and new candidates without modifying the package.
///
/// All paths in the collections are relative to the package root and start with `debian/`.
///
/// - `debian`: Existing Debian packaging directory from which to read primary files,
///   hints, and `ubucargo-state.json`, and to which the returned plan will apply changes.
/// - `managed`: Paths to reconcile, augmented with any paths retained in the manifest.
/// - `generated`: Latest generated contents. A managed path missing from
///   this map represents generated absence, so an unmodified primary may be removed.
/// - `inferred_bases`: Candidate baselines used only when both a manifest entry and a
///   hint are missing, and only if the primary matches the candidate's contents and
///   executable status. This allows ubucargo to recognize a raw debcargo 'debian/control'
///   output (before Ubuntu-specific changes to 'Maintainers' and VCS fields), allowing it
///   to be smoothly migrated without needing a manual '--keep' or '--replace' decision.
/// - `keep`: Ambiguous paths for which to preserve the current primary, including absence.
/// - `replace`: Ambiguous paths for which to adopt the latest generated state, including
///   absence. A path cannot appear in both `keep` and `replace`; decisions for paths
///   that are not ambiguous are rejected.
///
/// Ambiguities without a decision remain in the returned plan and prevent it from being applied.
pub fn build_plan(
    debian: &Path,
    managed: &BTreeSet<PathBuf>,
    generated: &BTreeMap<PathBuf, FileState>,
    inferred_bases: &BTreeMap<PathBuf, FileState>,
    keep: &BTreeSet<PathBuf>,
    replace: &BTreeSet<PathBuf>,
) -> Result<ManagedPlan> {
    let mut paths = Vec::new();
    let mut used_decisions = BTreeSet::new();
    let (mut manifest, manifest_before) = read_manifest(debian)?;
    let mut managed = managed.clone();
    for path in manifest.files.keys() {
        managed.insert(PathBuf::from(path));
    }

    for path in &managed {
        let old = read_state(&resolve_managed_path(debian, path)?)?;
        let hint_before = read_state(&resolve_managed_path(debian, &make_hint_path(path))?)?;
        let new = generated.get(path).cloned();
        let name = path.to_str().context("managed path is not UTF-8")?;
        let old_fingerprint = old.as_ref().map(compute_fingerprint).transpose()?;
        let hint_fingerprint = hint_before.as_ref().map(compute_fingerprint).transpose()?;
        // Some(None) is a known absence; None is an unknown baseline:
        let recorded = manifest.files.get(name);
        // An existing hint should agree with the manifest; otherwise it means one of the
        // two was modified in an abnormal way, requiring manual resolution:
        let conflict = match recorded {
            Some(base) => hint_before.is_some() && *base != hint_fingerprint,
            None => false,
        };
        let mut effective_base = recorded.cloned();
        if effective_base.is_none() {
            if hint_before.is_some() {
                effective_base = Some(hint_fingerprint);
            } else if inferred_bases
                .get(path)
                .is_some_and(|inferred| old.as_ref() == Some(inferred))
            {
                effective_base = Some(old_fingerprint.clone());
            }
        }
        let ambiguous = if conflict {
            true
        } else if effective_base.is_some() {
            false
        } else {
            match (&old, &new) {
                (Some(old), Some(new)) => old != new,
                _ => false,
            }
        };
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

        // Resolve conflicting evidence before considering any apparent match.
        let unresolved = ambiguous && decision_replace.is_none();
        let primary_after = if ambiguous {
            if decision_replace == Some(true) {
                new.clone()
            } else {
                old.clone()
            }
        } else {
            match effective_base {
                Some(base) if old_fingerprint == base => new.clone(),
                Some(_) => old.clone(),
                None => old.clone().or(new.clone()),
            }
        };
        let overridden = !unresolved && primary_after != new;
        let hint_after = if unresolved {
            hint_before.clone()
        } else if overridden {
            new.clone()
        } else {
            None
        };
        manifest.files.insert(
            name.to_owned(),
            new.as_ref().map(compute_fingerprint).transpose()?,
        );

        paths.push(PathPlan {
            path: path.clone(),
            old,
            hint_before,
            primary_after,
            hint_after,
            overridden,
            unresolved,
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

    let mut contents = serde_json::to_vec_pretty(&manifest)?;
    contents.push(b'\n');
    Ok(ManagedPlan {
        debian: debian.to_path_buf(),
        paths,
        manifest_before,
        manifest_after: FileState {
            contents,
            mode: None,
        },
    })
}

/// Captures a regular file's contents and permissions, preserving absence distinctly.
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
        mode: Some(metadata.permissions().mode() & 0o7777),
    }))
}

/// Resolves a package-relative managed path beneath the selected Debian directory.
fn resolve_managed_path(debian: &Path, path: &Path) -> Result<PathBuf> {
    Ok(debian.join(
        path.strip_prefix("debian")
            .with_context(|| format!("generated path is outside debian/: {}", path.display()))?,
    ))
}

/// Atomically installs captured contents and permissions, or removes the file.
pub fn install_state(path: &Path, state: Option<&FileState>) -> Result<()> {
    let parent = path
        .parent()
        .with_context(|| format!("{} has no parent directory", path.display()))?;

    match state {
        Some(state) => {
            fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
            write_file(path, &state.contents, state.mode)?;
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
