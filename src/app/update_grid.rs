//! update() handlers for grid/list interactions (thumb tick, scroll, thumb size, sort, filter/keyword input, select) and catalog-index result callbacks.

use cosmic::Task;

use crate::app::{AppModel, Message};

impl AppModel {
    pub(super) fn handle_thumb_tick(&mut self) -> Task<cosmic::Action<Message>> {
        // If `view()`'s responsive wrapper has measured a new grid size
        // (window opened/resized), propagate it to the request side so
        // the requested indices match what the view now renders.
        if self.has_pending_grid_geometry() {
            self.rebuild_snapshot();
        }
        self.apply_drained_thumbnails();
        // Otherwise nothing new to show — skip the snapshot rebuild.
        Task::none()
    }

    pub(super) fn handle_scroll(&mut self, y: f32) -> Task<cosmic::Action<Message>> {
        self.scroll_offset_y = y;
        // Recompute the visible range the new offset would produce; if it
        // matches the already-materialized range the visible cells are
        // identical and a full snapshot rebuild is unnecessary.
        let (avail_w, avail_h) = self.effective_grid_size();
        let cols = ((avail_w / self.config.thumb_size as f32).floor() as usize).max(1);
        let ch = super::cell_h(self.config.thumb_size);
        let new_range = crate::view::grid::visible_range(
            y,
            avail_h,
            ch,
            cols,
            self.grid_indices().len(),
            crate::view::grid::MARGIN_ROWS,
        );
        if new_range != self.visible_index_range {
            self.rebuild_snapshot();
        }
        Task::none()
    }

    pub(super) fn handle_thumb_size_changed(&mut self, size: u16) -> Task<cosmic::Action<Message>> {
        let snapped = super::snap_thumb_size(size);
        if self.config.thumb_size != snapped {
            self.config.thumb_size = snapped;
            // ThumbCache is path-keyed and the grid scales image handles
            // to the current display size, so keep the service/LRU intact
            // and only re-layout the visible snapshot.
            self.rebuild_snapshot();
        }
        Task::none()
    }

    pub(super) fn handle_sort_mode_changed(
        &mut self,
        mode: crate::config::SortMode,
    ) -> Task<cosmic::Action<Message>> {
        self.activate_sort_segment(mode);
        if self.config.sort_mode != mode {
            self.config.sort_mode = mode;
            if mode == crate::config::SortMode::Rating {
                self.ensure_all_cull_cached();
                self.config.save();
            }
            self.browser.sort_for_mode(self.config.sort_mode);
            self.scroll_offset_y = 0.0;
            self.selection.selected_index = None;
            self.selection.multi_selected.clear();
            self.selection.batch_status = None;
            self.selection.select_anchor = None;
            self.preview = None;
            self.text_preview = None;
            self.rebuild_snapshot();
            return self
                .browser
                .request_capture_epoch_batch(self.config.sort_mode, self.config.images_only);
        }
        Task::none()
    }

    pub(super) fn handle_filter_changed(&mut self, query: String) -> Task<cosmic::Action<Message>> {
        self.browser.set_filter(query);
        self.filter_focused = true;
        self.scroll_offset_y = 0.0;
        self.selection.selected_index = None;
        self.selection.multi_selected.clear();
        self.selection.batch_status = None;
        self.selection.select_anchor = None;
        self.preview = None;
        self.text_preview = None;
        self.rebuild_snapshot();
        Task::none()
    }

    pub(super) fn handle_clear_filter(&mut self) -> Task<cosmic::Action<Message>> {
        self.browser.clear_filter();
        self.filter_focused = false;
        self.scroll_offset_y = 0.0;
        self.selection.selected_index = None;
        self.selection.multi_selected.clear();
        self.selection.batch_status = None;
        self.selection.select_anchor = None;
        self.preview = None;
        self.text_preview = None;
        self.rebuild_snapshot();
        Task::none()
    }

    pub(super) fn handle_keyword_input_changed(
        &mut self,
        s: String,
    ) -> Task<cosmic::Action<Message>> {
        self.keyword_input = s;
        Task::none()
    }

    pub(super) fn handle_add_keyword(&mut self) -> Task<cosmic::Action<Message>> {
        let kw = self.keyword_input.trim().to_owned();
        if !kw.is_empty() {
            let (ok, total) = self.add_keyword_to_targets(&kw);
            if total > 0 {
                self.selection.batch_status = Some(crate::cull::batch_summary(ok, total, "tagged"));
            }
            self.keyword_input.clear();
            self.rebuild_snapshot();
        }
        Task::none()
    }

    pub(super) fn handle_remove_keyword(&mut self, kw: String) -> Task<cosmic::Action<Message>> {
        let (ok, total) = self.remove_keyword_from_targets(&kw);
        if total > 0 {
            self.selection.batch_status = Some(crate::cull::batch_summary(ok, total, "untagged"));
        }
        self.scroll_offset_y = 0.0;
        self.rebuild_snapshot();
        Task::none()
    }

    pub(super) fn handle_select_image(&mut self, index: usize) -> Task<cosmic::Action<Message>> {
        // Compute multi/primary/anchor using current modifiers + displayed order.
        let displayed = self.grid_indices();
        let (new_multi, primary, new_anchor) = super::compute_selection(
            &self.selection.multi_selected,
            self.selection.select_anchor,
            index,
            self.selection.modifiers.control(),
            self.selection.modifiers.shift(),
            &displayed,
        );
        self.selection.multi_selected = new_multi;
        self.selection.selected_index = Some(primary);
        self.selection.select_anchor = new_anchor;

        if let Some(entry) = self.browser.entries.get(index) {
            if matches!(
                entry.kind,
                crate::scan::EntryKind::Image | crate::scan::EntryKind::Raw
            ) {
                let path = entry.path.clone();
                self.text_preview = None;
                self.preview = Some(crate::inspection::PreviewState {
                    path: path.clone(),
                    handle: None,
                    dimensions: None,
                    error: None,
                    metadata: None,
                    histogram: None,
                });
                // Selection changes the highlighted cell and preview content,
                // but the preview pane width is constant, so this rebuild
                // refreshes cell state without reflowing the grid width.
                self.rebuild_snapshot();
                return cosmic::Task::batch([
                    crate::tasks::decode_preview_task(path.clone()),
                    crate::tasks::metadata_task(path),
                ]);
            } else if crate::scan::is_text_previewable(&entry.name) {
                let path = entry.path.clone();
                self.preview = None;
                self.text_preview = Some((path.clone(), crate::scan::read_text_preview(&path)));
                self.rebuild_snapshot();
                return cosmic::Task::none();
            }
        }
        Task::none()
    }

    pub(super) fn handle_folder_indexed(
        &mut self,
        items: Vec<(usize, Option<i64>, Option<String>)>,
    ) -> Task<cosmic::Action<Message>> {
        self.folder_metadata = items.into_iter().map(|(i, c, cam)| (i, (c, cam))).collect();
        Task::none()
    }

    pub(super) fn handle_folder_keywords_indexed(
        &mut self,
        items: Vec<(usize, Vec<String>)>,
    ) -> Task<cosmic::Action<Message>> {
        self.folder_tags = items.into_iter().collect();
        Task::none()
    }
}
