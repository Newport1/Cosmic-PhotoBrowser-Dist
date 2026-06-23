//! Decoded image data passed from core decode logic to UI rendering.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DecodedRgbaImage {
    pub width: u32,
    pub height: u32,
    pub original_width: u32,
    pub original_height: u32,
    pub rgba: Vec<u8>,
}

impl DecodedRgbaImage {
    /// Build from a resized RGBA image plus the pre-resize (original) dimensions.
    pub fn from_rgba_image(
        img: image::RgbaImage,
        original_width: u32,
        original_height: u32,
    ) -> Self {
        let (width, height) = img.dimensions();
        Self {
            width,
            height,
            original_width,
            original_height,
            rgba: img.into_raw(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_rgba_image_preserves_dims_and_buffer_len() {
        let img = image::RgbaImage::from_pixel(3, 2, image::Rgba([10u8, 20, 30, 255]));
        let dto = DecodedRgbaImage::from_rgba_image(img, 99, 88);
        assert_eq!(dto.width, 3);
        assert_eq!(dto.height, 2);
        assert_eq!(dto.original_width, 99);
        assert_eq!(dto.original_height, 88);
        assert_eq!(dto.rgba.len(), 3 * 2 * 4);
    }
}
