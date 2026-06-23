use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use cosmic::Task;

use crate::app::Message;
use crate::export::export_paths;
use crate::histogram::HISTOGRAM_BINS;
use crate::inspection::{DecodedImage, LoupeDecodeMode};
use crate::metadata::read_exif;

type PreviewDecodeResult = (
    PathBuf,
    DecodedImage,
    Option<[u32; HISTOGRAM_BINS]>,
    Option<String>,
);
type LoupeDecodeResult = (PathBuf, LoupeDecodeMode, DecodedImage, Option<String>);

pub fn decode_preview_task(path: PathBuf) -> Task<cosmic::Action<Message>> {
    Task::perform(
        async move { decode_preview_image(path) },
        |(path, decoded, hist, err)| {
            cosmic::Action::App(Message::PreviewDecoded(path, decoded, hist, err))
        },
    )
}

pub fn decode_loupe_task(
    path: PathBuf,
    bound: u32,
    mode: LoupeDecodeMode,
    develop_look: bool,
) -> Task<cosmic::Action<Message>> {
    Task::perform(
        async move { decode_loupe_image(path, bound, mode, develop_look) },
        |(path, mode, decoded, err)| {
            cosmic::Action::App(Message::LoupeDecoded(path, mode, decoded, err))
        },
    )
}

pub fn decode_compare_task(path: PathBuf, bound: u32) -> Task<cosmic::Action<Message>> {
    Task::perform(
        async move { decode_compare_image(path, bound) },
        |(path, decoded, err)| cosmic::Action::App(Message::CompareDecoded(path, decoded, err)),
    )
}

pub fn metadata_task(path: PathBuf) -> Task<cosmic::Action<Message>> {
    Task::perform(
        async move {
            let summary = read_exif(&path);
            (path, summary)
        },
        |(path, summary)| cosmic::Action::App(Message::MetadataLoaded(path, summary)),
    )
}

fn decode_preview_image(path: PathBuf) -> PreviewDecodeResult {
    match crate::decode::load_image(&path) {
        Ok(img) => {
            let orig_w = img.width();
            let orig_h = img.height();
            let thumb = img.thumbnail(800, 800).into_rgba8();
            let hist = crate::histogram::luminance_histogram(thumb.as_raw());
            let dto =
                crate::decoded_image::DecodedRgbaImage::from_rgba_image(thumb, orig_w, orig_h);
            let triple = crate::cosmic_adapter::to_handle_triple(dto);
            (path, Some(triple), Some(hist), None)
        }
        Err(err) => (path, None, None, Some(err.to_string())),
    }
}

/// Display-bounded decode for the loupe. Mirrors `decode_preview_image` but caps
/// the longest edge to `bound` (the display-derived clamp from
/// `view::loupe::loupe_decode_bound` for normal path, or 8192 for the distinct
/// HighRes* full-res Actual path when full_res_loupe enabled).
/// HighRes* modes use the same load choice as FullRaw/embedded based on demosaic
/// setting at request time (via the *HighResFullRaw vs HighRes variant).
fn decode_loupe_image(
    path: PathBuf,
    bound: u32,
    mode: LoupeDecodeMode,
    develop_look: bool,
) -> LoupeDecodeResult {
    let raw_path = path
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(crate::decode::is_raw_extension);
    // HighRes* variants are the separate (≤8192) path for native Actual; they still
    // respect the demosaic toggle for RAWs (HighResFullRaw means "use full demosaic load").
    let use_full_demosaic = matches!(
        mode,
        LoupeDecodeMode::FullRaw | LoupeDecodeMode::HighResFullRaw
    );
    let decoded = if use_full_demosaic && raw_path {
        crate::decode::load_image_full_demosaic(&path)
    } else {
        crate::decode::load_image(&path)
    };

    match decoded {
        Ok(img) => {
            let orig_w = img.width();
            let orig_h = img.height();
            let scaled = img.thumbnail(bound, bound);
            let scaled = if develop_look {
                let params = crate::xmp::read_sidecar_xmp(&path)
                    .map(|x| x.develop)
                    .unwrap_or_default();
                image::DynamicImage::ImageRgb8(crate::develop::apply_develop(
                    &scaled.to_rgb8(),
                    &params,
                ))
            } else {
                scaled
            };
            let scaled = scaled.into_rgba8();
            let dto =
                crate::decoded_image::DecodedRgbaImage::from_rgba_image(scaled, orig_w, orig_h);
            let triple = crate::cosmic_adapter::to_handle_triple(dto);
            (path, mode, Some(triple), None)
        }
        Err(err) => (path, mode, None, Some(err.to_string())),
    }
}

fn decode_compare_image(
    path: PathBuf,
    bound: u32,
) -> (
    PathBuf,
    Option<(cosmic::widget::image::Handle, u32, u32)>,
    Option<String>,
) {
    // Mirror the bounded loupe Fit path exactly: load_image (which handles RAW previews etc) + thumbnail(bound).
    match crate::decode::load_image(&path) {
        Ok(img) => {
            let orig_w = img.width();
            let orig_h = img.height();
            let scaled = img.thumbnail(bound, bound).into_rgba8();
            let dto =
                crate::decoded_image::DecodedRgbaImage::from_rgba_image(scaled, orig_w, orig_h);
            let triple = crate::cosmic_adapter::to_handle_triple(dto);
            (path, Some(triple), None)
        }
        Err(err) => (path, None, Some(err.to_string())),
    }
}

/// Extract mtime (secs since UNIX_EPOCH as i64) and size (bytes as i64) from metadata.
/// Returns (None, 0) if metadata cannot be read.
fn file_mtime_size(path: &std::path::Path) -> (Option<i64>, i64) {
    match std::fs::metadata(path) {
        Ok(meta) => {
            let mtime = meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                .map(|d| d.as_secs() as i64);
            let size = meta.len() as i64;
            (mtime, size)
        }
        Err(_) => (None, 0),
    }
}

fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Background hash pass for "Find Duplicates". Loads embedded-preview thumbnails
/// (fast path) for the provided (entry_index, path) pairs and computes dHash.
/// If a `db_path` is provided, attempts to reuse a cached dHash from the catalog
/// (mtime-validated) and on miss will persist the computed dHash.
/// Catalog open/write failures degrade gracefully: the scan continues with decode.
/// Returns only successful hashes; I/O or decode errors are silently dropped for
/// that entry.
pub(crate) fn hash_entries(
    pairs: Vec<(usize, std::path::PathBuf)>,
    db_path: Option<std::path::PathBuf>,
) -> Vec<(usize, u64)> {
    let cat = db_path
        .as_deref()
        .and_then(|p| crate::catalog::Catalog::open(p).ok());

    pairs
        .into_iter()
        .filter_map(|(i, p)| {
            let path_str = match p.to_str() {
                Some(s) => s.to_owned(),
                None => {
                    // Non-UTF8 path: decode-only for this entry
                    let img = crate::decode::load_thumbnail_image(&p).ok()?;
                    let rgba = img.to_rgba8();
                    let (w, h) = (rgba.width(), rgba.height());
                    return crate::dedupe::dhash_rgba(&rgba.into_raw(), w, h).map(|hsh| (i, hsh));
                }
            };

            let (mtime, size_bytes) = file_mtime_size(&p);

            // Cache hit?
            if let Some(ref c) = cat {
                if let Some(mt) = mtime {
                    if let Ok(Some(h)) = c.get_dhash(&path_str, mt) {
                        return Some((i, h));
                    }
                }
            }

            // Miss: decode and (optionally) store
            let img = crate::decode::load_thumbnail_image(&p).ok()?;
            let rgba = img.to_rgba8();
            let (w, h) = (rgba.width(), rgba.height());
            let hsh = crate::dedupe::dhash_rgba(&rgba.into_raw(), w, h)?;

            if let Some(ref c) = cat {
                if let Some(mt) = mtime {
                    let exif = crate::metadata::read_exif(&p);
                    let cap = exif
                        .captured_date
                        .as_deref()
                        .and_then(crate::app::parse_exif_datetime);
                    let camera = exif.camera.clone();
                    let _ = c.upsert_file(
                        &path_str,
                        size_bytes,
                        mt,
                        cap,
                        camera.as_deref(),
                        now_unix(),
                    );
                    let _ = c.set_dhash(&path_str, hsh);
                }
            }

            Some((i, hsh))
        })
        .collect()
}

/// Background metadata index for a folder. For each (entry_index, path): reuse catalog metadata when the
/// file's mtime is unchanged; otherwise read EXIF (capture date + camera), upsert into the catalog, and
/// return it. Cheap: EXIF header read only, NO image decode. Catalog failures degrade gracefully (still
/// returns metadata read from EXIF). Returns (entry_index, exif_captured_unix, camera) for every entry.
pub(crate) fn index_folder_metadata(
    pairs: Vec<(usize, std::path::PathBuf)>,
    db_path: Option<std::path::PathBuf>,
) -> Vec<(usize, Option<i64>, Option<String>)> {
    let cat = db_path
        .as_deref()
        .and_then(|p| crate::catalog::Catalog::open(p).ok());

    pairs
        .into_iter()
        .map(|(i, p)| {
            let path_str = match p.to_str() {
                Some(s) => s.to_owned(),
                None => {
                    // Non-UTF8 path: EXIF read only, no catalog involvement
                    let exif = crate::metadata::read_exif(&p);
                    let cap = exif
                        .captured_date
                        .as_deref()
                        .and_then(crate::app::parse_exif_datetime);
                    let camera = exif.camera.clone();
                    return (i, cap, camera);
                }
            };

            let (mtime, size) = file_mtime_size(&p);

            // Fresh cache hit?
            if let Some(ref c) = cat {
                if let Some(mt) = mtime {
                    if let Ok(Some(m)) = c.get_file_meta_fresh(&path_str, mt) {
                        return (i, m.0, m.1);
                    }
                }
            }

            // Miss or no catalog: read EXIF (header only)
            let exif = crate::metadata::read_exif(&p);
            let cap = exif
                .captured_date
                .as_deref()
                .and_then(crate::app::parse_exif_datetime);
            let camera = exif.camera.clone();

            // Persist if possible (degrade gracefully on catalog write failure)
            if let Some(ref c) = cat {
                if let Some(mt) = mtime {
                    let _ = c.upsert_file(&path_str, size, mt, cap, camera.as_deref(), now_unix());
                }
            }

            (i, cap, camera)
        })
        .collect()
}

/// Background pass: read each image's XMP sidecar keywords (dc:subject) for the folder.
/// Read-only (sidecar reads only; no catalog, no writes). Returns (entry_index, keywords) for entries
/// that have at least one keyword (entries with none are omitted to keep the map small).
pub(crate) fn index_folder_keywords(
    pairs: Vec<(usize, std::path::PathBuf)>,
) -> Vec<(usize, Vec<String>)> {
    pairs
        .into_iter()
        .filter_map(|(i, p)| {
            let kws = crate::xmp::read_sidecar_keywords(&p);
            if kws.is_empty() {
                None
            } else {
                Some((i, kws))
            }
        })
        .collect()
}

/// SHA-256 (lowercase hex) of a file's raw bytes, streamed. None on I/O error.
/// Read-only: opens the file for reading only; never writes the original.
fn sha256_file(path: &std::path::Path) -> Option<String> {
    use sha2::{Digest, Sha256};
    use std::fmt::Write as _;
    let mut file = std::fs::File::open(path).ok()?;
    let mut hasher = Sha256::new();
    std::io::copy(&mut file, &mut hasher).ok()?;
    let digest = hasher.finalize();
    let mut hex = String::with_capacity(64);
    for b in digest {
        let _ = write!(hex, "{b:02x}");
    }
    Some(hex)
}

pub fn export_task(sources: Vec<PathBuf>, dest_dir: PathBuf) -> Task<cosmic::Action<Message>> {
    Task::perform(
        async move {
            let res = export_paths(&sources, &dest_dir);
            (res, dest_dir)
        },
        |(res, dest)| cosmic::Action::App(Message::ExportDone(res, dest)),
    )
}

/// Background SHA-256 pass for "Find Exact Duplicates". Stats all candidates, then hashes ONLY files
/// whose byte-size collides with another candidate (exact dups must be equal size), reusing/persisting
/// the catalog sha256 (mtime-validated) like `hash_entries` does for dHash. Read-only on originals;
/// the only write is the catalog .db. Returns (entry_index, sha256-hex) for hashed files.
pub(crate) fn hash_entries_sha256(
    pairs: Vec<(usize, std::path::PathBuf)>,
    db_path: Option<std::path::PathBuf>,
) -> Vec<(usize, String)> {
    use std::collections::HashMap;
    let cat = db_path
        .as_deref()
        .and_then(|p| crate::catalog::Catalog::open(p).ok());

    // Stat once.
    let metas: Vec<(usize, std::path::PathBuf, Option<i64>, i64)> = pairs
        .into_iter()
        .map(|(i, p)| {
            let (mtime, size) = file_mtime_size(&p);
            (i, p, mtime, size)
        })
        .collect();

    // Count sizes; only sizes shared by >=2 files (and size > 0) can be exact dups.
    let mut size_counts: HashMap<i64, usize> = HashMap::new();
    for (_, _, _, size) in &metas {
        *size_counts.entry(*size).or_insert(0) += 1;
    }

    metas
        .into_iter()
        .filter(|(_, _, _, size)| *size > 0 && size_counts.get(size).copied().unwrap_or(0) >= 2)
        .filter_map(|(i, p, mtime, size)| {
            let path_str = match p.to_str() {
                Some(s) => s.to_owned(),
                None => {
                    // Non-UTF8 path: hash directly, no catalog.
                    return sha256_file(&p).map(|s| (i, s));
                }
            };

            // Cache hit?
            if let Some(ref c) = cat {
                if let Some(mt) = mtime {
                    if let Ok(Some(s)) = c.get_sha256(&path_str, mt) {
                        return Some((i, s));
                    }
                }
            }

            // Miss: hash and (optionally) store. Read EXIF for cap/camera so upsert does not
            // clobber previously-indexed metadata to NULL (mirrors hash_entries).
            let s = sha256_file(&p)?;
            if let Some(ref c) = cat {
                if let Some(mt) = mtime {
                    let exif = crate::metadata::read_exif(&p);
                    let cap = exif
                        .captured_date
                        .as_deref()
                        .and_then(crate::app::parse_exif_datetime);
                    let camera = exif.camera.clone();
                    let _ = c.upsert_file(&path_str, size, mt, cap, camera.as_deref(), now_unix());
                    let _ = c.set_sha256(&path_str, &s);
                }
            }
            Some((i, s))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::file_mtime_size;
    use std::fs::File;
    use std::io::Write;
    use tempfile::tempdir;

    #[test]
    fn file_mtime_size_returns_values_for_real_file() {
        let td = tempdir().unwrap();
        let fpath = td.path().join("t.bin");
        {
            let mut f = File::create(&fpath).unwrap();
            f.write_all(&[0u8; 123]).unwrap();
        }
        let (mtime, size) = file_mtime_size(&fpath);
        assert!(mtime.is_some());
        assert_eq!(size, 123);
    }

    #[test]
    fn file_mtime_size_missing_file_is_none_zero() {
        let td = tempdir().unwrap();
        let missing = td.path().join("nope");
        let (mtime, size) = file_mtime_size(&missing);
        assert!(mtime.is_none());
        assert_eq!(size, 0);
    }

    #[test]
    fn sha256_file_known_values() {
        let td = tempdir().unwrap();
        // Non-empty file with known SHA-256 of "abc"
        let fpath = td.path().join("abc.bin");
        {
            let mut f = File::create(&fpath).unwrap();
            f.write_all(b"abc").unwrap();
        }
        let h = super::sha256_file(&fpath).expect("sha256_file should succeed");
        assert_eq!(
            h,
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn index_folder_keywords_reads_sidecar() {
        use tempfile::tempdir;
        let dir = tempdir().unwrap();
        let img0 = dir.path().join("A.NEF");
        let img1 = dir.path().join("B.NEF");
        let img2 = dir.path().join("C.NEF");
        // img0 has keywords
        crate::xmp::write_sidecar_keywords(&img0, &["beach".into(), "sunset".into()]).unwrap();
        // img1 has none (no sidecar)
        // img2 has one
        crate::xmp::write_sidecar_keywords(&img2, &["mountain".into()]).unwrap();

        let pairs: Vec<(usize, std::path::PathBuf)> =
            vec![(0, img0.clone()), (1, img1.clone()), (2, img2.clone())];
        let items = super::index_folder_keywords(pairs);
        // entry 1 omitted (no keywords)
        assert_eq!(items.len(), 2);
        // order not guaranteed by flat_map/dedup; check membership
        let map: std::collections::HashMap<_, _> = items.into_iter().collect();
        assert_eq!(
            map.get(&0).unwrap(),
            &vec!["beach".to_string(), "sunset".to_string()]
        );
        assert_eq!(map.get(&2).unwrap(), &vec!["mountain".to_string()]);
        assert!(!map.contains_key(&1));
    }

    #[test]
    fn decode_loupe_image_with_develop_look_and_no_sidecar_returns_handle() {
        // Minimal wiring test: develop_look=true + no sidecar must not panic and must
        // return a decoded handle (apply_develop is a no-op when no params).
        use image::{DynamicImage, ImageFormat, RgbImage};
        use std::io::Cursor;
        use tempfile::tempdir;

        let dir = tempdir().unwrap();
        let path = dir.path().join("tiny.png");

        let tiny = RgbImage::from_pixel(16, 16, image::Rgb([64u8, 128, 192]));
        let mut png_bytes = Vec::new();
        DynamicImage::ImageRgb8(tiny)
            .write_to(&mut Cursor::new(&mut png_bytes), ImageFormat::Png)
            .unwrap();
        std::fs::write(&path, &png_bytes).unwrap();

        let (_p, mode, decoded, err) = super::decode_loupe_image(
            path,
            800,
            crate::inspection::LoupeDecodeMode::EmbeddedPreview,
            true,
        );
        assert!(err.is_none(), "no error expected for valid tiny png");
        assert!(decoded.is_some(), "expected a decoded handle");
        // mode should round-trip
        assert_eq!(mode, crate::inspection::LoupeDecodeMode::EmbeddedPreview);
    }
}
