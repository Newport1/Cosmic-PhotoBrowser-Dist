//! update() handlers for grid culling — batch rating/label/reject and the digit/reject hotkeys.

use cosmic::Application;
use cosmic::Task;

use crate::app::{AppModel, Message};

impl AppModel {
    pub(super) fn handle_digit_key(&mut self, d: u8) -> Task<cosmic::Action<Message>> {
        if self.loupe.is_some() {
            match d {
                1 => {
                    return self.update(Message::SetLoupeZoom(crate::inspection::LoupeZoom::Actual))
                }
                0 => return self.update(Message::SetLoupeZoom(crate::inspection::LoupeZoom::Fit)),
                _ => {}
            }
        } else if self.compare.is_none() {
            let idx = self.cull_target_indices();
            if !idx.is_empty() {
                match d {
                    0..=5 => {
                        self.apply_cull(&idx, |p| crate::xmp::write_sidecar_rating(p, d), "Rated")
                    }
                    6 => self.apply_cull(
                        &idx,
                        |p| crate::xmp::write_sidecar_label(p, Some(crate::xmp::ColorLabel::Red)),
                        "Labeled",
                    ),
                    7 => self.apply_cull(
                        &idx,
                        |p| {
                            crate::xmp::write_sidecar_label(p, Some(crate::xmp::ColorLabel::Yellow))
                        },
                        "Labeled",
                    ),
                    8 => self.apply_cull(
                        &idx,
                        |p| crate::xmp::write_sidecar_label(p, Some(crate::xmp::ColorLabel::Green)),
                        "Labeled",
                    ),
                    9 => self.apply_cull(
                        &idx,
                        |p| crate::xmp::write_sidecar_label(p, Some(crate::xmp::ColorLabel::Blue)),
                        "Labeled",
                    ),
                    _ => {}
                }
            }
        }
        Task::none()
    }

    pub(super) fn handle_reject_key(&mut self) -> Task<cosmic::Action<Message>> {
        if self.loupe.is_none() && self.compare.is_none() {
            let idx = self.cull_target_indices();
            if !idx.is_empty() {
                self.apply_cull(
                    &idx,
                    |p| crate::xmp::write_sidecar_reject(p, true),
                    "Rejected",
                );
            }
        }
        Task::none()
    }

    pub(super) fn handle_batch_set_rating(&mut self, n: u8) -> Task<cosmic::Action<Message>> {
        let idx = self.cull_target_indices();
        self.apply_cull(&idx, |p| crate::xmp::write_sidecar_rating(p, n), "Rated");
        Task::none()
    }

    pub(super) fn handle_batch_set_label(
        &mut self,
        lab: Option<crate::xmp::ColorLabel>,
    ) -> Task<cosmic::Action<Message>> {
        let idx = self.cull_target_indices();
        self.apply_cull(&idx, |p| crate::xmp::write_sidecar_label(p, lab), "Labeled");
        Task::none()
    }

    pub(super) fn handle_batch_set_reject(&mut self, r: bool) -> Task<cosmic::Action<Message>> {
        let verb = if r { "Rejected" } else { "Cleared reject on" };
        let idx = self.cull_target_indices();
        self.apply_cull(&idx, |p| crate::xmp::write_sidecar_reject(p, r), verb);
        Task::none()
    }
}
