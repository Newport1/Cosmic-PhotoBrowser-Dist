use std::path::PathBuf;

use crate::histogram::HISTOGRAM_BINS;
use crate::metadata::ExifSummary;

pub type DecodedImage = Option<(cosmic::widget::image::Handle, u32, u32)>;

/// Decoded preview image plus metadata for the selected image.
#[derive(Debug, Clone)]
pub struct PreviewState {
    pub path: PathBuf,
    pub handle: Option<cosmic::widget::image::Handle>,
    pub dimensions: Option<(u32, u32)>,
    pub error: Option<String>,
    pub metadata: Option<ExifSummary>,
    pub histogram: Option<[u32; HISTOGRAM_BINS]>,
}

/// Render source used for the current loupe image.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LoupeDecodeMode {
    EmbeddedPreview,
    FullRaw,
    /// High-resolution (capped at 8192 long edge) decode, used exclusively for the
    /// Actual/1:1 zoom when `Config::full_res_loupe` is true. Separate from the
    /// ≤2560 Fit decode path. EXIF orientation is still applied. At most one
    /// such decode is kept resident and is purged on loupe close or navigation.
    HighRes,
    /// Same as HighRes but using full demosaic load for RAWs (when loupe_full_demosaic also on).
    HighResFullRaw,
}

/// Loupe display scale. `Actual` means 1:1 for the existing display-bounded
/// decoded handle, not a native-resolution re-decode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoupeZoom {
    Fit,
    Actual,
}

/// Read-only right-panel metadata sections. Runtime-only UI state; not persisted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MetadataSection {
    FileProperties,
    Dimensions,
    CameraExif,
    CaptureDate,
}

/// Expanded/collapsed state for the inspection column.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MetadataSectionState {
    pub file_properties: bool,
    pub dimensions: bool,
    pub camera_exif: bool,
    pub capture_date: bool,
}

impl Default for MetadataSectionState {
    fn default() -> Self {
        Self {
            file_properties: true,
            dimensions: true,
            camera_exif: true,
            capture_date: true,
        }
    }
}

impl MetadataSectionState {
    pub fn is_expanded(&self, section: MetadataSection) -> bool {
        match section {
            MetadataSection::FileProperties => self.file_properties,
            MetadataSection::Dimensions => self.dimensions,
            MetadataSection::CameraExif => self.camera_exif,
            MetadataSection::CaptureDate => self.capture_date,
        }
    }
}

pub(crate) fn toggle_metadata_section(state: &mut MetadataSectionState, section: MetadataSection) {
    match section {
        MetadataSection::FileProperties => state.file_properties = !state.file_properties,
        MetadataSection::Dimensions => state.dimensions = !state.dimensions,
        MetadataSection::CameraExif => state.camera_exif = !state.camera_exif,
        MetadataSection::CaptureDate => state.capture_date = !state.capture_date,
    }
}

pub(crate) fn toggle_loupe_zoom(zoom: LoupeZoom) -> LoupeZoom {
    match zoom {
        LoupeZoom::Fit => LoupeZoom::Actual,
        LoupeZoom::Actual => LoupeZoom::Fit,
    }
}

/// Selects the distinct HighRes* cache variant for the native-resolution Actual decode path.
/// This keeps the ≤8192 decode separate from the ≤2560 bounded decode for the same path.
pub(crate) fn high_res_loupe_mode(
    full_demosaic_setting: bool,
    kind: &crate::scan::EntryKind,
) -> LoupeDecodeMode {
    if full_demosaic_setting && matches!(kind, crate::scan::EntryKind::Raw) {
        LoupeDecodeMode::HighResFullRaw
    } else {
        LoupeDecodeMode::HighRes
    }
}

impl LoupeDecodeMode {
    pub fn label(self) -> &'static str {
        match self {
            Self::EmbeddedPreview => "Embedded preview",
            Self::FullRaw => "Full RAW",
            Self::HighRes => "High-res",
            Self::HighResFullRaw => "High-res (full)",
        }
    }
}

/// Full-window single-image view state. Holds exactly one decoded handle.
/// When full_res_loupe is enabled and zoom==Actual, a second high-res (≤8192) handle
/// may be resident here (populated asynchronously); the main handle remains the
/// Fit-bounded (≤2560) decode so Fit view and "until ready" fallback keep working.
/// High-res handle is dropped on CloseLoupe or loupe navigation (at most one full-res
/// decode resident at any time, also enforced via preview_cache purge for HighRes* keys).
#[derive(Debug, Clone)]
pub struct LoupeState {
    /// Index into `self.browser.entries` for previous/next navigation.
    #[allow(dead_code)]
    pub index: usize, // index into self.browser.entries
    pub path: PathBuf,
    pub decode_mode: LoupeDecodeMode,
    pub zoom: LoupeZoom,
    pub handle: Option<cosmic::widget::image::Handle>,
    pub dimensions: Option<(u32, u32)>, // original (w,h)
    pub error: Option<String>,
    /// High-res (≤8192 long-edge) decoded pixels for 1:1 Actual when full_res_loupe on.
    /// Falls back to `handle` (the bounded decode) until this is populated or when flag off.
    pub high_res_handle: Option<cosmic::widget::image::Handle>,
    /// Decoded (not original) size of the high_res_handle for Fixed layout in Actual.
    pub high_res_dimensions: Option<(u32, u32)>,
    /// Full-demosaic Fit upgrade (≤bound). Layered over `handle` for RAW when loupe_full_demosaic on;
    /// shown once it lands so the Fit view is never blank during the slow demosaic.
    pub demosaic_handle: Option<cosmic::widget::image::Handle>,
    pub demosaic_dimensions: Option<(u32, u32)>,
    /// Whether this loupe wants the full-demosaic upgrade (config.loupe_full_demosaic && RAW at open time).
    pub want_full_demosaic: bool,
    /// Previous image's displayed handle, kept on-screen during nav while the next image decodes,
    /// so the loupe shows the prior frame instead of a blank gray flash. Fit fallback only; lowest
    /// priority (handle/demosaic_handle take over once the new image lands).
    pub placeholder_handle: Option<cosmic::widget::image::Handle>,
    /// Star rating (0..=5; None = unknown/unrated) read from the XMP sidecar. Set via clicking stars.
    pub rating: Option<u8>,
    /// Parsed XMP sidecar data for the info display (None if no sidecar / unparseable).
    pub xmp: Option<crate::xmp::XmpData>,
    pub histogram: Option<[u32; HISTOGRAM_BINS]>,
}

/// View-only 2-up compare pane for a single image.
/// It does not write files or support zoom and pan.
#[derive(Debug, Clone)]
pub struct ComparePane {
    #[allow(dead_code)]
    pub index: usize,
    pub path: PathBuf,
    pub handle: Option<cosmic::widget::image::Handle>,
    pub dimensions: Option<(u32, u32)>,
    pub error: Option<String>,
}

/// 2-up compare state. `Some` means the compare takeover view is active.
#[derive(Debug, Clone)]
pub struct CompareState {
    pub panes: Vec<ComparePane>,
}

#[cfg(test)]
mod tests {
    use super::{
        high_res_loupe_mode, toggle_loupe_zoom, toggle_metadata_section, LoupeDecodeMode,
        LoupeZoom, MetadataSection, MetadataSectionState,
    };

    #[test]
    fn metadata_sections_default_expanded() {
        let sections = MetadataSectionState::default();

        for section in [
            MetadataSection::FileProperties,
            MetadataSection::Dimensions,
            MetadataSection::CameraExif,
            MetadataSection::CaptureDate,
        ] {
            assert!(sections.is_expanded(section));
        }
    }

    #[test]
    fn toggle_metadata_section_flips_only_requested_section() {
        let mut sections = MetadataSectionState::default();

        toggle_metadata_section(&mut sections, MetadataSection::CameraExif);

        assert!(sections.file_properties);
        assert!(sections.dimensions);
        assert!(!sections.camera_exif);
        assert!(sections.capture_date);

        toggle_metadata_section(&mut sections, MetadataSection::CameraExif);

        assert!(sections.camera_exif);
    }

    #[test]
    fn loupe_decode_mode_labels_are_honest() {
        assert_eq!(LoupeDecodeMode::FullRaw.label(), "Full RAW");
        assert_eq!(LoupeDecodeMode::EmbeddedPreview.label(), "Embedded preview");
        assert_eq!(LoupeDecodeMode::HighRes.label(), "High-res");
        assert_eq!(LoupeDecodeMode::HighResFullRaw.label(), "High-res (full)");
    }

    #[test]
    fn loupe_zoom_toggle_flips_fit_and_actual() {
        assert_eq!(toggle_loupe_zoom(LoupeZoom::Fit), LoupeZoom::Actual);
        assert_eq!(toggle_loupe_zoom(LoupeZoom::Actual), LoupeZoom::Fit);
    }

    #[test]
    fn high_res_loupe_mode_selects_distinct_variants_for_cache_key() {
        use crate::scan::EntryKind;
        assert_eq!(
            high_res_loupe_mode(false, &EntryKind::Image),
            LoupeDecodeMode::HighRes
        );
        assert_eq!(
            high_res_loupe_mode(true, &EntryKind::Image),
            LoupeDecodeMode::HighRes
        );
        assert_eq!(
            high_res_loupe_mode(false, &EntryKind::Raw),
            LoupeDecodeMode::HighRes
        );
        assert_eq!(
            high_res_loupe_mode(true, &EntryKind::Raw),
            LoupeDecodeMode::HighResFullRaw
        );
    }
}
