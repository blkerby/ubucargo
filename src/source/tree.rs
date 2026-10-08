//! Compares directory trees and plans conservative upstream source merges.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    os::unix::fs::{PermissionsExt, symlink},
    path::{Path, PathBuf},
};

use crate::util::{files_differ, write_file};
use anyhow::{Context, Result, bail};

/// Regular file metadata and its backing path on disk.
#[derive(Clone, Debug)]
pub struct SourceFile {
    /// Whether any executable bit is set.
    executable: bool,
    /// Backing file path, valid while the scanned tree's directory exists.
    origin: PathBuf,
}

/// Source-tree entry relevant to three-way merging.
#[derive(Clone, Debug)]
pub enum TreeNode {
    /// Directory; its permissions do not participate in comparisons.
    Directory,
    /// Regular file metadata backed by an on-disk path.
    File(SourceFile),
    /// Symbolic-link target.
    Symlink(PathBuf),
}

/// Complete changes between two directory trees.
pub struct TreePlan {
    /// Changed path transitions in deterministic order; retained directories are excluded.
    pub paths: BTreeMap<PathBuf, (Option<TreeNode>, Option<TreeNode>)>,
}

impl TreePlan {
    /// Reports whether applying the tree plan changes any path.
    pub fn has_changes(&self) -> bool {
        !self.paths.is_empty()
    }

    /// Prints source-tree changes in deterministic path order.
    pub fn print_report(&self) {
        for (path, (old, new)) in &self.paths {
            let verb = match (old, new) {
                (None, Some(_)) => "Create",
                (Some(_), None) => "Remove",
                _ => "Update",
            };
            println!("{verb} {}", path.display());
        }
    }

    /// Removes old entries, atomically writes regular files, and creates directories and links.
    pub fn apply(&self, root: &Path) -> Result<()> {
        // Reverse path order removes descendants before their parents.
        for (path, (old, new)) in self.paths.iter().rev() {
            if matches!(
                (old, new),
                (Some(TreeNode::File(_)), Some(TreeNode::File(_)))
            ) {
                continue;
            }
            let destination = root.join(path);
            match old {
                Some(TreeNode::Directory) => fs::remove_dir(&destination),
                Some(TreeNode::File(_) | TreeNode::Symlink(_)) => fs::remove_file(&destination),
                None => continue,
            }
            .with_context(|| format!("remove {}", destination.display()))?;
        }

        // Forward path order creates parents before writing their contents.
        for (path, (_, new)) in &self.paths {
            let destination = root.join(path);
            match new {
                Some(TreeNode::Directory) => {
                    fs::create_dir(&destination)
                        .with_context(|| format!("create {}", destination.display()))?;
                }
                Some(TreeNode::File(file)) => {
                    write_file(
                        &destination,
                        &fs::read(&file.origin)?,
                        Some(fs::metadata(&file.origin)?.permissions().mode()),
                    )
                    .with_context(|| {
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

/// Scans a tree into path order, optionally excluding one subtree and rejecting special files.
pub fn scan_tree(root: &Path, exclude: Option<&Path>) -> Result<BTreeMap<PathBuf, TreeNode>> {
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
            if Some(path.as_path()) == exclude {
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
) -> Result<TreePlan> {
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

    let mut result = BTreeMap::new();
    for path in all {
        if let Some(state) = after.remove(&path).unwrap() {
            result.insert(path, state);
        }
    }
    Ok(build_tree_plan(old, &result))
}

/// Plans exact tree differences for writing, without applying source ownership rules.
pub fn build_tree_plan(
    old: &BTreeMap<PathBuf, TreeNode>,
    new: &BTreeMap<PathBuf, TreeNode>,
) -> TreePlan {
    let mut all = BTreeSet::new();
    all.extend(old.keys().cloned());
    all.extend(new.keys().cloned());
    let mut paths = BTreeMap::new();
    for path in all {
        let old_state = old.get(&path);
        let new_state = new.get(&path);
        if !option_states_match(old_state, new_state) {
            paths.insert(path, (old_state.cloned(), new_state.cloned()));
        }
    }
    TreePlan { paths }
}
