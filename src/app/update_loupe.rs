//! update() handlers for the loupe (open/close, zoom/pan, decode, slideshow, loupe culling) and 2-up compare.

use cosmic::Application;
use cosmic::Task;

use crate::app::{AppModel, Message};

impl AppModel {
    pub(super) fn handle_set_loupe_zoom(
        &mut self,
        zoom: crate::inspection::LoupeZoom,
    ) -> Task<cosmic::Action<Message>> {
        if self.filter_focused || self.settings_open {
            return Task::none();
        }
        if let Some(loupe) = &mut self.loupe {
            loupe.zoom = zoom;
            if zoom == crate::inspection::LoupeZoom::Fit {
                self.loupe_rt.scroll = cosmic::iced::widget::scrollable::AbsoluteOffset::default();
                self.loupe_rt.drag = None;
                self.loupe_rt.pan_last_cursor = None;
                self.loupe_rt.zoom_factor = 1.0;
            } else if zoom == crate::inspection::LoupeZoom::Actual {
                self.loupe_rt.zoom_factor = 1.0;
            }
            if zoom == crate::inspection::LoupeZoom::Actual && self.config.full_res_loupe {
                return self.ensure_high_res_for_current_loupe();
            }
        }
        Task::none()
    }

    pub(super) fn handle_toggle_loupe_zoom(&mut self) -> Task<cosmic::Action<Message>> {
        if self.filter_focused || self.settings_open {
            return Task::none();
        }
        if let Some(loupe) = &mut self.loupe {
            loupe.zoom = crate::inspection::toggle_loupe_zoom(loupe.zoom);
            if loupe.zoom == crate::inspection::LoupeZoom::Fit {
                self.loupe_rt.scroll = cosmic::iced::widget::scrollable::AbsoluteOffset::default();
                self.loupe_rt.drag = None;
                self.loupe_rt.pan_last_cursor = None;
                self.loupe_rt.zoom_factor = 1.0;
            } else if loupe.zoom == crate::inspection::LoupeZoom::Actual {
                self.loupe_rt.zoom_factor = 1.0;
            }
            if loupe.zoom == crate::inspection::LoupeZoom::Actual && self.config.full_res_loupe {
                return self.ensure_high_res_for_current_loupe();
            }
        }
        Task::none()
    }

    pub(super) fn handle_loupe_zoom_step(
        &mut self,
        zoom_in: bool,
    ) -> Task<cosmic::Action<Message>> {
        if self.filter_focused || self.settings_open {
            return Task::none();
        }
        if let Some(loupe) = &mut self.loupe {
            // From Fit, the first zoom-in enters Actual so there is something to scale/pan.
            if loupe.zoom == crate::inspection::LoupeZoom::Fit {
                if zoom_in {
                    loupe.zoom = crate::inspection::LoupeZoom::Actual;
                    self.loupe_rt.zoom_factor = super::step_loupe_zoom(1.0, true);
                    if self.config.full_res_loupe {
                        return self.ensure_high_res_for_current_loupe();
                    }
                }
                // zoom-out while already Fit: nothing to do.
            } else {
                let old = self.loupe_rt.zoom_factor;
                let new = super::step_loupe_zoom(old, zoom_in);
                self.loupe_rt.zoom_factor = new;
                let vw = {
                    let m = self.measured_w.load(std::sync::atomic::Ordering::Relaxed);
                    if m > 0 {
                        m as f32
                    } else {
                        self.viewport_w
                    }
                };
                let vh = {
                    let m = self.measured_h.load(std::sync::atomic::Ordering::Relaxed);
                    if m > 0 {
                        m as f32
                    } else {
                        self.viewport_h
                    }
                };
                if let Some(handle) = loupe
                    .high_res_handle
                    .as_ref()
                    .or(loupe.demosaic_handle.as_ref())
                    .or(loupe.handle.as_ref())
                {
                    if let Some((pw, ph)) = crate::view::loupe::decoded_handle_dimensions(handle) {
                        let content_w = pw as f32 * new;
                        let content_h = ph as f32 * new;
                        let max_off_x = (content_w - vw).max(0.0);
                        let max_off_y = (content_h - vh).max(0.0);
                        let new_x = crate::view::loupe::focal_zoom_offset(
                            self.loupe_rt.scroll.x,
                            vw / 2.0,
                            old,
                            new,
                            max_off_x,
                        );
                        let new_y = crate::view::loupe::focal_zoom_offset(
                            self.loupe_rt.scroll.y,
                            vh / 2.0,
                            old,
                            new,
                            max_off_y,
                        );
                        let target =
                            cosmic::iced::widget::scrollable::AbsoluteOffset { x: new_x, y: new_y };
                        self.loupe_rt.scroll = target;
                        let id = crate::view::loupe::LOUPE_SCROLL_ID.clone();
                        let t: cosmic::iced::Task<Message> =
                            cosmic::iced::widget::scrollable::scroll_to(id, target.into());
                        return t.map(cosmic::Action::App);
                    }
                }
                // No dims or handle: step factor only (no offset adjust).
            }
        }
        Task::none()
    }

    pub(super) fn handle_open_loupe(&mut self, index: usize) -> Task<cosmic::Action<Message>> {
        let entering_loupe = self.loupe.is_none();
        if let Some(entry) = self.browser.entries.get(index) {
            if crate::view::loupe::can_open_loupe(&entry.kind) {
                let path = entry.path.clone();
                let bound =
                    crate::view::loupe::loupe_decode_bound(self.viewport_w, self.viewport_h);
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
                let (rating, xmp) = crate::xmp::read_loupe_sidecar(&path);
                let mut decode_tasks = Vec::new();
                let (handle, dimensions) = if let Some((handle, dimensions)) = base_cached {
                    self.loupe_rt.decode_pending = None;
                    (Some(handle), Some(dimensions))
                } else {
                    self.loupe_rt.decode_pending = Some((path.clone(), mode));
                    decode_tasks.push(crate::tasks::decode_loupe_task(
                        path.clone(),
                        bound,
                        mode,
                        self.config.develop_look,
                    ));
                    (None, None)
                };
                let (demosaic_handle, demosaic_dimensions) = match demosaic_cached {
                    Some(Some((handle, dimensions))) => (Some(handle), Some(dimensions)),
                    Some(None) => {
                        decode_tasks.push(crate::tasks::decode_loupe_task(
                            path.clone(),
                            bound,
                            crate::inspection::LoupeDecodeMode::FullRaw,
                            self.config.develop_look,
                        ));
                        (None, None)
                    }
                    None => (None, None),
                };
                // If re-opening from within the loupe (e.g. a filmstrip jump), keep the
                // current frame on-screen as a placeholder until the new image decodes.
                // Fall back to the prior placeholder if the current frame hasn't decoded
                // yet, so a jump from a still-loading image doesn't flash gray.
                let placeholder_handle = self.loupe.as_ref().and_then(|l| {
                    l.demosaic_handle
                        .clone()
                        .or_else(|| l.handle.clone())
                        .or_else(|| l.placeholder_handle.clone())
                });
                let histogram = handle.as_ref().and_then(super::histogram_from_handle);
                self.loupe = Some(crate::inspection::LoupeState {
                    index,
                    path: path.clone(),
                    decode_mode: mode,
                    zoom: crate::inspection::LoupeZoom::Fit,
                    handle,
                    dimensions,
                    error: None,
                    high_res_handle: None,
                    high_res_dimensions: None,
                    demosaic_handle,
                    demosaic_dimensions,
                    want_full_demosaic,
                    placeholder_handle,
                    rating,
                    xmp,
                    histogram,
                });
                self.loupe_rt.scroll = cosmic::iced::widget::scrollable::AbsoluteOffset::default();
                self.loupe_rt.drag = None;
                self.loupe_rt.pan_last_cursor = None;
                self.loupe_rt.zoom_factor = 1.0;
                self.request_filmstrip_thumbs();
                if entering_loupe {
                    self.loupe_rt.slideshow_playing = false;
                }
                return super::chain_decode_tasks(decode_tasks);
            }
        }
        Task::none()
    }

    pub(super) fn handle_loupe_decoded(
        &mut self,
        path: std::path::PathBuf,
        mode: crate::inspection::LoupeDecodeMode,
        decoded: crate::inspection::DecodedImage,
        error: Option<String>,
    ) -> Task<cosmic::Action<Message>> {
        let mut recovery_decode: Option<(std::path::PathBuf, crate::inspection::LoupeDecodeMode)> =
            None;
        if let Some(loupe) = &mut self.loupe {
            let result_role = if loupe.path == path {
                super::loupe_result_role(loupe.decode_mode, mode)
            } else {
                None
            };
            if let Some(result_role) = result_role {
                if result_role == super::LoupeResultRole::Base
                    && self
                        .loupe_rt
                        .decode_pending
                        .as_ref()
                        .is_some_and(|(p, m)| p == &path && *m == mode)
                {
                    self.loupe_rt.decode_pending = None;
                }
                match decoded {
                    Some((handle, w, h)) => match result_role {
                        super::LoupeResultRole::HighResUpgrade => {
                            // High-res path (≤8192) for Actual when full_res_loupe on.
                            // Enforce at-most-one by purging other highs, then insert under distinct mode.
                            // The outer path== check already guarantees this is for the current loupe image
                            // (nav/close paths clear high or change loupe before/during; stale results simply
                            // do not enter this arm and never insert a high-res for a non-current image).
                            self.preview_cache.purge_high_res_except(Some(&path));
                            self.preview_cache.insert(
                                path.clone(),
                                mode,
                                self.config.develop_look,
                                handle.clone(),
                                (w, h),
                            );
                            loupe.high_res_handle = Some(handle);
                            loupe.high_res_dimensions = Some((w, h));
                            // main handle/dimensions/error unchanged (fit decode stays)
                        }
                        super::LoupeResultRole::DemosaicUpgrade => {
                            self.preview_cache.insert(
                                path.clone(),
                                crate::inspection::LoupeDecodeMode::FullRaw,
                                self.config.develop_look,
                                handle.clone(),
                                (w, h),
                            );
                            if loupe.want_full_demosaic {
                                loupe.demosaic_handle = Some(handle);
                                loupe.demosaic_dimensions = Some((w, h));
                            }
                        }
                        super::LoupeResultRole::Base => {
                            // Populate the separate preview/loupe cache on successful
                            // decode (key includes mode so FullRaw vs embedded differ).
                            self.preview_cache.insert(
                                path.clone(),
                                mode,
                                self.config.develop_look,
                                handle.clone(),
                                (w, h),
                            );
                            let hist = super::histogram_from_handle(&handle);
                            loupe.handle = Some(handle);
                            loupe.histogram = hist;
                            loupe.dimensions = Some((w, h));
                            loupe.error = None;
                        }
                    },
                    None => match result_role {
                        super::LoupeResultRole::HighResUpgrade => {
                            // high-res decode failed; keep showing bounded fallback
                            loupe.high_res_handle = None;
                            loupe.high_res_dimensions = None;
                        }
                        super::LoupeResultRole::DemosaicUpgrade => {}
                        super::LoupeResultRole::Base => {
                            loupe.handle = None;
                            loupe.histogram = None;
                            loupe.dimensions = None;
                            loupe.error = error;
                        }
                    },
                }
            } else if super::loupe_decode_recovery_needed(
                &loupe.path,
                loupe.decode_mode,
                loupe.handle.is_some(),
                self.loupe_rt
                    .decode_pending
                    .as_ref()
                    .map(|(p, m)| (p.as_path(), *m)),
                &path,
                mode,
            ) {
                recovery_decode = Some((loupe.path.clone(), loupe.decode_mode));
            }
        }
        if let Some((path, mode)) = recovery_decode {
            if let Some(cached) = self
                .preview_cache
                .get(&path, mode, self.config.develop_look)
            {
                self.loupe_rt.decode_pending = None;
                if let Some(loupe) = &mut self.loupe {
                    if loupe.path == path && loupe.decode_mode == mode {
                        let handle = cached.handle.clone();
                        loupe.histogram = super::histogram_from_handle(&handle);
                        loupe.handle = Some(handle);
                        loupe.dimensions = Some(cached.dimensions);
                        loupe.error = None;
                    }
                }
                return Task::none();
            }
            let bound = crate::view::loupe::loupe_decode_bound(self.viewport_w, self.viewport_h);
            self.loupe_rt.decode_pending = Some((path.clone(), mode));
            return crate::tasks::decode_loupe_task(path, bound, mode, self.config.develop_look);
        }
        Task::none()
    }

    pub(super) fn handle_toggle_slideshow(&mut self) -> Task<cosmic::Action<Message>> {
        if self.loupe.is_some() {
            self.loupe_rt.slideshow_playing = !self.loupe_rt.slideshow_playing;
        } else {
            self.loupe_rt.slideshow_playing = false;
        }
        Task::none()
    }

    pub(super) fn handle_slideshow_tick(&mut self) -> Task<cosmic::Action<Message>> {
        if self.loupe.is_some() && self.loupe_rt.slideshow_playing {
            // Advance over LOUPE-ELIGIBLE siblings only (Image/Raw under filter), NOT
            // displayed_indices() — the latter includes Dirs/text files, and OpenLoupe
            // no-ops on those, which would freeze the slideshow on the first non-image.
            let eligible = crate::view::loupe::loupe_eligible_indices(
                &self.browser.entries,
                &self.browser.filter_query,
            );
            let Some(current) = self.loupe.as_ref().map(|loupe| loupe.index) else {
                return Task::none();
            };
            if let Some(next_index) = super::slideshow_next_index(&eligible, current) {
                // Advance by reusing OpenLoupe + loupe_step (wrap=true) over loupe-eligible.
                // This ensures decode/cache/zoom=Fit/filmstrip all happen on the normal path.
                // No second advance/decode implementation.
                return self.update(Message::OpenLoupe(next_index));
            }
        }
        // fall to trailing Task::none() after match for type inference from fn sig
        Task::none()
    }

    pub(super) fn handle_loupe_pan_press(&mut self) -> Task<cosmic::Action<Message>> {
        if self
            .loupe
            .as_ref()
            .is_some_and(|l| l.zoom == crate::inspection::LoupeZoom::Actual)
        {
            let start_c = self.loupe_rt.pan_last_cursor.unwrap_or_default();
            let start_o = self.loupe_rt.scroll;
            self.loupe_rt.drag = Some((start_c, start_o));
        }
        Task::none()
    }

    pub(super) fn handle_loupe_pan_release(&mut self) -> Task<cosmic::Action<Message>> {
        self.loupe_rt.drag = None;
        Task::none()
    }

    pub(super) fn handle_loupe_pan_move(
        &mut self,
        p: cosmic::iced::Point,
    ) -> Task<cosmic::Action<Message>> {
        self.loupe_rt.pan_last_cursor = Some(p);
        if let Some((start_c, start_o)) = self.loupe_rt.drag {
            if self
                .loupe
                .as_ref()
                .is_some_and(|l| l.zoom == crate::inspection::LoupeZoom::Actual)
            {
                let target = cosmic::iced::widget::scrollable::AbsoluteOffset {
                    x: start_o.x + (start_c.x - p.x),
                    y: start_o.y + (start_c.y - p.y),
                };
                self.loupe_rt.scroll = target;
                let id = crate::view::loupe::LOUPE_SCROLL_ID.clone();
                let t: cosmic::iced::Task<Message> =
                    cosmic::iced::widget::scrollable::scroll_to(id, target.into());
                return t.map(cosmic::Action::App);
            }
        }
        Task::none()
    }

    pub(super) fn handle_loupe_scrolled(
        &mut self,
        off: cosmic::iced::widget::scrollable::AbsoluteOffset,
    ) -> Task<cosmic::Action<Message>> {
        self.loupe_rt.scroll = off;
        Task::none()
    }

    pub(super) fn handle_set_loupe_rating(&mut self, n: u8) -> Task<cosmic::Action<Message>> {
        if let Some(loupe) = &self.loupe {
            let path = loupe.path.clone();
            let n = crate::cull::next_rating_on_click(loupe.rating, n); // toggle-to-clear (pure helper below)
            if crate::xmp::write_sidecar_rating(&path, n).is_ok() {
                let (r, x) = crate::xmp::read_loupe_sidecar(&path);
                if let Some(loupe) = &mut self.loupe {
                    loupe.rating = r;
                    loupe.xmp = x;
                }
                // Refresh cull cache from sidecar (rating may have changed reject too).
                self.filter
                    .cull_cache
                    .insert(path.clone(), crate::xmp::read_sidecar_cull(&path));
            }
        }
        Task::none()
    }

    pub(super) fn handle_set_loupe_label(
        &mut self,
        lab: Option<crate::xmp::ColorLabel>,
    ) -> Task<cosmic::Action<Message>> {
        if let Some(loupe) = &self.loupe {
            let path = loupe.path.clone();
            if crate::xmp::write_sidecar_label(&path, lab).is_ok() {
                let (r, x) = crate::xmp::read_loupe_sidecar(&path);
                if let Some(loupe) = &mut self.loupe {
                    loupe.rating = r;
                    loupe.xmp = x;
                }
                self.filter
                    .cull_cache
                    .insert(path.clone(), crate::xmp::read_sidecar_cull(&path));
            }
        }
        Task::none()
    }

    pub(super) fn handle_toggle_loupe_reject(&mut self) -> Task<cosmic::Action<Message>> {
        if let Some(loupe) = &self.loupe {
            let path = loupe.path.clone();
            let current = loupe.xmp.as_ref().map(|d| d.rejected).unwrap_or(false);
            let new_rej = !current;
            if crate::xmp::write_sidecar_reject(&path, new_rej).is_ok() {
                let (r, x) = crate::xmp::read_loupe_sidecar(&path);
                if let Some(loupe) = &mut self.loupe {
                    loupe.rating = r;
                    loupe.xmp = x;
                }
                self.filter
                    .cull_cache
                    .insert(path.clone(), crate::xmp::read_sidecar_cull(&path));
            }
        }
        Task::none()
    }

    pub(super) fn handle_close_loupe(&mut self) -> Task<cosmic::Action<Message>> {
        // Dropping the LoupeState drops its decoded handle → frees pixels.
        // Also purge any high-res (full-res loupe) entry so at most one is ever resident.
        self.preview_cache.purge_high_res();
        self.loupe = None;
        self.loupe_rt.decode_pending = None;
        self.loupe_rt.slideshow_playing = false;
        self.loupe_rt.scroll = cosmic::iced::widget::scrollable::AbsoluteOffset::default();
        self.loupe_rt.drag = None;
        self.loupe_rt.pan_last_cursor = None;
        self.loupe_rt.zoom_factor = 1.0;
        Task::none()
    }

    pub(super) fn handle_enter_compare(&mut self) -> Task<cosmic::Action<Message>> {
        if self.selection.multi_selected.len() < 2 {
            return Task::none();
        }
        let displayed = self.grid_indices();
        let sel_idxs = Self::first_two_selected(&displayed, &self.selection.multi_selected);
        if sel_idxs.len() < 2 {
            return Task::none();
        }
        let panes: Vec<_> = sel_idxs
            .into_iter()
            .filter_map(|idx| {
                self.browser
                    .entries
                    .get(idx)
                    .map(|e| crate::inspection::ComparePane {
                        index: idx,
                        path: e.path.clone(),
                        handle: None,
                        dimensions: None,
                        error: None,
                    })
            })
            .collect();
        if panes.len() != 2 {
            self.compare = None;
            return Task::none();
        }
        self.compare = Some(crate::inspection::CompareState { panes });
        // Launch bounded Fit decodes for both (mirror loupe; use full vp bound for simplicity of Fit).
        let bound = crate::view::loupe::loupe_decode_bound(self.viewport_w, self.viewport_h);
        let Some(compare) = self.compare.as_ref() else {
            return Task::none();
        };
        let tasks: Vec<_> = compare
            .panes
            .iter()
            .map(|p| crate::tasks::decode_compare_task(p.path.clone(), bound))
            .collect();
        Task::batch(tasks)
    }

    pub(super) fn handle_close_compare(&mut self) -> Task<cosmic::Action<Message>> {
        self.compare = None; // drop handles
        Task::none()
    }

    pub(super) fn handle_compare_decoded(
        &mut self,
        path: std::path::PathBuf,
        decoded: Option<(cosmic::widget::image::Handle, u32, u32)>,
        error: Option<String>,
    ) -> Task<cosmic::Action<Message>> {
        if let Some(cstate) = &mut self.compare {
            if let Some(pane) = cstate.panes.iter_mut().find(|p| p.path == path) {
                match decoded {
                    Some((handle, w, h)) => {
                        pane.handle = Some(handle);
                        pane.dimensions = Some((w, h));
                        pane.error = None;
                    }
                    None => {
                        pane.handle = None;
                        pane.dimensions = None;
                        pane.error = error;
                    }
                }
            }
        }
        Task::none()
    }

    pub(super) fn handle_toggle_loupe_full_demosaic(&mut self) -> Task<cosmic::Action<Message>> {
        self.config.loupe_full_demosaic = !self.config.loupe_full_demosaic;
        self.config.save();
        if let Some(loupe) = &mut self.loupe {
            if self
                .browser
                .entries
                .get(loupe.index)
                .is_some_and(|entry| entry.kind == crate::scan::EntryKind::Raw)
            {
                let path = loupe.path.clone();
                let bound =
                    crate::view::loupe::loupe_decode_bound(self.viewport_w, self.viewport_h);
                let mode = super::loupe_base_decode_mode(&crate::scan::EntryKind::Raw);
                let want_full_demosaic = super::loupe_decode_mode(
                    self.config.loupe_full_demosaic,
                    &crate::scan::EntryKind::Raw,
                ) == crate::inspection::LoupeDecodeMode::FullRaw;
                loupe.decode_mode = mode;
                loupe.want_full_demosaic = want_full_demosaic;
                if !want_full_demosaic {
                    loupe.demosaic_handle = None;
                    loupe.demosaic_dimensions = None;
                }

                let mut decode_tasks = Vec::new();
                if let Some(cached) = self
                    .preview_cache
                    .get(&path, mode, self.config.develop_look)
                {
                    self.loupe_rt.decode_pending = None;
                    let handle = cached.handle.clone();
                    loupe.histogram = super::histogram_from_handle(&handle);
                    loupe.handle = Some(handle);
                    loupe.dimensions = Some(cached.dimensions);
                    loupe.error = None;
                } else {
                    loupe.handle = None;
                    loupe.histogram = None;
                    loupe.dimensions = None;
                    loupe.error = None;
                    self.loupe_rt.decode_pending = Some((path.clone(), mode));
                    decode_tasks.push(crate::tasks::decode_loupe_task(
                        path.clone(),
                        bound,
                        mode,
                        self.config.develop_look,
                    ));
                }

                if want_full_demosaic {
                    if let Some(cached) = self.preview_cache.get(
                        &path,
                        crate::inspection::LoupeDecodeMode::FullRaw,
                        self.config.develop_look,
                    ) {
                        loupe.demosaic_handle = Some(cached.handle.clone());
                        loupe.demosaic_dimensions = Some(cached.dimensions);
                    } else {
                        loupe.demosaic_handle = None;
                        loupe.demosaic_dimensions = None;
                        decode_tasks.push(crate::tasks::decode_loupe_task(
                            path.clone(),
                            bound,
                            crate::inspection::LoupeDecodeMode::FullRaw,
                            self.config.develop_look,
                        ));
                    }
                }

                return super::chain_decode_tasks(decode_tasks);
            }
        }
        Task::none()
    }

    pub(super) fn handle_toggle_full_res_loupe(&mut self) -> Task<cosmic::Action<Message>> {
        self.config.full_res_loupe = !self.config.full_res_loupe;
        self.config.save();
        if let Some(loupe) = &mut self.loupe {
            if !self.config.full_res_loupe {
                loupe.high_res_handle = None;
                loupe.high_res_dimensions = None;
            } else if loupe.zoom == crate::inspection::LoupeZoom::Actual {
                return self.ensure_high_res_for_current_loupe();
            }
        }
        Task::none()
    }

    pub(super) fn handle_toggle_develop_look(&mut self) -> Task<cosmic::Action<Message>> {
        self.config.develop_look = !self.config.develop_look;
        self.config.save();
        if let Some(loupe) = &mut self.loupe {
            let path = loupe.path.clone();
            let bound = crate::view::loupe::loupe_decode_bound(self.viewport_w, self.viewport_h);
            // Use the same base mode selection as OpenLoupe / ToggleLoupeFullDemosaic.
            let mode = loupe.decode_mode;
            let mut decode_tasks = Vec::new();
            self.loupe_rt.decode_pending = Some((path.clone(), mode));
            decode_tasks.push(crate::tasks::decode_loupe_task(
                path.clone(),
                bound,
                mode,
                self.config.develop_look,
            ));
            // If a demosaic upgrade is active for this loupe, also re-decode it.
            if loupe.want_full_demosaic {
                loupe.demosaic_handle = None;
                loupe.demosaic_dimensions = None;
                decode_tasks.push(crate::tasks::decode_loupe_task(
                    path,
                    bound,
                    crate::inspection::LoupeDecodeMode::FullRaw,
                    self.config.develop_look,
                ));
            }
            return super::chain_decode_tasks(decode_tasks);
        }
        Task::none()
    }

    pub(super) fn handle_toggle_show_filmstrip(&mut self) -> Task<cosmic::Action<Message>> {
        self.config.show_filmstrip = !self.config.show_filmstrip;
        self.config.save();
        Task::none()
    }
}
