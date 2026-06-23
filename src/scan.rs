use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crate::config::SortMode;

/// The kind of a filesystem entry returned by [`scan_dir`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EntryKind {
    Dir,
    Image,
    Raw,
    Other(FileCategory),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileCategory {
    Video,
    Audio,
    Document,
    Archive,
    Code,
    Unknown,
}

/// A single filesystem entry (directory or image file).
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct Entry {
    pub path: PathBuf,
    pub name: String,
    pub kind: EntryKind,
    pub modified: Option<SystemTime>,
    pub size: u64,
}

/// Recognised image extensions (lowercase).
const IMAGE_EXTS: &[&str] = &["jpg", "jpeg", "png", "webp", "gif"];

/// Classify a file name by lowercase extension.
pub fn classify_file(name: &str) -> EntryKind {
    let ext = Path::new(name)
        .extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .unwrap_or_default();

    if IMAGE_EXTS.contains(&ext.as_str()) {
        return EntryKind::Image;
    }

    match ext.as_str() {
        "nef" | "cr2" | "cr3" | "arw" | "dng" | "raf" | "orf" | "rw2" | "pef" | "srw" => {
            EntryKind::Raw
        }
        "mp4" | "mov" | "mkv" | "avi" | "webm" | "m4v" => EntryKind::Other(FileCategory::Video),
        "mp3" | "flac" | "wav" | "ogg" | "m4a" | "aac" => EntryKind::Other(FileCategory::Audio),
        "pdf" | "txt" | "md" | "doc" | "docx" | "odt" | "rtf" => {
            EntryKind::Other(FileCategory::Document)
        }
        "zip" | "tar" | "gz" | "tgz" | "7z" | "rar" | "xz" => {
            EntryKind::Other(FileCategory::Archive)
        }
        "rs" | "js" | "ts" | "py" | "c" | "cpp" | "h" | "hpp" | "go" | "java" | "sh" | "json"
        | "toml" | "yaml" | "yml" | "html" | "css" => EntryKind::Other(FileCategory::Code),
        _ => EntryKind::Other(FileCategory::Unknown),
    }
}

/// Returns true for filenames whose extension is .md or .txt (case-insensitive).
/// This is the (only) signal for offering a bounded text preview in the right panel.
/// .pdf and other Documents are intentionally excluded (deferred).
pub fn is_text_previewable(name: &str) -> bool {
    let ext = Path::new(name)
        .extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    ext == "md" || ext == "txt"
}

/// Read at most a bounded prefix of a text file for non-image preview.
/// Caps at ~64 KiB and first ~2000 lines. Lossy UTF-8 (never panics on non-UTF8).
/// Browse-only (open + read via take); no write. Call only for is_text_previewable entries.
pub fn read_text_preview(path: &Path) -> String {
    use std::io::Read;
    const MAX_BYTES: u64 = 64 * 1024;
    const MAX_LINES: usize = 2000;

    let file = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(err) => return format!("[cannot open: {err}]"),
    };
    let mut buf = Vec::new();
    if let Err(err) = file.take(MAX_BYTES).read_to_end(&mut buf) {
        return format!("[read error: {err}]");
    }
    let text = String::from_utf8_lossy(&buf).into_owned();
    let lines: Vec<&str> = text.lines().take(MAX_LINES).collect();
    let mut out = lines.join("\n");
    let hit_byte_cap = buf.len() == MAX_BYTES as usize;
    let hit_line_cap = text.lines().nth(MAX_LINES).is_some();
    if hit_byte_cap || hit_line_cap {
        if !out.is_empty() && !out.ends_with('\n') {
            out.push('\n');
        }
        out.push_str("… (truncated)");
    }
    out
}

/// Sidecar/companion files that should be hidden from the browse listing like dotfiles
/// (revealed only when show_hidden is on). These are metadata next to a photo, not browsable content.
fn is_sidecar(name: &str) -> bool {
    let ext = std::path::Path::new(name)
        .extension()
        .map(|e| e.to_string_lossy().to_lowercase());
    matches!(ext.as_deref(), Some("xmp"))
}

/// Lists ONLY the direct children of `dir`. Never descends.
///
/// Directories are always included unless hidden entries are disabled.
/// Files are included with a kind determined by [`classify_file`].
/// Hidden entries (dotfiles) and XMP sidecar files are omitted unless `show_hidden` is true.
/// Symlinked directories are NOT followed — they are listed as `Dir` entries
/// if the symlink itself is not hidden, but their target is never read.
///
/// Sort order: dirs first, then images; each group sorted by name
/// case-insensitively; stable within equal names.
pub fn scan_dir(dir: &Path, show_hidden: bool) -> std::io::Result<Vec<Entry>> {
    let mut entries: Vec<Entry> = Vec::new();

    for de in std::fs::read_dir(dir)? {
        let de = de?;
        let name = de.file_name().to_string_lossy().into_owned();

        // Skip hidden entries unless requested by configuration.
        if !show_hidden && (name.starts_with('.') || is_sidecar(&name)) {
            continue;
        }

        // Use symlink_metadata so we do NOT follow symlinks.
        let meta = de.path().symlink_metadata()?;
        let file_type = meta.file_type();

        let modified = meta.modified().ok();
        let size = meta.len();
        let path = de.path();

        if file_type.is_dir() {
            // Symlinked dirs appear as a Dir entry but are never descended.
            entries.push(Entry {
                path,
                name,
                kind: EntryKind::Dir,
                modified,
                size,
            });
        } else if file_type.is_file() {
            let kind = classify_file(&name);
            entries.push(Entry {
                path,
                name,
                kind,
                modified,
                size,
            });
        }
        // Symlinks to files: also checked above via file_type.is_file() on
        // the symlink's own metadata — symlinks to images are intentionally
        // omitted (they would report is_file() == false on symlink_metadata).
    }

    sort_entries(&mut entries, SortMode::Name);
    Ok(entries)
}

/// Sort entries in-place according to `mode`.
///
/// Directories are always grouped before images. Name mode sorts each group
/// case-insensitively. Date mode sorts each group newest-first, with entries
/// lacking `modified` timestamps last.
#[allow(clippy::ptr_arg)]
pub fn sort_entries(entries: &mut Vec<Entry>, mode: SortMode) {
    entries.sort_by(|a, b| {
        let group_rank = |kind: &EntryKind| match kind {
            EntryKind::Dir => 0,
            EntryKind::Image | EntryKind::Raw => 1,
            EntryKind::Other(_) => 2,
        };
        let kind_order = group_rank(&a.kind).cmp(&group_rank(&b.kind));

        if kind_order != std::cmp::Ordering::Equal {
            return kind_order;
        }

        match mode {
            SortMode::Name | SortMode::Rating => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
            SortMode::Date | SortMode::Captured => match (a.modified, b.modified) {
                (Some(a_time), Some(b_time)) => b_time.cmp(&a_time),
                (Some(_), None) => std::cmp::Ordering::Less,
                (None, Some(_)) => std::cmp::Ordering::Greater,
                (None, None) => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
            },
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    // ─── helper ───────────────────────────────────────────────────────────────

    fn mk(root: &Path, rel: &str) -> PathBuf {
        let p = root.join(rel);
        if rel.ends_with('/') {
            fs::create_dir_all(&p).unwrap();
        } else {
            if let Some(parent) = p.parent() {
                fs::create_dir_all(parent).unwrap();
            }
            fs::write(&p, b"").unwrap();
        }
        p
    }

    #[test]
    fn no_recursion_fixture() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        mk(root, "a.jpg");
        mk(root, "b.txt");
        mk(root, ".hidden.jpg");
        mk(root, "sub/b.jpg");
        mk(root, "sub/deep/c.jpg");

        let entries = scan_dir(root, false).expect("scan_dir failed");
        let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();

        // All-files: a.jpg (Image), b.txt (Other), sub (Dir) all appear.
        assert_eq!(entries.len(), 3, "expected 3 entries, got {entries:?}");
        assert_eq!(entries[0].name, "sub");
        assert_eq!(entries[0].kind, EntryKind::Dir);
        assert!(names.contains(&"a.jpg"));
        assert!(names.contains(&"b.txt"));
        assert!(entries
            .iter()
            .any(|e| e.name == "a.jpg" && e.kind == EntryKind::Image));
        assert!(entries
            .iter()
            .any(|e| e.name == "b.txt" && matches!(e.kind, EntryKind::Other(_))));
        // No-recursion invariant PRESERVED: nested files never appear.
        assert!(!names.contains(&"b.jpg"), "b.jpg from sub/ must not appear");
        assert!(
            !names.contains(&"c.jpg"),
            "c.jpg from deep/ must not appear"
        );
        // Hidden still filtered.
        assert!(
            !names.contains(&".hidden.jpg"),
            ".hidden.jpg must be filtered"
        );
    }

    // ─── SORT ORDER: dirs first, case-insensitive names ───────────────────────
    #[test]
    fn sort_order_dirs_first_case_insensitive() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();

        mk(root, "Zebra/"); // dir
        mk(root, "apple/"); // dir
        mk(root, "Mango.jpg"); // image
        mk(root, "banana.png"); // image

        let entries = scan_dir(root, false).expect("scan_dir failed");

        // Dirs first.
        let dir_count = entries.iter().filter(|e| e.kind == EntryKind::Dir).count();
        let img_count = entries
            .iter()
            .filter(|e| e.kind == EntryKind::Image)
            .count();
        assert_eq!(dir_count, 2);
        assert_eq!(img_count, 2);

        assert_eq!(entries[0].kind, EntryKind::Dir);
        assert_eq!(entries[1].kind, EntryKind::Dir);
        assert_eq!(entries[2].kind, EntryKind::Image);
        assert_eq!(entries[3].kind, EntryKind::Image);

        // Case-insensitive sort within dirs: apple < Zebra.
        assert_eq!(entries[0].name, "apple");
        assert_eq!(entries[1].name, "Zebra");

        // Case-insensitive sort within images: banana < Mango.
        assert_eq!(entries[2].name, "banana.png");
        assert_eq!(entries[3].name, "Mango.jpg");
    }

    #[test]
    fn sort_entries_date_keeps_dirs_first_newest_first_missing_last() {
        use std::time::Duration;

        let base = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000);
        let older = base + Duration::from_secs(10);
        let newer = base + Duration::from_secs(20);

        let mut entries = vec![
            Entry {
                path: PathBuf::from("/x/old.jpg"),
                name: "old.jpg".into(),
                kind: EntryKind::Image,
                modified: Some(older),
                size: 0,
            },
            Entry {
                path: PathBuf::from("/x/missing.jpg"),
                name: "missing.jpg".into(),
                kind: EntryKind::Image,
                modified: None,
                size: 0,
            },
            Entry {
                path: PathBuf::from("/x/new.jpg"),
                name: "new.jpg".into(),
                kind: EntryKind::Image,
                modified: Some(newer),
                size: 0,
            },
            Entry {
                path: PathBuf::from("/x/dir"),
                name: "dir".into(),
                kind: EntryKind::Dir,
                modified: None,
                size: 0,
            },
        ];

        sort_entries(&mut entries, SortMode::Date);

        let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, vec!["dir", "new.jpg", "old.jpg", "missing.jpg"]);
    }

    #[test]
    fn sort_entries_date_sorts_dirs_by_date_too() {
        use std::time::Duration;

        let base = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000);
        let mut entries = vec![
            Entry {
                path: PathBuf::from("/x/older-dir"),
                name: "older-dir".into(),
                kind: EntryKind::Dir,
                modified: Some(base + Duration::from_secs(10)),
                size: 0,
            },
            Entry {
                path: PathBuf::from("/x/newer-dir"),
                name: "newer-dir".into(),
                kind: EntryKind::Dir,
                modified: Some(base + Duration::from_secs(20)),
                size: 0,
            },
            Entry {
                path: PathBuf::from("/x/photo.jpg"),
                name: "photo.jpg".into(),
                kind: EntryKind::Image,
                modified: Some(base + Duration::from_secs(30)),
                size: 0,
            },
        ];

        sort_entries(&mut entries, SortMode::Date);

        let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, vec!["newer-dir", "older-dir", "photo.jpg"]);
    }

    #[test]
    fn extension_classification() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        mk(root, "photo.JPG");
        mk(root, "image.JPEG");
        mk(root, "shot.PNG");
        mk(root, "anim.GIF");
        mk(root, "next.WEBP");
        mk(root, "doc.txt");
        mk(root, "archive.zip");
        mk(root, "raw.NEF");
        mk(root, "binary");

        let entries = scan_dir(root, false).expect("scan_dir failed");
        let img = |n: &str| {
            entries
                .iter()
                .any(|e| e.name == n && e.kind == EntryKind::Image)
        };
        assert!(
            img("photo.JPG")
                && img("image.JPEG")
                && img("shot.PNG")
                && img("anim.GIF")
                && img("next.WEBP")
        );
        // Non-images now appear (not filtered).
        assert!(entries
            .iter()
            .any(|e| e.name == "raw.NEF" && e.kind == EntryKind::Raw));
        assert!(entries
            .iter()
            .any(|e| e.name == "doc.txt" && matches!(e.kind, EntryKind::Other(_))));
        assert!(entries
            .iter()
            .any(|e| e.name == "archive.zip" && matches!(e.kind, EntryKind::Other(_))));
        assert!(entries
            .iter()
            .any(|e| e.name == "binary" && matches!(e.kind, EntryKind::Other(_))));
        // All 9 entries present (nothing dropped).
        assert_eq!(
            entries.len(),
            9,
            "all files should be listed, got {entries:?}"
        );
    }

    #[test]
    fn show_hidden_true_includes_dotfile() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        mk(root, "visible.jpg");
        mk(root, ".hidden.jpg");

        let hidden_excluded = scan_dir(root, false).expect("scan_dir failed");
        let hidden_included = scan_dir(root, true).expect("scan_dir failed");
        let excluded_names: Vec<&str> = hidden_excluded.iter().map(|e| e.name.as_str()).collect();
        let included_names: Vec<&str> = hidden_included.iter().map(|e| e.name.as_str()).collect();

        assert!(!excluded_names.contains(&".hidden.jpg"));
        assert!(included_names.contains(&".hidden.jpg"));
    }

    #[test]
    fn sidecar_xmp_excluded_by_default() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        mk(root, "a.jpg");
        mk(root, "a.xmp");

        let entries = scan_dir(root, false).expect("scan_dir failed");
        let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();

        assert!(names.contains(&"a.jpg"), "a.jpg must be present");
        assert!(
            !names.contains(&"a.xmp"),
            "a.xmp must be excluded by default"
        );
    }

    #[test]
    fn sidecar_xmp_included_when_show_hidden() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        mk(root, "a.jpg");
        mk(root, "a.xmp");

        let entries = scan_dir(root, true).expect("scan_dir failed");
        let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();

        assert!(names.contains(&"a.jpg"), "a.jpg must be present");
        assert!(
            names.contains(&"a.xmp"),
            "a.xmp must be included when show_hidden"
        );
    }

    #[test]
    fn classify_by_extension() {
        assert_eq!(classify_file("a.jpg"), EntryKind::Image);
        assert_eq!(classify_file("a.NEF"), EntryKind::Raw);
        assert_eq!(classify_file("a.cr3"), EntryKind::Raw);
        assert_eq!(
            classify_file("clip.mp4"),
            EntryKind::Other(FileCategory::Video)
        );
        assert_eq!(
            classify_file("song.mp3"),
            EntryKind::Other(FileCategory::Audio)
        );
        assert_eq!(
            classify_file("readme.md"),
            EntryKind::Other(FileCategory::Document)
        );
        assert_eq!(
            classify_file("a.zip"),
            EntryKind::Other(FileCategory::Archive)
        );
        assert_eq!(
            classify_file("main.rs"),
            EntryKind::Other(FileCategory::Code)
        );
        assert_eq!(
            classify_file("noext"),
            EntryKind::Other(FileCategory::Unknown)
        );
        assert_eq!(
            classify_file("weird.xyz"),
            EntryKind::Other(FileCategory::Unknown)
        );
    }

    #[test]
    fn is_text_previewable_detects_md_txt_only() {
        assert!(is_text_previewable("readme.md"));
        assert!(is_text_previewable("NOTES.TXT"));
        assert!(is_text_previewable("design.Md"));
        assert!(!is_text_previewable("doc.pdf"));
        assert!(!is_text_previewable("photo.jpg"));
        assert!(!is_text_previewable("main.rs"));
        assert!(!is_text_previewable("binary"));
        assert!(!is_text_previewable("weird.xyz"));
    }
}
