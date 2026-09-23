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

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use tempfile::Builder;

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

/// Hashes captured bytes through sha256sum, without rereading a changing working file.
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
    /// Whether missing or conflicting baselines require an explicit decision.
    pub ambiguous: bool,
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

/// Complete, validated set of filesystem changes for one package operation.
pub struct Plan {
    /// Resolved directory containing the package's Debian files.
    debian: PathBuf,
    /// Per-path reconciliation results in deterministic order.
    pub paths: Vec<PathPlan>,
    /// Manifest bytes observed before planning.
    manifest_before: Option<FileState>,
    /// Deterministically serialized latest generator state.
    manifest_after: FileState,
}

impl Plan {
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
            if path.ambiguous {
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
            bail!("unresolved generated-file ambiguities");
        }
        // Install new generated files before changing references such as the
        // patch series, then remove obsolete files and update hints last.
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
pub fn build_plan(
    debian: &Path,
    managed: &BTreeSet<PathBuf>,
    generated: &BTreeMap<PathBuf, FileState>,
    inferred_bases: &BTreeMap<PathBuf, FileState>,
    keep: &BTreeSet<PathBuf>,
    replace: &BTreeSet<PathBuf>,
) -> Result<Plan> {
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
        // Some(None) is a known absence; None is an unknown baseline.
        let recorded = manifest.files.get(name);
        let conflict =
            recorded.is_some_and(|base| hint_before.is_some() && *base != hint_fingerprint);
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
        let ambiguous = conflict
            || (effective_base.is_none()
                && matches!((&old, &new), (Some(old), Some(new)) if old != new));
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

    let mut contents = serde_json::to_vec_pretty(&manifest)?;
    contents.push(b'\n');
    Ok(Plan {
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
            let mut temporary = Builder::new()
                .permissions(fs::Permissions::from_mode(state.mode.unwrap_or(0o666)))
                .tempfile_in(parent)
                .with_context(|| format!("create temporary file in {}", parent.display()))?;
            temporary
                .write_all(&state.contents)
                .with_context(|| format!("write temporary file for {}", path.display()))?;
            if let Some(mode) = state.mode {
                temporary
                    .as_file()
                    .set_permissions(fs::Permissions::from_mode(mode))
                    .with_context(|| format!("set mode for {}", path.display()))?;
            }
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
    use super::super::output::collect_managed_paths;
    use super::*;

    /// Plans a generation using filesystem discovery and the persisted manifest.
    fn plan_generation(debian: &Path, generated: &BTreeMap<PathBuf, FileState>) -> Plan {
        build_plan(
            debian,
            &collect_managed_paths(debian, generated).unwrap(),
            generated,
            &BTreeMap::new(),
            &BTreeSet::new(),
            &BTreeSet::new(),
        )
        .unwrap()
    }

    #[test]
    /// Checks the hash against a known digest and feeds input larger than a pipe buffer.
    fn computes_content_fingerprints() {
        let mut state = make_state("abc");
        let fingerprint = compute_fingerprint(&state).unwrap();
        assert_eq!(
            fingerprint.sha256,
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        state.mode = Some(0o700);
        assert_ne!(compute_fingerprint(&state).unwrap(), fingerprint);
        state.contents = vec![0xff; 1024 * 1024];
        assert_eq!(
            compute_fingerprint(&state).unwrap(),
            compute_fingerprint(&state).unwrap()
        );
    }

    #[test]
    /// Preserves edits made without hints, then clears overrides on adoption or convergence.
    fn preserves_edits_without_initial_hints() {
        for name in [
            "debian/control",
            "debian/rules",
            "debian/cargo-checksum.json",
        ] {
            for edited in [
                Some(make_state("maintainer")),
                Some(FileState {
                    mode: Some(0o700),
                    ..make_state("first")
                }),
                None,
            ] {
                let root = tempfile::tempdir().unwrap();
                let debian = root.path().join("debian");
                fs::create_dir(&debian).unwrap();
                let path = PathBuf::from(name);
                let primary = root.path().join(&path);
                let hint = root.path().join(make_hint_path(&path));
                let mut generated = BTreeMap::from([(path.clone(), make_state("first"))]);
                let plan = plan_generation(&debian, &generated);
                assert!(plan.has_changes());
                assert!(!primary.exists());
                plan.apply().unwrap();
                assert!(!hint.exists());
                assert!(!plan_generation(&debian, &generated).has_changes());
                let (manifest, _) = read_manifest(&debian).unwrap();
                assert_eq!(
                    manifest.files[name],
                    Some(compute_fingerprint(&make_state("first")).unwrap())
                );

                install_state(&primary, edited.as_ref()).unwrap();
                for next in ["second", "third"] {
                    generated.insert(path.clone(), make_state(next));
                    let plan = plan_generation(&debian, &generated);
                    assert_eq!(read_state(&primary).unwrap(), edited);
                    plan.apply().unwrap();
                    assert_eq!(read_state(&primary).unwrap(), edited);
                    assert_eq!(read_state(&hint).unwrap(), Some(make_state(next)));
                    assert!(!plan_generation(&debian, &generated).has_changes());
                }
                install_state(&primary, read_state(&hint).unwrap().as_ref()).unwrap();
                generated.insert(path.clone(), make_state("fourth"));
                plan_generation(&debian, &generated).apply().unwrap();
                assert_eq!(read_state(&primary).unwrap(), Some(make_state("fourth")));
                assert!(!hint.exists());
                assert!(!plan_generation(&debian, &generated).has_changes());

                install_state(&primary, Some(&make_state("fifth"))).unwrap();
                plan_generation(&debian, &generated).apply().unwrap();
                assert!(hint.exists());
                generated.insert(path.clone(), make_state("fifth"));
                plan_generation(&debian, &generated).apply().unwrap();
                assert!(!hint.exists());
            }
        }
    }

    #[test]
    /// Retains explicit absence and discovers vanished dynamic paths from the manifest.
    fn handles_generator_removal_and_reintroduction() {
        for name in [
            "debian/patches/auto/change.patch",
            "debian/librust-example+feature-dev.lintian-overrides",
        ] {
            for edited in [Some(make_state("base")), Some(make_state("local")), None] {
                let root = tempfile::tempdir().unwrap();
                let debian = root.path().join("debian");
                fs::create_dir(&debian).unwrap();
                let path = PathBuf::from(name);
                let primary = root.path().join(&path);
                let hint = root.path().join(make_hint_path(&path));
                let generated = BTreeMap::from([(path.clone(), make_state("base"))]);
                plan_generation(&debian, &generated).apply().unwrap();
                install_state(&primary, edited.as_ref()).unwrap();
                let plan = plan_generation(&debian, &BTreeMap::new());
                assert!(plan.paths.iter().any(|entry| entry.path == path));
                plan.apply().unwrap();
                let retained = if edited == Some(make_state("local")) {
                    edited
                } else {
                    None
                };
                assert_eq!(read_state(&primary).unwrap(), retained);
                assert!(!hint.exists());
                let (manifest, _) = read_manifest(&debian).unwrap();
                assert_eq!(manifest.files.get(name), Some(&None));
                assert!(!plan_generation(&debian, &BTreeMap::new()).has_changes());
                plan_generation(&debian, &generated).apply().unwrap();
                assert_eq!(
                    read_state(&primary).unwrap(),
                    retained.clone().or(Some(make_state("base")))
                );
                assert_eq!(hint.exists(), retained.is_some());
            }
        }
    }

    #[test]
    /// Imports debcargo hints, removes redundant copies, and leaves unknown hints alone.
    fn migrates_debcargo_hints() {
        let root = tempfile::tempdir().unwrap();
        let debian = root.path().join("debian");
        fs::create_dir(&debian).unwrap();
        let mut generated = BTreeMap::new();
        for (name, working) in [
            ("control", "edited"),
            ("rules", "base"),
            ("cargo-checksum.json", "base"),
        ] {
            install_state(&debian.join(name), Some(&make_state(working))).unwrap();
            install_state(
                &debian.join(format!("{name}.debcargo.hint")),
                Some(&make_state("base")),
            )
            .unwrap();
            generated.insert(PathBuf::from("debian").join(name), make_state("new"));
        }
        fs::write(debian.join("unknown.debcargo.hint"), "unknown").unwrap();
        let plan = plan_generation(&debian, &generated);
        assert!(plan.collect_ambiguities().is_empty());
        assert!(!debian.join(MANIFEST_NAME).exists());
        plan.apply().unwrap();
        assert_eq!(
            fs::read_to_string(debian.join("control")).unwrap(),
            "edited"
        );
        assert_eq!(
            fs::read_to_string(debian.join("control.debcargo.hint")).unwrap(),
            "new"
        );
        for name in ["rules", "cargo-checksum.json"] {
            assert_eq!(fs::read_to_string(debian.join(name)).unwrap(), "new");
            assert!(!debian.join(format!("{name}.debcargo.hint")).exists());
        }
        assert_eq!(
            fs::read_to_string(debian.join("unknown.debcargo.hint")).unwrap(),
            "unknown"
        );
        assert!(!plan_generation(&debian, &generated).has_changes());
    }

    #[test]
    /// Requires decisions when hints conflict, even if the primary matches the manifest.
    fn resolves_conflicting_baselines() {
        for replace in [false, true] {
            for initial in [Some(make_state("base")), None] {
                let root = tempfile::tempdir().unwrap();
                let debian = root.path().join("debian");
                fs::create_dir(&debian).unwrap();
                let path = PathBuf::from("debian/control");
                let mut generated = BTreeMap::new();
                if let Some(state) = &initial {
                    generated.insert(path.clone(), state.clone());
                }
                plan_generation(&debian, &generated).apply().unwrap();
                let hint = debian.join("control.debcargo.hint");
                install_state(
                    &hint,
                    Some(&FileState {
                        mode: Some(0o700),
                        ..make_state("base")
                    }),
                )
                .unwrap();
                generated.insert(path.clone(), make_state("new"));
                let before = fs::read(debian.join(MANIFEST_NAME)).unwrap();
                let plan = plan_generation(&debian, &generated);
                assert_eq!(plan.collect_ambiguities(), vec![path.as_path()]);
                assert!(plan.apply().is_err());
                assert_eq!(fs::read(debian.join(MANIFEST_NAME)).unwrap(), before);
                assert_eq!(read_state(&debian.join("control")).unwrap(), initial);
                let decision = BTreeSet::from([path]);
                let empty = BTreeSet::new();
                build_plan(
                    &debian,
                    &decision,
                    &generated,
                    &BTreeMap::new(),
                    if replace { &empty } else { &decision },
                    if replace { &decision } else { &empty },
                )
                .unwrap()
                .apply()
                .unwrap();
                assert_eq!(
                    read_state(&debian.join("control")).unwrap(),
                    if replace {
                        Some(make_state("new"))
                    } else {
                        initial
                    }
                );
                assert_eq!(hint.exists(), !replace);
                assert!(!plan_generation(&debian, &generated).has_changes());
            }
        }
    }

    #[test]
    /// Rejects invalid state before modifying primaries or hints.
    fn rejects_invalid_manifests() {
        let directory = tempfile::tempdir().unwrap();
        let debian = directory.path();
        fs::write(debian.join("control"), "local").unwrap();
        let mut invalid = vec![
            "{".to_owned(),
            r#"{"version":2,"files":{}}"#.to_owned(),
            r#"{"version":1}"#.to_owned(),
        ];
        for path in [
            "/debian/control",
            "debian/../control",
            "debian/patches/auto/../../control",
            "debian/patches/auto",
            "debian//control",
            "debian/changelog",
            "debian/ubucargo-state.json",
            "debian/patches/series",
            "debian/control.debcargo.hint",
        ] {
            invalid.push(serde_json::json!({"version":1, "files":{path:null}}).to_string());
        }
        for (sha256, executable) in [
            ("bad".to_owned(), serde_json::json!(false)),
            ("a".repeat(64), serde_json::json!(420)),
            ("A".repeat(64), serde_json::json!(false)),
        ] {
            invalid.push(serde_json::json!({"version":1,"files":{"debian/control":{"sha256":sha256,"executable":executable}}}).to_string());
        }
        for contents in invalid {
            fs::write(debian.join(MANIFEST_NAME), &contents).unwrap();
            assert!(
                build_plan(
                    debian,
                    &BTreeSet::new(),
                    &BTreeMap::new(),
                    &BTreeMap::new(),
                    &BTreeSet::new(),
                    &BTreeSet::new()
                )
                .is_err(),
                "accepted {contents}"
            );
            assert_eq!(fs::read_to_string(debian.join("control")).unwrap(), "local");
            assert_eq!(
                fs::read_to_string(debian.join(MANIFEST_NAME)).unwrap(),
                contents
            );
        }
    }

    #[test]
    /// Reports unsupported versions before validating the version 1 shape.
    fn reports_unsupported_manifest_version() {
        let directory = tempfile::tempdir().unwrap();
        fs::write(
            directory.path().join(MANIFEST_NAME),
            r#"{"version":2,"new_field":true}"#,
        )
        .unwrap();
        let error = read_manifest(directory.path()).unwrap_err();
        assert_eq!(
            error.to_string(),
            "unsupported ubucargo-state.json version: 2"
        );
    }

    #[test]
    /// Reruns after partial installation preserve edits and expose conflicting hints.
    fn preserves_edits_after_interrupted_installation() {
        let root = tempfile::tempdir().unwrap();
        let debian = root.path().join("debian");
        fs::create_dir(&debian).unwrap();
        let generated = BTreeMap::from([
            (PathBuf::from("debian/control"), make_state("base")),
            (PathBuf::from("debian/rules"), make_state("base")),
        ]);
        plan_generation(&debian, &generated).apply().unwrap();
        install_state(&debian.join("control"), Some(&make_state("local"))).unwrap();
        let next = BTreeMap::from([
            (PathBuf::from("debian/control"), make_state("new")),
            (PathBuf::from("debian/rules"), make_state("new")),
        ]);
        let plan = plan_generation(&debian, &next);
        // Simulate a stop after installing primaries and hints, before the manifest.
        for path in &plan.paths {
            if path.has_primary_changed() {
                install_state(&root.path().join(&path.path), path.primary_after.as_ref()).unwrap();
            }
            if path.has_hint_changed() {
                install_state(
                    &root.path().join(make_hint_path(&path.path)),
                    path.hint_after.as_ref(),
                )
                .unwrap();
            }
        }
        install_state(&debian.join("rules"), Some(&make_state("another edit"))).unwrap();
        let retry = plan_generation(&debian, &next);
        assert_eq!(
            retry.collect_ambiguities(),
            vec![Path::new("debian/control")]
        );
        assert!(retry.apply().is_err());
        let managed = collect_managed_paths(&debian, &next).unwrap();
        build_plan(
            &debian,
            &managed,
            &next,
            &BTreeMap::new(),
            &BTreeSet::from([PathBuf::from("debian/control")]),
            &BTreeSet::new(),
        )
        .unwrap()
        .apply()
        .unwrap();
        assert_eq!(fs::read_to_string(debian.join("control")).unwrap(), "local");
        assert_eq!(
            fs::read_to_string(debian.join("rules")).unwrap(),
            "another edit"
        );
        assert!(!plan_generation(&debian, &next).has_changes());
    }

    /// Creates a non-executable text state for planner tests.
    fn make_state(value: &str) -> FileState {
        FileState {
            contents: value.as_bytes().to_vec(),
            mode: None,
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
            &BTreeMap::new(),
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
            &BTreeMap::from([(path.clone(), make_state("maintainer"))]),
            &BTreeSet::new(),
            &BTreeSet::new(),
        )
        .unwrap();
        assert!(!plan.paths[0].ambiguous);
        assert_eq!(plan.paths[0].primary_after, Some(make_state("new")));
        assert_eq!(plan.paths[0].hint_after, None);

        let plan = build_plan(
            &debian,
            &managed,
            &generated,
            &BTreeMap::new(),
            &BTreeSet::new(),
            &BTreeSet::new(),
        )
        .unwrap();
        assert_eq!(plan.collect_ambiguities(), vec![path.as_path()]);

        let plan = build_plan(
            &debian,
            &managed,
            &generated,
            &BTreeMap::new(),
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
            &BTreeMap::new(),
            &BTreeSet::new(),
            &BTreeSet::from([path.clone()]),
        )
        .unwrap();
        assert_eq!(plan.paths[0].primary_after, Some(make_state("new")));
        assert_eq!(plan.paths[0].hint_after, None);
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
            &BTreeMap::new(),
            &BTreeSet::new(),
            &BTreeSet::new(),
        )
        .unwrap();
        assert_eq!(plan.paths[0].primary_after, Some(make_state("new")));
        assert_eq!(plan.paths[0].hint_after, None);

        install_state(&root.join(&path), Some(&make_state("base"))).unwrap();
        install_state(&root.join(make_hint_path(&path)), Some(&make_state("base"))).unwrap();
        let plan = build_plan(
            &debian,
            &managed,
            &BTreeMap::new(),
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
            &BTreeMap::new(),
            &BTreeSet::new(),
            &BTreeSet::new(),
        )
        .unwrap();
        assert_eq!(plan.paths[0].primary_after, None);
        assert_eq!(plan.paths[0].hint_after, None);
        assert!(!plan.paths[0].overridden);
    }

    #[test]
    /// Preserves every unambiguous missing-baseline case.
    fn handles_missing_baselines() {
        let directory = tempfile::tempdir().unwrap();
        let debian = directory.path().join("debian");
        fs::create_dir(&debian).unwrap();
        let managed = BTreeSet::from([PathBuf::from("debian/control")]);
        for (old, new) in [
            (None, None),
            (None, Some("new")),
            (Some("same"), Some("same")),
            (Some("local"), None),
        ] {
            install_state(&debian.join("control"), old.map(make_state).as_ref()).unwrap();
            let mut generated = BTreeMap::new();
            if let Some(new) = new {
                generated.insert(PathBuf::from("debian/control"), make_state(new));
            }
            let plan = build_plan(
                &debian,
                &managed,
                &generated,
                &BTreeMap::new(),
                &BTreeSet::new(),
                &BTreeSet::new(),
            )
            .unwrap();
            assert_eq!(plan.paths[0].primary_after, old.or(new).map(make_state));
            assert_eq!(plan.paths[0].hint_after, None);
            assert!(!plan.paths[0].ambiguous);
            assert_eq!(plan.paths[0].overridden, old.is_some() && new.is_none());
        }
    }

    #[test]
    /// Preserves source and override modes without treating incidental changes as ownership changes.
    fn compares_executable_status() {
        let directory = tempfile::tempdir().unwrap();
        let debian = directory.path();
        let rules = debian.join("rules");
        let staged = debian.join("staged");
        fs::write(&staged, "generated").unwrap();
        let initial_mode = fs::metadata(&staged).unwrap().permissions().mode() & 0o7777;
        install_state(&debian.join("fresh"), Some(&make_state("fresh"))).unwrap();
        assert_eq!(
            fs::metadata(debian.join("fresh"))
                .unwrap()
                .permissions()
                .mode()
                & 0o7777,
            initial_mode
        );
        for mode in [0o600, 0o660, 0o710, 0o775] {
            fs::set_permissions(&staged, fs::Permissions::from_mode(mode)).unwrap();
            let state = read_state(&staged).unwrap().unwrap();
            let installed = debian.join("nested/child/file");
            install_state(&installed, Some(&state)).unwrap();
            assert_eq!(
                fs::metadata(&installed).unwrap().permissions().mode() & 0o7777,
                mode
            );
            assert_eq!(read_state(&installed).unwrap(), Some(state));
        }
        let generated = BTreeMap::from([(
            PathBuf::from("debian/rules"),
            read_state(&staged).unwrap().unwrap(),
        )]);
        plan_generation(debian, &generated).apply().unwrap();
        for mode in [0o700, 0o750, 0o775] {
            fs::set_permissions(&rules, fs::Permissions::from_mode(mode)).unwrap();
            let plan = plan_generation(debian, &generated);
            assert!(!plan.has_changes());
            plan.apply().unwrap();
            assert_eq!(
                fs::metadata(&rules).unwrap().permissions().mode() & 0o7777,
                mode
            );
        }
        fs::set_permissions(&rules, fs::Permissions::from_mode(0o600)).unwrap();
        let plan = plan_generation(debian, &generated);
        assert!(
            plan.paths
                .iter()
                .any(|path| path.path == Path::new("debian/rules") && path.overridden)
        );
        plan.apply().unwrap();
        assert_eq!(
            fs::metadata(&rules).unwrap().permissions().mode() & 0o7777,
            0o600
        );
        assert_eq!(
            fs::metadata(make_hint_path(&rules))
                .unwrap()
                .permissions()
                .mode()
                & 0o7777,
            0o775
        );
        assert!(!plan_generation(debian, &generated).has_changes());
    }
}
