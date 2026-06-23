//! update() handlers for View filters (rating/label/camera/tag/date/hide-rejected/images-only) and saved collections.

use cosmic::Task;

use crate::app::{AppModel, Message};

impl AppModel {
    pub(super) fn handle_toggle_images_only(&mut self) -> Task<cosmic::Action<Message>> {
        self.config.images_only = !self.config.images_only;
        self.config.save();
        // Rebuild the grid snapshot (view filter only; no rescan or thumb invalidation,
        // unlike ToggleShowHidden which affects the underlying entries list).
        self.scroll_offset_y = 0.0;
        self.selection.selected_index = None;
        self.selection.multi_selected.clear();
        self.selection.batch_status = None;
        self.selection.select_anchor = None;
        self.preview = None;
        self.rebuild_snapshot();
        self.browser
            .request_capture_epoch_batch(self.config.sort_mode, self.config.images_only)
    }

    pub(super) fn handle_cycle_rating_filter(&mut self) -> Task<cosmic::Action<Message>> {
        self.filter.rating = crate::cull::next_rating_filter(self.filter.rating);
        if self.filter.rating.is_some() {
            self.ensure_all_cull_cached();
        }
        self.scroll_offset_y = 0.0; // filtered view starts at top
        self.rebuild_snapshot();
        Task::none()
    }

    pub(super) fn handle_cycle_label_filter(&mut self) -> Task<cosmic::Action<Message>> {
        self.filter.label = crate::cull::next_label_filter(self.filter.label);
        if self.filter.label.is_some() {
            self.ensure_all_cull_cached();
        }
        self.scroll_offset_y = 0.0;
        self.rebuild_snapshot();
        Task::none()
    }

    pub(super) fn handle_toggle_hide_rejected(&mut self) -> Task<cosmic::Action<Message>> {
        self.filter.hide_rejected = !self.filter.hide_rejected;
        if self.filter.hide_rejected {
            self.ensure_all_cull_cached();
        }
        self.scroll_offset_y = 0.0;
        self.rebuild_snapshot();
        Task::none()
    }

    pub(super) fn handle_set_label_filter(
        &mut self,
        x: Option<crate::xmp::ColorLabel>,
    ) -> Task<cosmic::Action<Message>> {
        self.filter.label = x;
        if x.is_some() {
            self.ensure_all_cull_cached();
        }
        self.scroll_offset_y = 0.0;
        self.rebuild_snapshot();
        Task::none()
    }

    pub(super) fn handle_set_camera_filter(
        &mut self,
        idx: Option<usize>,
    ) -> Task<cosmic::Action<Message>> {
        self.filter.camera = idx.and_then(|i| self.sorted_cameras().get(i).cloned());
        self.scroll_offset_y = 0.0;
        self.rebuild_snapshot();
        Task::none()
    }

    pub(super) fn handle_set_tag_filter(
        &mut self,
        idx: Option<usize>,
    ) -> Task<cosmic::Action<Message>> {
        self.filter.tag = idx.and_then(|i| self.sorted_tags().get(i).cloned());
        self.scroll_offset_y = 0.0;
        self.rebuild_snapshot();
        Task::none()
    }

    pub(super) fn handle_set_date_filter(
        &mut self,
        year: Option<i32>,
    ) -> Task<cosmic::Action<Message>> {
        self.filter.date = year;
        self.scroll_offset_y = 0.0;
        self.rebuild_snapshot();
        Task::none()
    }

    pub(super) fn handle_save_collection(&mut self) -> Task<cosmic::Action<Message>> {
        let label = self.filter.label.map(|l| l.as_str().to_owned());
        if let Some(name) = super::describe_filters(
            self.filter.rating,
            label.as_deref(),
            self.filter.camera.as_deref(),
            self.filter.date,
            self.filter.tag.as_deref(),
        ) {
            self.config
                .collections
                .push(crate::config::SavedCollection {
                    name,
                    rating_min: self.filter.rating,
                    label,
                    camera: self.filter.camera.clone(),
                    date_year: self.filter.date,
                    tag: self.filter.tag.clone(),
                });
            self.config.save();
        } else {
            self.selection.batch_status = Some("No active filters to save".to_owned());
        }
        Task::none()
    }

    pub(super) fn handle_apply_collection(&mut self, i: usize) -> Task<cosmic::Action<Message>> {
        if let Some(c) = self.config.collections.get(i).cloned() {
            self.filter.rating = c.rating_min;
            self.filter.label = c
                .label
                .as_deref()
                .and_then(crate::xmp::ColorLabel::from_str);
            self.filter.camera = c.camera;
            self.filter.date = c.date_year;
            self.filter.tag = c.tag;
            self.scroll_offset_y = 0.0;
            self.rebuild_snapshot();
        }
        Task::none()
    }

    pub(super) fn handle_clear_collections(&mut self) -> Task<cosmic::Action<Message>> {
        self.config.collections.clear();
        self.config.save();
        Task::none()
    }
}
