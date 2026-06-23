//! update() handlers for async decode/metadata callbacks (preview decode, EXIF load, capture-epoch, metadata sections).

use cosmic::Task;

use crate::app::{AppModel, Message};

impl AppModel {
    pub(super) fn handle_preview_decoded(
        &mut self,
        path: std::path::PathBuf,
        decoded: crate::inspection::DecodedImage,
        histogram: Option<[u32; crate::histogram::HISTOGRAM_BINS]>,
        error: Option<String>,
    ) -> Task<cosmic::Action<Message>> {
        if self.preview.as_ref().map(|p| p.path.as_path()) == Some(path.as_path()) {
            let metadata = self
                .preview
                .as_ref()
                .and_then(|preview| preview.metadata.clone());
            self.preview = Some(match decoded {
                Some((handle, w, h)) => crate::inspection::PreviewState {
                    path,
                    handle: Some(handle),
                    dimensions: Some((w, h)),
                    error: None,
                    metadata,
                    histogram,
                },
                None => crate::inspection::PreviewState {
                    path,
                    handle: None,
                    dimensions: None,
                    error,
                    metadata,
                    histogram: None,
                },
            });
        }
        Task::none()
    }

    pub(super) fn handle_metadata_loaded(
        &mut self,
        path: std::path::PathBuf,
        metadata: crate::metadata::ExifSummary,
    ) -> Task<cosmic::Action<Message>> {
        if let Some(preview) = &mut self.preview {
            if preview.path == path {
                preview.metadata = Some(metadata);
            }
        }
        Task::none()
    }

    pub(super) fn handle_toggle_metadata_section(
        &mut self,
        section: crate::inspection::MetadataSection,
    ) -> Task<cosmic::Action<Message>> {
        crate::inspection::toggle_metadata_section(&mut self.metadata_sections, section);
        Task::none()
    }

    pub(super) fn handle_capture_epoch_loaded(
        &mut self,
        path: std::path::PathBuf,
        epoch: Option<i64>,
    ) -> Task<cosmic::Action<Message>> {
        // Coalesce: cache every result, but only re-sort the whole list
        // ONCE the in-flight batch has fully drained (was re-sorting on
        // every single result, which is unnecessarily expensive for large folders.
        if self
            .browser
            .record_capture_epoch(self.config.sort_mode, path, epoch)
        {
            self.browser.sort_for_mode(self.config.sort_mode);
            self.rebuild_snapshot();
            return self
                .browser
                .request_capture_epoch_batch(self.config.sort_mode, self.config.images_only);
        }
        Task::none()
    }
}
