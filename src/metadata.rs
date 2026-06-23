use std::path::Path;

/// Optional EXIF fields shown in the preview pane.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ExifSummary {
    pub captured_date: Option<String>,
    pub camera: Option<String>,
    pub lens: Option<String>,
    pub iso: Option<String>,
    pub shutter: Option<String>,
    pub aperture: Option<String>,
    pub focal_length: Option<String>,
    pub color_profile: Option<String>,
    pub orientation: Option<String>,
}

/// Best-effort EXIF reader. Missing tags, unsupported formats, and read errors
/// all produce an empty/partial summary rather than failing preview rendering.
pub fn read_exif(path: &Path) -> ExifSummary {
    read_exif_impl(path)
}

#[cfg(feature = "exif")]
fn extract_iso_bmff_box_body<'a>(bytes: &'a [u8], name: &[u8; 4]) -> Option<&'a [u8]> {
    let pos = bytes.windows(4).position(|w| w == name)?;
    let size_at = pos.checked_sub(4)?;
    let size = u32::from_be_bytes(bytes.get(size_at..pos)?.try_into().ok()?) as usize;
    let body_start = pos.checked_add(4)?;
    let box_end = size_at.checked_add(size)?;
    if box_end <= body_start || box_end > bytes.len() {
        return None;
    }
    bytes.get(body_start..box_end)
}

#[cfg(feature = "exif")]
fn read_exif_from_cr3_cmt_boxes(path: &Path) -> ExifSummary {
    use std::io::Cursor;

    use exif::{In, Reader, Tag};

    let bytes = match std::fs::read(path) {
        Ok(b) => b,
        Err(_) => return ExifSummary::default(),
    };
    if bytes.get(4..8) != Some(b"ftyp") {
        return ExifSummary::default();
    }

    let read_tiff_box = |name: &[u8; 4]| -> Option<exif::Exif> {
        let body = extract_iso_bmff_box_body(&bytes, name)?;
        if body.len() < 2 {
            return None;
        }
        if !body.starts_with(b"II") && !body.starts_with(b"MM") {
            return None;
        }
        Reader::new()
            .read_from_container(&mut Cursor::new(body))
            .ok()
    };

    let cmt1 = read_tiff_box(b"CMT1");
    let cmt2 = read_tiff_box(b"CMT2");

    let field = |exif: &Option<exif::Exif>, tag| -> Option<String> {
        exif.as_ref().and_then(|e| {
            e.get_field(tag, In::PRIMARY)
                // ASCII fields in the bare-TIFF CMT boxes render quoted (e.g. "Canon");
                // strip the surrounding quotes so the camera label is clean.
                .map(|f| {
                    f.display_value()
                        .with_unit(e)
                        .to_string()
                        .trim_matches('"')
                        .to_string()
                })
                .filter(|v| !v.trim().is_empty())
        })
    };

    let make = field(&cmt1, Tag::Make);
    let model = field(&cmt1, Tag::Model);
    let camera = match (make, model) {
        (Some(make), Some(model)) if model.contains(&make) => Some(model),
        (Some(make), Some(model)) => Some(format!("{make} {model}")),
        (Some(make), None) => Some(make),
        (None, Some(model)) => Some(model),
        (None, None) => None,
    };

    let captured_date = field(&cmt1, Tag::DateTime).or_else(|| field(&cmt2, Tag::DateTimeOriginal));

    ExifSummary {
        captured_date,
        camera,
        lens: None,
        iso: None,
        shutter: None,
        aperture: None,
        focal_length: None,
        color_profile: None,
        orientation: field(&cmt1, Tag::Orientation),
    }
}

#[cfg(feature = "exif")]
fn read_exif_impl(path: &Path) -> ExifSummary {
    use std::fs::File;
    use std::io::BufReader;

    use exif::{In, Reader, Tag};

    let file = match File::open(path) {
        Ok(file) => file,
        Err(_) => return ExifSummary::default(),
    };
    let exif = match Reader::new().read_from_container(&mut BufReader::new(file)) {
        Ok(exif) => exif,
        Err(_) => return read_exif_from_cr3_cmt_boxes(path),
    };

    let field = |tag| {
        exif.get_field(tag, In::PRIMARY)
            .map(|field| field.display_value().with_unit(&exif).to_string())
            .filter(|value| !value.trim().is_empty())
    };

    let make = field(Tag::Make);
    let model = field(Tag::Model);
    let camera = match (make, model) {
        (Some(make), Some(model)) if model.contains(&make) => Some(model),
        (Some(make), Some(model)) => Some(format!("{make} {model}")),
        (Some(make), None) => Some(make),
        (None, Some(model)) => Some(model),
        (None, None) => None,
    };

    ExifSummary {
        captured_date: field(Tag::DateTimeOriginal),
        camera,
        lens: field(Tag::LensModel).or_else(|| field(Tag::LensMake)),
        iso: field(Tag::PhotographicSensitivity),
        shutter: field(Tag::ExposureTime),
        aperture: field(Tag::FNumber),
        focal_length: field(Tag::FocalLength),
        color_profile: field(Tag::ColorSpace),
        orientation: field(Tag::Orientation),
    }
}

#[cfg(not(feature = "exif"))]
fn read_exif_impl(_path: &Path) -> ExifSummary {
    ExifSummary::default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_exif_missing_file_returns_empty_summary() {
        let summary = read_exif(Path::new("/definitely/missing/photobrowser-photo.jpg"));

        assert_eq!(summary, ExifSummary::default());
    }

    #[test]
    #[cfg(feature = "exif")]
    fn read_exif_reads_generated_jpeg_tag() {
        // Self-generated (in-memory at test time) JPEG + minimal valid EXIF APP1
        // with DateTimeOriginal in PRIMARY. This exercises the success path for
        // read_exif without committing any binary fixture (per tests/fixtures/README
        // rules: <30 KiB, license-safe, generated). Uses same APP1 assembly pattern
        // as the embedded-thumb tests in decode.rs.
        use exif::{experimental::Writer, Field, In, Tag, Value};
        use image::{DynamicImage, ImageFormat, RgbImage};
        use std::io::Cursor;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tiny-with-exif-date.jpg");

        // Tiny main image (its pixels are never decoded by read_exif; only container + APP1)
        let main_img = RgbImage::from_pixel(8, 8, image::Rgb([128u8, 128, 128]));
        let mut main_jpeg = Vec::new();
        DynamicImage::ImageRgb8(main_img)
            .write_to(&mut Cursor::new(&mut main_jpeg), ImageFormat::Jpeg)
            .unwrap();

        // EXIF payload with DateTimeOriginal (the tag asserted on)
        let mut writer = Writer::new();
        let dt_field = Field {
            tag: Tag::DateTimeOriginal,
            ifd_num: In::PRIMARY,
            value: Value::Ascii(vec![b"2025:01:15 10:30:00".to_vec()]),
        };
        writer.push_field(&dt_field);
        let mut exif_data = Vec::new();
        writer
            .write(&mut Cursor::new(&mut exif_data), false)
            .unwrap();

        // SOI + APP1 (Exif\0\0 + exif_data) + main image bytes (strip SOI if present)
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

        let summary = read_exif(&path);
        assert!(summary.captured_date.is_some());
    }

    #[test]
    #[ignore = "env-gated; real Canon CR3 EXIF via CMT1 boxes; local samples only"]
    #[cfg(feature = "exif")]
    fn cr3_exif_fallback_reads_camera_and_capture_env_gated() {
        let Ok(candidate) = std::env::var("PHOTOBROWSER_TEST_CR3") else {
            eprintln!("SKIP: set PHOTOBROWSER_TEST_CR3 to a local CR3 fixture path");
            return;
        };
        let path = Path::new(&candidate);
        if !path.exists() {
            eprintln!("SKIP: PHOTOBROWSER_TEST_CR3 path does not exist");
            return;
        }
        let s = read_exif(path);
        assert!(
            s.camera.is_some(),
            "camera should be Some, got {:?}",
            s.camera
        );
        assert!(
            s.captured_date.is_some(),
            "captured_date should be Some, got {:?}",
            s.captured_date
        );
        println!(
            "CR3 EXIF: camera={:?} captured={:?}",
            s.camera, s.captured_date
        );
    }
}
