//! update() handlers for folder navigation, favorites, and the folder tree.

use cosmic::Task;

use crate::app::{AppModel, Message};

impl AppModel {
    pub(super) fn handle_navigate_to(
        &mut self,
        path: std::path::PathBuf,
    ) -> Task<cosmic::Action<Message>> {
        super::navigation_new_destination(
            &mut self.nav.back,
            &mut self.nav.forward,
            self.browser.current_dir.as_deref(),
        );
        self.set_dir_and_reload(path)
    }

    pub(super) fn handle_navigate_up(&mut self) -> Task<cosmic::Action<Message>> {
        if let Some(dir) = self.browser.current_dir.clone() {
            if let Some(parent) = dir.parent() {
                super::navigation_new_destination(
                    &mut self.nav.back,
                    &mut self.nav.forward,
                    Some(dir.as_path()),
                );
                return self.set_dir_and_reload(parent.to_path_buf());
            }
        }
        Task::none()
    }

    pub(super) fn handle_navigate_back(&mut self) -> Task<cosmic::Action<Message>> {
        if let Some(path) = super::navigation_back(
            &mut self.nav.back,
            &mut self.nav.forward,
            self.browser.current_dir.as_deref(),
        ) {
            return self.set_dir_and_reload(path);
        }
        Task::none()
    }

    pub(super) fn handle_navigate_forward(&mut self) -> Task<cosmic::Action<Message>> {
        if let Some(path) = super::navigation_forward(
            &mut self.nav.back,
            &mut self.nav.forward,
            self.browser.current_dir.as_deref(),
        ) {
            return self.set_dir_and_reload(path);
        }
        Task::none()
    }

    pub(super) fn handle_add_favorite(&mut self) -> Task<cosmic::Action<Message>> {
        if let Some(path) = self.browser.current_dir.clone() {
            if super::add_favorite(&mut self.config.favorites, path) {
                self.config.save();
            }
        }
        Task::none()
    }

    pub(super) fn handle_remove_favorite(
        &mut self,
        path: std::path::PathBuf,
    ) -> Task<cosmic::Action<Message>> {
        if super::remove_favorite(&mut self.config.favorites, &path) {
            self.config.save();
        }
        Task::none()
    }

    pub(super) fn handle_toggle_tree_node(
        &mut self,
        path: std::path::PathBuf,
    ) -> Task<cosmic::Action<Message>> {
        let expanded_now = self.nav.tree.toggle(path.clone());
        if expanded_now && !self.nav.tree.has_loaded_node(&path) {
            let children = crate::scan::scan_dir(&path, self.config.show_hidden)
                .map(|entries| {
                    if self.config.tree_show_files {
                        for entry in &entries {
                            if entry.kind != crate::scan::EntryKind::Dir {
                                self.nav.tree.file_leaves.insert(entry.path.clone());
                            }
                        }
                        crate::folder_tree::child_nodes_from_entries(entries)
                    } else {
                        crate::folder_tree::child_dirs_from_entries(entries)
                    }
                })
                .unwrap_or_else(|err| {
                    tracing::warn!(path = %path.display(), %err, "failed to load tree node");
                    Vec::new()
                });
            self.nav.tree.load_children_if_absent(path, children);
        }
        Task::none()
    }
}
