//! Pure file-export: copy selected photos (+ .xmp sidecars) to a destination, copy-only, never modifying originals.

use std::path::{Path, PathBuf};

/// Result of an export pass.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ExportResult {
    pub copied: usize,
    pub skipped: usize, // destination already existed (never overwritten)
    pub failed: usize,
}

/// Copy each source photo (+ its `.xmp` sidecar if present) into `dest_dir`, keeping the file name.
/// Copy-only: never moves, deletes, or modifies a source. Never overwrites an existing destination file
/// (counts it as `skipped`). Creates `dest_dir` if missing. A photo counts as `copied` if the photo
/// itself was newly copied; sidecar copy is best-effort and does not change the photo's count.
pub fn export_paths(sources: &[PathBuf], dest_dir: &Path) -> ExportResult {
    let mut r = ExportResult::default();
    if std::fs::create_dir_all(dest_dir).is_err() {
        r.failed = sources.len();
        return r;
    }
    for src in sources {
        let Some(name) = src.file_name() else {
            r.failed += 1;
            continue;
        };
        let dest = dest_dir.join(name);
        if dest.exists() {
            r.skipped += 1;
        } else if std::fs::copy(src, &dest).is_ok() {
            r.copied += 1;
        } else {
            r.failed += 1;
            continue;
        }
        // Best-effort: copy the sidecar too (skip if exists; never overwrite).
        let side = src.with_extension("xmp");
        if side.exists() {
            let dest_side = dest_dir.join(side.file_name().unwrap_or_default());
            if !dest_side.exists() {
                let _ = std::fs::copy(&side, &dest_side);
            }
        }
    }
    r
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn export_paths_copies_photo_and_sidecar() {
        use std::fs;
        use tempfile::tempdir;
        let src_dir = tempdir().unwrap();
        let dest_dir = tempdir().unwrap();
        let src = src_dir.path().join("a.jpg");
        let side = src_dir.path().join("a.xmp");
        fs::write(&src, b"JPGDATA").unwrap();
        fs::write(&side, b"<xmp></xmp>").unwrap();

        let res = export_paths(std::slice::from_ref(&src), dest_dir.path());
        assert_eq!(res.copied, 1);
        assert_eq!(res.skipped, 0);
        assert_eq!(res.failed, 0);

        let out_j = dest_dir.path().join("a.jpg");
        let out_x = dest_dir.path().join("a.xmp");
        assert!(out_j.exists());
        assert!(out_x.exists());
        assert_eq!(fs::read(&out_j).unwrap(), b"JPGDATA");
        assert_eq!(fs::read(&out_x).unwrap(), b"<xmp></xmp>");
    }

    #[test]
    fn export_paths_skips_existing_never_overwrites() {
        use std::fs;
        use tempfile::tempdir;
        let src_dir = tempdir().unwrap();
        let dest_dir = tempdir().unwrap();
        let src = src_dir.path().join("a.jpg");
        fs::write(&src, b"SOURCE").unwrap();

        let dest = dest_dir.path().join("a.jpg");
        fs::write(&dest, b"ORIGINAL_DEST").unwrap();

        let res = export_paths(std::slice::from_ref(&src), dest_dir.path());
        assert_eq!(res.copied, 0);
        assert_eq!(res.skipped, 1);
        assert_eq!(res.failed, 0);
        assert_eq!(fs::read(&dest).unwrap(), b"ORIGINAL_DEST");
    }

    #[test]
    fn export_never_modifies_or_removes_source() {
        use std::fs;
        use tempfile::tempdir;
        let src_dir = tempdir().unwrap();
        let dest_dir = tempdir().unwrap();
        let src = src_dir.path().join("a.jpg");
        let side = src_dir.path().join("a.xmp");
        fs::write(&src, b"ORIGJPG").unwrap();
        fs::write(&side, b"ORIGXMP").unwrap();

        let res = export_paths(std::slice::from_ref(&src), dest_dir.path());
        assert_eq!(res.copied, 1);
        assert!(src.exists());
        assert!(side.exists());
        assert_eq!(fs::read(&src).unwrap(), b"ORIGJPG");
        assert_eq!(fs::read(&side).unwrap(), b"ORIGXMP");
    }

    #[test]
    fn export_paths_creates_dest_dir() {
        use std::fs;
        use tempfile::tempdir;
        let src_dir = tempdir().unwrap();
        let base = tempdir().unwrap();
        let dest_dir = base.path().join("nested").join("exports");
        let src = src_dir.path().join("p.jpg");
        fs::write(&src, b"PIX").unwrap();

        assert!(!dest_dir.exists());
        let res = export_paths(std::slice::from_ref(&src), &dest_dir);
        assert_eq!(res.copied, 1);
        assert!(dest_dir.exists());
        assert_eq!(fs::read(dest_dir.join("p.jpg")).unwrap(), b"PIX");
    }
}
