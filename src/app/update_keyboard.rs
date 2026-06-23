//! update() handlers for keyboard/input (modifiers, select-all, the global key dispatcher, space, refresh).

use cosmic::Application;
use cosmic::ApplicationExt;
use cosmic::Task;

use crate::app::{AppModel, Message};

impl AppModel {
    pub(super) fn handle_modifiers_changed(
        &mut self,
        m: cosmic::iced::keyboard::Modifiers,
    ) -> Task<cosmic::Action<Message>> {
        self.selection.modifiers = m;
        Task::none()
    }

    pub(super) fn handle_select_all(&mut self) -> Task<cosmic::Action<Message>> {
        if self.filter_focused {
            return Task::none();
        }
        let displayed = self.grid_indices();
        self.selection.multi_selected = displayed.iter().copied().collect();
        let primary = displayed.first().copied();
        self.selection.selected_index = primary;
        self.selection.select_anchor = primary;
        self.rebuild_snapshot();
        Task::none()
    }

    pub(super) fn handle_key_pressed(
        &mut self,
        named: cosmic::iced::keyboard::key::Named,
    ) -> Task<cosmic::Action<Message>> {
        use cosmic::iced::keyboard::key::Named;

        if self.filter_focused || self.settings_open {
            if named == Named::Escape {
                if self.filter_focused && !self.browser.filter_query.is_empty() {
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
                } else {
                    self.filter_focused = false;
                    self.settings_open = false;
                    self.set_show_context(false);
                }
            }
            return Task::none();
        }

        if self.compare.is_some() {
            if named == Named::Escape {
                self.compare = None;
            }
            return Task::none();
        }

        if self.loupe.is_some() {
            match named {
                Named::ArrowLeft | Named::ArrowRight => {
                    let displayed = self.displayed_indices();
                    if displayed.is_empty() {
                        return Task::none();
                    }
                    let current_index = self.loupe.as_ref().map(|loupe| loupe.index).unwrap_or(0);
                    let cursor = displayed
                        .iter()
                        .position(|idx| *idx == current_index)
                        .unwrap_or(0);
                    let next_cursor = crate::nav::loupe_step(
                        displayed.len(),
                        cursor,
                        named == Named::ArrowRight,
                        true,
                    );
                    let next_index = displayed[next_cursor];
                    if let Some(entry) = self.browser.entries.get(next_index) {
                        let path = entry.path.clone();
                        let bound = crate::view::loupe::loupe_decode_bound(
                            self.viewport_w,
                            self.viewport_h,
                        );
                        let mode = super::loupe_base_decode_mode(&entry.kind);
                        let want_full_demosaic =
                            super::loupe_decode_mode(self.config.loupe_full_demosaic, &entry.kind)
                                == crate::inspection::LoupeDecodeMode::FullRaw;
                        let base_cached = self
                            .preview_cache
                            .get(&path, mode, self.config.develop_look)
                            .map(|cached| (cached.handle.clone(), cached.dimensions));
                        let demosaic_cached = want_full_demosaic.then(|| {
                            self.preview_cache
                                .get(
                                    &path,
                                    crate::inspection::LoupeDecodeMode::FullRaw,
                                    self.config.develop_look,
                                )
                                .map(|cached| (cached.handle.clone(), cached.dimensions))
                        });
                        let mut decode_tasks = Vec::new();
                        if let Some(loupe) = &mut self.loupe {
                            // Keep the frame we're leaving on-screen until the next image
                            // decodes, so nav shows the prior photo instead of gray. If the
                            // image we're leaving hasn't decoded yet (fast scrolling past
                            // unloaded images), fall back to the existing placeholder so we
                            // never overwrite the last good frame with a blank.
                            let prev_shown = loupe
                                .demosaic_handle
                                .clone()
                                .or_else(|| loupe.handle.clone())
                                .or_else(|| loupe.placeholder_handle.clone());
                            loupe.index = next_index;
                            loupe.path = path.clone();
                            loupe.decode_mode = mode;
                            loupe.zoom = crate::inspection::LoupeZoom::Fit;
                            loupe.placeholder_handle = prev_shown;
                            if let Some((handle, dimensions)) = base_cached {
                                loupe.histogram = super::histogram_from_handle(&handle);
                                loupe.handle = Some(handle);
                                loupe.dimensions = Some(dimensions);
                                self.loupe_rt.decode_pending = None;
                            } else {
                                loupe.handle = None;
                                loupe.histogram = None;
                                loupe.dimensions = None;
                                self.loupe_rt.decode_pending = Some((path.clone(), mode));
                                decode_tasks.push(crate::tasks::decode_loupe_task(
                                    path.clone(),
                                    bound,
                                    mode,
                                    self.config.develop_look,
                                ));
                            }
                            loupe.error = None;
                            loupe.high_res_handle = None;
                            loupe.high_res_dimensions = None;
                            loupe.demosaic_handle = None;
                            loupe.demosaic_dimensions = None;
                            loupe.want_full_demosaic = want_full_demosaic;
                            match demosaic_cached {
                                Some(Some((handle, dimensions))) => {
                                    loupe.demosaic_handle = Some(handle);
                                    loupe.demosaic_dimensions = Some(dimensions);
                                }
                                Some(None) => decode_tasks.push(crate::tasks::decode_loupe_task(
                                    path.clone(),
                                    bound,
                                    crate::inspection::LoupeDecodeMode::FullRaw,
                                    self.config.develop_look,
                                )),
                                None => {}
                            }
                            let (r, x) = crate::xmp::read_loupe_sidecar(&path);
                            loupe.rating = r;
                            loupe.xmp = x;
                        }
                        return super::chain_decode_tasks(decode_tasks);
                    }
                }
                Named::Escape => {
                    self.loupe = None;
                    self.loupe_rt.decode_pending = None;
                    self.loupe_rt.slideshow_playing = false;
                    self.loupe_rt.scroll =
                        cosmic::iced::widget::scrollable::AbsoluteOffset::default();
                    self.loupe_rt.drag = None;
                    self.loupe_rt.pan_last_cursor = None;
                    self.loupe_rt.zoom_factor = 1.0;
                }
                _ => {}
            }
            return Task::none();
        }

        match named {
            Named::ArrowLeft
            | Named::ArrowRight
            | Named::ArrowUp
            | Named::ArrowDown
            | Named::Home
            | Named::End
            | Named::PageUp
            | Named::PageDown => {
                let grid_indices = self.grid_indices();
                if grid_indices.is_empty() {
                    return Task::none();
                }
                let selected_pos = self
                    .selection
                    .selected_index
                    .and_then(|idx| grid_indices.iter().position(|entry_idx| *entry_idx == idx))
                    .unwrap_or(0);
                let (avail_w, avail_h) = self.effective_grid_size();
                let cols = ((avail_w / self.config.thumb_size as f32).floor() as usize).max(1);
                let visible_rows =
                    ((avail_h / super::cell_h(self.config.thumb_size)).floor() as usize).max(1);
                let new_pos = match named {
                    Named::ArrowLeft => {
                        crate::nav::grid_step(selected_pos, grid_indices.len(), cols, -1, 0)
                    }
                    Named::ArrowRight => {
                        crate::nav::grid_step(selected_pos, grid_indices.len(), cols, 1, 0)
                    }
                    Named::ArrowUp => {
                        crate::nav::grid_step(selected_pos, grid_indices.len(), cols, 0, -1)
                    }
                    Named::ArrowDown => {
                        crate::nav::grid_step(selected_pos, grid_indices.len(), cols, 0, 1)
                    }
                    Named::Home => 0,
                    Named::End => grid_indices.len().saturating_sub(1),
                    Named::PageUp => crate::nav::page_step(
                        selected_pos,
                        grid_indices.len(),
                        cols,
                        visible_rows,
                        false,
                    ),
                    Named::PageDown => crate::nav::page_step(
                        selected_pos,
                        grid_indices.len(),
                        cols,
                        visible_rows,
                        true,
                    ),
                    _ => selected_pos,
                };
                let new = grid_indices[new_pos];

                self.scroll_position_into_view(new_pos);
                let new_entry = &self.browser.entries[new];
                if matches!(
                    new_entry.kind,
                    crate::scan::EntryKind::Image | crate::scan::EntryKind::Raw
                ) || crate::scan::is_text_previewable(&new_entry.name)
                {
                    return self.update(Message::SelectImage(new));
                }
                if matches!(new_entry.kind, crate::scan::EntryKind::Dir) {
                    self.selection.selected_index = Some(new);
                    self.selection.multi_selected = std::iter::once(new).collect();
                    self.selection.select_anchor = Some(new);
                }
            }
            Named::Enter => {
                let grid_indices = self.grid_indices();
                let selected = self
                    .selection
                    .selected_index
                    .filter(|idx| grid_indices.contains(idx))
                    .or_else(|| grid_indices.first().copied())
                    .unwrap_or(0);
                if let Some(entry) = self.browser.entries.get(selected) {
                    match entry.kind {
                        crate::scan::EntryKind::Dir => {
                            return self.update(Message::NavigateTo(entry.path.clone()));
                        }
                        crate::scan::EntryKind::Image | crate::scan::EntryKind::Raw => {
                            return self.update(Message::OpenLoupe(selected));
                        }
                        crate::scan::EntryKind::Other(_) => {}
                    }
                }
            }
            Named::Escape => {
                // Loupe closed (this arm): reduce multi-selection to the current primary single.
                // Loupe Escape handling and filter/settings Escape handling happen earlier.
                self.selection.multi_selected = self.selection.selected_index.into_iter().collect();
                self.selection.select_anchor = self.selection.selected_index;
                self.rebuild_snapshot();
            }
            _ => {}
        }
        Task::none()
    }

    pub(super) fn handle_space_pressed(&mut self) -> Task<cosmic::Action<Message>> {
        if let Some(selected) = super::space_loupe_selection(
            &self.browser.entries,
            self.selection.selected_index,
            self.filter_focused,
            self.settings_open,
        ) {
            return self.update(Message::OpenLoupe(selected));
        }
        Task::none()
    }

    pub(super) fn handle_refresh_current_folder(&mut self) -> Task<cosmic::Action<Message>> {
        if self.filter_focused || self.settings_open {
            return Task::none();
        }
        self.thumb.next_generation();
        self.reload();
        self.prune_cache_to_configured_max();
        Task::batch([
            self.set_current_window_title(),
            self.browser
                .request_capture_epoch_batch(self.config.sort_mode, self.config.images_only),
        ])
    }
}
