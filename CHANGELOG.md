# PhotoBrowser Changelog

All entries below are shortened to actual feature, behavior, or user-visible fix changes only. Internal refactors, test counts, implementation gates, agent/process notes, and planning-file references are intentionally omitted.

## 1.36.0 — 2026-06-22

- Develop-look rendering now supports highlights, shadows, whites, and blacks from XMP/ACR settings.

## 1.35.0 — 2026-06-21

- Added a Settings → Develop-look toggle in the loupe to preview approximate XMP/ACR edits.
- Develop-look previews now apply exposure, contrast, tone curve, vibrance, and saturation.
- Preview cache keys now account for Develop-look state so toggling updates immediately.

## 1.34.0 — 2026-06-21

- Added the core Develop-look pipeline for approximate Adobe Camera Raw style rendering.
- Added exposure, contrast, tone-curve, vibrance, and saturation support for demosaiced images.

## 1.33.0 — 2026-06-21

- Added parsing for Adobe Camera Raw XMP settings, including exposure, contrast, tone curves, temperature/tint, vibrance, saturation, highlights, shadows, whites, and blacks.

## 1.32.2 — 2026-06-21

- Fixed Full-RAW / Actual zoom in the loupe rendering black until the window was resized.

## 1.32.1 — 2026-06-21

- Fixed loupe repainting during arrow-key navigation so the newly selected image appears immediately.

## 1.32.0 — 2026-06-21

- Added smart collections for saved filter sets.
- Added View → Collections → Save current filters.
- Saved collections can restore rating, label, camera, date, and tag filters.

## 1.31.0 — 2026-06-21

- Added File → Export selection.
- Export copies selected photos and their XMP sidecars to a chosen export folder.
- Export skips existing files instead of overwriting them.
- Source images remain untouched during export.

## 1.30.0 — 2026-06-21

- Added batch keyword removal across the current selection.
- Keyword add and remove workflows are now symmetric for multi-select.

## 1.29.0 — 2026-06-21

- Added keyboard shortcuts for folder navigation:
  - Alt+Left: back
  - Alt+Right: forward
  - Alt+Up: parent folder

## 1.28.2 — 2026-06-21

- XMP sidecar files are now hidden from the photo grid by default.
- Sidecars only appear when show-hidden is enabled.

## 1.28.1 — 2026-06-21

- Fixed RAW capture dates not appearing in the View → Date filter for hyphenated EXIF date formats.
- Existing cached metadata is refreshed so older catalog entries can populate capture dates correctly.

## 1.28.0 — 2026-06-21

- Added batch keyword tagging for multi-selected photos.
- The preview-pane keyword box now applies to the full current selection.
- Tag filters refresh immediately after batch tagging.

## 1.27.0 — 2026-06-21

- Added keyword editing in the preview pane.
- Existing keywords can be removed from a photo.
- New keywords can be added to a photo.
- Keyword edits are written to XMP sidecars.

## 1.26.0 — 2026-06-21

- Added View → Tag filtering.
- Tags are read from XMP sidecars.
- Tag filters compose with camera, date, rating, label, and duplicate filters.

## 1.25.0 — 2026-06-21

- Added XMP keyword read/write support.
- Keywords are stored in XMP sidecars using `dc:subject`.
- Existing sidecar metadata is preserved when keywords are written.

## 1.24.0 — 2026-06-21

- Added catalog support for tags.
- Added tag/file relationships for future tag filtering and approval workflows.

## 1.23.0 — 2026-06-21

- Added Find Exact Duplicates.
- Exact duplicates are detected using byte-for-byte SHA-256 matching.
- Exact duplicate mode complements the existing near-duplicate finder.
- Duplicate hash results are cached for faster re-scans.

## 1.22.0 — 2026-06-21

- Added View → Date filtering by capture year.
- Date filtering composes with existing camera, rating, and duplicate filters.

## 1.21.0 — 2026-06-21

- Added a luminance histogram to the full-screen loupe.
- Added Canon CR3 camera and capture-date extraction for supported CR3 files.
- Fixed stray quote characters in CR3 camera labels.

## 1.20.0 — 2026-06-20

- Added View → Camera filtering.
- Camera metadata is indexed in the background when opening a folder.
- Camera filters compose with rating, label, hide-rejected, and duplicate filters.

## 1.19.0 — 2026-06-20

- Added catalog indexing for capture date and camera metadata.
- Improved catalog performance for metadata scans.

## 1.18.0 — 2026-06-20

- Added a SQLite catalog for cached image metadata and duplicate-scan data.
- Duplicate scans now persist perceptual hashes between sessions.

## 1.17.0 — 2026-06-20

- Added color-coded duplicate groups in Find Duplicates mode.
- Fixed the loupe image painting on first open in a known render-bug case.

## 1.16.0 — 2026-06-20

- Added Find Duplicates for visually similar near-duplicate images.
- Duplicate detection filters the grid to duplicate groups.
- Duplicate detection is review-only and does not delete, move, or modify source files.

## 1.15.0 — 2026-06-20

- Added Canon CR3 embedded-preview decoding.
- Added focal-point-preserving zoom in the loupe.

## 1.14.0 — 2026-06-20

- Added a luminance histogram to the preview panel.

## 1.13.0 — 2026-06-20

- Added command-line folder opening with `photobrowser [PATH]`.
- Added a UI smoke harness for visual review.

## 1.12.0 — 2026-06-20

- Reworded loupe quality settings so the two quality toggles are clearer.
- Improved RAW preview extraction by trying embedded JPEG candidates in preference order.

## 1.11.1 — 2026-06-20

- Fixed a loupe issue where images could stay blank until navigating away and back.

## 1.11.0 — 2026-06-20

- Added keyboard shortcuts for culling in the grid:
  - 1–5: set star rating
  - 0: clear rating
  - 6–9: set color label
  - x: mark reject

## 1.10.0 — 2026-06-20

- Added batch cull actions for multi-selected photos.
- Multi-selection can now apply rating, color label, reject, or unreject actions in one operation.

## 1.9.0 — 2026-06-19

- Added a menu bar with File, View, and Settings menus.
- Moved display toggles, sort controls, label filters, and preferences into menus.
- Simplified the main toolbar.

## 1.8.0 — 2026-06-19

- Added multi-select with Ctrl-click, Shift-click, and Ctrl+A.
- Added 2-up Compare view for selected photos.

## 1.7.0 — 2026-06-18

- Added color labels for photos.
- Added reject marking.
- Added filtering by color label.
- Added a Hide rejects toggle.
- Labels and reject states are stored in XMP sidecars.

## 1.6.0 — 2026-06-18

- Added Sort by rating.

## 1.5.0 — 2026-06-18

- Added star ratings in the grid.
- Added filtering by star rating.
- Fixed missing star and toolbar/breadcrumb label rendering.

## 1.4.0 — 2026-06-18

- Added XMP star ratings.
- Added XMP info in the loupe for camera, exposure, f-number, ISO, and edited-state indicators.
- Ratings are written to XMP sidecars without modifying original image files.

## 1.3.0 — 2026-06-18

- Added a clearer placeholder icon for unknown or uncategorized files.

## 1.2.0 — 2026-06-18

- Added adjustable loupe zoom.
- Added a setting to show or hide the loupe filmstrip.

## 1.1.2 — 2026-06-18

- Fixed black-frame rendering when switching from Actual zoom back to Fit.
- Fixed drag-to-pan occasionally staying active after mouse release.

## 1.1.1 — 2026-06-16

- Added click-drag panning in Actual/full-res loupe zoom.
- Fixed drag-pan jitter.

## 1.1.0 — 2026-06-16

- Moved settings UI into a libcosmic context drawer.
- Added optional native-resolution Actual loupe zoom.

## 1.0.0 — 2026-06-16

- Released PhotoBrowser 1.0 as a fast, browse-only photo browser for COSMIC desktop.
- Added a three-pane layout with sidebar, thumbnail grid, preview, and metadata panel.
- Added virtualized thumbnail browsing.
- Added bounded thumbnail cache and memory behavior.
- Added RAW embedded-preview thumbnails with demosaic fallback.
- Added XDG thumbnail cache support.
- Added preview and read-only metadata panel.
- Added full-window loupe with Fit / 100% zoom, pan, filmstrip, and slideshow.
- Added keyboard navigation.
- Added browsing controls for thumbnail size, sorting, filename filtering, images-only mode, hidden files, and live folder updates.
- Added GPU-rendered UI with software fallback.
