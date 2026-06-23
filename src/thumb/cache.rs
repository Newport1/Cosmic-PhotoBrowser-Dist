//! Bounded LRU thumbnail cache.
//!
//! The cache keeps at most `cap` entries and evicts the least-recently-used
//! entry when capacity is exceeded.

use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};

use lru::LruCache;

/// An opaque pixel handle stored in the cache.
///
/// In production this wraps `cosmic::widget::image::Handle`.  The cache itself
/// is kept toolkit-agnostic so the unit tests (which run outside a Wayland
/// compositor) can compile and run without a live display server.
///
/// `ThumbService::on_decoded` constructs the real `Handle` and stores it here.
#[derive(Debug, Clone)]
pub struct CachedHandle(pub cosmic::widget::image::Handle);

/// Bounded LRU cache: path → decoded thumbnail handle.
pub struct ThumbCache {
    inner: LruCache<PathBuf, CachedHandle>,
}

#[allow(dead_code)]
impl ThumbCache {
    /// Create a new cache with the given capacity.
    ///
    /// # Panics
    /// Panics if `cap == 0`.
    pub fn new(cap: usize) -> Self {
        let cap = NonZeroUsize::new(cap).expect("ThumbCache cap must be > 0");
        Self {
            inner: LruCache::new(cap),
        }
    }

    /// Insert or update an entry.  If the cache is at capacity the LRU entry
    /// is evicted (and its handle dropped) before insertion.
    pub fn insert(&mut self, path: PathBuf, handle: CachedHandle) {
        self.inner.put(path, handle);
    }

    /// Look up a path, bumping its recency on hit.
    pub fn get(&mut self, path: &Path) -> Option<&CachedHandle> {
        self.inner.get(path)
    }

    /// Returns the current number of cached entries.
    pub fn len(&self) -> usize {
        self.inner.len()
    }

    /// Returns `true` if the cache is empty.
    #[allow(dead_code)]
    pub fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }

    /// Returns the configured capacity.
    pub fn cap(&self) -> usize {
        self.inner.cap().get()
    }

    /// Returns whether `path` is present in the cache (without bumping recency).
    pub fn contains(&self, path: &Path) -> bool {
        self.inner.contains(path)
    }

    /// Peek a path without bumping its recency in the LRU. For passive readers
    /// (e.g. loupe filmstrip) that must not affect eviction order.
    pub fn peek(&self, path: &Path) -> Option<&CachedHandle> {
        self.inner.peek(path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    /// Build a fake CachedHandle for use in tests.
    ///
    /// We cannot create a real `cosmic::widget::image::Handle` in a headless
    /// environment, so we wrap a bytes-from-memory handle which does not
    /// require a running compositor.
    fn dummy_handle() -> CachedHandle {
        // from_rgba with 1×1 transparent pixel — valid in headless builds.
        CachedHandle(cosmic::widget::image::Handle::from_rgba(1, 1, vec![0u8; 4]))
    }

    fn p(s: &str) -> PathBuf {
        PathBuf::from(s)
    }

    // ── CAP test ──────────────────────────────────────────────────────────────
    //
    // cap=8, insert 20 distinct paths ⇒ len()==8;
    // first 12 absent, last 8 present;
    // get() bumps recency (assert via eviction order).
    #[test]
    fn cap_test() {
        let cap = 8usize;
        let mut cache = ThumbCache::new(cap);

        // Insert paths /p/0 … /p/19.
        for i in 0..20usize {
            cache.insert(p(&format!("/p/{i}")), dummy_handle());
        }

        assert_eq!(cache.len(), cap, "len must equal cap after overflow");

        // First 12 entries (/p/0 … /p/11) should be evicted (LRU).
        for i in 0..12usize {
            assert!(
                !cache.contains(&p(&format!("/p/{i}"))),
                "/p/{i} should have been evicted"
            );
        }

        // Last 8 entries (/p/12 … /p/19) should still be present.
        for i in 12..20usize {
            assert!(
                cache.contains(&p(&format!("/p/{i}"))),
                "/p/{i} should be cached"
            );
        }
    }

    // ── get() bumps recency ───────────────────────────────────────────────────
    //
    // With cap=2: insert /a, /b.  Touch /a (bumps it to MRU).  Insert /c.
    // /b should be evicted (it became the LRU after we touched /a), /a present.
    #[test]
    fn get_bumps_recency() {
        let mut cache = ThumbCache::new(2);

        cache.insert(p("/a"), dummy_handle());
        cache.insert(p("/b"), dummy_handle());

        // Touch /a — now /b is LRU.
        let _ = cache.get(&p("/a"));

        // Insert /c — should evict /b (LRU).
        cache.insert(p("/c"), dummy_handle());

        assert!(cache.contains(&p("/a")), "/a should still be present");
        assert!(!cache.contains(&p("/b")), "/b should have been evicted");
        assert!(cache.contains(&p("/c")), "/c should be present");
    }
}
