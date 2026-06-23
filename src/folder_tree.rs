use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use crate::scan::{Entry, EntryKind};

#[derive(Default)]
pub struct FolderTree {
    pub roots: Vec<PathBuf>,
    pub expanded: HashSet<PathBuf>,
    pub children: HashMap<PathBuf, Vec<PathBuf>>,
    /// Paths that are file (non-dir) leaves when tree_show_files is enabled. These are never keys in
    /// `children` and never expandable (no toggle affordance, no children list).
    pub file_leaves: HashSet<PathBuf>,
}

impl FolderTree {
    pub fn with_roots(roots: Vec<PathBuf>) -> Self {
        Self {
            roots,
            ..Self::default()
        }
    }

    pub fn toggle(&mut self, path: PathBuf) -> bool {
        toggle_expansion(&mut self.expanded, path)
    }

    pub fn load_children_if_absent(&mut self, path: PathBuf, children: Vec<PathBuf>) {
        if children.is_empty() {
            self.expanded.remove(&path);
        }
        self.children.insert(path, children);
    }

    pub fn visible_nodes(&self) -> Vec<(PathBuf, usize)> {
        let mut visible = Vec::new();
        let mut stack: Vec<(PathBuf, usize)> = self
            .roots
            .iter()
            .rev()
            .map(|path| (path.clone(), 0))
            .collect();

        while let Some((path, depth)) = stack.pop() {
            visible.push((path.clone(), depth));
            if self.expanded.contains(&path) {
                if let Some(children) = self.children.get(&path) {
                    for child in children.iter().rev() {
                        stack.push((child.clone(), depth + 1));
                    }
                }
            }
        }
        visible
    }

    pub fn is_expanded(&self, path: &Path) -> bool {
        self.expanded.contains(path)
    }

    pub fn has_loaded_children(&self, path: &Path) -> bool {
        self.children
            .get(path)
            .map(|children| !children.is_empty())
            .unwrap_or(false)
    }

    pub fn has_loaded_node(&self, path: &Path) -> bool {
        self.children.contains_key(path)
    }

    /// Returns true if `path` is a file leaf (non-directory) that was enumerated under
    /// tree_show_files mode. File leaves are never expandable and have no subtree.
    pub fn is_file_leaf(&self, path: &Path) -> bool {
        self.file_leaves.contains(path)
    }
}

pub(crate) fn child_dirs_from_entries(entries: Vec<Entry>) -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = entries
        .into_iter()
        .filter(|entry| entry.kind == EntryKind::Dir)
        .map(|entry| entry.path)
        .collect();
    dirs.sort_by_key(|path| path.to_string_lossy().to_lowercase());
    dirs
}

/// Returns *all* direct child paths (dirs + files + other) sorted case-insensitively by path.
/// Used when config.tree_show_files is true to render a full file tree in the sidebar.
/// File entries become leaves (no expander); only dir entries remain expandable.
/// Interleaves files and folders in alpha order (no "dirs first" grouping like the grid).
pub(crate) fn child_nodes_from_entries(entries: Vec<Entry>) -> Vec<PathBuf> {
    let mut nodes: Vec<PathBuf> = entries.into_iter().map(|entry| entry.path).collect();
    nodes.sort_by_key(|path| path.to_string_lossy().to_lowercase());
    nodes
}

pub(crate) fn toggle_expansion(expanded: &mut HashSet<PathBuf>, path: PathBuf) -> bool {
    if expanded.contains(&path) {
        expanded.remove(&path);
        false
    } else {
        expanded.insert(path);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::{child_dirs_from_entries, child_nodes_from_entries, toggle_expansion};
    use crate::scan::{Entry, EntryKind};
    use std::collections::HashSet;
    use std::path::PathBuf;

    #[test]
    fn child_dirs_from_entries_keeps_only_dirs_sorted_by_path() {
        let entries = vec![
            Entry {
                path: PathBuf::from("/x/Zed"),
                name: "Zed".into(),
                kind: EntryKind::Dir,
                modified: None,
                size: 0,
            },
            Entry {
                path: PathBuf::from("/x/photo.jpg"),
                name: "photo.jpg".into(),
                kind: EntryKind::Image,
                modified: None,
                size: 0,
            },
            Entry {
                path: PathBuf::from("/x/apple"),
                name: "apple".into(),
                kind: EntryKind::Dir,
                modified: None,
                size: 0,
            },
        ];

        assert_eq!(
            child_dirs_from_entries(entries),
            vec![PathBuf::from("/x/apple"), PathBuf::from("/x/Zed")]
        );
    }

    #[test]
    fn child_nodes_from_entries_includes_files_and_dirs_sorted_case_insens() {
        let entries = vec![
            Entry {
                path: PathBuf::from("/x/Zed"),
                name: "Zed".into(),
                kind: EntryKind::Dir,
                modified: None,
                size: 0,
            },
            Entry {
                path: PathBuf::from("/x/photo.jpg"),
                name: "photo.jpg".into(),
                kind: EntryKind::Image,
                modified: None,
                size: 0,
            },
            Entry {
                path: PathBuf::from("/x/apple"),
                name: "apple".into(),
                kind: EntryKind::Dir,
                modified: None,
                size: 0,
            },
        ];

        assert_eq!(
            child_nodes_from_entries(entries),
            vec![
                PathBuf::from("/x/apple"),
                PathBuf::from("/x/photo.jpg"),
                PathBuf::from("/x/Zed")
            ]
        );
    }

    #[test]
    fn toggle_expansion_returns_new_state_and_flips_membership() {
        let mut expanded = HashSet::new();
        let path = PathBuf::from("/home/test");

        assert!(toggle_expansion(&mut expanded, path.clone()));
        assert!(expanded.contains(&path));
        assert!(!toggle_expansion(&mut expanded, path.clone()));
        assert!(!expanded.contains(&path));
    }
}
