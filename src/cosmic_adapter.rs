//! Bridges the pure `DecodedRgbaImage` DTO to COSMIC image handles.
//!
//! This module centralizes conversion from decoded RGBA pixels into
//! `cosmic::widget::image::Handle`.
use crate::decoded_image::DecodedRgbaImage;

/// Convert a decoded DTO into the (handle, original_width, original_height) triple the
/// loupe/preview/compare state stores.
pub(crate) fn to_handle_triple(d: DecodedRgbaImage) -> (cosmic::widget::image::Handle, u32, u32) {
    let handle = cosmic::widget::image::Handle::from_rgba(d.width, d.height, d.rgba);
    (handle, d.original_width, d.original_height)
}
