//! Rayon-backed thumbnail decode worker.
//!
//! The worker pool is deliberately toolkit-free: it produces raw RGBA8 bytes
//! (and dimensions) that the UI thread turns into a `Handle`.  This keeps the
//! worker fully unit-testable without a running compositor.
//!
//! The pending queue is bounded. When full, prioritized enqueue drops the
//! farthest pending job, while FIFO enqueue drops the oldest pending job.
//!
//! Each job carries the folder generation at submission time so stale results
//! can be ignored after navigation.

use std::collections::VecDeque;
use std::path::Path;
use std::path::PathBuf;
use std::sync::mpsc;

/// A decoded-thumbnail result delivered from the worker to the UI thread.
#[derive(Debug)]
pub struct DecodeResult {
    pub path: PathBuf,
    /// Generation at the time the job was *enqueued*.
    pub gen: u64,
    /// `Some((bytes, width, height))` on success; `None` on failure.
    pub rgba: Option<(Vec<u8>, u32, u32)>,
}

/// A pending decode job.
#[derive(Debug)]
#[allow(dead_code)]
struct DecodeJob {
    path: PathBuf,
    size: u16,
    gen: u64,
    priority: f32,
}

/// The rayon-backed decode worker with a bounded FIFO pending queue.
#[allow(dead_code)]
pub struct ThumbWorker {
    /// Bounded ring of pending jobs.  Flush drains front-to-back.
    pending: VecDeque<DecodeJob>,
    queue_cap: usize,
    /// Sender half of the result channel; kept here so workers can clone it.
    tx: mpsc::Sender<DecodeResult>,
    /// Rayon thread-pool sized to available parallelism with UI headroom.
    pool: rayon::ThreadPool,
}

fn decode_thread_count() -> usize {
    let available = std::thread::available_parallelism()
        .map(usize::from)
        .unwrap_or(1);
    decode_thread_count_for(available)
}

fn decode_thread_count_for(available: usize) -> usize {
    available.saturating_sub(1).max(1)
}

#[allow(dead_code)]
impl ThumbWorker {
    /// Create a new worker with the given pending-queue capacity.
    ///
    /// Returns `(worker, receiver)`.  The receiver must be drained by the UI
    /// thread (see `ThumbService`).
    pub fn new(queue_cap: usize) -> (Self, mpsc::Receiver<DecodeResult>) {
        let (tx, rx) = mpsc::channel();
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(decode_thread_count())
            .thread_name(|i| format!("photobrowser-thumb-{i}"))
            .build()
            .expect("failed to create rayon thumb pool");

        let worker = ThumbWorker {
            pending: VecDeque::with_capacity(queue_cap),
            queue_cap,
            tx,
            pool,
        };
        (worker, rx)
    }

    /// Enqueue a decode job.
    ///
    /// If the queue is already at capacity the oldest (front) entry is dropped
    /// (newest-visible-wins policy).
    pub fn enqueue(&mut self, path: PathBuf, size: u16, gen: u64) -> Option<PathBuf> {
        let mut evicted = None;
        if self.pending.len() >= self.queue_cap {
            // Drop the oldest pending job.
            evicted = self.pending.pop_front().map(|job| job.path);
        }
        self.pending.push_back(DecodeJob {
            path,
            size,
            gen,
            priority: f32::INFINITY,
        });
        evicted
    }

    /// Enqueue a priority-ordered decode job.
    ///
    /// Lower `priority` values are dispatched first.  When full, the farthest
    /// (highest-priority-value) pending job is dropped so center/visible work is
    /// retained over off-screen margin work.
    pub fn enqueue_prioritized(
        &mut self,
        path: PathBuf,
        size: u16,
        gen: u64,
        priority: f32,
    ) -> Option<PathBuf> {
        let job = DecodeJob {
            path,
            size,
            gen,
            priority,
        };
        let insert_at = self
            .pending
            .iter()
            .position(|pending| pending.priority > priority)
            .unwrap_or(self.pending.len());
        self.pending.insert(insert_at, job);

        if self.pending.len() <= self.queue_cap {
            return None;
        }

        let farthest_index = self
            .pending
            .iter()
            .enumerate()
            .max_by(|(_, a), (_, b)| a.priority.total_cmp(&b.priority))
            .map(|(index, _)| index)
            .expect("overflow means at least one pending job exists");
        self.pending.remove(farthest_index).map(|job| job.path)
    }

    /// Dispatch all currently-pending jobs to the rayon pool, clearing the queue.
    ///
    /// Should be called after enqueuing (e.g. at the end of `get_or_request`
    /// or from the update loop).
    pub fn flush(&mut self) {
        for job in self.pending.drain(..) {
            let tx = self.tx.clone();
            self.pool.spawn(move || {
                let result = decode_thumb(&job.path, job.size);
                let _ = tx.send(DecodeResult {
                    path: job.path,
                    gen: job.gen,
                    rgba: result,
                });
            });
        }
    }

    /// Returns the number of jobs currently sitting in the queue (not yet dispatched).
    #[allow(dead_code)]
    pub fn pending_len(&self) -> usize {
        self.pending.len()
    }

    /// Ensure future enqueues can hold at least `queue_cap` jobs.
    pub fn ensure_queue_cap(&mut self, queue_cap: usize) {
        self.queue_cap = self.queue_cap.max(queue_cap);
    }

    pub fn queue_cap(&self) -> usize {
        self.queue_cap
    }

    pub fn contains_pending_path(&self, path: &Path) -> bool {
        self.pending.iter().any(|job| job.path == path)
    }

    #[cfg(test)]
    pub(crate) fn pending_paths_for_test(&self) -> Vec<PathBuf> {
        self.pending.iter().map(|job| job.path.clone()).collect()
    }

    #[cfg(test)]
    pub(crate) fn send_result_for_test(&self, result: DecodeResult) {
        self.tx
            .send(result)
            .expect("test decode result receiver should be alive");
    }
}

/// Decode a single image file into RGBA8 thumbnail bytes (no toolkit dependency).
// Called from within rayon closures; Rust dead-code analysis doesn't see through closures.
#[allow(dead_code)]
fn decode_thumb(path: &std::path::Path, size: u16) -> Option<(Vec<u8>, u32, u32)> {
    let thumb = if disk_cache_enabled() {
        match crate::thumb::xdg::load(path) {
            Some(img) => {
                tracing::debug!("loaded disk-cached thumb for {}", path.display());
                img
            }
            None => {
                let img = decode_thumbnail_source(path)?;
                let thumb = img.thumbnail(256, 256);
                crate::thumb::xdg::store(path, &thumb);
                thumb
            }
        }
    } else {
        let img = decode_thumbnail_source(path)?;
        img.thumbnail(256, 256)
    };

    let thumb = thumb.thumbnail(size as u32, size as u32);
    // Convert to RGBA8 for use with `Handle::from_rgba`.
    let thumb = thumb.into_rgba8();
    let (w, h) = thumb.dimensions();
    tracing::debug!("decoded thumb {}×{} for {}", w, h, path.display());
    Some((thumb.into_raw(), w, h))
}

fn decode_thumbnail_source(path: &std::path::Path) -> Option<image::DynamicImage> {
    match crate::decode::load_thumbnail_image_reduced(path, 256) {
        Ok(img) => Some(img),
        Err(e) => {
            tracing::warn!("thumb decode failed for {}: {e}", path.display());
            None
        }
    }
}

#[cfg(not(test))]
fn disk_cache_enabled() -> bool {
    true
}

#[cfg(test)]
fn disk_cache_enabled() -> bool {
    std::env::var_os("PHOTOBROWSER_THUMBNAIL_CACHE_DIR").is_some()
}

// ── Alternative: synchronous decode for tests ────────────────────────────────

/// Synchronous decode used in unit tests (bypasses the rayon pool).
#[cfg(test)]
pub(crate) fn decode_sync(path: &std::path::Path, size: u16) -> Option<(Vec<u8>, u32, u32)> {
    decode_thumb(path, size)
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{ImageFormat, RgbImage};
    use std::io::Write as IoWrite;
    use tempfile::TempDir;

    /// Create a tiny 2×2 image in `dir` with the given format and filename.
    fn make_image(dir: &TempDir, filename: &str, fmt: ImageFormat) -> PathBuf {
        let path = dir.path().join(filename);
        let img = RgbImage::from_pixel(2, 2, image::Rgb([128u8, 64, 32]));
        img.save_with_format(&path, fmt)
            .expect("failed to write test image");
        path
    }

    fn make_large_jpeg(dir: &TempDir, filename: &str) -> PathBuf {
        let path = dir.path().join(filename);
        let img = RgbImage::from_fn(1024, 512, |x, y| {
            image::Rgb([(x % 251) as u8, (y % 241) as u8, ((x + y) % 239) as u8])
        });
        img.save_with_format(&path, ImageFormat::Jpeg)
            .expect("failed to write large jpeg");
        path
    }

    /// Serializes every test that calls `decode_sync`. The 256px-cache test mutates the
    /// process-global `PHOTOBROWSER_THUMBNAIL_CACHE_DIR` env var while the other decode tests read it
    /// (`var_os`) — concurrent `set_var`/`var_os` is a data race and made that test flaky under
    /// parallel `cargo test`. Uses the SAME crate-wide lock as the xdg env tests so the two modules'
    /// env mutations never overlap. Poison-tolerant so one test's panic doesn't cascade-fail the rest.
    fn decode_guard() -> std::sync::MutexGuard<'static, ()> {
        crate::thumb::xdg::TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner())
    }

    // ── decode-fixture tests ─────────────────────────────────────────────────
    //
    // Generate tiny images in a tempdir AT TEST TIME.
    // 2×2 px: .jpg, .png, .webp, .gif ⇒ decode yields expected dimensions.
    // Corrupt file ⇒ Failed (None), no panic.

    #[test]
    fn decode_jpg() {
        let _g = decode_guard();
        let dir = tempfile::tempdir().unwrap();
        let path = make_image(&dir, "test.jpg", ImageFormat::Jpeg);
        let result = decode_sync(&path, 160);
        assert!(result.is_some(), "jpeg should decode");
        let (bytes, w, h) = result.unwrap();
        assert!(w <= 160 && h <= 160, "thumbnail must fit in 160×160");
        assert!(!bytes.is_empty());
    }

    #[test]
    fn decode_png() {
        let _g = decode_guard();
        let dir = tempfile::tempdir().unwrap();
        let path = make_image(&dir, "test.png", ImageFormat::Png);
        let result = decode_sync(&path, 160);
        assert!(result.is_some(), "png should decode");
        let (bytes, w, h) = result.unwrap();
        assert!(w <= 160 && h <= 160);
        assert!(!bytes.is_empty());
    }

    #[test]
    fn decode_webp() {
        let _g = decode_guard();
        let dir = tempfile::tempdir().unwrap();
        let path = make_image(&dir, "test.webp", ImageFormat::WebP);
        let result = decode_sync(&path, 160);
        assert!(result.is_some(), "webp should decode");
        let (bytes, w, h) = result.unwrap();
        assert!(w <= 160 && h <= 160);
        assert!(!bytes.is_empty());
    }

    #[test]
    fn decode_gif() {
        let _g = decode_guard();
        let dir = tempfile::tempdir().unwrap();
        let path = make_image(&dir, "test.gif", ImageFormat::Gif);
        let result = decode_sync(&path, 160);
        assert!(result.is_some(), "gif should decode");
        let (bytes, w, h) = result.unwrap();
        assert!(w <= 160 && h <= 160);
        assert!(!bytes.is_empty());
    }

    #[test]
    fn decode_jpeg_stores_256px_cache_entry_after_reduced_decode() {
        let _g = decode_guard();
        let dir = tempfile::tempdir().unwrap();
        let cache_dir = tempfile::tempdir().unwrap();
        let path = make_large_jpeg(&dir, "large-no-thumb.jpg");
        let old_cache = std::env::var_os("PHOTOBROWSER_THUMBNAIL_CACHE_DIR");
        std::env::set_var("PHOTOBROWSER_THUMBNAIL_CACHE_DIR", cache_dir.path());

        let result = decode_sync(&path, 96);

        match &old_cache {
            Some(value) => std::env::set_var("PHOTOBROWSER_THUMBNAIL_CACHE_DIR", value),
            None => std::env::remove_var("PHOTOBROWSER_THUMBNAIL_CACHE_DIR"),
        }

        let (bytes, w, h) = result.expect("large jpeg should decode");
        assert!(w <= 96 && h <= 96);
        assert!(!bytes.is_empty());

        std::env::set_var("PHOTOBROWSER_THUMBNAIL_CACHE_DIR", cache_dir.path());
        let cached = crate::thumb::xdg::load(&path).expect("256px disk-cache entry should exist");
        match &old_cache {
            Some(value) => std::env::set_var("PHOTOBROWSER_THUMBNAIL_CACHE_DIR", value),
            None => std::env::remove_var("PHOTOBROWSER_THUMBNAIL_CACHE_DIR"),
        }
        assert_eq!(cached.width(), 256);
        assert_eq!(cached.height(), 128);
    }

    #[test]
    fn decode_corrupt_no_panic() {
        let _g = decode_guard();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("corrupt.jpg");
        // Write garbage bytes with .jpg extension.
        let mut f = std::fs::File::create(&path).unwrap();
        f.write_all(b"this is not a jpeg \x00\xff\xd8 garbage")
            .unwrap();
        drop(f);

        let result = decode_sync(&path, 160);
        assert!(
            result.is_none(),
            "corrupt file should return None, not panic"
        );
    }

    /// Env-gated thumbnail decode micro-benchmark.
    ///
    /// Uses the same synchronous wrapper around the worker decode path as the
    /// existing decode tests: `decode_thumb` calls `decode::load_thumbnail_image`,
    /// builds the 256px intermediate thumbnail, then downsizes to the requested
    /// size. The test is ignored and gracefully skips when the fixture directory
    /// is absent so normal test runs and CI are unaffected.
    ///
    /// Re-run:
    ///   cargo test --release thumbnail_decode_request_path_microbench -- --ignored --nocapture
    #[test]
    #[ignore = "env-gated performance benchmark"]
    fn thumbnail_decode_request_path_microbench() {
        use crate::view::grid::{
            grid_available_h, grid_available_w, visible_range, MARGIN_ROWS, SCROLLBAR_W,
        };
        use std::path::{Path, PathBuf};
        use std::time::Instant;

        const REPRESENTATIVE_VIEWPORT_W: f32 = 1920.0;
        const REPRESENTATIVE_VIEWPORT_H: f32 = 1000.0;
        const BENCH_THUMB_SMALL: u16 = 96;
        const BENCH_THUMB_LARGE: u16 = 256;
        const QUEUE_CAP: usize = 256;

        #[derive(Debug)]
        struct DecodeStats {
            decoded: usize,
            failures: usize,
            total_ms: f64,
            mean_ms: f64,
            median_ms: f64,
            p95_ms: f64,
            viewport_total_ms: f64,
        }

        fn collect_pngs(dir: &Path) -> Vec<PathBuf> {
            let mut paths: Vec<PathBuf> = std::fs::read_dir(dir)
                .expect("fixture directory should be readable")
                .filter_map(Result::ok)
                .map(|entry| entry.path())
                .filter(|path| {
                    path.extension()
                        .and_then(|ext| ext.to_str())
                        .is_some_and(|ext| ext.eq_ignore_ascii_case("png"))
                })
                .collect();
            paths.sort();
            paths
        }

        fn percentile(sorted_ms: &[f64], percentile: f64) -> f64 {
            if sorted_ms.is_empty() {
                return 0.0;
            }
            let rank = ((sorted_ms.len() - 1) as f64 * percentile).ceil() as usize;
            sorted_ms[rank.min(sorted_ms.len() - 1)]
        }

        fn bench_decode(paths: &[PathBuf], size: u16, viewport_batch: usize) -> DecodeStats {
            let mut per_image_ms = Vec::with_capacity(paths.len());
            let mut failures = 0usize;
            let total_start = Instant::now();

            for path in paths {
                let start = Instant::now();
                if decode_sync(path, size).is_none() {
                    failures += 1;
                }
                per_image_ms.push(start.elapsed().as_secs_f64() * 1000.0);
            }

            let total_ms = total_start.elapsed().as_secs_f64() * 1000.0;
            per_image_ms.sort_by(|a, b| a.total_cmp(b));
            let decoded = paths.len().saturating_sub(failures);
            let mean_ms = if per_image_ms.is_empty() {
                0.0
            } else {
                per_image_ms.iter().sum::<f64>() / per_image_ms.len() as f64
            };
            let median_ms = percentile(&per_image_ms, 0.50);
            let p95_ms = percentile(&per_image_ms, 0.95);
            let viewport_total_ms = per_image_ms.iter().take(viewport_batch).sum::<f64>();

            DecodeStats {
                decoded,
                failures,
                total_ms,
                mean_ms,
                median_ms,
                p95_ms,
                viewport_total_ms,
            }
        }

        let Ok(fixture_dir) = std::env::var("PHOTOBROWSER_THUMB_BENCH_DIR") else {
            eprintln!("SKIP: set PHOTOBROWSER_THUMB_BENCH_DIR to a local fixture directory");
            return;
        };
        let fixture_dir = PathBuf::from(fixture_dir);
        if !fixture_dir.is_dir() {
            eprintln!("SKIP: fixture directory not found");
            return;
        }

        let mut paths = collect_pngs(&fixture_dir);
        if let Ok(limit) = std::env::var("PHOTOBROWSER_THUMB_BENCH_LIMIT") {
            let limit = limit
                .parse::<usize>()
                .expect("PHOTOBROWSER_THUMB_BENCH_LIMIT must be a positive integer");
            paths.truncate(limit);
        }
        if paths.is_empty() {
            eprintln!(
                "SKIP thumbnail_decode_request_path_microbench: no PNG fixtures in {}",
                fixture_dir.display()
            );
            return;
        }

        let pane_w = grid_available_w(REPRESENTATIVE_VIEWPORT_W, true) - SCROLLBAR_W;
        let pane_h = grid_available_h(REPRESENTATIVE_VIEWPORT_H);
        let cols = ((pane_w / BENCH_THUMB_SMALL as f32).floor() as usize).max(1);
        let cell_h = crate::app::cell_h(BENCH_THUMB_SMALL);
        let envelope = visible_range(0.0, pane_h, cell_h, cols, paths.len(), MARGIN_ROWS);
        let viewport_batch = envelope.len().min(paths.len());

        println!(
            "fixture={} png_count={}",
            fixture_dir.display(),
            paths.len()
        );
        println!(
            "geometry thumb={}px viewport={}x{} pane={:.1}x{:.1} cols={} cell_h={:.1} margin_rows={} envelope_cells={} queue_cap={} exceeds_queue_cap={}",
            BENCH_THUMB_SMALL,
            REPRESENTATIVE_VIEWPORT_W,
            REPRESENTATIVE_VIEWPORT_H,
            pane_w,
            pane_h,
            cols,
            cell_h,
            MARGIN_ROWS,
            envelope.len(),
            QUEUE_CAP,
            envelope.len() > QUEUE_CAP
        );

        let small = bench_decode(&paths, BENCH_THUMB_SMALL, viewport_batch);
        let large = bench_decode(&paths, BENCH_THUMB_LARGE, viewport_batch);

        println!(
            "decode size={}px decoded={} failures={} total_ms={:.2} mean_ms={:.4} median_ms={:.4} p95_ms={:.4} viewport_batch_n={} viewport_total_ms={:.2}",
            BENCH_THUMB_SMALL,
            small.decoded,
            small.failures,
            small.total_ms,
            small.mean_ms,
            small.median_ms,
            small.p95_ms,
            viewport_batch,
            small.viewport_total_ms
        );
        println!(
            "decode size={}px decoded={} failures={} total_ms={:.2} mean_ms={:.4} median_ms={:.4} p95_ms={:.4} viewport_batch_n={} viewport_total_ms={:.2}",
            BENCH_THUMB_LARGE,
            large.decoded,
            large.failures,
            large.total_ms,
            large.mean_ms,
            large.median_ms,
            large.p95_ms,
            viewport_batch,
            large.viewport_total_ms
        );

        assert_eq!(small.failures, 0, "all 96px fixtures should decode");
        assert_eq!(large.failures, 0, "all 256px fixtures should decode");
    }

    #[test]
    fn decode_thread_count_leaves_ui_headroom() {
        assert_eq!(decode_thread_count_for(1), 1);
        assert_eq!(decode_thread_count_for(2), 1);
        assert_eq!(decode_thread_count_for(4), 3);
        assert_eq!(decode_thread_count_for(8), 7);
    }

    // ── bounded-queue test ───────────────────────────────────────────────────
    //
    // queue_cap=4, push 10 ⇒ only 4 pending, newest kept.
    #[test]
    fn bounded_queue() {
        let (mut worker, _rx) = ThumbWorker::new(4);

        for i in 0..10usize {
            worker.enqueue(PathBuf::from(format!("/p/{i}")), 160, 0);
        }

        // Only 4 should be pending (the newest 4: /p/6 … /p/9).
        assert_eq!(worker.pending_len(), 4, "queue must be bounded to cap=4");

        let pending_paths: Vec<_> = worker.pending.iter().map(|j| j.path.clone()).collect();
        for i in 6..10usize {
            assert!(
                pending_paths.contains(&PathBuf::from(format!("/p/{i}"))),
                "/p/{i} should be in the queue"
            );
        }
        for i in 0..6usize {
            assert!(
                !pending_paths.contains(&PathBuf::from(format!("/p/{i}"))),
                "/p/{i} should have been dropped (oldest)"
            );
        }
    }

    #[test]
    fn prioritized_enqueue_keeps_fifo_center_outward_order() {
        let (mut worker, _rx) = ThumbWorker::new(4);

        worker.enqueue_prioritized(PathBuf::from("/p/far"), 160, 0, 9.0);
        worker.enqueue_prioritized(PathBuf::from("/p/center"), 160, 0, 0.0);
        worker.enqueue_prioritized(PathBuf::from("/p/near"), 160, 0, 1.0);

        let pending_paths = worker.pending_paths_for_test();
        assert_eq!(
            pending_paths,
            vec![
                PathBuf::from("/p/center"),
                PathBuf::from("/p/near"),
                PathBuf::from("/p/far"),
            ],
            "flush drains front-to-back, so prioritized pending order is dispatch order"
        );
    }

    #[test]
    fn prioritized_overflow_evicts_farthest_pending_job() {
        let (mut worker, _rx) = ThumbWorker::new(3);

        worker.enqueue_prioritized(PathBuf::from("/p/center"), 160, 0, 0.0);
        worker.enqueue_prioritized(PathBuf::from("/p/near"), 160, 0, 1.0);
        worker.enqueue_prioritized(PathBuf::from("/p/far"), 160, 0, 4.0);
        let evicted = worker.enqueue_prioritized(PathBuf::from("/p/mid"), 160, 0, 2.0);

        assert_eq!(evicted, Some(PathBuf::from("/p/far")));
        assert_eq!(
            worker.pending_paths_for_test(),
            vec![
                PathBuf::from("/p/center"),
                PathBuf::from("/p/near"),
                PathBuf::from("/p/mid"),
            ],
            "the farthest queued job is the overflow victim, not the visible center"
        );
    }
}
