use std::io::BufReader;
use std::path::Path;

use rawler::decoders::RawDecodeParams;
use rawler::rawsource::RawSource;

/// Lowercase RAW extensions PhotoBrowser recognizes (keep in sync with scan.rs).
pub fn is_raw_extension(name: &str) -> bool {
    let ext = Path::new(name)
        .extension()
        .map(|ext| ext.to_string_lossy().to_lowercase())
        .unwrap_or_default();

    matches!(
        ext.as_str(),
        "nef" | "cr2" | "cr3" | "arw" | "dng" | "raf" | "orf" | "rw2" | "pef" | "srw"
    )
}

fn is_jpeg_extension(name: &str) -> bool {
    let ext = Path::new(name)
        .extension()
        .map(|ext| ext.to_string_lossy().to_lowercase())
        .unwrap_or_default();

    matches!(ext.as_str(), "jpg" | "jpeg")
}

/// Load a decodable image for a path.
pub fn load_image(path: &Path) -> image::ImageResult<image::DynamicImage> {
    let img = if !is_raw_extension(
        path.file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_default(),
    ) {
        image::open(path)?
    } else {
        load_raw_preview(path)?
    };

    Ok(apply_orientation(img, read_orientation(path)))
}

/// Load a full-demosaic image for RAW paths; non-RAW paths use the normal path.
///
/// This is intentionally separate from [`load_thumbnail_image`]: thumbnails stay
/// embedded-preview-only/fast, while the loupe may opt into full RAW development
/// for a single display-bounded image.
pub fn load_image_full_demosaic(path: &Path) -> image::ImageResult<image::DynamicImage> {
    let filename = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default();
    if !is_raw_extension(filename) {
        return load_image(path);
    }

    Ok(apply_orientation(
        load_raw_full_demosaic(path)?,
        read_orientation(path),
    ))
}

/// Load a thumbnail-oriented image for a path.
///
/// RAW thumbnails use embedded thumbnail/preview images only and never request
/// rawler full RAW development.
#[allow(dead_code)]
pub fn load_thumbnail_image(path: &Path) -> image::ImageResult<image::DynamicImage> {
    let filename = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default();
    let img = if is_raw_extension(filename) {
        load_raw_thumbnail_preview(path)?
    } else if is_jpeg_extension(filename) {
        match load_exif_embedded_thumbnail(path) {
            Some(img) => img,
            None => image::open(path)?,
        }
    } else {
        image::open(path)?
    };

    Ok(apply_orientation(img, read_orientation(path)))
}

/// Load a thumbnail-oriented image for a path, with JPEG full-image fallbacks
/// DCT-scaled toward `target_px` before later cache/render resize steps.
///
/// Embedded JPEG thumbnails and RAW embedded previews are still returned as-is.
pub fn load_thumbnail_image_reduced(
    path: &Path,
    target_px: u16,
) -> image::ImageResult<image::DynamicImage> {
    let filename = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default();
    let img = if is_raw_extension(filename) {
        load_raw_thumbnail_preview(path)?
    } else if is_jpeg_extension(filename) {
        match load_exif_embedded_thumbnail(path) {
            Some(img) => img,
            None => load_jpeg_reduced(path, target_px)?,
        }
    } else {
        image::open(path)?
    };

    Ok(apply_orientation(img, read_orientation(path)))
}

fn load_jpeg_reduced(path: &Path, target_px: u16) -> image::ImageResult<image::DynamicImage> {
    let file = std::fs::File::open(path)?;
    let mut decoder = jpeg_decoder::Decoder::new(BufReader::new(file));
    let target_px = target_px.max(1);
    decoder
        .scale(target_px, target_px)
        .map_err(jpeg_decode_error)?;
    let pixels = decoder.decode().map_err(jpeg_decode_error)?;
    let info = decoder
        .info()
        .ok_or_else(|| jpeg_decode_error("JPEG decoder did not return image info"))?;
    dynamic_image_from_jpeg_pixels(pixels, info).ok_or_else(|| {
        jpeg_decode_error(format!(
            "JPEG decoder returned invalid {}x{} {:?} buffer",
            info.width, info.height, info.pixel_format
        ))
    })
}

fn dynamic_image_from_jpeg_pixels(
    pixels: Vec<u8>,
    info: jpeg_decoder::ImageInfo,
) -> Option<image::DynamicImage> {
    let width = u32::from(info.width);
    let height = u32::from(info.height);
    match info.pixel_format {
        jpeg_decoder::PixelFormat::L8 => {
            image::GrayImage::from_raw(width, height, pixels).map(image::DynamicImage::ImageLuma8)
        }
        jpeg_decoder::PixelFormat::L16 => {
            let luma8: Vec<u8> = pixels.chunks_exact(2).map(|chunk| chunk[0]).collect();
            image::GrayImage::from_raw(width, height, luma8).map(image::DynamicImage::ImageLuma8)
        }
        jpeg_decoder::PixelFormat::RGB24 => {
            image::RgbImage::from_raw(width, height, pixels).map(image::DynamicImage::ImageRgb8)
        }
        jpeg_decoder::PixelFormat::CMYK32 => {
            let rgb: Vec<u8> = pixels
                .chunks_exact(4)
                .flat_map(|chunk| {
                    let c = u16::from(chunk[0]);
                    let m = u16::from(chunk[1]);
                    let y = u16::from(chunk[2]);
                    let k = u16::from(chunk[3]);
                    [
                        255u8.saturating_sub((c + k).min(255) as u8),
                        255u8.saturating_sub((m + k).min(255) as u8),
                        255u8.saturating_sub((y + k).min(255) as u8),
                    ]
                })
                .collect();
            image::RgbImage::from_raw(width, height, rgb).map(image::DynamicImage::ImageRgb8)
        }
    }
}

fn jpeg_decode_error(err: impl std::fmt::Display) -> image::ImageError {
    image::ImageError::Decoding(image::error::DecodingError::new(
        image::ImageFormat::Jpeg.into(),
        err.to_string(),
    ))
}

/// Apply an EXIF orientation transform to decoded pixels.
///
/// Values outside the EXIF 1..=8 range are treated as orientation 1 (no-op).
pub fn apply_orientation(img: image::DynamicImage, orientation: u8) -> image::DynamicImage {
    match orientation {
        2 => img.fliph(),
        3 => img.rotate180(),
        4 => img.flipv(),
        5 => img.rotate90().flipv(),
        6 => img.rotate90(),
        7 => img.rotate90().fliph(),
        8 => img.rotate270(),
        _ => img,
    }
}

/// Read numeric EXIF orientation, defaulting to 1 on missing tags or read errors.
#[cfg(feature = "exif")]
pub fn read_orientation(path: &Path) -> u8 {
    use std::fs::File;
    use std::io::BufReader;

    use exif::{In, Reader, Tag};

    let Ok(file) = File::open(path) else {
        return 1;
    };
    let Ok(exif) = Reader::new().read_from_container(&mut BufReader::new(file)) else {
        return 1;
    };
    exif.get_field(Tag::Orientation, In::PRIMARY)
        .and_then(|field| field.value.get_uint(0))
        .and_then(|value| u8::try_from(value).ok())
        .filter(|value| (1..=8).contains(value))
        .unwrap_or(1)
}

#[cfg(not(feature = "exif"))]
pub fn read_orientation(_path: &Path) -> u8 {
    1
}

fn load_raw_preview(path: &Path) -> image::ImageResult<image::DynamicImage> {
    let raw = RawSource::new(path).map_err(raw_decode_error)?;
    let decoder = rawler::get_decoder(&raw).map_err(raw_decode_error)?;
    let params = RawDecodeParams::default();

    let candidates = [
        decoder
            .preview_image(&raw, &params)
            .map_err(raw_decode_error)?,
        decoder
            .thumbnail_image(&raw, &params)
            .map_err(raw_decode_error)?,
    ];

    let largest = candidates
        .into_iter()
        .flatten()
        .max_by_key(|img| u64::from(img.width()) * u64::from(img.height()));
    let direct = load_direct_raw_preview(path, DirectRawPreviewKind::Loupe);
    if let Some(img) = pick_larger_image(largest, direct) {
        return Ok(img);
    }

    // rawler exposed no embedded preview (e.g. the Nikon Z preview lives in a
    // proprietary PreviewIFD rawler does not surface) — fall back to a full
    // demosaic so the image still renders. Orientation is applied by the caller.
    decoder
        .full_image(&raw, &params)
        .map_err(raw_decode_error)?
        .ok_or_else(|| raw_decode_error("RAW has no decodable image"))
}

fn load_raw_thumbnail_preview(path: &Path) -> image::ImageResult<image::DynamicImage> {
    let direct = match load_direct_raw_preview(path, DirectRawPreviewKind::Thumbnail) {
        Some(img) if is_direct_raw_thumbnail_short_circuit_candidate(&img) => return Ok(img),
        other => other,
    };

    let raw = RawSource::new(path).map_err(raw_decode_error)?;
    let decoder = rawler::get_decoder(&raw).map_err(raw_decode_error)?;
    let params = RawDecodeParams::default();

    let thumbnail = decoder
        .thumbnail_image(&raw, &params)
        .map_err(raw_decode_error)?;
    let preview = decoder
        .preview_image(&raw, &params)
        .map_err(raw_decode_error)?;

    let rawler_candidate = pick_raw_thumbnail_candidate(thumbnail, preview);
    if let Some(img) = pick_smaller_image(rawler_candidate, direct) {
        return Ok(img);
    }

    // rawler exposed no embedded thumbnail/preview — fall back to a full demosaic
    // (slow first view; the XDG disk cache makes subsequent views fast). Fast
    // direct embedded-JPEG extraction is the v5-M2 follow-up.
    decoder
        .full_image(&raw, &params)
        .map_err(raw_decode_error)?
        .ok_or_else(|| raw_decode_error("RAW has no decodable image"))
}

fn load_raw_full_demosaic(path: &Path) -> image::ImageResult<image::DynamicImage> {
    let raw = RawSource::new(path).map_err(raw_decode_error)?;
    let decoder = rawler::get_decoder(&raw).map_err(raw_decode_error)?;
    decoder
        .full_image(&raw, &RawDecodeParams::default())
        .map_err(raw_decode_error)?
        .ok_or_else(|| raw_decode_error("RAW has no decodable image"))
}

fn pick_raw_thumbnail_candidate(
    thumbnail: Option<image::DynamicImage>,
    preview: Option<image::DynamicImage>,
) -> Option<image::DynamicImage> {
    thumbnail.or(preview)
}

fn image_area(img: &image::DynamicImage) -> u64 {
    u64::from(img.width()) * u64::from(img.height())
}

const DIRECT_RAW_THUMBNAIL_MAX_EDGE: u32 = 1024;

fn is_direct_raw_thumbnail_short_circuit_candidate(img: &image::DynamicImage) -> bool {
    img.width().max(img.height()) <= DIRECT_RAW_THUMBNAIL_MAX_EDGE
}

fn pick_larger_image(
    a: Option<image::DynamicImage>,
    b: Option<image::DynamicImage>,
) -> Option<image::DynamicImage> {
    match (a, b) {
        (Some(a), Some(b)) if image_area(&b) > image_area(&a) => Some(b),
        (Some(a), _) => Some(a),
        (None, b) => b,
    }
}

fn pick_smaller_image(
    a: Option<image::DynamicImage>,
    b: Option<image::DynamicImage>,
) -> Option<image::DynamicImage> {
    match (a, b) {
        (Some(a), Some(b)) if image_area(&b) < image_area(&a) => Some(b),
        (Some(a), _) => Some(a),
        (None, b) => b,
    }
}

#[derive(Copy, Clone)]
enum DirectRawPreviewKind {
    Thumbnail,
    Loupe,
}

#[derive(Copy, Clone)]
struct EmbeddedJpegRange {
    offset: usize,
    len: usize,
}

#[derive(Copy, Clone)]
enum TiffEndian {
    Little,
    Big,
}

impl TiffEndian {
    fn read_u16(self, bytes: &[u8], offset: usize) -> Option<u16> {
        let end = offset.checked_add(2)?;
        let raw: [u8; 2] = bytes.get(offset..end)?.try_into().ok()?;
        Some(match self {
            Self::Little => u16::from_le_bytes(raw),
            Self::Big => u16::from_be_bytes(raw),
        })
    }

    fn read_u32(self, bytes: &[u8], offset: usize) -> Option<u32> {
        let end = offset.checked_add(4)?;
        let raw: [u8; 4] = bytes.get(offset..end)?.try_into().ok()?;
        Some(match self {
            Self::Little => u32::from_le_bytes(raw),
            Self::Big => u32::from_be_bytes(raw),
        })
    }

    #[cfg(test)]
    fn u16_bytes(self, value: u16) -> [u8; 2] {
        match self {
            Self::Little => value.to_le_bytes(),
            Self::Big => value.to_be_bytes(),
        }
    }

    #[cfg(test)]
    fn u32_bytes(self, value: u32) -> [u8; 4] {
        match self {
            Self::Little => value.to_le_bytes(),
            Self::Big => value.to_be_bytes(),
        }
    }
}

fn load_direct_raw_preview(path: &Path, kind: DirectRawPreviewKind) -> Option<image::DynamicImage> {
    let bytes = std::fs::read(path).ok()?;
    extract_direct_raw_preview_from_bytes(&bytes, kind)
}

fn extract_direct_raw_preview_from_bytes(
    bytes: &[u8],
    kind: DirectRawPreviewKind,
) -> Option<image::DynamicImage> {
    let mut ranges = Vec::new();
    collect_tiff_embedded_jpegs(bytes, 0, &mut ranges);
    collect_iso_bmff_embedded_jpegs(bytes, &mut ranges);
    // Order candidates by preference (Thumbnail = smallest/fastest, Loupe = largest/best quality),
    // then return the FIRST that actually decodes. Some RAWs (e.g. Nikon Z) carry a largest embedded
    // "preview" that image-rs can't decode; trying only that single best candidate would fall back to a
    // slow full demosaic even though a perfectly good slightly-smaller embedded JPEG is present.
    // Iterating in preference order keeps the previous result whenever every candidate decodes.
    ranges.sort_by_key(|range| range.len);
    if matches!(kind, DirectRawPreviewKind::Loupe) {
        ranges.reverse();
    }
    ranges.into_iter().find_map(|range| {
        let end = range.offset.checked_add(range.len)?;
        let slice = bytes.get(range.offset..end)?;
        image::load_from_memory_with_format(slice, image::ImageFormat::Jpeg).ok()
    })
}

/// Format-agnostic TIFF-embedded JPEG collector (NEF/CR2/ARW/DNG etc).
/// Walks primary IFD + SubIFDs (0x014a), EXIF (0x8769); extracts via 0x0201/0x0202.
fn collect_tiff_embedded_jpegs(
    bytes: &[u8],
    tiff_base: usize,
    ranges: &mut Vec<EmbeddedJpegRange>,
) {
    let Some((endian, first_ifd_rel)) = parse_tiff_header(bytes, tiff_base) else {
        return;
    };
    let Some(first_ifd) = tiff_base.checked_add(first_ifd_rel) else {
        return;
    };
    collect_ifd_embedded_jpegs(bytes, endian, tiff_base, first_ifd, 0, ranges);
}

/// ISO-BMFF (Canon CR3 etc.) embedded-JPEG collector. CR3 is a box-based
/// container, NOT TIFF, so `collect_tiff_embedded_jpegs` finds nothing in it.
/// Canon stores its thumbnail/preview as standalone JPEG streams (THMB/PRVW
/// boxes); rather than parse the full box tree we scan for JPEG SOI markers and
/// push each stream as a candidate. The caller decode-validates and size-orders,
/// so a false positive is harmless and the largest decodable JPEG wins the loupe.
///
/// Each candidate spans from its SOI to the NEXT SOI (or EOF), not to the first
/// EOI: a JPEG decoder stops at its own EOI and ignores trailing bytes, so
/// overshooting is safe and — unlike ending at the first `FF D9` — never
/// truncates on an EOI that appears inside the entropy/EXIF data.
fn collect_iso_bmff_embedded_jpegs(bytes: &[u8], ranges: &mut Vec<EmbeddedJpegRange>) {
    // Engage only for ISO base-media files ("ftyp" box at offset 4); leaves TIFF
    // RAWs and unrelated data to the other collectors / untouched.
    if bytes.get(4..8) != Some(b"ftyp") {
        return;
    }
    const SOI: [u8; 3] = [0xFF, 0xD8, 0xFF];
    let mut i = 0usize;
    let mut found = 0usize;
    while let Some(rel) = bytes
        .get(i..)
        .and_then(|s| s.windows(3).position(|w| w == SOI))
    {
        let start = i + rel;
        let next = bytes
            .get(start + 3..)
            .and_then(|s| s.windows(3).position(|w| w == SOI))
            .map(|p| start + 3 + p)
            .unwrap_or(bytes.len());
        if next > start {
            ranges.push(EmbeddedJpegRange {
                offset: start,
                len: next - start,
            });
        }
        i = next;
        found += 1;
        // Defensive cap: real CR3s carry only a handful of embedded JPEGs; this
        // also bounds the scan if RAW image data contains spurious SOI patterns.
        if found >= 16 {
            break;
        }
    }
}

fn parse_tiff_header(bytes: &[u8], base: usize) -> Option<(TiffEndian, usize)> {
    let byte_order = bytes.get(base..base.checked_add(2)?)?;
    let endian = match byte_order {
        b"II" => TiffEndian::Little,
        b"MM" => TiffEndian::Big,
        _ => return None,
    };
    if endian.read_u16(bytes, base.checked_add(2)?)? != 42 {
        return None;
    }
    let first = usize::try_from(endian.read_u32(bytes, base.checked_add(4)?)?).ok()?;
    Some((endian, first))
}

fn collect_ifd_embedded_jpegs(
    bytes: &[u8],
    endian: TiffEndian,
    tiff_base: usize,
    ifd_offset: usize,
    depth: u8,
    ranges: &mut Vec<EmbeddedJpegRange>,
) {
    if depth > 8 || ifd_offset >= bytes.len() {
        return;
    }
    let Some(entries) = endian.read_u16(bytes, ifd_offset).map(usize::from) else {
        return;
    };
    let Some(entries_bytes) = entries.checked_mul(12) else {
        return;
    };
    let Some(entries_start) = ifd_offset.checked_add(2) else {
        return;
    };
    let Some(next_offset_pos) = entries_start.checked_add(entries_bytes) else {
        return;
    };
    if next_offset_pos
        .checked_add(4)
        .is_none_or(|end| end > bytes.len())
    {
        return;
    }

    let mut jpeg_offset = None;
    let mut jpeg_len = None;
    for i in 0..entries {
        let Some(entry) = entries_start.checked_add(i.saturating_mul(12)) else {
            return;
        };
        let Some(tag) = endian.read_u16(bytes, entry) else {
            return;
        };
        let value_type = endian.read_u16(bytes, entry + 2).unwrap_or(0);
        let count = endian.read_u32(bytes, entry + 4).unwrap_or(0);
        let value = endian.read_u32(bytes, entry + 8).unwrap_or(0);
        match tag {
            0x0201 => jpeg_offset = Some(value),
            0x0202 => jpeg_len = Some(value),
            0x014a | 0x8769 | 0x8825 => collect_sub_ifds(
                bytes, endian, tiff_base, value_type, count, value, depth, ranges,
            ),
            0x927c => collect_nikon_maker_note_preview(
                bytes, endian, tiff_base, value_type, count, value, depth, ranges,
            ),
            _ => {}
        }
    }

    if let (Some(offset), Some(len)) = (jpeg_offset, jpeg_len) {
        if let (Ok(offset), Ok(len)) = (usize::try_from(offset), usize::try_from(len)) {
            if len > 0 {
                if let Some(offset) = tiff_base.checked_add(offset) {
                    if offset
                        .checked_add(len)
                        .is_some_and(|end| end <= bytes.len())
                    {
                        ranges.push(EmbeddedJpegRange { offset, len });
                    }
                }
            }
        }
    }

    if let Some(next_rel) = endian
        .read_u32(bytes, next_offset_pos)
        .and_then(|v| usize::try_from(v).ok())
    {
        if next_rel != 0 {
            if let Some(next_ifd) = tiff_base.checked_add(next_rel) {
                collect_ifd_embedded_jpegs(bytes, endian, tiff_base, next_ifd, depth + 1, ranges);
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn collect_sub_ifds(
    bytes: &[u8],
    endian: TiffEndian,
    tiff_base: usize,
    value_type: u16,
    count: u32,
    value: u32,
    depth: u8,
    ranges: &mut Vec<EmbeddedJpegRange>,
) {
    if value_type != 4 || count == 0 {
        return;
    }
    for n in 0..count.min(16) {
        let rel = if count == 1 {
            value
        } else {
            let Some(pos) = usize::try_from(value)
                .ok()
                .and_then(|v| tiff_base.checked_add(v))
                .and_then(|p| p.checked_add(usize::try_from(n).ok()?.saturating_mul(4)))
            else {
                continue;
            };
            endian.read_u32(bytes, pos).unwrap_or(0)
        };
        let Some(ifd) = usize::try_from(rel)
            .ok()
            .and_then(|v| tiff_base.checked_add(v))
        else {
            continue;
        };
        collect_ifd_embedded_jpegs(bytes, endian, tiff_base, ifd, depth + 1, ranges);
    }
}

#[allow(clippy::too_many_arguments)]
fn collect_nikon_maker_note_preview(
    bytes: &[u8],
    _outer_endian: TiffEndian,
    tiff_base: usize,
    value_type: u16,
    count: u32,
    value: u32,
    depth: u8,
    ranges: &mut Vec<EmbeddedJpegRange>,
) {
    if value_type != 7 || count == 0 {
        return;
    }
    let Some(maker_note_offset) = usize::try_from(value)
        .ok()
        .and_then(|v| tiff_base.checked_add(v))
    else {
        return;
    };
    let Some(count) = usize::try_from(count).ok() else {
        return;
    };
    if maker_note_offset
        .checked_add(count)
        .is_none_or(|end| end > bytes.len())
    {
        return;
    }

    for maker_tiff_base in [maker_note_offset.checked_add(10), Some(maker_note_offset)]
        .into_iter()
        .flatten()
    {
        let Some((maker_endian, first_ifd_rel)) = parse_tiff_header(bytes, maker_tiff_base) else {
            continue;
        };
        let Some(first_ifd) = maker_tiff_base.checked_add(first_ifd_rel) else {
            continue;
        };
        let Some(preview_ifd_rel) = find_ifd_u32_value(bytes, maker_endian, first_ifd, 0x0011)
        else {
            continue;
        };
        let Some(preview_ifd) = usize::try_from(preview_ifd_rel)
            .ok()
            .and_then(|v| maker_tiff_base.checked_add(v))
        else {
            continue;
        };
        collect_ifd_embedded_jpegs(
            bytes,
            maker_endian,
            maker_tiff_base,
            preview_ifd,
            depth + 1,
            ranges,
        );
        return;
    }
}

fn find_ifd_u32_value(
    bytes: &[u8],
    endian: TiffEndian,
    ifd_offset: usize,
    wanted_tag: u16,
) -> Option<u32> {
    let entries = usize::from(endian.read_u16(bytes, ifd_offset)?);
    let entries_start = ifd_offset.checked_add(2)?;
    for i in 0..entries {
        let entry = entries_start.checked_add(i.checked_mul(12)?)?;
        if endian.read_u16(bytes, entry)? == wanted_tag {
            return endian.read_u32(bytes, entry.checked_add(8)?);
        }
    }
    None
}

#[cfg(feature = "exif")]
fn load_exif_embedded_thumbnail(path: &Path) -> Option<image::DynamicImage> {
    use std::fs::File;
    use std::io::BufReader;

    use exif::{In, Reader, Tag};

    let file = File::open(path).ok()?;
    let exif = Reader::new()
        .read_from_container(&mut BufReader::new(file))
        .ok()?;
    let offset = exif
        .get_field(Tag::JPEGInterchangeFormat, In::THUMBNAIL)?
        .value
        .get_uint(0)? as usize;
    let len = exif
        .get_field(Tag::JPEGInterchangeFormatLength, In::THUMBNAIL)?
        .value
        .get_uint(0)? as usize;
    let end = offset.checked_add(len)?;

    image::load_from_memory(exif.buf().get(offset..end)?).ok()
}

#[cfg(not(feature = "exif"))]
fn load_exif_embedded_thumbnail(_path: &Path) -> Option<image::DynamicImage> {
    None
}

fn raw_decode_error(err: impl std::fmt::Display) -> image::ImageError {
    image::ImageError::IoError(std::io::Error::new(
        std::io::ErrorKind::InvalidData,
        err.to_string(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{DynamicImage, GenericImageView, ImageFormat, RgbImage, Rgba, RgbaImage};
    use std::path::Path;

    fn asymmetric_image() -> DynamicImage {
        let mut img = RgbaImage::from_pixel(3, 2, Rgba([0, 0, 0, 255]));
        img.put_pixel(0, 0, Rgba([255, 0, 0, 255]));
        img.put_pixel(2, 0, Rgba([0, 255, 0, 255]));
        img.put_pixel(0, 1, Rgba([0, 0, 255, 255]));
        img.put_pixel(2, 1, Rgba([255, 255, 0, 255]));
        DynamicImage::ImageRgba8(img)
    }

    fn assert_oriented(orientation: u8, expected: &[(u32, u32, [u8; 4])], dims: (u32, u32)) {
        let out = apply_orientation(asymmetric_image(), orientation);
        assert_eq!(out.dimensions(), dims, "orientation {orientation}");
        for &(x, y, rgba) in expected {
            assert_eq!(
                out.get_pixel(x, y),
                Rgba(rgba),
                "orientation {orientation} at {x},{y}"
            );
        }
    }

    #[test]
    fn recognizes_photobrowser_raw_extensions_case_insensitively() {
        assert!(is_raw_extension("photo.nef"));
        assert!(is_raw_extension("photo.CR2"));
        assert!(is_raw_extension("photo.dng"));
        assert!(!is_raw_extension("photo.jpg"));
        assert!(!is_raw_extension("notes.txt"));
    }

    #[test]
    fn load_image_decodes_non_raw_through_normal_image_path() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tiny.png");
        let img = RgbImage::from_pixel(2, 3, image::Rgb([1, 2, 3]));
        img.save_with_format(&path, ImageFormat::Png).unwrap();

        let decoded = load_image(&path).expect("tiny png should decode");
        assert_eq!(decoded.width(), 2);
        assert_eq!(decoded.height(), 3);
    }

    #[test]
    fn reduced_thumbnail_jpeg_fallback_uses_dct_scaled_decode() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("large-no-thumb.jpg");
        let img = RgbImage::from_fn(1024, 512, |x, y| {
            image::Rgb([(x % 251) as u8, (y % 241) as u8, ((x + y) % 239) as u8])
        });
        img.save_with_format(&path, ImageFormat::Jpeg).unwrap();

        let full = load_thumbnail_image(&path).expect("baseline JPEG thumbnail path should decode");
        assert_eq!(full.dimensions(), (1024, 512));

        let reduced = load_thumbnail_image_reduced(&path, 256)
            .expect("reduced JPEG thumbnail path should decode");
        assert_eq!(reduced.dimensions(), (256, 128));
    }

    #[test]
    fn apply_orientation_maps_all_exif_values() {
        assert_oriented(
            1,
            &[(0, 0, [255, 0, 0, 255]), (2, 1, [255, 255, 0, 255])],
            (3, 2),
        );
        assert_oriented(
            2,
            &[(2, 0, [255, 0, 0, 255]), (0, 1, [255, 255, 0, 255])],
            (3, 2),
        );
        assert_oriented(
            3,
            &[(2, 1, [255, 0, 0, 255]), (0, 0, [255, 255, 0, 255])],
            (3, 2),
        );
        assert_oriented(
            4,
            &[(0, 1, [255, 0, 0, 255]), (2, 0, [255, 255, 0, 255])],
            (3, 2),
        );
        assert_oriented(
            5,
            &[(1, 2, [255, 0, 0, 255]), (0, 0, [255, 255, 0, 255])],
            (2, 3),
        );
        assert_oriented(
            6,
            &[(1, 0, [255, 0, 0, 255]), (0, 2, [255, 255, 0, 255])],
            (2, 3),
        );
        assert_oriented(
            7,
            &[(0, 0, [255, 0, 0, 255]), (1, 2, [255, 255, 0, 255])],
            (2, 3),
        );
        assert_oriented(
            8,
            &[(0, 2, [255, 0, 0, 255]), (1, 0, [255, 255, 0, 255])],
            (2, 3),
        );
        assert_oriented(
            99,
            &[(0, 0, [255, 0, 0, 255]), (2, 1, [255, 255, 0, 255])],
            (3, 2),
        );
    }

    #[test]
    fn read_orientation_reads_numeric_exif_and_defaults_to_one() {
        use exif::{experimental::Writer, Field, In, Tag, Value};
        use std::io::Cursor;

        let dir = tempfile::tempdir().unwrap();
        let oriented = dir.path().join("oriented.tif");
        let missing = dir.path().join("missing.tif");

        let field = Field {
            tag: Tag::Orientation,
            ifd_num: In::PRIMARY,
            value: Value::Short(vec![6]),
        };
        let mut writer = Writer::new();
        writer.push_field(&field);
        let mut buf = Cursor::new(Vec::new());
        writer.write(&mut buf, false).unwrap();
        std::fs::write(&oriented, buf.into_inner()).unwrap();
        std::fs::write(&missing, b"not exif").unwrap();

        assert_eq!(read_orientation(&oriented), 6);
        assert_eq!(read_orientation(&missing), 1);
        assert_eq!(
            read_orientation(Path::new("/definitely/missing/orientation.jpg")),
            1
        );
    }

    #[test]
    fn raw_thumbnail_candidate_prefers_embedded_thumbnail_over_preview() {
        let thumbnail =
            DynamicImage::ImageRgba8(RgbaImage::from_pixel(2, 2, Rgba([10, 20, 30, 255])));
        let preview = DynamicImage::ImageRgba8(RgbaImage::from_pixel(8, 8, Rgba([1, 2, 3, 255])));

        let picked = pick_raw_thumbnail_candidate(Some(thumbnail), Some(preview))
            .expect("thumbnail candidate should be selected");

        assert_eq!(picked.dimensions(), (2, 2));
        assert_eq!(picked.get_pixel(0, 0), Rgba([10, 20, 30, 255]));
    }

    #[test]
    fn raw_thumbnail_candidate_falls_back_to_preview() {
        let preview = DynamicImage::ImageRgba8(RgbaImage::from_pixel(8, 8, Rgba([1, 2, 3, 255])));

        let picked = pick_raw_thumbnail_candidate(None, Some(preview))
            .expect("preview candidate should be selected");

        assert_eq!(picked.dimensions(), (8, 8));
        assert_eq!(picked.get_pixel(0, 0), Rgba([1, 2, 3, 255]));
    }

    #[test]
    fn direct_raw_thumbnail_short_circuit_only_accepts_thumbnail_scale_images() {
        let thumbnail = DynamicImage::ImageRgba8(RgbaImage::from_pixel(
            DIRECT_RAW_THUMBNAIL_MAX_EDGE,
            DIRECT_RAW_THUMBNAIL_MAX_EDGE,
            Rgba([10, 20, 30, 255]),
        ));
        let oversized = DynamicImage::ImageRgba8(RgbaImage::from_pixel(
            DIRECT_RAW_THUMBNAIL_MAX_EDGE + 1,
            DIRECT_RAW_THUMBNAIL_MAX_EDGE,
            Rgba([1, 2, 3, 255]),
        ));

        assert!(is_direct_raw_thumbnail_short_circuit_candidate(&thumbnail));
        assert!(!is_direct_raw_thumbnail_short_circuit_candidate(&oversized));
    }

    #[test]
    fn reads_exif_embedded_thumbnail_before_full_decode() {
        use exif::{experimental::Writer, Field, In, Tag, Value};
        use std::io::Cursor;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("with-thumb.tif");
        let tiny = DynamicImage::ImageRgba8(RgbaImage::from_pixel(3, 2, Rgba([7, 8, 9, 255])));
        let mut jpeg = Vec::new();
        tiny.write_to(&mut Cursor::new(&mut jpeg), ImageFormat::Jpeg)
            .unwrap();

        let orientation = Field {
            tag: Tag::Orientation,
            ifd_num: In::PRIMARY,
            value: Value::Short(vec![1]),
        };
        let mut writer = Writer::new();
        writer.push_field(&orientation);
        writer.set_jpeg(&jpeg, In::THUMBNAIL);
        let mut exif_buf = Cursor::new(Vec::new());
        writer.write(&mut exif_buf, false).unwrap();
        std::fs::write(&path, exif_buf.into_inner()).unwrap();

        let decoded = load_exif_embedded_thumbnail(&path).expect("embedded JPEG thumbnail");
        assert_eq!(decoded.dimensions(), (3, 2));
    }

    fn tiny_jpeg(width: u32, height: u32, rgb: [u8; 3]) -> Vec<u8> {
        let img = DynamicImage::ImageRgb8(RgbImage::from_pixel(width, height, image::Rgb(rgb)));
        let mut jpeg = Vec::new();
        img.write_to(&mut std::io::Cursor::new(&mut jpeg), ImageFormat::Jpeg)
            .unwrap();
        jpeg
    }

    fn write_u16(out: &mut [u8], offset: usize, value: u16, endian: TiffEndian) {
        out[offset..offset + 2].copy_from_slice(&endian.u16_bytes(value));
    }

    fn write_u32(out: &mut [u8], offset: usize, value: u32, endian: TiffEndian) {
        out[offset..offset + 4].copy_from_slice(&endian.u32_bytes(value));
    }

    fn write_ifd_entry(
        out: &mut [u8],
        offset: usize,
        tag: u16,
        value_type: u16,
        count: u32,
        value: u32,
        endian: TiffEndian,
    ) {
        write_u16(out, offset, tag, endian);
        write_u16(out, offset + 2, value_type, endian);
        write_u32(out, offset + 4, count, endian);
        write_u32(out, offset + 8, value, endian);
    }

    fn synthetic_nef_with_preview_ifd(small_jpeg: &[u8], large_jpeg: &[u8]) -> Vec<u8> {
        let endian = TiffEndian::Little;
        let small_offset = 300usize;
        let large_offset = small_offset + small_jpeg.len() + 24;
        let maker_note_offset = 120usize;
        let preview_ifd_offset = 32usize;
        let sub_ifd_offset = 220usize;
        let total_len = large_offset + large_jpeg.len() + 16;
        let mut out = vec![0u8; total_len];

        out[0..2].copy_from_slice(b"II");
        write_u16(&mut out, 2, 42, endian);
        write_u32(&mut out, 4, 8, endian);
        write_u16(&mut out, 8, 3, endian);
        write_ifd_entry(
            &mut out,
            10,
            0x927c,
            7,
            120,
            maker_note_offset as u32,
            endian,
        );
        write_ifd_entry(&mut out, 22, 0x014a, 4, 1, sub_ifd_offset as u32, endian);
        write_ifd_entry(&mut out, 34, 0x0112, 3, 1, 1, endian);
        write_u32(&mut out, 46, 0, endian);

        out[maker_note_offset..maker_note_offset + 10].copy_from_slice(b"Nikon\0\x02\x10\0\0");
        let maker_tiff = maker_note_offset + 10;
        out[maker_tiff..maker_tiff + 2].copy_from_slice(b"II");
        write_u16(&mut out, maker_tiff + 2, 42, endian);
        write_u32(&mut out, maker_tiff + 4, 8, endian);
        write_u16(&mut out, maker_tiff + 8, 1, endian);
        write_ifd_entry(
            &mut out,
            maker_tiff + 10,
            0x0011,
            4,
            1,
            preview_ifd_offset as u32,
            endian,
        );
        write_u32(&mut out, maker_tiff + 22, 0, endian);
        let preview_ifd = maker_tiff + preview_ifd_offset;
        write_u16(&mut out, preview_ifd, 2, endian);
        write_ifd_entry(
            &mut out,
            preview_ifd + 2,
            0x0201,
            4,
            1,
            (small_offset - maker_tiff) as u32,
            endian,
        );
        write_ifd_entry(
            &mut out,
            preview_ifd + 14,
            0x0202,
            4,
            1,
            small_jpeg.len() as u32,
            endian,
        );
        write_u32(&mut out, preview_ifd + 26, 0, endian);

        write_u16(&mut out, sub_ifd_offset, 2, endian);
        write_ifd_entry(
            &mut out,
            sub_ifd_offset + 2,
            0x0201,
            4,
            1,
            large_offset as u32,
            endian,
        );
        write_ifd_entry(
            &mut out,
            sub_ifd_offset + 14,
            0x0202,
            4,
            1,
            large_jpeg.len() as u32,
            endian,
        );
        write_u32(&mut out, sub_ifd_offset + 26, 0, endian);

        out[small_offset..small_offset + small_jpeg.len()].copy_from_slice(small_jpeg);
        out[large_offset..large_offset + large_jpeg.len()].copy_from_slice(large_jpeg);
        out
    }

    fn synthetic_cr2_with_embedded_jpeg(jpeg: &[u8]) -> Vec<u8> {
        let endian = TiffEndian::Little;
        let jpeg_offset = 200usize;
        let total_len = jpeg_offset + jpeg.len() + 16;
        let mut out = vec![0u8; total_len];

        out[0..2].copy_from_slice(b"II");
        write_u16(&mut out, 2, 42, endian);
        write_u32(&mut out, 4, 8, endian);
        write_u16(&mut out, 8, 3, endian);
        write_ifd_entry(&mut out, 10, 0x0112, 3, 1, 1, endian);
        write_ifd_entry(&mut out, 22, 0x0201, 4, 1, jpeg_offset as u32, endian);
        write_ifd_entry(&mut out, 34, 0x0202, 4, 1, jpeg.len() as u32, endian);
        write_u32(&mut out, 46, 0, endian);

        out[jpeg_offset..jpeg_offset + jpeg.len()].copy_from_slice(jpeg);
        out
    }

    fn synthetic_arw_with_embedded_jpeg(jpeg: &[u8]) -> Vec<u8> {
        let endian = TiffEndian::Little;
        let sub_ifd_offset = 100usize;
        let jpeg_offset = 200usize;
        let total_len = jpeg_offset + jpeg.len() + 16;
        let mut out = vec![0u8; total_len];

        out[0..2].copy_from_slice(b"II");
        write_u16(&mut out, 2, 42, endian);
        write_u32(&mut out, 4, 8, endian);
        // IFD0: unrelated tag + SubIFD 0x014a pointer (count=2)
        write_u16(&mut out, 8, 2, endian);
        write_ifd_entry(&mut out, 10, 0x0112, 3, 1, 1, endian);
        write_ifd_entry(&mut out, 22, 0x014a, 4, 1, sub_ifd_offset as u32, endian);
        write_u32(&mut out, 34, 0, endian);

        // Sub-IFD carries the embedded JPEG (0x0201/0x0202)
        write_u16(&mut out, sub_ifd_offset, 2, endian);
        write_ifd_entry(
            &mut out,
            sub_ifd_offset + 2,
            0x0201,
            4,
            1,
            jpeg_offset as u32,
            endian,
        );
        write_ifd_entry(
            &mut out,
            sub_ifd_offset + 14,
            0x0202,
            4,
            1,
            jpeg.len() as u32,
            endian,
        );
        write_u32(&mut out, sub_ifd_offset + 26, 0, endian);

        out[jpeg_offset..jpeg_offset + jpeg.len()].copy_from_slice(jpeg);
        out
    }

    fn synthetic_cr3_with_embedded_jpegs(small: &[u8], large: &[u8]) -> Vec<u8> {
        // Minimal ISO-BMFF: a 'ftyp' box (so the collector engages), then the two
        // JPEG streams Canon stores as THMB/PRVW, separated by a few bytes of
        // box-header-like padding. The collector scans for SOI markers, so exact
        // box framing is irrelevant — only the 'ftyp' signature + the JPEGs matter.
        let mut out = Vec::new();
        let ftyp_body = b"crx \x00\x00\x00\x01crx isom"; // brand + minor + compat
        let ftyp_size = (8 + ftyp_body.len()) as u32;
        out.extend_from_slice(&ftyp_size.to_be_bytes());
        out.extend_from_slice(b"ftyp");
        out.extend_from_slice(ftyp_body);
        out.extend_from_slice(b"\x00\x00\x00\x00THMB"); // pseudo box header
        out.extend_from_slice(small);
        out.extend_from_slice(b"\x00\x00\x00\x00PRVW");
        out.extend_from_slice(large);
        out
    }

    #[test]
    fn direct_raw_preview_extracts_from_cr3_iso_bmff_container() {
        // CR3 is box-based, not TIFF: the TIFF collector finds nothing, so this
        // exercises the ISO-BMFF SOI-scan path. Thumbnail picks the smaller, Loupe
        // the larger embedded JPEG — same preference as TIFF RAWs.
        let small = tiny_jpeg(8, 4, [180, 40, 30]);
        let large = tiny_jpeg(96, 48, [20, 160, 60]);
        let cr3 = synthetic_cr3_with_embedded_jpegs(&small, &large);

        let thumb = extract_direct_raw_preview_from_bytes(&cr3, DirectRawPreviewKind::Thumbnail)
            .expect("CR3 thumbnail JPEG should decode");
        let loupe = extract_direct_raw_preview_from_bytes(&cr3, DirectRawPreviewKind::Loupe)
            .expect("CR3 largest embedded JPEG should decode");

        assert_eq!(thumb.dimensions(), (8, 4));
        assert_eq!(loupe.dimensions(), (96, 48));
    }

    #[test]
    fn iso_bmff_collector_ignores_non_ftyp_data() {
        // A TIFF RAW (no 'ftyp') must not be touched by the BMFF collector.
        let mut ranges = Vec::new();
        collect_iso_bmff_embedded_jpegs(b"II*\x00not an iso bmff file at all", &mut ranges);
        assert!(ranges.is_empty());
    }

    #[test]
    fn direct_raw_preview_extracts_small_preview_and_largest_jpeg() {
        let small = tiny_jpeg(4, 2, [200, 10, 20]);
        let large = tiny_jpeg(64, 32, [10, 200, 20]);
        let nef = synthetic_nef_with_preview_ifd(&small, &large);

        let thumb = extract_direct_raw_preview_from_bytes(&nef, DirectRawPreviewKind::Thumbnail)
            .expect("small PreviewIFD JPEG should decode");
        let loupe = extract_direct_raw_preview_from_bytes(&nef, DirectRawPreviewKind::Loupe)
            .expect("largest embedded JPEG should decode");

        assert_eq!(thumb.dimensions(), (4, 2));
        assert_eq!(loupe.dimensions(), (64, 32));
    }

    #[test]
    fn direct_raw_preview_returns_none_for_truncated_or_malformed_input() {
        let small = tiny_jpeg(4, 2, [200, 10, 20]);
        let large = tiny_jpeg(64, 32, [10, 200, 20]);
        let nef = synthetic_nef_with_preview_ifd(&small, &large);

        for len in 0..nef.len().min(256) {
            let _ =
                extract_direct_raw_preview_from_bytes(&nef[..len], DirectRawPreviewKind::Thumbnail);
        }
        assert!(
            extract_direct_raw_preview_from_bytes(&nef[..16], DirectRawPreviewKind::Thumbnail)
                .is_none()
        );
        assert!(
            extract_direct_raw_preview_from_bytes(b"not a tiff", DirectRawPreviewKind::Loupe)
                .is_none()
        );
    }

    #[test]
    fn cr2_embedded_jpeg_is_extracted() {
        let jpeg = tiny_jpeg(48, 24, [180, 40, 90]);
        let cr2 = synthetic_cr2_with_embedded_jpeg(&jpeg);

        let thumb = extract_direct_raw_preview_from_bytes(&cr2, DirectRawPreviewKind::Thumbnail)
            .expect("CR2 direct IFD0 JPEG should decode for Thumbnail");
        let loupe = extract_direct_raw_preview_from_bytes(&cr2, DirectRawPreviewKind::Loupe)
            .expect("CR2 direct IFD0 JPEG should decode for Loupe");

        assert_eq!(thumb.dimensions(), (48, 24));
        assert_eq!(loupe.dimensions(), (48, 24));
    }

    #[test]
    fn arw_embedded_jpeg_via_subifd_is_extracted() {
        let jpeg = tiny_jpeg(48, 24, [30, 120, 200]);
        let arw = synthetic_arw_with_embedded_jpeg(&jpeg);

        let thumb = extract_direct_raw_preview_from_bytes(&arw, DirectRawPreviewKind::Thumbnail)
            .expect("ARW SubIFD JPEG should decode for Thumbnail");
        let loupe = extract_direct_raw_preview_from_bytes(&arw, DirectRawPreviewKind::Loupe)
            .expect("ARW SubIFD JPEG should decode for Loupe");

        assert_eq!(thumb.dimensions(), (48, 24));
        assert_eq!(loupe.dimensions(), (48, 24));
    }

    /// Env-gated decode micro-benchmark + orientation dump (perf evidence).
    ///
    /// Runs only when the env vars are set, so a normal `cargo test` (which also
    /// skips `#[ignore]`) and CI are unaffected. To produce real numbers:
    ///   PHOTOBROWSER_BENCH_JPEG=/path/a.jpg PHOTOBROWSER_BENCH_NEF=/path/b.nef \
    ///   PHOTOBROWSER_BENCH_DUMP=/tmp/orient \
    ///   cargo test --release decode::tests::decode_micro_benchmark -- --ignored --nocapture
    /// Compares the new thumbnail path against the pre-v5 baseline (full JPEG
    /// decode / full RAW demosaic) on the same files, and optionally dumps the
    /// oriented decode outputs to PHOTOBROWSER_BENCH_DUMP for a visual orientation check.
    #[test]
    #[ignore = "env-gated; needs real PHOTOBROWSER_BENCH_JPEG / PHOTOBROWSER_BENCH_NEF files"]
    fn decode_micro_benchmark() {
        use std::time::Instant;

        fn ms(f: impl FnOnce()) -> f64 {
            let start = Instant::now();
            f();
            start.elapsed().as_secs_f64() * 1000.0
        }

        if let Ok(jpeg) = std::env::var("PHOTOBROWSER_BENCH_JPEG") {
            let path = Path::new(&jpeg);
            let baseline = ms(|| {
                let _ = image::open(path);
            });
            match load_thumbnail_image(path) {
                Ok(_) => {
                    let updated = ms(|| {
                        let _ = load_thumbnail_image(path);
                    });
                    println!(
                        "JPEG baseline(full image::open)={baseline:.1}ms  new(thumbnail path)={updated:.1}ms  speedup={:.1}x",
                        baseline / updated.max(0.0001)
                    );
                }
                Err(e) => {
                    println!("JPEG new(thumbnail path) FAILED: {e} (baseline full decode={baseline:.1}ms)")
                }
            }
        }

        if let Ok(nef) = std::env::var("PHOTOBROWSER_BENCH_NEF") {
            let path = Path::new(&nef);
            let baseline = ms(|| {
                if let Ok(raw) = RawSource::new(path) {
                    if let Ok(decoder) = rawler::get_decoder(&raw) {
                        let _ = decoder.full_image(&raw, &RawDecodeParams::default());
                    }
                }
            });
            let direct_embedded = ms(|| {
                let _ = load_direct_raw_preview(path, DirectRawPreviewKind::Thumbnail);
            });
            match load_thumbnail_image(path) {
                Ok(_) => {
                    let updated = ms(|| {
                        let _ = load_thumbnail_image(path);
                    });
                    println!(
                        "NEF  baseline(rawler full_image demosaic)={baseline:.1}ms  new(direct embedded JPEG)={direct_embedded:.1}ms  load_thumbnail_image={updated:.1}ms  speedup={:.1}x",
                        baseline / direct_embedded.max(0.0001)
                    );
                }
                Err(e) => println!(
                    "NEF  new(embedded thumbnail) FAILED: {e} (no embedded preview; baseline full_image demosaic={baseline:.1}ms)"
                ),
            }
        }

        if let Ok(dir) = std::env::var("PHOTOBROWSER_BENCH_DUMP") {
            std::fs::create_dir_all(&dir).ok();
            let dump = |label: &str, file: &str, img: &image::DynamicImage| {
                let out = format!("{dir}/{label}.png");
                img.save(&out).ok();
                println!(
                    "dumped {label} ({file}) {}x{} -> {out}",
                    img.width(),
                    img.height()
                );
            };
            if let Ok(nef) = std::env::var("PHOTOBROWSER_BENCH_NEF") {
                if let Ok(img) = load_image(Path::new(&nef)) {
                    dump("nef_loupe", "load_image", &img);
                }
                if let Ok(img) = load_thumbnail_image(Path::new(&nef)) {
                    dump("nef_thumb", "load_thumbnail_image", &img);
                }
            }
            if let Ok(jpeg) = std::env::var("PHOTOBROWSER_BENCH_JPEG") {
                if let Ok(img) = load_thumbnail_image(Path::new(&jpeg)) {
                    dump("jpeg_thumb", "load_thumbnail_image", &img);
                }
            }
        }
    }

    // ─────────────────────────────────────────────────────────────────────────
    // Real-image thumbnail decode integration coverage
    // These exercise the decision branches in load_thumbnail_image* (DCT reduced,
    // exif-embedded early return, RAW embedded) using generated inputs at test
    // time or env-gated local files. No production code was modified.
    // ─────────────────────────────────────────────────────────────────────────

    /// Case 1: JPEG WITHOUT embedded EXIF thumbnail exercises the DCT reduced-scale
    /// fallback (`load_thumbnail_image_reduced` → `load_jpeg_reduced` using jpeg_decoder
    /// scale). A largish generated JPEG (per the pattern in pre-existing tests) is used
    /// so the P2 reduced path is taken and we can assert the resulting dimensions are
    /// the expected DCT-scaled ones (bounded near the target, preserving aspect).
    /// This guards future changes to the DCT / P2 path.
    #[test]
    fn load_thumbnail_image_reduced_jpeg_no_embedded_thumb_uses_dct_scale() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("large-no-thumb-for-p2.jpg");
        // Use 1024x512 (matching the pre-existing reduced_thumbnail_jpeg_fallback pattern)
        // so that jpeg_decoder.scale produces exactly the 256x128 DCT output. This guards
        // the P2 reduced path with deterministic " ~256-bound" dimensions.
        let img = RgbImage::from_fn(1024, 512, |x, y| {
            image::Rgb([(x % 251) as u8, (y % 241) as u8, ((x + y) % 239) as u8])
        });
        img.save_with_format(&path, ImageFormat::Jpeg).unwrap();

        let reduced = load_thumbnail_image_reduced(&path, 256)
            .expect("reduced JPEG (no thumb) should decode via load_jpeg_reduced DCT path");
        // Expect the exact DCT-scaled size for this input (256 on long edge).
        assert_eq!(
            reduced.dimensions(),
            (256, 128),
            "DCT reduced must yield the expected scaled dims for 1024x512 @ target 256"
        );

        // The non-reduced thumbnail path yields the full main image (no thumb present).
        let via_full = load_thumbnail_image(&path).expect("load_thumbnail_image baseline");
        assert_eq!(via_full.dimensions(), (1024, 512));
    }

    /// Case 2: JPEG WITH an embedded EXIF thumbnail must take the embedded-thumb path
    /// in both load_thumbnail_image and load_thumbnail_image_reduced (i.e. return the
    /// thumb as-is, never falling through to full image::open or load_jpeg_reduced DCT).
    ///
    /// Because `image` crate write does not emit EXIF thumbnails, we synthesize at test
    /// time (self-generated, effectively CC0) a .jpg whose APP1/Exif segment contains a
    /// THUMBNAIL IFD with JPEGInterchangeFormat data (via exif::experimental::Writer +
    /// minimal JPEG SOI+APP1 assembly). The main image is largish; the embedded thumb
    /// uses a distinctive small size so dimension asserts prove which path was chosen.
    ///
    /// This guards the exif-embedded early return branch for real JPEGs.
    #[test]
    fn load_thumbnail_image_reduced_jpeg_with_exif_embedded_thumb_prefers_embedded() {
        use exif::{experimental::Writer, Field, In, Tag, Value};
        use std::io::Cursor;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("large-with-exif-thumb.jpg");

        // Distinctive thumb size (not a probable DCT scale output from the main image below).
        let thumb_w: u32 = 140;
        let thumb_h: u32 = 90;

        let thumb_img = DynamicImage::ImageRgb8(RgbImage::from_pixel(
            thumb_w,
            thumb_h,
            image::Rgb([30, 60, 90]),
        ));
        let mut thumb_jpeg = Vec::new();
        thumb_img
            .write_to(&mut Cursor::new(&mut thumb_jpeg), ImageFormat::Jpeg)
            .unwrap();

        // Construct the Exif payload with the thumbnail in the THUMBNAIL sub-IFD.
        let mut writer = Writer::new();
        let orient = Field {
            tag: Tag::Orientation,
            ifd_num: In::PRIMARY,
            value: Value::Short(vec![1]),
        };
        writer.push_field(&orient);
        writer.set_jpeg(&thumb_jpeg, In::THUMBNAIL);
        let mut exif_data = Vec::new();
        writer
            .write(&mut Cursor::new(&mut exif_data), false)
            .unwrap();

        // Large main image (this data is never decoded for the thumbnail paths).
        let main_img = RgbImage::from_fn(1024, 768, |x, y| {
            image::Rgb([(x % 180) as u8, (y % 180) as u8, 90])
        });
        let mut main_jpeg = Vec::new();
        DynamicImage::ImageRgb8(main_img)
            .write_to(&mut Cursor::new(&mut main_jpeg), ImageFormat::Jpeg)
            .unwrap();

        // Minimal assembly of a JPEG that carries the Exif APP1 + embedded thumb:
        // SOI + APP1 (marker + len + "Exif\0\0" + exif_data) + (main_jpeg without its SOI)
        let mut jpeg_file: Vec<u8> = vec![0xff, 0xd8];
        let app1_sig = b"Exif\0\0";
        let payload = app1_sig.len() + exif_data.len();
        let seg_len: u16 = (2 + payload) as u16;
        jpeg_file.extend_from_slice(&[0xff, 0xe1]);
        jpeg_file.extend_from_slice(&seg_len.to_be_bytes());
        jpeg_file.extend_from_slice(app1_sig);
        jpeg_file.extend_from_slice(&exif_data);
        if main_jpeg.len() >= 2 && main_jpeg[0] == 0xff && main_jpeg[1] == 0xd8 {
            jpeg_file.extend_from_slice(&main_jpeg[2..]);
        } else {
            jpeg_file.extend_from_slice(&main_jpeg);
        }
        std::fs::write(&path, &jpeg_file).unwrap();

        // The critical assert: embedded path returns the *thumb's* native size (as-is),
        // not a ~256-bound DCT reduction of the 1024x768 main image.
        let reduced = load_thumbnail_image_reduced(&path, 256)
            .expect("should take embedded-thumb path for JPEG that has EXIF thumb");
        assert_eq!(
            reduced.dimensions(),
            (thumb_w, thumb_h),
            "embedded-thumb path (not DCT) must have been used"
        );

        // Same preference via the plain (non-reduced) thumbnail entry point.
        let via_plain = load_thumbnail_image(&path)
            .expect("load_thumbnail_image must also prefer the embedded thumb");
        assert_eq!(via_plain.dimensions(), (thumb_w, thumb_h));
    }

    /// Case 3 (RAW/NEF): env-var-gated test for the NEF
    /// thumbnail decode paths (embedded preview / direct / rawler thumbnail, P3).
    ///
    /// This is `#[ignore]` so normal `cargo test` and CI are unaffected (per the
    /// pattern used by decode_micro_benchmark and the C2 thumb bench).
    ///
    /// The test reads PHOTOBROWSER_TEST_NEF. If the fixture is absent it prints a
    /// skip note and returns without failing.
    ///
    /// Real NEFs are large (~45 MiB) and not license-safe for committing; coverage
    /// for RAW thumbnail decode is therefore provided via local fixtures only.
    /// Run with:
    ///   PHOTOBROWSER_TEST_NEF=/path/to/fixture.nef \
    ///   cargo test --release decode::tests::load_thumbnail_raw_nef_env_gated -- --ignored --nocapture
    #[test]
    #[ignore = "env-gated; real NEF thumbnail decode. See body for PHOTOBROWSER_TEST_NEF. Local fixtures only, not committed."]
    fn load_thumbnail_raw_nef_env_gated() {
        let Ok(candidate) = std::env::var("PHOTOBROWSER_TEST_NEF") else {
            eprintln!("SKIP: set PHOTOBROWSER_TEST_NEF to a local NEF fixture path");
            return;
        };
        let path = Path::new(&candidate);
        if !path.exists() {
            eprintln!("SKIP: PHOTOBROWSER_TEST_NEF path does not exist");
            return;
        }

        // Exercise both the reduced (256 target, used by thumb worker) and plain paths.
        // For real NEFs with embedded previews this exercises load_raw_thumbnail_preview
        // (direct short-circuit or rawler thumbnail/preview) rather than full demosaic.
        let reduced = load_thumbnail_image_reduced(path, 256)
            .expect("NEF reduced thumbnail decode should succeed on local fixture");
        assert!(
            reduced.width() > 0 && reduced.height() > 0,
            "reduced NEF thumb must have size"
        );

        let plain = load_thumbnail_image(path)
            .expect("NEF thumbnail decode should succeed on local fixture");
        assert!(
            plain.width() > 0 && plain.height() > 0,
            "plain NEF thumb must have size"
        );

        println!(
            "NEF thumb (env-gated): reduced={}x{}  plain={}x{}  (path={})",
            reduced.width(),
            reduced.height(),
            plain.width(),
            plain.height(),
            candidate
        );
    }

    /// Case 4 (RAW/CR3): env-var-gated real Canon CR3 decode. CR3 is ISO-BMFF
    /// (box-based), exercised here through the public thumbnail entry points which
    /// route to the ISO-BMFF embedded-JPEG collector. `#[ignore]` like the NEF
    /// case; reads PHOTOBROWSER_TEST_CR3. An absent fixture prints a skip message
    /// and does not fail. Real CR3s are large and not license-safe to
    /// commit, so committed coverage is the synthetic-fixture test above.
    /// Run with:
    ///   cargo test --release decode::tests::load_thumbnail_raw_cr3_env_gated -- --ignored --nocapture
    #[test]
    #[ignore = "env-gated; real Canon CR3 decode via ISO-BMFF collector. See body for PHOTOBROWSER_TEST_CR3. Local fixtures only, not committed."]
    fn load_thumbnail_raw_cr3_env_gated() {
        let Ok(candidate) = std::env::var("PHOTOBROWSER_TEST_CR3") else {
            eprintln!("SKIP: set PHOTOBROWSER_TEST_CR3 to a local CR3 fixture path");
            return;
        };
        let path = Path::new(&candidate);
        if !path.exists() {
            eprintln!("SKIP: PHOTOBROWSER_TEST_CR3 path does not exist");
            return;
        }

        let reduced = load_thumbnail_image_reduced(path, 256)
            .expect("CR3 reduced thumbnail decode should succeed (embedded preview)");
        assert!(reduced.width() > 0 && reduced.height() > 0);

        let plain = load_thumbnail_image(path)
            .expect("CR3 thumbnail decode should succeed (embedded preview)");
        assert!(plain.width() > 0 && plain.height() > 0);

        println!(
            "CR3 thumb (env-gated): reduced={}x{}  plain={}x{}  (path={candidate})",
            reduced.width(),
            reduced.height(),
            plain.width(),
            plain.height(),
        );
    }
}
