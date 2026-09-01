use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    os::unix::fs::{PermissionsExt, symlink},
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};

use crate::materialize::{FileState, install_state};

/// Source-tree entry relevant to three-way reconciliation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TreeNode {
    /// Directory with its Unix permission mode.
    Directory(u32),
    /// Regular file contents and Unix permission mode.
    File(FileState),
    /// Symbolic-link target.
    Symlink(PathBuf),
}

/// Complete source-tree update outside `debian/`.
pub struct SourcePlan {
    /// Deterministic path transitions.
    paths: BTreeMap<PathBuf, (Option<TreeNode>, Option<TreeNode>)>,
}

impl SourcePlan {
    /// Reports whether applying the source plan changes any path.
    pub fn has_changes(&self) -> bool {
        for (old, new) in self.paths.values() {
            if old != new {
                return true;
            }
        }
        false
    }

    /// Prints source-tree changes in deterministic path order.
    pub fn print_report(&self) {
        for (path, (old, new)) in &self.paths {
            if old == new {
                continue;
            }
            let verb = match (old, new) {
                (None, Some(_)) => "create",
                (Some(_), None) => "remove",
                _ => "update",
            };
            println!("{verb} {}", path.display());
        }
    }

    /// Applies validated source transitions without recursively deleting paths.
    pub fn apply(&self, root: &Path) -> Result<()> {
        let mut paths = Vec::new();
        for (path, states) in &self.paths {
            if states.0 != states.1 {
                paths.push(path);
            }
        }
        paths.sort_by_key(|path| std::cmp::Reverse(path.components().count()));

        for path in &paths {
            let (old, new) = self.paths.get(*path).unwrap();
            let remove = match (old, new) {
                (Some(TreeNode::Directory(_)), Some(TreeNode::Directory(_))) => false,
                (Some(TreeNode::File(_)), Some(TreeNode::File(_))) => false,
                (Some(_), _) => true,
                _ => false,
            };
            if !remove || matches!(old, Some(TreeNode::Directory(_))) {
                continue;
            }
            fs::remove_file(root.join(path))
                .with_context(|| format!("remove {}", root.join(path).display()))?;
        }
        for path in &paths {
            let (old, new) = self.paths.get(*path).unwrap();
            if matches!(old, Some(TreeNode::Directory(_)))
                && !matches!(new, Some(TreeNode::Directory(_)))
            {
                fs::remove_dir(root.join(path))
                    .with_context(|| format!("remove {}", root.join(path).display()))?;
            }
        }

        paths.sort_by_key(|path| path.components().count());
        for path in &paths {
            let (old, new) = self.paths.get(*path).unwrap();
            if matches!(new, Some(TreeNode::Directory(_)))
                && !matches!(old, Some(TreeNode::Directory(_)))
            {
                fs::create_dir(root.join(path))
                    .with_context(|| format!("create {}", root.join(path).display()))?;
            }
        }
        for path in &paths {
            let destination = root.join(path);
            let (old, new) = self.paths.get(*path).unwrap();
            match new {
                Some(TreeNode::File(state)) if old != new => {
                    install_state(&destination, Some(state))?;
                }
                Some(TreeNode::Symlink(target)) if old != new => {
                    symlink(target, &destination)
                        .with_context(|| format!("create symlink {}", destination.display()))?;
                }
                _ => {}
            }
        }
        for path in &paths {
            if let Some(TreeNode::Directory(mode)) = self.paths.get(*path).unwrap().1 {
                fs::set_permissions(root.join(path), fs::Permissions::from_mode(mode))
                    .with_context(|| format!("set mode for {}", root.join(path).display()))?;
            }
        }
        Ok(())
    }
}

/// Scans a deterministic tree while rejecting special files and optionally excluding `debian/`.
pub fn scan_tree(root: &Path, exclude_debian: bool) -> Result<BTreeMap<PathBuf, TreeNode>> {
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
                    TreeNode::File(FileState {
                        contents: fs::read(entry.path())?,
                        mode: metadata.permissions().mode() & 0o7777,
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
        let selected = if old_state == base_state {
            new_state.cloned()
        } else if old_state == new_state || (base_state.is_none() && new_state.is_none()) {
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
    for path in all {
        paths.insert(
            path.clone(),
            (old.get(&path).cloned(), after.remove(&path).unwrap()),
        );
    }
    Ok(SourcePlan { paths })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Creates a compact file node for source-merge tests.
    fn file(value: &str) -> TreeNode {
        TreeNode::File(FileState {
            contents: value.as_bytes().to_vec(),
            mode: 0o644,
        })
    }

    #[test]
    /// Verifies the complete non-conflicting source merge table.
    fn merges_source_states() {
        let base = BTreeMap::from([
            (PathBuf::from("unchanged"), file("base")),
            (PathBuf::from("removed"), file("base")),
            (PathBuf::from("upstream"), file("base")),
            (PathBuf::from("same"), file("old-new")),
        ]);
        let old = BTreeMap::from([
            (PathBuf::from("unchanged"), file("base")),
            (PathBuf::from("removed"), file("base")),
            (PathBuf::from("upstream"), file("base")),
            (PathBuf::from("same"), file("old-new")),
            (PathBuf::from("local"), file("local")),
        ]);
        let new = BTreeMap::from([
            (PathBuf::from("unchanged"), file("new")),
            (PathBuf::from("upstream"), file("new")),
            (PathBuf::from("same"), file("old-new")),
            (PathBuf::from("added"), file("new")),
        ]);
        let plan = build_source_plan(&base, &old, &new, false).unwrap();
        assert_eq!(plan.paths[Path::new("unchanged")].1, Some(file("new")));
        assert_eq!(plan.paths[Path::new("removed")].1, None);
        assert_eq!(plan.paths[Path::new("local")].1, Some(file("local")));
        assert_eq!(plan.paths[Path::new("added")].1, Some(file("new")));
    }

    #[test]
    /// Verifies conflict refusal, forced replacement, and structural descendant pruning.
    fn handles_source_conflicts_and_force() {
        let base = BTreeMap::from([(PathBuf::from("path"), file("base"))]);
        let old = BTreeMap::from([(PathBuf::from("path"), file("local"))]);
        let new = BTreeMap::from([(PathBuf::from("path"), file("new"))]);
        assert!(build_source_plan(&base, &old, &new, false).is_err());
        let plan = build_source_plan(&base, &old, &new, true).unwrap();
        assert_eq!(plan.paths[Path::new("path")].1, Some(file("new")));

        let base = BTreeMap::from([(PathBuf::from("dir"), TreeNode::Directory(0o755))]);
        let old = BTreeMap::from([
            (PathBuf::from("dir"), TreeNode::Directory(0o755)),
            (PathBuf::from("dir/local"), file("local")),
        ]);
        let new = BTreeMap::new();
        assert!(build_source_plan(&base, &old, &new, false).is_err());
        let plan = build_source_plan(&base, &old, &new, true).unwrap();
        assert_eq!(plan.paths[Path::new("dir/local")].1, None);
    }

    #[test]
    /// Verifies source reconciliation preserves modes and accepts upstream file-type changes.
    fn handles_modes_and_symlink_changes() {
        let base = BTreeMap::from([
            (PathBuf::from("mode"), file("same")),
            (
                PathBuf::from("kind"),
                TreeNode::Symlink(PathBuf::from("old-target")),
            ),
        ]);
        let old = base.clone();
        let new = BTreeMap::from([
            (
                PathBuf::from("mode"),
                TreeNode::File(FileState {
                    contents: b"same".to_vec(),
                    mode: 0o755,
                }),
            ),
            (PathBuf::from("kind"), file("now a file")),
        ]);
        let plan = build_source_plan(&base, &old, &new, false).unwrap();
        assert_eq!(
            plan.paths[Path::new("mode")].1,
            new.get(Path::new("mode")).cloned()
        );
        assert_eq!(
            plan.paths[Path::new("kind")].1,
            new.get(Path::new("kind")).cloned()
        );
    }
}
