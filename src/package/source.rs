use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    os::unix::fs::{PermissionsExt, symlink},
    path::{Path, PathBuf},
};

use super::tree::files_differ;
use anyhow::{Context, Result, bail};

/// Regular file metadata and its backing path on disk.
#[derive(Clone, Debug)]
pub struct SourceFile {
    /// Permission and special bits, excluding the file-type bits.
    mode: u32,
    /// Backing file path, valid while the scanned tree's directory exists.
    origin: PathBuf,
}

/// Source-tree entry relevant to three-way reconciliation.
#[derive(Clone, Debug)]
pub enum TreeNode {
    /// Directory with its Unix permission mode.
    Directory(u32),
    /// Regular file metadata backed by an on-disk path.
    File(SourceFile),
    /// Symbolic-link target.
    Symlink(PathBuf),
}

/// Complete source-tree update outside `debian/`.
pub struct SourcePlan {
    /// Deterministic path transitions.
    paths: BTreeMap<PathBuf, (Option<TreeNode>, Option<TreeNode>)>,
    /// Paths whose old and new states differ.
    changed: BTreeSet<PathBuf>,
}

impl SourcePlan {
    /// Reports whether applying the source plan changes any path.
    pub fn has_changes(&self) -> bool {
        !self.changed.is_empty()
    }

    /// Prints source-tree changes in deterministic path order.
    pub fn print_report(&self) {
        for path in &self.changed {
            let (old, new) = &self.paths[path];
            let verb = match (old, new) {
                (None, Some(_)) => "create",
                (Some(_), None) => "remove",
                _ => "update",
            };
            println!("{verb} {}", path.display());
        }
    }

    /// Applies a source transition plan, creating/overwriting/deleting files and directories.
    pub fn apply(&self, root: &Path) -> Result<()> {
        let mut changed = Vec::new();
        for path in &self.changed {
            changed.push(path);
        }
        changed.sort_by_key(|path| std::cmp::Reverse(path.components().count()));

        // Remove leaf files and symlinks that are being deleted or replaced.
        for path in &changed {
            let (old, new) = &self.paths[*path];
            let keep_file = matches!(
                (old, new),
                (Some(TreeNode::File(_)), Some(TreeNode::File(_)))
            );
            if !keep_file && matches!(old, Some(TreeNode::File(_) | TreeNode::Symlink(_))) {
                fs::remove_file(root.join(path))
                    .with_context(|| format!("remove {}", root.join(path).display()))?;
            }
        }

        // Remove directories that are being deleted or replaced, deepest first so
        // child directories are emptied before their parents.
        for path in &changed {
            let (old, new) = &self.paths[*path];
            if matches!(old, Some(TreeNode::Directory(_)))
                && !matches!(new, Some(TreeNode::Directory(_)))
            {
                fs::remove_dir(root.join(path))
                    .with_context(|| format!("remove {}", root.join(path).display()))?;
            }
        }

        changed.sort_by_key(|path| path.components().count());

        // Create new directories, shallowest first so parents exist before children.
        for path in &changed {
            let (old, new) = &self.paths[*path];
            if matches!(new, Some(TreeNode::Directory(_)))
                && !matches!(old, Some(TreeNode::Directory(_)))
            {
                fs::create_dir(root.join(path))
                    .with_context(|| format!("create {}", root.join(path).display()))?;
            }
        }

        // Copy changed file contents from their origin trees and recreate symlinks.
        for path in &changed {
            let destination = root.join(path);
            let (_old, new) = &self.paths[*path];
            match new {
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
                _ => {}
            }
        }

        // Set directory permissions after all content exists beneath them.
        for path in &changed {
            if let Some(TreeNode::Directory(mode)) = &self.paths[*path].1 {
                fs::set_permissions(root.join(path), fs::Permissions::from_mode(*mode))
                    .with_context(|| format!("set mode for {}", root.join(path).display()))?;
            }
        }
        Ok(())
    }
}

/// Scans a deterministic tree while rejecting special files and optionally excluding `debian/`.
pub fn scan_tree(root: &Path, exclude_debian: bool) -> Result<BTreeMap<PathBuf, TreeNode>> {
    let root = fs::canonicalize(root).with_context(|| format!("resolve {}", root.display()))?;
    let mut tree = BTreeMap::new();
    let mut directories = vec![PathBuf::new()];
    while let Some(relative) = directories.pop() {
        let directory = root.join(&relative);
        let mut entries = Vec::new();
        for entry in
            fs::read_dir(&directory).with_context(|| format!("read {}", directory.display()))?
        {
            entries.push(entry?);
        }
        entries.sort_by_key(|entry| entry.file_name());
        for entry in entries.into_iter().rev() {
            let path = relative.join(entry.file_name());
            if exclude_debian && path == Path::new("debian") {
                continue;
            }
            let metadata = fs::symlink_metadata(entry.path())?;
            let file_type = metadata.file_type();
            if file_type.is_dir() {
                tree.insert(
                    path.clone(),
                    TreeNode::Directory(metadata.permissions().mode() & 0o7777),
                );
                directories.push(path);
            } else if file_type.is_file() {
                tree.insert(
                    path,
                    TreeNode::File(SourceFile {
                        mode: metadata.permissions().mode() & 0o7777,
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
        (TreeNode::Directory(first_mode), TreeNode::Directory(second_mode)) => {
            first_mode == second_mode
        }
        (TreeNode::Symlink(first_target), TreeNode::Symlink(second_target)) => {
            first_target == second_target
        }
        (TreeNode::File(first), TreeNode::File(second)) => {
            first.mode == second.mode
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
fn option_states_match(first: &Option<TreeNode>, second: &Option<TreeNode>) -> bool {
    match (first, second) {
        (Some(first), Some(second)) => states_match(first, second),
        (None, None) => true,
        _ => false,
    }
}

/// Builds the complete conservative three-tree source merge.
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
        let old_matches_base = option_states_match(&old_state.cloned(), &base_state.cloned());
        let old_matches_new = option_states_match(&old_state.cloned(), &new_state.cloned());
        let selected = if old_matches_base {
            new_state.cloned()
        } else if old_matches_new || (base_state.is_none() && new_state.is_none()) {
            old_state.cloned()
        } else if force {
            new_state.cloned()
        } else {
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
            if !matches!(after.get(parent), Some(Some(TreeNode::Directory(_)))) {
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
    let mut changed = BTreeSet::new();
    for path in all {
        let old_state = old.get(&path).cloned();
        let after_state = after.remove(&path).unwrap();
        if !option_states_match(&old_state, &after_state) {
            changed.insert(path.clone());
        }
        paths.insert(path, (old_state, after_state));
    }
    Ok(SourcePlan { paths, changed })
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
        let base = scan_tree(base_directory.path(), false).unwrap();

        let old_directory = tempdir().unwrap();
        write_file(old_directory.path(), "unchanged", "base");
        write_file(old_directory.path(), "removed", "base");
        write_file(old_directory.path(), "upstream", "base");
        write_file(old_directory.path(), "same", "old-new");
        write_file(old_directory.path(), "local", "local");
        let old = scan_tree(old_directory.path(), false).unwrap();

        let new_directory = tempdir().unwrap();
        write_file(new_directory.path(), "unchanged", "new");
        write_file(new_directory.path(), "upstream", "new");
        write_file(new_directory.path(), "same", "old-new");
        write_file(new_directory.path(), "added", "new");
        let new = scan_tree(new_directory.path(), false).unwrap();

        let plan = build_source_plan(&base, &old, &new, false).unwrap();
        let after = |path: &str| plan.paths[Path::new(path)].1.as_ref().unwrap();
        assert!(states_match(
            new.get(Path::new("unchanged")).unwrap(),
            after("unchanged")
        ));
        assert!(plan.paths[Path::new("removed")].1.is_none());
        assert!(states_match(
            old.get(Path::new("local")).unwrap(),
            after("local")
        ));
        assert!(states_match(
            new.get(Path::new("added")).unwrap(),
            after("added")
        ));
        assert!(states_match(
            new.get(Path::new("upstream")).unwrap(),
            after("upstream")
        ));
        assert!(states_match(
            old.get(Path::new("same")).unwrap(),
            after("same")
        ));
    }

    #[test]
    /// Verifies conflict refusal, forced replacement, and structural descendant pruning.
    fn handles_source_conflicts_and_force() {
        let base_directory = tempdir().unwrap();
        write_file(base_directory.path(), "path", "base");
        let base = scan_tree(base_directory.path(), false).unwrap();
        let old_directory = tempdir().unwrap();
        write_file(old_directory.path(), "path", "local");
        let old = scan_tree(old_directory.path(), false).unwrap();
        let new_directory = tempdir().unwrap();
        write_file(new_directory.path(), "path", "new");
        let new = scan_tree(new_directory.path(), false).unwrap();
        assert!(build_source_plan(&base, &old, &new, false).is_err());
        let plan = build_source_plan(&base, &old, &new, true).unwrap();
        assert!(states_match(
            new.get(Path::new("path")).unwrap(),
            plan.paths[Path::new("path")].1.as_ref().unwrap()
        ));

        let base_directory = tempdir().unwrap();
        fs::create_dir(base_directory.path().join("dir")).unwrap();
        let base = scan_tree(base_directory.path(), false).unwrap();
        let old_directory = tempdir().unwrap();
        fs::create_dir(old_directory.path().join("dir")).unwrap();
        write_file(old_directory.path(), "dir/local", "local");
        let old = scan_tree(old_directory.path(), false).unwrap();
        let new_directory = tempdir().unwrap();
        let new = scan_tree(new_directory.path(), false).unwrap();
        assert!(build_source_plan(&base, &old, &new, false).is_err());
        let plan = build_source_plan(&base, &old, &new, true).unwrap();
        assert!(plan.paths[Path::new("dir/local")].1.is_none());
    }

    #[test]
    /// Verifies source reconciliation preserves modes and accepts upstream file-type changes.
    fn handles_modes_and_symlink_changes() {
        let base_directory = tempdir().unwrap();
        write_file(base_directory.path(), "mode", "same");
        create_symlink("old-target", base_directory.path().join("kind")).unwrap();
        let base = scan_tree(base_directory.path(), false).unwrap();

        let new_directory = tempdir().unwrap();
        write_file(new_directory.path(), "mode", "same");
        set_mode(&new_directory.path().join("mode"), 0o755);
        write_file(new_directory.path(), "kind", "now a file");
        let new = scan_tree(new_directory.path(), false).unwrap();

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
    /// Verifies applying a plan copies changed contents and modes from their origins.
    fn applies_source_updates_from_disk() {
        let old_directory = tempdir().unwrap();
        write_file(old_directory.path(), "file", "old");
        let old = scan_tree(old_directory.path(), false).unwrap();

        let new_directory = tempdir().unwrap();
        write_file(new_directory.path(), "file", "new");
        set_mode(&new_directory.path().join("file"), 0o755);
        write_file(new_directory.path(), "added", "new");
        let new = scan_tree(new_directory.path(), false).unwrap();

        let plan = build_source_plan(&old, &old, &new, false).unwrap();
        plan.apply(old_directory.path()).unwrap();
        assert_eq!(fs::read(old_directory.path().join("file")).unwrap(), b"new");
        assert_eq!(
            fs::metadata(old_directory.path().join("file"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o755
        );
        assert_eq!(
            fs::read(old_directory.path().join("added")).unwrap(),
            b"new"
        );
    }
}
