//! Plans and applies updates to the upstream source tree.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    os::unix::fs::{PermissionsExt, symlink},
    path::{Path, PathBuf},
};

use crate::util::files_differ;
use anyhow::{Context, Result, bail};

/// Regular file metadata and its backing path on disk.
#[derive(Clone, Debug)]
pub struct SourceFile {
    /// Whether any executable bit is set.
    executable: bool,
    /// Backing file path, valid while the scanned tree's directory exists.
    origin: PathBuf,
}

/// Source-tree entry relevant to three-way reconciliation.
#[derive(Clone, Debug)]
pub enum TreeNode {
    /// Directory; its permissions do not participate in reconciliation.
    Directory,
    /// Regular file metadata backed by an on-disk path.
    File(SourceFile),
    /// Symbolic-link target.
    Symlink(PathBuf),
}

/// Complete source-tree update outside `debian/`.
pub struct SourcePlan {
    /// Changed path transitions in deterministic order; retained directories are excluded.
    paths: BTreeMap<PathBuf, (Option<TreeNode>, Option<TreeNode>)>,
}

impl SourcePlan {
    /// Reports whether applying the source plan changes any path.
    pub fn has_changes(&self) -> bool {
        !self.paths.is_empty()
    }

    /// Prints source-tree changes in deterministic path order.
    pub fn print_report(&self) {
        for (path, (old, new)) in &self.paths {
            let verb = match (old, new) {
                (None, Some(_)) => "create",
                (Some(_), None) => "remove",
                _ => "update",
            };
            println!("{verb} {}", path.display());
        }
    }

    /// Removes old entries, copies new files with their permissions, and creates new directories.
    pub fn apply(&self, root: &Path) -> Result<()> {
        // Reverse path order removes descendants before their parents.
        for (path, (old, _)) in self.paths.iter().rev() {
            let destination = root.join(path);
            match old {
                Some(TreeNode::Directory) => fs::remove_dir(&destination),
                Some(TreeNode::File(_) | TreeNode::Symlink(_)) => fs::remove_file(&destination),
                None => continue,
            }
            .with_context(|| format!("remove {}", destination.display()))?;
        }

        // Forward path order creates parents before installing their contents.
        for (path, (_, new)) in &self.paths {
            let destination = root.join(path);
            match new {
                Some(TreeNode::Directory) => {
                    fs::create_dir(&destination)
                        .with_context(|| format!("create {}", destination.display()))?;
                }
                Some(TreeNode::File(file)) => {
                    fs::copy(&file.origin, &destination).with_context(|| {
                        format!(
                            "copy {} to {}",
                            file.origin.display(),
                            destination.display()
                        )
                    })?;
                }
                Some(TreeNode::Symlink(target)) => {
                    symlink(target, &destination)
                        .with_context(|| format!("create symlink {}", destination.display()))?;
                }
                None => {}
            }
        }

        Ok(())
    }
}

/// Scans a tree outside `debian/` into path order, rejecting special files.
pub fn scan_tree(root: &Path) -> Result<BTreeMap<PathBuf, TreeNode>> {
    let root = fs::canonicalize(root).with_context(|| format!("resolve {}", root.display()))?;
    let mut tree = BTreeMap::new();
    let mut directories = vec![PathBuf::new()];
    while let Some(relative) = directories.pop() {
        let directory = root.join(&relative);
        for entry in
            fs::read_dir(&directory).with_context(|| format!("read {}", directory.display()))?
        {
            let entry = entry?;
            let path = relative.join(entry.file_name());
            if path == Path::new("debian") {
                continue;
            }
            let metadata = fs::symlink_metadata(entry.path())?;
            let file_type = metadata.file_type();
            if file_type.is_dir() {
                tree.insert(path.clone(), TreeNode::Directory);
                directories.push(path);
            } else if file_type.is_file() {
                tree.insert(
                    path,
                    TreeNode::File(SourceFile {
                        executable: metadata.permissions().mode() & 0o111 != 0,
                        origin: entry.path(),
                    }),
                );
            } else if file_type.is_symlink() {
                tree.insert(path, TreeNode::Symlink(fs::read_link(entry.path())?));
            } else {
                bail!(
                    "source tree contains special file {}",
                    entry.path().display()
                );
            }
        }
    }
    Ok(tree)
}

/// Reports whether two source-tree states are equivalent, comparing file contents on disk.
pub fn states_match(first: &TreeNode, second: &TreeNode) -> bool {
    match (first, second) {
        (TreeNode::Directory, TreeNode::Directory) => true,
        (TreeNode::Symlink(first_target), TreeNode::Symlink(second_target)) => {
            first_target == second_target
        }
        (TreeNode::File(first), TreeNode::File(second)) => {
            first.executable == second.executable
                // Treat comparison errors as a difference to stay conservative.
                && !files_differ(&first.origin, &second.origin).unwrap_or(true)
        }
        _ => false,
    }
}

/// Reports whether two scanned trees contain equivalent states.
pub fn trees_match(
    first: &BTreeMap<PathBuf, TreeNode>,
    second: &BTreeMap<PathBuf, TreeNode>,
) -> bool {
    first.len() == second.len()
        && first.iter().zip(second.iter()).all(
            |((first_path, first_node), (second_path, second_node))| {
                first_path == second_path && states_match(first_node, second_node)
            },
        )
}

/// Reports whether two optional states are equivalent.
fn option_states_match(first: Option<&TreeNode>, second: Option<&TreeNode>) -> bool {
    match (first, second) {
        (Some(first), Some(second)) => states_match(first, second),
        (None, None) => true,
        _ => false,
    }
}

/// Builds the complete conservative three-tree source merge.
///
/// - `base`: source extracted from the existing package's orig tarball.
/// - `old`: current working source, including local changes.
/// - `new`: candidate source produced by debcargo after configured repacking.
pub fn build_source_plan(
    base: &BTreeMap<PathBuf, TreeNode>,
    old: &BTreeMap<PathBuf, TreeNode>,
    new: &BTreeMap<PathBuf, TreeNode>,
    force: bool,
) -> Result<SourcePlan> {
    let mut all = BTreeSet::new();
    all.extend(base.keys().cloned());
    all.extend(old.keys().cloned());
    all.extend(new.keys().cloned());
    let mut after = BTreeMap::new();
    let mut conflicts = Vec::new();
    for path in &all {
        let base_state = base.get(path);
        let old_state = old.get(path);
        let new_state = new.get(path);
        let selected = if option_states_match(old_state, base_state)
            || option_states_match(old_state, new_state)
        {
            // Accept upstream changes or retain an already-matching result.
            new_state.cloned()
        } else if base_state.is_none() && new_state.is_none() {
            // Neither orig owns this path, so preserve the local-only state.
            old_state.cloned()
        } else if force {
            // Resolve a local/upstream conflict in favor of the candidate.
            new_state.cloned()
        } else {
            // Retain the working state for reporting, then reject the plan below.
            conflicts.push(path.clone());
            old_state.cloned()
        };
        after.insert(path.clone(), selected);
    }
    if !conflicts.is_empty() {
        let names: Vec<String> = conflicts
            .iter()
            .map(|path| path.display().to_string())
            .collect();
        bail!("source conflicts: {}", names.join(", "));
    }

    // Reject or remove retained paths whose selected parent is absent or not a directory.
    let mut blocked = Vec::new();
    for path in &all {
        if after.get(path).and_then(Option::as_ref).is_none() {
            continue;
        }
        let mut ancestor = path.parent();
        while let Some(parent) = ancestor {
            if parent.as_os_str().is_empty() {
                break;
            }
            if !matches!(after.get(parent), Some(Some(TreeNode::Directory))) {
                blocked.push(path.clone());
                break;
            }
            ancestor = parent.parent();
        }
    }
    if !blocked.is_empty() && !force {
        let names: Vec<String> = blocked
            .iter()
            .map(|path| path.display().to_string())
            .collect();
        bail!("structural source conflicts: {}", names.join(", "));
    }
    for path in blocked {
        after.insert(path, None);
    }

    let mut paths = BTreeMap::new();
    for path in all {
        let old_state = old.get(&path);
        let after_state = after.remove(&path).unwrap();
        if !option_states_match(old_state, after_state.as_ref()) {
            paths.insert(path, (old_state.cloned(), after_state));
        }
    }
    Ok(SourcePlan { paths })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink as create_symlink;
    use tempfile::tempdir;

    /// Writes a file with contents into a directory.
    fn write_file(root: &Path, name: &str, contents: &str) {
        fs::write(root.join(name), contents).unwrap();
    }

    /// Applies a file's permission mode on disk.
    fn set_mode(path: &Path, mode: u32) {
        fs::set_permissions(path, fs::Permissions::from_mode(mode)).unwrap();
    }

    #[test]
    /// Verifies the complete non-conflicting source merge table.
    fn merges_source_states() {
        let base_directory = tempdir().unwrap();
        write_file(base_directory.path(), "unchanged", "base");
        write_file(base_directory.path(), "removed", "base");
        write_file(base_directory.path(), "upstream", "base");
        write_file(base_directory.path(), "same", "base");
        let base = scan_tree(base_directory.path()).unwrap();

        let old_directory = tempdir().unwrap();
        write_file(old_directory.path(), "unchanged", "base");
        write_file(old_directory.path(), "removed", "base");
        write_file(old_directory.path(), "upstream", "base");
        write_file(old_directory.path(), "same", "old-new");
        write_file(old_directory.path(), "local", "local");
        let old = scan_tree(old_directory.path()).unwrap();

        let new_directory = tempdir().unwrap();
        write_file(new_directory.path(), "unchanged", "new");
        write_file(new_directory.path(), "upstream", "new");
        write_file(new_directory.path(), "same", "old-new");
        write_file(new_directory.path(), "added", "new");
        let new = scan_tree(new_directory.path()).unwrap();

        let plan = build_source_plan(&base, &old, &new, false).unwrap();
        let after = |path: &str| plan.paths[Path::new(path)].1.as_ref().unwrap();
        assert!(states_match(
            new.get(Path::new("unchanged")).unwrap(),
            after("unchanged")
        ));
        assert!(plan.paths[Path::new("removed")].1.is_none());
        assert!(!plan.paths.contains_key(Path::new("local")));
        assert!(states_match(
            new.get(Path::new("added")).unwrap(),
            after("added")
        ));
        assert!(states_match(
            new.get(Path::new("upstream")).unwrap(),
            after("upstream")
        ));
        assert!(!plan.paths.contains_key(Path::new("same")));
        plan.apply(old_directory.path()).unwrap();
        assert_eq!(
            fs::read(old_directory.path().join("local")).unwrap(),
            b"local"
        );
        assert_eq!(
            fs::read(old_directory.path().join("same")).unwrap(),
            b"old-new"
        );
        assert!(
            !build_source_plan(&new, &new, &new, false)
                .unwrap()
                .has_changes()
        );
    }

    #[test]
    /// Verifies conflict refusal, forced replacement, and structural descendant pruning.
    fn handles_source_conflicts_and_force() {
        let base_directory = tempdir().unwrap();
        write_file(base_directory.path(), "path", "base");
        let base = scan_tree(base_directory.path()).unwrap();
        let old_directory = tempdir().unwrap();
        write_file(old_directory.path(), "path", "local");
        let old = scan_tree(old_directory.path()).unwrap();
        let new_directory = tempdir().unwrap();
        write_file(new_directory.path(), "path", "new");
        let new = scan_tree(new_directory.path()).unwrap();
        assert!(build_source_plan(&base, &old, &new, false).is_err());
        let plan = build_source_plan(&base, &old, &new, true).unwrap();
        assert!(states_match(
            new.get(Path::new("path")).unwrap(),
            plan.paths[Path::new("path")].1.as_ref().unwrap()
        ));

        let base_directory = tempdir().unwrap();
        fs::create_dir(base_directory.path().join("dir")).unwrap();
        let base = scan_tree(base_directory.path()).unwrap();
        let old_directory = tempdir().unwrap();
        fs::create_dir(old_directory.path().join("dir")).unwrap();
        write_file(old_directory.path(), "dir/local", "local");
        let old = scan_tree(old_directory.path()).unwrap();
        let new_directory = tempdir().unwrap();
        let new = scan_tree(new_directory.path()).unwrap();
        assert!(build_source_plan(&base, &old, &new, false).is_err());
        let plan = build_source_plan(&base, &old, &new, true).unwrap();
        assert!(plan.paths[Path::new("dir/local")].1.is_none());
        plan.apply(old_directory.path()).unwrap();
        assert!(!old_directory.path().join("dir").exists());
    }

    #[test]
    /// Ignores incidental permission differences and accepts executable and file-type changes.
    fn handles_modes_and_symlink_changes() {
        let base_directory = tempdir().unwrap();
        fs::create_dir(base_directory.path().join("directory")).unwrap();
        write_file(base_directory.path(), "mode", "same");
        write_file(base_directory.path(), "executable-mode", "same");
        set_mode(&base_directory.path().join("executable-mode"), 0o744);
        write_file(base_directory.path(), "non-executable-mode", "same");
        create_symlink("old-target", base_directory.path().join("kind")).unwrap();
        let base = scan_tree(base_directory.path()).unwrap();

        let new_directory = tempdir().unwrap();
        fs::create_dir(new_directory.path().join("directory")).unwrap();
        set_mode(&new_directory.path().join("directory"), 0o775);
        write_file(new_directory.path(), "mode", "same");
        set_mode(&new_directory.path().join("mode"), 0o755);
        write_file(new_directory.path(), "executable-mode", "same");
        set_mode(&new_directory.path().join("executable-mode"), 0o755);
        write_file(new_directory.path(), "non-executable-mode", "same");
        set_mode(&new_directory.path().join("non-executable-mode"), 0o664);
        write_file(new_directory.path(), "kind", "now a file");
        let new = scan_tree(new_directory.path()).unwrap();

        assert!(states_match(
            base.get(Path::new("directory")).unwrap(),
            new.get(Path::new("directory")).unwrap()
        ));
        assert!(states_match(
            base.get(Path::new("non-executable-mode")).unwrap(),
            new.get(Path::new("non-executable-mode")).unwrap()
        ));
        assert!(states_match(
            base.get(Path::new("executable-mode")).unwrap(),
            new.get(Path::new("executable-mode")).unwrap()
        ));
        let plan = build_source_plan(&base, &base, &new, false).unwrap();
        assert!(states_match(
            new.get(Path::new("mode")).unwrap(),
            plan.paths[Path::new("mode")].1.as_ref().unwrap()
        ));
        assert!(states_match(
            new.get(Path::new("kind")).unwrap(),
            plan.paths[Path::new("kind")].1.as_ref().unwrap()
        ));
    }

    #[test]
    /// Preserves copied and local file modes while creating directories according to umask.
    fn applies_source_updates_from_disk() {
        let old_directory = tempdir().unwrap();
        write_file(old_directory.path(), "file", "old");
        write_file(old_directory.path(), "local", "local");
        set_mode(&old_directory.path().join("local"), 0o600);
        fs::create_dir_all(old_directory.path().join("to-file/child")).unwrap();
        write_file(old_directory.path(), "to-file/child/file", "removed");
        write_file(old_directory.path(), "to-directory", "old file");
        create_symlink("file", old_directory.path().join("link")).unwrap();
        let old = scan_tree(old_directory.path()).unwrap();

        let new_directory = tempdir().unwrap();
        write_file(new_directory.path(), "file", "new");
        set_mode(&new_directory.path().join("file"), 0o710);
        write_file(new_directory.path(), "added", "new");
        set_mode(&new_directory.path().join("added"), 0o660);
        write_file(new_directory.path(), "to-file", "was a directory");
        fs::create_dir_all(new_directory.path().join("to-directory/child")).unwrap();
        create_symlink(
            "../../file",
            new_directory.path().join("to-directory/child/link"),
        )
        .unwrap();
        create_symlink("added", new_directory.path().join("link")).unwrap();
        fs::create_dir_all(new_directory.path().join("nested/child")).unwrap();
        set_mode(&new_directory.path().join("nested"), 0o2700);
        write_file(new_directory.path(), "nested/child/file", "nested");
        fs::create_dir(new_directory.path().join("debian")).unwrap();
        write_file(new_directory.path(), "debian/control", "packaging");
        let new = scan_tree(new_directory.path()).unwrap();

        let reference = old_directory.path().join("reference-directory");
        fs::create_dir(&reference).unwrap();
        let directory_mode = fs::metadata(&reference).unwrap().permissions().mode() & 0o7777;
        fs::remove_dir(&reference).unwrap();
        let mut base = old.clone();
        base.remove(Path::new("local"));
        let plan = build_source_plan(&base, &old, &new, false).unwrap();
        plan.apply(old_directory.path()).unwrap();
        let mut expected = new.clone();
        expected.insert(PathBuf::from("local"), old[Path::new("local")].clone());
        assert!(trees_match(
            &scan_tree(old_directory.path()).unwrap(),
            &expected
        ));
        assert!(!old_directory.path().join("debian").exists());
        assert_eq!(
            fs::metadata(old_directory.path().join("nested"))
                .unwrap()
                .permissions()
                .mode()
                & 0o7777,
            directory_mode
        );
        for (path, mode) in [("file", 0o710), ("added", 0o660), ("local", 0o600)] {
            assert_eq!(
                fs::metadata(old_directory.path().join(path))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o7777,
                mode
            );
        }
    }
}
