//! Thumbnail pipeline — bounded LRU cache + rayon decode worker.
//!
//! ## Memory behavior
//! Decoded thumbnails are stored in a path-keyed bounded LRU cache.
//! Eviction drops the handle so pixels are freed immediately.
//!
//! ## Public surface
//! [`ThumbService`] is the single entry point the app uses:
//! - [`ThumbService::get_or_request`] — cache hit → `Ready`; miss → enqueue + `Pending`.
//! - [`ThumbService::flush_requests`] — dispatch pending decode requests in one batch.
//! - [`ThumbService::on_decoded`] — call when a worker result message arrives.
//! - [`ThumbService::next_generation`] — call on folder navigation.
//! - [`ThumbService::drain`] — drain the mpsc channel (called every frame/update).

pub mod cache;
pub mod worker;
pub mod xdg;

use std::path::{Path, PathBuf};
use std::sync::mpsc;

use crate::thumb::cache::{CachedHandle, ThumbCache};
use crate::thumb::worker::{DecodeResult, ThumbWorker};

// Re-export for callers that need worker result details.
#[allow(unused_imports)]
pub use crate::thumb::worker::DecodeResult as ThumbDecodeResult;

/// The state of a thumbnail as seen by the caller.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub enum ThumbState {
    /// Decoded and cached; `Handle` is ready to render.
    Ready(cosmic::widget::image::Handle),
    /// Enqueued but not yet decoded.
    Pending,
    /// Decoding failed (corrupt / unsupported file).
    Failed,
}

/// Unified thumbnail service: LRU cache + bounded decode queue + rayon pool.
pub struct ThumbService {
    cache: ThumbCache,
    worker: ThumbWorker,
    /// mpsc receiver — drained by `drain()` each frame.
    rx: mpsc::Receiver<DecodeResult>,
    /// Current generation counter.  Bumped on every folder navigation.
    gen: u64,
    /// Set of paths for which a decode job is already in-flight so we do not
    /// enqueue duplicates.
    in_flight: std::collections::HashSet<PathBuf>,
}

impl ThumbService {
    /// Create a new `ThumbService`.
    ///
    /// - `cap` — LRU cache capacity (in entries).
    /// - `queue_cap` — maximum number of jobs in the pending decode queue.
    pub fn new(cap: usize, queue_cap: usize) -> Self {
        let (worker, rx) = ThumbWorker::new(queue_cap);
        ThumbService {
            cache: ThumbCache::new(cap),
            worker,
            rx,
            gen: 0,
            in_flight: std::collections::HashSet::new(),
        }
    }

    /// Cache hit → `Ready` (bumps recency).
    /// Cache miss → enqueue (size px, current gen) and return `Pending`.
    #[allow(dead_code)]
    pub fn get_or_request(&mut self, path: &Path, size: u16) -> ThumbState {
        self.get_or_request_with_priority(path, size, f32::INFINITY)
    }

    /// Cache hit → `Ready` (bumps recency).
    /// Cache miss → enqueue with a lower-is-nearer dispatch priority and return `Pending`.
    pub fn get_or_request_with_priority(
        &mut self,
        path: &Path,
        size: u16,
        priority: f32,
    ) -> ThumbState {
        // Cache hit.
        if let Some(handle) = self.cache.get(path) {
            return ThumbState::Ready(handle.0.clone());
        }

        // Not in cache — enqueue if not already in-flight.
        if !self.in_flight.contains(path) {
            let request_path = path.to_path_buf();
            if let Some(evicted) =
                self.worker
                    .enqueue_prioritized(request_path.clone(), size, self.gen, priority)
            {
                self.in_flight.remove(&evicted);
            }
            if self.worker.contains_pending_path(&request_path) {
                self.in_flight.insert(request_path);
            }
        }

        ThumbState::Pending
    }

    /// Dispatch all queued thumbnail decode requests.  `rebuild_snapshot` calls
    /// this once after batching the visible+margin envelope.
    pub fn flush_requests(&mut self) {
        self.worker.flush();
    }

    /// Raise the pending decode queue cap so the current request envelope fits.
    pub fn ensure_queue_cap(&mut self, queue_cap: usize) {
        self.worker.ensure_queue_cap(queue_cap);
    }

    #[allow(dead_code)]
    pub fn queue_cap(&self) -> usize {
        self.worker.queue_cap()
    }

    /// Called from the app when a worker result message arrives.
    ///
    /// **GENERATION invariant**: if the result's `gen` differs from the current
    /// generation, it is silently discarded (never inserted into the cache).
    pub fn on_decoded(&mut self, path: PathBuf, gen: u64, rgba: Option<(Vec<u8>, u32, u32)>) {
        // Remove from in-flight set regardless.
        self.in_flight.remove(&path);

        // Discard stale results.
        if gen != self.gen {
            tracing::debug!(
                "discarding stale thumb result for {} (result gen={gen}, current gen={})",
                path.display(),
                self.gen
            );
            return;
        }

        match rgba {
            Some((bytes, w, h)) => {
                let handle = cosmic::widget::image::Handle::from_rgba(w, h, bytes);
                tracing::debug!(
                    "cached thumb {}×{} for {} (cache len={}/{})",
                    w,
                    h,
                    path.display(),
                    self.cache.len() + 1,
                    self.cache.cap()
                );
                self.cache.insert(path, CachedHandle(handle));
            }
            None => {
                tracing::debug!("thumb decode failed for {}", path.display());
            }
        }
    }

    /// Folder navigation: bump generation, clear in-flight set.
    ///
    /// The LRU cache is kept intact — entries decoded for previously-visited
    /// paths remain valid and will be served immediately if the user navigates
    /// back.
    pub fn next_generation(&mut self) {
        self.gen += 1;
        self.in_flight.clear();
        tracing::debug!("thumb generation → {}", self.gen);
    }

    /// Returns the current number of cached thumbnails.
    pub fn len(&self) -> usize {
        self.cache.len()
    }

    /// Returns the number of thumbnail decode jobs currently in flight.
    pub fn loading_len(&self) -> usize {
        self.in_flight.len()
    }

    /// Non-mutating peek: returns Ready if the path is in the LRU cache (cloned handle),
    /// otherwise Pending. Never enqueues a request and never bumps recency.
    /// (Used by loupe filmstrip to read thumb state during render without side-effects.)
    pub fn peek_state(&self, path: &Path) -> ThumbState {
        if let Some(ch) = self.cache.peek(path) {
            ThumbState::Ready(ch.0.clone())
        } else {
            ThumbState::Pending
        }
    }

    /// Returns `true` if no thumbnails are cached.
    #[allow(dead_code)]
    pub fn is_empty(&self) -> bool {
        self.cache.is_empty()
    }

    /// Returns the cache capacity.
    #[allow(dead_code)]
    pub fn cap(&self) -> usize {
        self.cache.cap()
    }

    /// Returns the current generation.
    #[allow(dead_code)]
    pub fn generation(&self) -> u64 {
        self.gen
    }

    /// Drain all pending decode results from the channel.
    ///
    /// Call this once per app `update` cycle (or from the subscription).
    /// Returns the results for the caller to process with `on_decoded`.
    pub fn drain(&mut self) -> Vec<DecodeResult> {
        let mut results = Vec::new();
        loop {
            match self.rx.try_recv() {
                Ok(r) => results.push(r),
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => break,
            }
        }
        results
    }

    #[cfg(test)]
    pub(crate) fn queue_decoded_for_test(
        &mut self,
        path: PathBuf,
        gen: u64,
        rgba: Option<(Vec<u8>, u32, u32)>,
    ) {
        self.in_flight.insert(path.clone());
        self.worker
            .send_result_for_test(DecodeResult { path, gen, rgba });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn p(s: &str) -> PathBuf {
        PathBuf::from(s)
    }

    fn fake_rgba() -> (Vec<u8>, u32, u32) {
        // 1×1 RGBA pixel.
        (vec![255u8, 0, 0, 255], 1, 1)
    }

    // ── generation test ───────────────────────────────────────────────────────
    //
    // Request at gen G, next_generation(), deliver result tagged G ⇒ NOT inserted.
    // Deliver result tagged G+1 ⇒ inserted.
    #[test]
    fn generation_stale_discarded() {
        let mut svc = ThumbService::new(8, 4);

        // Gen starts at 0.
        let path = p("/test/image.jpg");
        svc.get_or_request(&path, 160); // enqueues at gen=0

        // Bump generation (simulates folder navigation).
        svc.next_generation(); // gen is now 1

        // Deliver a result tagged with old gen=0 — should be discarded.
        svc.on_decoded(path.clone(), 0, Some(fake_rgba()));
        assert_eq!(svc.len(), 0, "stale result (gen=0) must not be inserted");

        // Deliver a result tagged with current gen=1 — should be inserted.
        svc.on_decoded(path.clone(), 1, Some(fake_rgba()));
        assert_eq!(svc.len(), 1, "current-gen result (gen=1) must be inserted");
    }

    // ── generation: current-gen insert works ─────────────────────────────────
    #[test]
    fn generation_current_inserted() {
        let mut svc = ThumbService::new(8, 4);
        let path = p("/test/photo.png");

        // Deliver at gen=0 (initial).
        svc.on_decoded(path.clone(), 0, Some(fake_rgba()));
        assert_eq!(svc.len(), 1, "current-gen result must be inserted");
    }

    // ── len() reflects cache size ─────────────────────────────────────────────
    #[test]
    fn len_reflects_cache() {
        let mut svc = ThumbService::new(4, 4);

        for i in 0..6usize {
            svc.on_decoded(p(&format!("/p/{i}")), 0, Some(fake_rgba()));
        }
        // Cache cap is 4, so only 4 entries.
        assert_eq!(svc.len(), 4);
        assert_eq!(svc.cap(), 4);
    }

    #[test]
    fn loading_len_reflects_in_flight_requests() {
        let mut svc = ThumbService::new(8, 4);
        let first = p("/test/first.jpg");
        let second = p("/test/second.jpg");

        assert_eq!(svc.loading_len(), 0);
        svc.get_or_request(&first, 160);
        svc.get_or_request(&second, 160);
        svc.get_or_request(&first, 160);

        assert_eq!(svc.loading_len(), 2);

        svc.on_decoded(first, 0, Some(fake_rgba()));
        assert_eq!(svc.loading_len(), 1);
    }
}
