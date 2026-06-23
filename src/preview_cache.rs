//! Bounded LRU cache for loupe and high-resolution preview decodes.
//!
//! Entries are keyed by path, decode mode, and develop-look state so embedded,
//! full-RAW, high-resolution, and develop-look variants can coexist safely.

use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};

use lru::LruCache;

use crate::inspection::LoupeDecodeMode;

/// Cached decoded loupe image (the handle is the display-ready pixels; dims
/// are the *original* image dimensions before the display-bound thumbnail).
#[derive(Debug, Clone)]
pub struct CachedPreview {
    pub handle: cosmic::widget::image::Handle,
    pub dimensions: (u32, u32),
}

/// Default capacity for the loupe preview cache.
pub const PREVIEW_CACHE_CAP: usize = 8;

/// Bounded LRU cache for high-res loupe decodes.
pub struct PreviewCache {
    inner: LruCache<(PathBuf, LoupeDecodeMode, bool), CachedPreview>,
}

#[allow(dead_code)]
impl PreviewCache {
    /// Create a new cache with the given capacity.
    ///
    /// # Panics
    /// Panics if `cap == 0`.
    pub fn new(cap: usize) -> Self {
        let cap = NonZeroUsize::new(cap).expect("PreviewCache cap must be > 0");
        Self {
            inner: LruCache::new(cap),
        }
    }

    /// Insert or update an entry. If at capacity the LRU entry is evicted
    /// (its Handle is dropped) before the new insertion.
    pub fn insert(
        &mut self,
        path: PathBuf,
        mode: LoupeDecodeMode,
        develop: bool,
        handle: cosmic::widget::image::Handle,
        dimensions: (u32, u32),
    ) {
        let key = (path, mode, develop);
        let val = CachedPreview { handle, dimensions };
        self.inner.put(key, val);
    }

    /// Look up by (path, mode, develop), bumping the entry to MRU on hit.
    pub fn get(
        &mut self,
        path: &Path,
        mode: LoupeDecodeMode,
        develop: bool,
    ) -> Option<&CachedPreview> {
        // LruCache::get takes &K; we build a transient key for lookup.
        let key = (path.to_path_buf(), mode, develop);
        self.inner.get(&key)
    }

    /// Current number of entries.
    pub fn len(&self) -> usize {
        self.inner.len()
    }

    /// Returns true if empty.
    #[allow(dead_code)]
    pub fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }

    /// Configured capacity.
    pub fn cap(&self) -> usize {
        self.inner.cap().get()
    }

    /// Presence check (does not bump recency).
    pub fn contains(&self, path: &Path, mode: LoupeDecodeMode, develop: bool) -> bool {
        let key = (path.to_path_buf(), mode, develop);
        self.inner.contains(&key)
    }

    /// Remove all HighRes* keyed entries (for enforcing at most one full-res decode
    /// resident across the whole cache). Called on loupe close and before/around
    /// high-res inserts for a target path.
    pub fn purge_high_res(&mut self) {
        self.purge_high_res_except(None);
    }

    /// Remove HighRes* entries whose path is not `keep`; if keep=None, remove all.
    /// This keeps a high-res for the current loupe path while dropping any previous
    /// image's high-res decode.
    pub fn purge_high_res_except(&mut self, keep: Option<&Path>) {
        let keys_to_remove: Vec<_> = self
            .inner
            .iter()
            .filter_map(|(k, _)| {
                let is_high = matches!(
                    k.1,
                    LoupeDecodeMode::HighRes | LoupeDecodeMode::HighResFullRaw
                );
                let path_mismatch = keep.is_none_or(|p| k.0 != p);
                if is_high && path_mismatch {
                    Some(k.clone())
                } else {
                    None
                }
            })
            .collect();
        for k in keys_to_remove {
            let _ = self.inner.pop(&k);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn dummy_preview() -> CachedPreview {
        // 1x1 transparent RGBA works in headless test env (no compositor).
        CachedPreview {
            handle: cosmic::widget::image::Handle::from_rgba(1, 1, vec![0u8; 4]),
            dimensions: (100, 200),
        }
    }

    fn p(s: &str) -> PathBuf {
        PathBuf::from(s)
    }

    #[test]
    fn insert_and_hit() {
        let mut cache = PreviewCache::new(PREVIEW_CACHE_CAP);
        let path = p("/photos/a.nef");
        let mode = LoupeDecodeMode::EmbeddedPreview;

        assert!(cache.get(&path, mode, false).is_none());
        assert!(!cache.contains(&path, mode, false));

        let preview = dummy_preview();
        cache.insert(
            path.clone(),
            mode,
            false,
            preview.handle,
            preview.dimensions,
        );

        assert!(cache.contains(&path, mode, false));
        let got = cache.get(&path, mode, false).unwrap();
        assert_eq!(got.dimensions, (100, 200));
        assert_eq!(cache.len(), 1);
        assert_eq!(cache.cap(), PREVIEW_CACHE_CAP);
    }

    #[test]
    fn bounded_eviction_frees_oldest() {
        // Insert 3 into cap=2; oldest must be gone and its handle dropped on eviction.
        let cap = 2usize;
        let mut cache = PreviewCache::new(cap);

        let p1 = p("/photos/one.jpg");
        let p2 = p("/photos/two.jpg");
        let p3 = p("/photos/three.nef");
        let m = LoupeDecodeMode::EmbeddedPreview;

        cache.insert(p1.clone(), m, false, dummy_preview().handle, (1, 1));
        cache.insert(p2.clone(), m, false, dummy_preview().handle, (2, 2));
        assert_eq!(cache.len(), 2);

        // Third insert evicts the LRU (p1).
        cache.insert(p3.clone(), m, false, dummy_preview().handle, (3, 3));

        assert_eq!(cache.len(), cap, "len must equal cap after overflow");
        assert!(
            !cache.contains(&p1, m, false),
            "oldest (/p1) must have been evicted by bounded LRU"
        );
        assert!(cache.contains(&p2, m, false), "p2 must remain");
        assert!(cache.contains(&p3, m, false), "p3 must be present");
    }

    #[test]
    fn get_bumps_recency() {
        // cap=2: a,b inserted. get(a) makes a MRU. insert(c) must evict b.
        let mut cache = PreviewCache::new(2);

        let pa = p("/a");
        let pb = p("/b");
        let pc = p("/c");
        let m = LoupeDecodeMode::FullRaw;

        cache.insert(pa.clone(), m, false, dummy_preview().handle, (10, 10));
        cache.insert(pb.clone(), m, false, dummy_preview().handle, (20, 20));

        let _ = cache.get(&pa, m, false); // touch a → b becomes LRU

        cache.insert(pc.clone(), m, false, dummy_preview().handle, (30, 30));

        assert!(cache.contains(&pa, m, false), "/a should still be present");
        assert!(
            !cache.contains(&pb, m, false),
            "/b should have been evicted"
        );
        assert!(cache.contains(&pc, m, false), "/c should be present");
    }

    #[test]
    fn different_modes_distinct_entries() {
        let mut cache = PreviewCache::new(8);
        let path = p("/raw/raw.nef");
        let m_embed = LoupeDecodeMode::EmbeddedPreview;
        let m_full = LoupeDecodeMode::FullRaw;

        cache.insert(
            path.clone(),
            m_embed,
            false,
            dummy_preview().handle,
            (800, 600),
        );
        cache.insert(
            path.clone(),
            m_full,
            false,
            dummy_preview().handle,
            (4000, 3000),
        );

        assert_eq!(cache.len(), 2, "same path + different mode = two entries");
        assert!(cache.contains(&path, m_embed, false));
        assert!(cache.contains(&path, m_full, false));
    }

    #[test]
    fn develop_flag_distinct_entries() {
        let mut cache = PreviewCache::new(8);
        let path = p("/raw/raw.nef");
        let mode = LoupeDecodeMode::EmbeddedPreview;

        cache.insert(
            path.clone(),
            mode,
            false,
            dummy_preview().handle,
            (800, 600),
        );
        cache.insert(path.clone(), mode, true, dummy_preview().handle, (800, 600));

        assert_eq!(
            cache.len(),
            2,
            "same path+mode + different develop flag = two entries"
        );
        assert!(cache.contains(&path, mode, false));
        assert!(cache.contains(&path, mode, true));
    }
}
