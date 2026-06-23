//! update() handlers for the Settings panel (cache dir/size, export dir, show-hidden, tree show-files).

use cosmic::ApplicationExt;
use cosmic::Task;

use crate::app::{AppModel, Message};

impl AppModel {
    pub(super) fn handle_toggle_settings(&mut self) -> Task<cosmic::Action<Message>> {
        self.settings_open = !self.settings_open;
        self.set_show_context(self.settings_open);
        if self.settings_open {
            self.refresh_cache_inputs();
            self.export_dir_input = self
                .config
                .export_dir
                .as_ref()
                .map(|path| path.display().to_string())
                .unwrap_or_default();
        }
        Task::none()
    }

    pub(super) fn handle_close_settings(&mut self) -> Task<cosmic::Action<Message>> {
        self.settings_open = false;
        self.set_show_context(false);
        Task::none()
    }

    pub(super) fn handle_cache_max_gb_changed(
        &mut self,
        value: String,
    ) -> Task<cosmic::Action<Message>> {
        self.cache_max_input = value.clone();
        if let Ok(max_gb) = value.trim().parse::<f64>() {
            if max_gb.is_finite() && max_gb >= 0.0 {
                self.config.cache_max_gb = max_gb;
                self.persist_cache_config();
                self.prune_cache_to_configured_max();
            }
        }
        Task::none()
    }

    pub(super) fn handle_cache_dir_input_changed(
        &mut self,
        value: String,
    ) -> Task<cosmic::Action<Message>> {
        self.cache_dir_input = value;
        Task::none()
    }

    pub(super) fn handle_apply_cache_dir(&mut self) -> Task<cosmic::Action<Message>> {
        let trimmed = self.cache_dir_input.trim();
        if !trimmed.is_empty() {
            self.config.cache_dir = Some(std::path::PathBuf::from(trimmed));
            self.persist_cache_config();
            self.prune_cache_to_configured_max();
        }
        Task::none()
    }

    pub(super) fn handle_export_dir_input_changed(
        &mut self,
        value: String,
    ) -> Task<cosmic::Action<Message>> {
        self.export_dir_input = value;
        Task::none()
    }

    pub(super) fn handle_apply_export_dir(&mut self) -> Task<cosmic::Action<Message>> {
        let trimmed = self.export_dir_input.trim();
        self.config.export_dir = if trimmed.is_empty() {
            None
        } else {
            Some(std::path::PathBuf::from(trimmed))
        };
        self.config.save();
        Task::none()
    }

    pub(super) fn handle_reset_cache_dir(&mut self) -> Task<cosmic::Action<Message>> {
        self.config.cache_dir = None;
        self.persist_cache_config();
        self.prune_cache_to_configured_max();
        self.refresh_cache_inputs();
        Task::none()
    }

    pub(super) fn handle_toggle_show_hidden(&mut self) -> Task<cosmic::Action<Message>> {
        self.config.show_hidden = !self.config.show_hidden;
        self.config.save();
        self.thumb.next_generation();
        self.reload();
        self.prune_cache_to_configured_max();
        Task::batch([
            self.set_current_window_title(),
            self.browser
                .request_capture_epoch_batch(self.config.sort_mode, self.config.images_only),
        ])
    }

    pub(super) fn handle_toggle_tree_show_files(&mut self) -> Task<cosmic::Action<Message>> {
        self.config.tree_show_files = !self.config.tree_show_files;
        self.config.save();
        // Reset tree expansion/children so subsequent loads use the correct
        // child_nodes_from_entries (files+dirs) vs child_dirs_from_entries (dirs only).
        // Files leaves are also cleared; they will be repopulated on next expand.
        self.nav.tree.children.clear();
        self.nav.tree.expanded.clear();
        self.nav.tree.file_leaves.clear();
        Task::none()
    }
}
