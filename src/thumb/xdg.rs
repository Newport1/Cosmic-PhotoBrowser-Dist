//! Freedesktop-shaped persistent thumbnail disk cache.
//!
//! Cache keys use the freedesktop thumbnail spec's MD5 of a canonical file URI.
//! We canonicalize existing paths with `std::fs::canonicalize`; if that fails,
//! relative paths are joined to the current directory before URI encoding.

use std::collections::HashMap;
use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::UNIX_EPOCH;

use image::{DynamicImage, ImageFormat};

const PNG_SIGNATURE: &[u8; 8] = b"\x89PNG\r\n\x1a\n";
const CACHE_VERSION_KEY: &str = "PhotoBrowser::CacheVersion";
const CACHE_VERSION: &str = "v3-raw-embedded-jpeg";
static CACHE_DIR_OVERRIDE: OnceLock<Mutex<Option<PathBuf>>> = OnceLock::new();

/// Set a process-global thumbnail cache directory override from persisted config.
pub fn set_cache_dir_override(path: Option<PathBuf>) {
    let lock = CACHE_DIR_OVERRIDE.get_or_init(|| Mutex::new(None));
    if let Ok(mut override_path) = lock.lock() {
        *override_path = path;
    }
}

/// Root of the thumbnail cache. Honors configured override first, then
/// $PHOTOBROWSER_THUMBNAIL_CACHE_DIR, then $XDG_CACHE_HOME/thumbnails, then
/// $HOME/.cache/thumbnails.
pub fn cache_root() -> Option<PathBuf> {
    if let Some(path) = configured_cache_dir_override() {
        return Some(path);
    }
    if let Some(path) = non_empty_env_path("PHOTOBROWSER_THUMBNAIL_CACHE_DIR") {
        return Some(path);
    }
    if let Some(path) = non_empty_env_path("XDG_CACHE_HOME") {
        return Some(path.join("thumbnails"));
    }
    non_empty_env_path("HOME").map(|home| home.join(".cache").join("thumbnails"))
}

fn configured_cache_dir_override() -> Option<PathBuf> {
    CACHE_DIR_OVERRIDE
        .get()
        .and_then(|lock| lock.lock().ok().and_then(|path| path.clone()))
}

/// Convert GiB entered by the user to bytes. 0.0 means unlimited.
pub fn gb_to_bytes(gb: f64) -> u64 {
    if gb <= 0.0 || !gb.is_finite() {
        return 0;
    }
    (gb * 1024.0 * 1024.0 * 1024.0).round() as u64
}

/// Sum file sizes under `root`, best-effort.
pub fn dir_size_bytes(root: &Path) -> u64 {
    let mut total = 0u64;
    visit_files(root, &mut |path| {
        if let Ok(metadata) = std::fs::metadata(path) {
            if metadata.is_file() {
                total = total.saturating_add(metadata.len());
            }
        }
    });
    total
}

/// Prune the thumbnail cache oldest-first until it is under `max_bytes`.
/// `max_bytes == 0` is unlimited/no-op.
pub fn prune_to_max(max_bytes: u64) {
    if max_bytes == 0 {
        return;
    }
    let Some(root) = cache_root() else {
        return;
    };
    let mut files = Vec::new();
    visit_files(&root, &mut |path| {
        if let Ok(metadata) = std::fs::metadata(path) {
            if metadata.is_file() {
                let modified = metadata.modified().unwrap_or(UNIX_EPOCH);
                files.push((modified, path.to_path_buf(), metadata.len()));
            }
        }
    });

    let mut total = files
        .iter()
        .fold(0u64, |sum, (_, _, len)| sum.saturating_add(*len));
    if total <= max_bytes {
        return;
    }
    files.sort_by_key(|(modified, _, _)| *modified);
    for (_, path, len) in files {
        if total <= max_bytes {
            break;
        }
        if std::fs::remove_file(&path).is_ok() {
            total = total.saturating_sub(len);
        }
    }
}

fn visit_files(root: &Path, f: &mut impl FnMut(&Path)) {
    let Ok(read_dir) = std::fs::read_dir(root) else {
        return;
    };
    for entry in read_dir.flatten() {
        let path = entry.path();
        if let Ok(file_type) = entry.file_type() {
            if file_type.is_dir() {
                visit_files(&path, f);
            } else if file_type.is_file() {
                f(&path);
            }
        }
    }
}

/// Look up a valid cached thumbnail for `path`.
pub fn load(path: &Path) -> Option<DynamicImage> {
    let uri = file_uri(path)?;
    let source_mtime = mtime_seconds(path)?;
    let cache_path = cache_path_for_uri(&uri)?;
    if !cache_path.is_file() {
        return None;
    }

    let metadata = read_text_chunks(&cache_path)?;
    if metadata.get("Thumb::URI")? != &uri {
        return None;
    }
    if metadata.get("Thumb::MTime")? != &source_mtime.to_string() {
        return None;
    }
    if metadata.get(CACHE_VERSION_KEY)? != CACHE_VERSION {
        return None;
    }

    image::open(cache_path).ok()
}

/// Best-effort store of `thumb` as the cached thumbnail for `path`.
pub fn store(path: &Path, thumb: &DynamicImage) {
    let _ = store_inner(path, thumb);
}

fn store_inner(path: &Path, thumb: &DynamicImage) -> Option<()> {
    let uri = file_uri(path)?;
    let source_mtime = mtime_seconds(path)?;
    let cache_path = cache_path_for_uri(&uri)?;
    let cache_dir = cache_path.parent()?;
    std::fs::create_dir_all(cache_dir).ok()?;

    let mut encoded = Vec::new();
    thumb
        .write_to(&mut Cursor::new(&mut encoded), ImageFormat::Png)
        .ok()?;
    let encoded = with_text_chunks(
        &encoded,
        &[
            ("Thumb::URI", uri.as_str()),
            ("Thumb::MTime", &source_mtime.to_string()),
            (CACHE_VERSION_KEY, CACHE_VERSION),
        ],
    )?;

    let tmp = cache_path.with_extension(format!(
        "png.tmp-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    std::fs::write(&tmp, encoded).ok()?;
    if std::fs::rename(&tmp, &cache_path).is_err() {
        let _ = std::fs::remove_file(&tmp);
        return None;
    }
    Some(())
}

fn non_empty_env_path(key: &str) -> Option<PathBuf> {
    std::env::var_os(key)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

fn cache_path_for_uri(uri: &str) -> Option<PathBuf> {
    let digest = md5::compute(uri.as_bytes());
    Some(cache_root()?.join("large").join(format!("{digest:x}.png")))
}

fn mtime_seconds(path: &Path) -> Option<u64> {
    std::fs::metadata(path)
        .ok()?
        .modified()
        .ok()?
        .duration_since(UNIX_EPOCH)
        .ok()
        .map(|duration| duration.as_secs())
}

fn file_uri(path: &Path) -> Option<String> {
    let absolute = match std::fs::canonicalize(path) {
        Ok(path) => path,
        Err(_) if path.is_absolute() => path.to_path_buf(),
        Err(_) => std::env::current_dir().ok()?.join(path),
    };
    Some(format!("file://{}", percent_encode_path(&absolute)))
}

#[cfg(unix)]
fn path_bytes(path: &Path) -> Vec<u8> {
    use std::os::unix::ffi::OsStrExt;
    path.as_os_str().as_bytes().to_vec()
}

#[cfg(not(unix))]
fn path_bytes(path: &Path) -> Vec<u8> {
    path.to_string_lossy().as_bytes().to_vec()
}

fn percent_encode_path(path: &Path) -> String {
    path_bytes(path)
        .into_iter()
        .flat_map(|byte| match byte {
            b'A'..=b'Z'
            | b'a'..=b'z'
            | b'0'..=b'9'
            | b'-'
            | b'_'
            | b'.'
            | b'!'
            | b'~'
            | b'*'
            | b'\''
            | b'('
            | b')'
            | b'/' => vec![byte as char],
            _ => format!("%{byte:02X}").chars().collect(),
        })
        .collect()
}

fn read_text_chunks(path: &Path) -> Option<HashMap<String, String>> {
    let bytes = std::fs::read(path).ok()?;
    if bytes.get(..8)? != PNG_SIGNATURE {
        return None;
    }

    let mut chunks = HashMap::new();
    let mut index = 8usize;
    while index.checked_add(12)? <= bytes.len() {
        let length = u32::from_be_bytes(bytes.get(index..index + 4)?.try_into().ok()?) as usize;
        let chunk_type = bytes.get(index + 4..index + 8)?;
        let data_start = index + 8;
        let data_end = data_start.checked_add(length)?;
        let crc_end = data_end.checked_add(4)?;
        let data = bytes.get(data_start..data_end)?;

        if chunk_type == b"tEXt" {
            if let Some(nul) = data.iter().position(|byte| *byte == 0) {
                let key = String::from_utf8_lossy(&data[..nul]).into_owned();
                let value = String::from_utf8_lossy(&data[nul + 1..]).into_owned();
                chunks.insert(key, value);
            }
        } else if chunk_type == b"IEND" {
            break;
        }

        index = crc_end;
    }
    Some(chunks)
}

fn with_text_chunks(png: &[u8], text_chunks: &[(&str, &str)]) -> Option<Vec<u8>> {
    if png.get(..8)? != PNG_SIGNATURE {
        return None;
    }

    let ihdr_len = u32::from_be_bytes(png.get(8..12)?.try_into().ok()?) as usize;
    let after_ihdr = 8usize.checked_add(12)?.checked_add(ihdr_len)?;
    if png.get(12..16)? != b"IHDR" || after_ihdr > png.len() {
        return None;
    }

    let mut out = Vec::with_capacity(png.len() + 128);
    out.extend_from_slice(&png[..after_ihdr]);
    for (keyword, text) in text_chunks {
        write_text_chunk(&mut out, keyword, text)?;
    }
    out.extend_from_slice(&png[after_ihdr..]);
    Some(out)
}

fn write_text_chunk(out: &mut Vec<u8>, keyword: &str, text: &str) -> Option<()> {
    if keyword.is_empty() || keyword.as_bytes().contains(&0) {
        return None;
    }
    let length = keyword.len().checked_add(1)?.checked_add(text.len())?;
    let length = u32::try_from(length).ok()?;

    let mut data = Vec::with_capacity(length as usize);
    data.extend_from_slice(keyword.as_bytes());
    data.push(0);
    data.extend_from_slice(text.as_bytes());

    out.extend_from_slice(&length.to_be_bytes());
    out.extend_from_slice(b"tEXt");
    out.extend_from_slice(&data);

    let mut crc_data = Vec::with_capacity(4 + data.len());
    crc_data.extend_from_slice(b"tEXt");
    crc_data.extend_from_slice(&data);
    out.extend_from_slice(&crc32(&crc_data).to_be_bytes());
    Some(())
}

fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for &byte in bytes {
        crc ^= u32::from(byte);
        for _ in 0..8 {
            let mask = 0u32.wrapping_sub(crc & 1);
            crc = (crc >> 1) ^ (0xEDB8_8320 & mask);
        }
    }
    !crc
}

/// Crate-wide test lock serializing EVERY test that mutates or reads the process-global
/// thumbnail-cache env vars (`PHOTOBROWSER_THUMBNAIL_CACHE_DIR`, `XDG_CACHE_HOME`, `HOME`). Shared by the
/// xdg env tests here AND the `thumb::worker` decode tests, so the two modules' `set_var`/`var_os`
/// calls never run concurrently (that's a data race → was a flaky `decode_jpeg_stores_*` failure).
#[cfg(test)]
pub(crate) static TEST_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[cfg(test)]
mod tests {
    use super::*;
    use image::{DynamicImage, RgbaImage};
    use std::sync::MutexGuard;
    use std::time::Duration;
    use tempfile::TempDir;

    struct EnvGuard<'a> {
        _lock: MutexGuard<'a, ()>,
        old_cache: Option<std::ffi::OsString>,
        old_xdg: Option<std::ffi::OsString>,
        old_home: Option<std::ffi::OsString>,
    }

    impl EnvGuard<'_> {
        fn with_cache_dir(cache_dir: &std::path::Path) -> Self {
            let lock = TEST_ENV_LOCK
                .lock()
                .unwrap_or_else(|poison| poison.into_inner());
            let old_cache = std::env::var_os("PHOTOBROWSER_THUMBNAIL_CACHE_DIR");
            let old_xdg = std::env::var_os("XDG_CACHE_HOME");
            let old_home = std::env::var_os("HOME");
            std::env::set_var("PHOTOBROWSER_THUMBNAIL_CACHE_DIR", cache_dir);
            std::env::remove_var("XDG_CACHE_HOME");
            std::env::remove_var("HOME");
            Self {
                _lock: lock,
                old_cache,
                old_xdg,
                old_home,
            }
        }
    }

    impl Drop for EnvGuard<'_> {
        fn drop(&mut self) {
            restore_env("PHOTOBROWSER_THUMBNAIL_CACHE_DIR", &self.old_cache);
            restore_env("XDG_CACHE_HOME", &self.old_xdg);
            restore_env("HOME", &self.old_home);
        }
    }

    fn restore_env(key: &str, value: &Option<std::ffi::OsString>) {
        match value {
            Some(value) => std::env::set_var(key, value),
            None => std::env::remove_var(key),
        }
    }

    fn source_file(dir: &TempDir) -> PathBuf {
        let path = dir.path().join("source.jpg");
        std::fs::write(&path, b"source bytes").unwrap();
        path
    }

    fn generated_thumb() -> DynamicImage {
        DynamicImage::ImageRgba8(RgbaImage::from_pixel(3, 2, image::Rgba([10, 20, 30, 255])))
    }

    #[test]
    fn store_then_load_roundtrips() {
        let cache_dir = tempfile::tempdir().unwrap();
        let source_dir = tempfile::tempdir().unwrap();
        let _env = EnvGuard::with_cache_dir(cache_dir.path());
        let source = source_file(&source_dir);
        let thumb = generated_thumb();

        store(&source, &thumb);

        let loaded = load(&source).expect("stored thumbnail should load");
        assert_eq!(loaded.width(), 3);
        assert_eq!(loaded.height(), 2);
        assert!(loaded.width() <= 256 && loaded.height() <= 256);
    }

    #[test]
    fn load_misses_on_mtime_change() {
        let cache_dir = tempfile::tempdir().unwrap();
        let source_dir = tempfile::tempdir().unwrap();
        let _env = EnvGuard::with_cache_dir(cache_dir.path());
        let source = source_file(&source_dir);
        let thumb = generated_thumb();

        store(&source, &thumb);
        std::thread::sleep(Duration::from_secs(1));
        std::fs::write(&source, b"changed source bytes").unwrap();

        assert!(load(&source).is_none(), "stale mtime must miss");
    }

    #[test]
    fn load_misses_when_absent() {
        let cache_dir = tempfile::tempdir().unwrap();
        let source_dir = tempfile::tempdir().unwrap();
        let _env = EnvGuard::with_cache_dir(cache_dir.path());
        let source = source_file(&source_dir);

        assert!(load(&source).is_none());
    }

    #[test]
    fn cache_root_honors_env_override() {
        let cache_dir = tempfile::tempdir().unwrap();
        let _env = EnvGuard::with_cache_dir(cache_dir.path());

        assert_eq!(cache_root().as_deref(), Some(cache_dir.path()));
    }

    #[test]
    fn gb_to_bytes_uses_binary_gigabytes() {
        assert_eq!(gb_to_bytes(2.0), 2_147_483_648);
    }

    #[test]
    fn prune_to_max_deletes_oldest_files_until_under_cap() {
        let cache_dir = tempfile::tempdir().unwrap();
        let _env = EnvGuard::with_cache_dir(cache_dir.path());
        let large_dir = cache_dir.path().join("large");
        std::fs::create_dir_all(&large_dir).unwrap();
        let oldest = large_dir.join("oldest.png");
        let middle = large_dir.join("middle.png");
        let newest = large_dir.join("newest.png");
        std::fs::write(&oldest, vec![1u8; 60]).unwrap();
        std::thread::sleep(Duration::from_secs(1));
        std::fs::write(&middle, vec![2u8; 50]).unwrap();
        std::thread::sleep(Duration::from_secs(1));
        std::fs::write(&newest, vec![3u8; 40]).unwrap();

        prune_to_max(100);

        assert!(dir_size_bytes(cache_dir.path()) <= 100);
        assert!(!oldest.exists(), "oldest file should be pruned first");
        assert!(middle.exists(), "middle file should survive");
        assert!(newest.exists(), "newest file should survive");
    }

    #[test]
    fn prune_to_max_zero_is_unlimited() {
        let cache_dir = tempfile::tempdir().unwrap();
        let _env = EnvGuard::with_cache_dir(cache_dir.path());
        let large_dir = cache_dir.path().join("large");
        std::fs::create_dir_all(&large_dir).unwrap();
        let file = large_dir.join("thumb.png");
        std::fs::write(&file, vec![1u8; 128]).unwrap();

        prune_to_max(0);

        assert!(file.exists());
        assert_eq!(dir_size_bytes(cache_dir.path()), 128);
    }
}
