use std::collections::HashMap;
use std::path::PathBuf;

use cosmic::prelude::*;

use crate::app::{filter_matches, parse_exif_datetime, Message};
use crate::config::SortMode;
use crate::metadata::read_exif;
use crate::scan::{self, Entry, EntryKind};

#[derive(Default)]
pub struct BrowserState {
    pub current_dir: Option<PathBuf>,
    pub entries: Vec<Entry>,
    pub capture_epoch_cache: HashMap<PathBuf, Option<i64>>,
    pub capture_epochs_pending: usize,
    pub filter_query: String,
}

impl BrowserState {
    /// Re-scan `current_dir` and update `entries`.
    pub fn reload(&mut self, show_hidden: bool, sort_mode: SortMode) {
        if let Some(dir) = &self.current_dir {
            match scan::scan_dir(dir, show_hidden) {
                Ok(entries) => {
                    self.entries = entries;
                    self.sort_for_mode(sort_mode);
                }
                Err(e) => {
                    tracing::warn!("scan_dir failed: {e}");
                    self.entries.clear();
                }
            }
        } else {
            self.entries.clear();
        }
    }

    pub fn sort_for_mode(&mut self, sort_mode: SortMode) {
        match sort_mode {
            SortMode::Name | SortMode::Date => scan::sort_entries(&mut self.entries, sort_mode),
            SortMode::Captured => {
                sort_entries_by_captured(&mut self.entries, &self.capture_epoch_cache)
            }
            SortMode::Rating => {
                // Browser is pure: no rating data here. Sort by name as the stable base;
                // the rating ordering (desc, then name) is applied to indices in AppModel.
                scan::sort_entries(&mut self.entries, SortMode::Name)
            }
        }
    }

    pub fn set_filter(&mut self, q: String) {
        self.filter_query = q;
    }

    pub fn clear_filter(&mut self) {
        self.filter_query.clear();
    }

    pub fn request_capture_epoch_batch(
        &mut self,
        sort_mode: SortMode,
        images_only: bool,
    ) -> Task<cosmic::Action<Message>> {
        if sort_mode != SortMode::Captured {
            self.capture_epochs_pending = 0;
            return Task::none();
        }

        // Collect the uncached paths first (owned) so the immutable borrow of
        // `self.entries` ends before we record the pending count.
        let paths: Vec<PathBuf> = self
            .grid_indices(images_only)
            .into_iter()
            .take(128)
            .filter_map(|idx| self.entries.get(idx))
            .filter(|entry| matches!(entry.kind, EntryKind::Image | EntryKind::Raw))
            .filter(|entry| !self.capture_epoch_cache.contains_key(&entry.path))
            .map(|entry| entry.path.clone())
            .collect();

        self.capture_epochs_pending = paths.len();
        let tasks: Vec<_> = paths.into_iter().map(Self::capture_epoch_task).collect();
        Task::batch(tasks)
    }

    /// Record one capture-epoch result. Returns `true` when this result drains
    /// the in-flight batch (caller should then re-sort + rebuild + request the
    /// next batch) — so the list is sorted ONCE per batch, not once per result.
    /// Results arriving outside a tracked batch (stale / not Capture sort) are
    /// cached without signalling a re-sort.
    pub fn record_capture_epoch(
        &mut self,
        sort_mode: SortMode,
        path: PathBuf,
        epoch: Option<i64>,
    ) -> bool {
        self.capture_epoch_cache.insert(path, epoch);
        if sort_mode != SortMode::Captured || self.capture_epochs_pending == 0 {
            return false;
        }
        self.capture_epochs_pending -= 1;
        self.capture_epochs_pending == 0
    }

    pub fn displayed_indices(&self, images_only: bool) -> Vec<usize> {
        displayed_indices(&self.entries, &self.filter_query, images_only)
    }

    pub fn grid_indices(&self, images_only: bool) -> Vec<usize> {
        grid_indices_for(&self.entries, &self.filter_query, images_only)
    }

    fn capture_epoch_task(path: PathBuf) -> Task<cosmic::Action<Message>> {
        Task::perform(
            async move {
                let epoch = read_exif(&path)
                    .captured_date
                    .as_deref()
                    .and_then(parse_exif_datetime);
                (path, epoch)
            },
            |(path, epoch)| cosmic::Action::App(Message::CaptureEpochLoaded(path, epoch)),
        )
    }
}

pub(crate) fn sort_entries_by_captured(
    entries: &mut [Entry],
    cache: &HashMap<PathBuf, Option<i64>>,
) {
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

        let epoch_for = |entry: &Entry| {
            cache.get(&entry.path).and_then(|value| *value).or_else(|| {
                entry
                    .modified
                    .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|duration| duration.as_secs() as i64)
            })
        };

        match (epoch_for(a), epoch_for(b)) {
            (Some(a_epoch), Some(b_epoch)) => b_epoch.cmp(&a_epoch),
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (None, None) => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
        }
    });
}

pub(crate) fn grid_indices_for(entries: &[Entry], query: &str, images_only: bool) -> Vec<usize> {
    entries
        .iter()
        .enumerate()
        .filter_map(|(idx, entry)| {
            if !filter_matches(&entry.name, query) {
                return None;
            }
            if images_only {
                match entry.kind {
                    EntryKind::Dir | EntryKind::Image | EntryKind::Raw => Some(idx),
                    EntryKind::Other(_) => None,
                }
            } else {
                Some(idx)
            }
        })
        .collect()
}

pub(crate) fn displayed_indices(entries: &[Entry], query: &str, _images_only: bool) -> Vec<usize> {
    entries
        .iter()
        .enumerate()
        .filter_map(|(idx, entry)| match entry.kind {
            EntryKind::Image | EntryKind::Raw if filter_matches(&entry.name, query) => Some(idx),
            EntryKind::Dir | EntryKind::Other(_) => None,
            EntryKind::Image | EntryKind::Raw => None,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{displayed_indices, grid_indices_for, sort_entries_by_captured, BrowserState};
    use crate::config::SortMode;
    use crate::scan::{Entry, EntryKind, FileCategory};
    use std::path::PathBuf;
    use std::time::{Duration, UNIX_EPOCH};

    fn test_entry(name: &str, kind: EntryKind) -> Entry {
        Entry {
            path: PathBuf::from(name),
            name: name.to_owned(),
            kind,
            modified: None,
            size: 0,
        }
    }

    #[test]
    fn capture_epoch_results_coalesce_into_one_resort() {
        let mut browser = BrowserState {
            entries: vec![
                test_entry("/a.jpg", EntryKind::Image),
                test_entry("/b.jpg", EntryKind::Image),
                test_entry("/c.jpg", EntryKind::Image),
            ],
            capture_epochs_pending: 3,
            ..BrowserState::default()
        };

        // The first two results only cache + decrement — no re-sort signal yet.
        assert!(!browser.record_capture_epoch(
            SortMode::Captured,
            PathBuf::from("/a.jpg"),
            Some(100)
        ));
        assert_eq!(browser.capture_epochs_pending, 2);
        assert!(!browser.record_capture_epoch(
            SortMode::Captured,
            PathBuf::from("/b.jpg"),
            Some(200)
        ));
        assert_eq!(browser.capture_epochs_pending, 1);

        // The final result drains the batch → signals exactly one re-sort.
        assert!(browser.record_capture_epoch(
            SortMode::Captured,
            PathBuf::from("/c.jpg"),
            Some(300)
        ));
        assert_eq!(browser.capture_epochs_pending, 0);
        assert_eq!(browser.capture_epoch_cache.len(), 3);

        // A stale/extra result outside a tracked batch caches but never
        // re-signals a sort (no underflow, no storm).
        assert!(!browser.record_capture_epoch(
            SortMode::Captured,
            PathBuf::from("/d.jpg"),
            Some(400)
        ));
        assert_eq!(browser.capture_epochs_pending, 0);
    }

    #[test]
    fn record_capture_epoch_no_resort_when_not_capture_sort() {
        let mut browser = BrowserState {
            capture_epochs_pending: 5,
            ..BrowserState::default()
        }; // ignored outside Capture sort
        assert!(!browser.record_capture_epoch(SortMode::Name, PathBuf::from("/x.jpg"), Some(1)));
        assert_eq!(browser.capture_epochs_pending, 5);
        assert_eq!(
            browser.capture_epoch_cache.get(&PathBuf::from("/x.jpg")),
            Some(&Some(1))
        );
    }

    #[test]
    fn displayed_indices_image_raw_only_and_in_order() {
        let entries = vec![
            test_entry("folder", EntryKind::Dir),
            test_entry("photo.jpg", EntryKind::Image),
            test_entry("notes.txt", EntryKind::Other(FileCategory::Document)),
            test_entry("raw.nef", EntryKind::Raw),
            test_entry("archive.zip", EntryKind::Other(FileCategory::Archive)),
        ];

        assert_eq!(displayed_indices(&entries, "", false), vec![1, 3]);
    }

    #[test]
    fn captured_sort_uses_epoch_then_mtime_fallback_dirs_first() {
        use std::collections::HashMap;

        let base = UNIX_EPOCH + Duration::from_secs(1_000);
        let mut entries = vec![
            Entry {
                path: PathBuf::from("/x/fallback-new.jpg"),
                name: "fallback-new.jpg".into(),
                kind: EntryKind::Image,
                modified: Some(base + Duration::from_secs(20)),
                size: 0,
            },
            Entry {
                path: PathBuf::from("/x/captured-old.jpg"),
                name: "captured-old.jpg".into(),
                kind: EntryKind::Image,
                modified: Some(base + Duration::from_secs(30)),
                size: 0,
            },
            Entry {
                path: PathBuf::from("/x/dir"),
                name: "dir".into(),
                kind: EntryKind::Dir,
                modified: None,
                size: 0,
            },
            Entry {
                path: PathBuf::from("/x/captured-new.jpg"),
                name: "captured-new.jpg".into(),
                kind: EntryKind::Image,
                modified: Some(base + Duration::from_secs(10)),
                size: 0,
            },
        ];
        let mut cache = HashMap::new();
        cache.insert(PathBuf::from("/x/captured-old.jpg"), Some(2_000));
        cache.insert(PathBuf::from("/x/captured-new.jpg"), Some(3_000));

        sort_entries_by_captured(&mut entries, &cache);

        let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(
            names,
            vec![
                "dir",
                "captured-new.jpg",
                "captured-old.jpg",
                "fallback-new.jpg"
            ]
        );
    }

    #[test]
    fn filtered_indices_keep_grid_position_to_entry_index_round_trip() {
        let entries = vec![
            test_entry("album", EntryKind::Dir),
            test_entry("vacation.jpg", EntryKind::Image),
            test_entry("notes.txt", EntryKind::Other(FileCategory::Document)),
            test_entry("vacation.nef", EntryKind::Raw),
            test_entry("archive.zip", EntryKind::Other(FileCategory::Archive)),
        ];

        let grid = grid_indices_for(&entries, "vacation", false);
        assert_eq!(grid, vec![1, 3]);
        assert_eq!(entries[grid[0]].name, "vacation.jpg");
        assert_eq!(entries[grid[1]].name, "vacation.nef");
        assert_eq!(displayed_indices(&entries, "vacation", false), vec![1, 3]);
    }

    #[test]
    fn images_only_filter_keeps_dirs_and_images_raw_drops_documents() {
        let entries = vec![
            test_entry("album", EntryKind::Dir),
            test_entry("vacation.jpg", EntryKind::Image),
            test_entry("notes.txt", EntryKind::Other(FileCategory::Document)),
            test_entry("raw.nef", EntryKind::Raw),
            test_entry("movie.mp4", EntryKind::Other(FileCategory::Video)),
        ];

        // images_only=true: drops docs/videos, keeps dirs + images + raws (matching query="")
        let grid_only = grid_indices_for(&entries, "", true);
        assert_eq!(grid_only, vec![0, 1, 3]);

        // images_only=false: keeps all (query match)
        let grid_all = grid_indices_for(&entries, "", false);
        assert_eq!(grid_all, vec![0, 1, 2, 3, 4]);

        // query still works alongside
        let grid_only_vac = grid_indices_for(&entries, "vac", true);
        assert_eq!(grid_only_vac, vec![1]);

        // displayed_indices unaffected by images_only (still only image/raw)
        assert_eq!(displayed_indices(&entries, "", true), vec![1, 3]);
        assert_eq!(displayed_indices(&entries, "", false), vec![1, 3]);
    }

    // ─────────────────────────────────────────────────────────────────────────
    // 10k (and 50k) performance ceiling micro-benchmark for the grid/listing
    // hot paths (openq D2). Target: interaction-smooth to ~10k entries and
    // memory-bounded to ~50k. Currently only checked ad-hoc; this makes the
    // measurement repeatable and committed.
    //
    // Hard rules observed:
    // - Browse-only fence: ONLY synthetic in-memory `Entry` vectors (generated
    //   deterministically from index). Never touches real FS paths, user files,
    //   or metadata. (tempfile is available but not required for these pure fns.)
    // - No new deps: uses only std::time::Instant + existing dev-dep (none
    //   added; criterion is NOT present in Cargo.toml dev-dependencies).
    // - Pure deterministic hot paths only (no GPU, no event loop, no live UI):
    //   exercises grid_indices_for, displayed_indices, sort_entries_by_captured,
    //   plus the filter+sort pipeline (via crate::scan::sort_entries + indices).
    // - Deterministic + non-flaky: the synthetic input produces fixed outputs.
    //   We ASSERT only correctness (counts, round-trips, shapes). We PRINT the
    //   measured wall times (run with --nocapture) to document the target.
    //   NO time-based assert is performed (avoids flakes on variable CI runners);
    //   a generous intended budget is stated in the comment below (e.g. 10k
    //   filter+sort+indices comfortably <250ms on modern hardware, with large
    //   headroom). This documents the perf ceiling without risking spurious
    //   failures. (If a time assert were used it would be << e.g. 1000ms.)
    //
    // How to run:
    //   cargo test -- --ignored --nocapture
    //   # (or with -q etc.; timings appear on stdout of the ignored test)
    //
    // The test is placed here (inside browser_state.rs mod tests) as the
    // simplest location that can directly access the pub(crate) fns under test
    // without changing production code or existing tests.
    // ─────────────────────────────────────────────────────────────────────────
    fn make_synthetic_entries(n: usize) -> Vec<Entry> {
        use std::time::{Duration, UNIX_EPOCH};
        (0..n)
            .map(|i| {
                let kind = match i % 10 {
                    0 => EntryKind::Dir,
                    1..=5 => EntryKind::Image,
                    6 => EntryKind::Raw,
                    _ => {
                        let cat = match (i / 10) % 6 {
                            0 => FileCategory::Video,
                            1 => FileCategory::Audio,
                            2 => FileCategory::Document,
                            3 => FileCategory::Archive,
                            4 => FileCategory::Code,
                            _ => FileCategory::Unknown,
                        };
                        EntryKind::Other(cat)
                    }
                };
                // Varied names for interesting filter and sort behavior.
                // Inject "vacation", "raw", year-ish tokens on deterministic cadence.
                let name = if i % 23 == 0 {
                    format!("vacation2023_{:04}.jpg", i)
                } else if i % 37 == 0 {
                    format!("rawshot_{:04}.nef", i)
                } else if i % 41 == 0 {
                    format!("capture_{:04}_2024.jpg", i)
                } else if i % 7 == 0 && !matches!(kind, EntryKind::Dir) {
                    format!("item_{:05}_notes.txt", i)
                } else {
                    match kind {
                        EntryKind::Image => format!("photo_{:05}.jpg", i),
                        EntryKind::Raw => format!("raw_{:05}.cr2", i),
                        EntryKind::Dir => format!("album_{:03}", i),
                        _ => format!("misc_{:05}.bin", i),
                    }
                };
                let modified = if i % 5 == 0 {
                    Some(UNIX_EPOCH + Duration::from_secs(1_700_000_000 + (i as u64) * 123))
                } else {
                    None
                };
                Entry {
                    path: std::path::PathBuf::from(format!("/synth/{}", name)),
                    name,
                    kind,
                    modified,
                    size: (i % 1_000_000) as u64 + 1234,
                }
            })
            .collect()
    }

    #[test]
    #[ignore]
    fn perf_10k_filter_sort_grid_indices() {
        use std::time::Instant;

        let n10k: usize = 10_000;
        let entries_10k = make_synthetic_entries(n10k);
        assert_eq!(entries_10k.len(), n10k, "synthetic size");

        // Pre-compute expected counts for determinism (used for asserts, not timing).
        let vac_count = entries_10k
            .iter()
            .filter(|e| e.name.to_lowercase().contains("vacation"))
            .count();
        let img_raw_count = entries_10k
            .iter()
            .filter(|e| matches!(e.kind, EntryKind::Image | EntryKind::Raw))
            .count();
        let img_raw_dir_count = entries_10k
            .iter()
            .filter(|e| matches!(e.kind, EntryKind::Dir | EntryKind::Image | EntryKind::Raw))
            .count();
        let expected_disp_vac = entries_10k
            .iter()
            .filter(|e| {
                matches!(e.kind, EntryKind::Image | EntryKind::Raw)
                    && e.name.to_lowercase().contains("vacation")
            })
            .count();

        // ── 1. sort_entries_by_captured (with partial cache) ─────────────────
        let mut entries = entries_10k.clone();
        let mut cache: std::collections::HashMap<std::path::PathBuf, Option<i64>> =
            std::collections::HashMap::new();
        for (i, e) in entries.iter().enumerate() {
            if i % 5 == 0 {
                cache.insert(e.path.clone(), Some(2_000_000_000 + (i as i64 % 5000)));
            }
        }
        let t0 = Instant::now();
        sort_entries_by_captured(&mut entries, &cache);
        let sort_captured_ms = t0.elapsed().as_secs_f64() * 1000.0;

        // ── 2. filter + grid/displayed indices (query + images_only) ─────────
        let t1 = Instant::now();
        let grid_vac = grid_indices_for(&entries, "vacation", false);
        let disp_vac = displayed_indices(&entries, "vacation", false);
        let grid_empty_all = grid_indices_for(&entries, "", false);
        let grid_empty_images_only = grid_indices_for(&entries, "", true);
        let disp_empty = displayed_indices(&entries, "", false);
        let idx_ms = t1.elapsed().as_secs_f64() * 1000.0;

        // ── 3. filter+sort pipeline (Name sort + indices, representative) ────
        let mut entries2 = entries_10k.clone();
        let t2 = Instant::now();
        crate::scan::sort_entries(&mut entries2, SortMode::Name);
        let _g = grid_indices_for(&entries2, "2024", false);
        let _d = displayed_indices(&entries2, "", false);
        let pipeline_ms = t2.elapsed().as_secs_f64() * 1000.0;

        // ── 4. 50k scale (memory-bounded target; still pure hot path) ────────
        let entries_50k = make_synthetic_entries(50_000);
        let t50 = Instant::now();
        let mut e50 = entries_50k;
        sort_entries_by_captured(&mut e50, &std::collections::HashMap::new());
        let _g50 = grid_indices_for(&e50, "", false);
        let _d50 = displayed_indices(&e50, "raw", false);
        let ms50 = t50.elapsed().as_secs_f64() * 1000.0;

        println!(
            "PERF_BENCH_10k: sort_captured={:.3}ms filter+indices={:.3}ms name_sort+pipeline={:.3}ms | 50k_scale={:.3}ms (n10k={} vac={} img_raw={})",
            sort_captured_ms, idx_ms, pipeline_ms, ms50, n10k, vac_count, img_raw_count
        );

        // Deterministic correctness asserts only (non-flaky; documents counts for the
        // chosen synthetic mix). These prove the measured paths produced the right
        // results for the 10k/50k inputs.
        assert_eq!(grid_vac.len(), vac_count);
        assert_eq!(disp_vac.len(), expected_disp_vac);
        assert_eq!(grid_empty_all.len(), n10k);
        assert_eq!(grid_empty_images_only.len(), img_raw_dir_count);
        assert_eq!(disp_empty.len(), img_raw_count);
        // Spot-check a couple indices round-trip to names for the query.
        if !grid_vac.is_empty() {
            assert!(entries[grid_vac[0]]
                .name
                .to_lowercase()
                .contains("vacation"));
        }
        if !disp_vac.is_empty() {
            let k = &entries[disp_vac[0]].kind;
            assert!(matches!(k, EntryKind::Image | EntryKind::Raw));
        }

        // Sanity on 50k (no exact counts needed beyond non-empty for scale exercise)
        assert_eq!(e50.len(), 50_000);
        assert!(_g50.len() <= 50_000);

        // Target documentation (see top-of-test comment for run instructions and
        // rationale for "print + correctness only"):
        //   - 10k filter + sort + indices pipeline should be interaction-smooth
        //     (comfortably < ~100ms, here with generous CI-safe headroom intent).
        //   - 50k demonstrates memory-bounded behavior (no OOM, still runs in
        //     the hot path timing).
        // Measured numbers are emitted on each run (use --nocapture).
    }
}
