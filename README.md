# PhotoBrowser

A photo-focused file browser for the **COSMIC** desktop, built for fast folder browsing, photo review, and lightweight culling.

PhotoBrowser provides a virtualized thumbnail grid, a three-pane layout, a full-window loupe, duplicate detection, keyword/tag workflows, smart collections, export, and basic RAW develop-look previewing. It **never modifies original image files**. Ratings, labels, reject marks, and keywords are written to XMP sidecar files next to each photo.

Built in Rust with [libcosmic](https://github.com/pop-os/libcosmic).

## Features

### Browsing

- Virtualized thumbnail grid for smooth browsing through large folders.
- Current-folder-only scanning; subfolders are not recursively scanned.
- Adjustable thumbnail size.
- Sort by name, modified date, EXIF capture date, or rating.
- Live filename filtering.
- Menu bar with File, View, and Settings menus.
- Open a folder from the command line with `photobrowser [PATH]`.

### Layout

- Three-pane interface:
  - sidebar with Favorites, Drives, Places, and folder tree
  - central thumbnail grid
  - right-side preview and metadata panel
- Breadcrumb path bar.
- Bottom status bar with item counts and thumbnail-size control.
- Show-hidden-files toggle for dotfiles and sidecars.

### Loupe

- Full-window single-image loupe.
- Fit and 100% zoom modes.
- Adjustable zoom with pan.
- Filmstrip for quick sibling-image navigation.
- View-only slideshow.
- Optional full-resolution RAW demosaic preview.
- Optional native-resolution Actual zoom.
- Click-drag panning.
- Luminance histogram in the loupe.
- Focal-point-preserving zoom with `+` / `-`.

### Culling

- Star ratings from 0–5.
- Color labels: Red, Yellow, Green, Blue, Purple.
- Reject marking.
- Ratings, labels, and reject marks are stored in XMP sidecars.
- Filter by rating or color label.
- Hide rejected photos.
- Sort by rating.
- Multi-select with Ctrl-click, Shift-click, and Ctrl+A.
- Batch rating, label, reject, and unreject actions.
- Grid keyboard shortcuts:
  - `1`–`5`: set rating
  - `0`: clear rating
  - `6`–`9`: set color label
  - `x`: mark reject

### Compare

- 2-up Compare view for two selected photos.
- View-only comparison; no source files are modified.

### Duplicates

- Find Duplicates for perceptual near-duplicate detection.
- Find Exact Duplicates for byte-for-byte matching.
- Duplicate groups are shown with color-coded borders.
- Duplicate tools are review-only; they do not delete, move, or modify files.
- Duplicate scans can use cached results from the catalog.

### Keywords and Tags

- Reads XMP `dc:subject` keywords from sidecar files.
- Filter by keyword with View → Tag.
- Add and remove keywords from the preview pane.
- Batch keyword tagging for selected photos.
- Batch keyword removal for selected photos.
- Keyword filters compose with camera, date, rating, label, and duplicate filters.

### Smart Collections

- Save the current filter set as a reusable collection.
- Restore saved combinations of rating, label, camera, date, and tag filters.
- Clear active collections and filters from the View menu.

### Export

- Export selected photos with File → Export selection.
- Exports copies of selected photos and matching XMP sidecars.
- Existing destination files are skipped instead of overwritten.
- Original source files are not moved, deleted, or modified.

### Live Updates and Filtering

- Live folder updates when files change on disk.
- Manual refresh with `R` or `F5`.
- Images-only toggle.
- Read-only `.md` and `.txt` preview in the right panel.
- Filter by:
  - filename
  - rating
  - color label
  - rejected state
  - camera
  - capture year
  - keyword/tag
  - duplicate mode

### Formats and Metadata

- Supports JPEG, PNG, WebP, GIF, and RAW formats.
- RAW support includes NEF, CR2, CR3, ARW, DNG, RAF, ORF, RW2, PEF, and SRW.
- Other file types remain browsable with type icons.
- RAW thumbnails use embedded previews when available, with demosaic fallback.
- EXIF orientation is applied on decode.
- Preview panel shows file info, EXIF metadata, and luminance histogram.
- Canon CR3 support includes embedded-preview decode and camera/capture-date extraction.

### RAW Develop-Look Preview

- Optional approximate RAW develop-look rendering.
- Reads common XMP/Adobe Camera Raw settings from sidecars.
- Applies approximate exposure, contrast, tone curve, vibrance, saturation, highlights, shadows, whites, and blacks.
- Render-only; original RAW files and sidecars are not modified by previewing develop-look.

### Catalog

- Optional SQLite catalog stored in PhotoBrowser's cache directory.
- Indexes camera and capture-date metadata.
- Caches perceptual and exact duplicate hashes.
- Used as a rebuildable cache/index; the filesystem remains the source of truth.
- The catalog is never written into original image files.

## Build and Run

```bash
cargo build --release
cargo run --release
```

Requirements:

- Rust toolchain
- libcosmic build dependencies
- `libxkbcommon-dev`
- `libwayland-dev`
- `pkg-config`

These are typically present on COSMIC / Pop!_OS systems.

## Development

```bash
just check
bash scripts/check-scan-invariant.sh
```

## Status

Current release: **v1.37.0**

PhotoBrowser currently includes:

- browse-focused photo management
- virtualized thumbnail grid
- three-pane layout
- loupe with zoom, pan, filmstrip, slideshow, and histogram
- XMP sidecar ratings, labels, rejects, and keywords
- multi-select and batch culling
- duplicate and exact-duplicate detection
- camera, date, rating, label, keyword, filename, and duplicate filters
- SQLite catalog cache
- smart collections
- export/copy workflow
- approximate RAW develop-look preview

PhotoBrowser writes only:

- app configuration
- thumbnail/preview cache
- SQLite catalog cache
- XMP sidecar files
- user-selected export copies

PhotoBrowser does **not** write into, move, delete, or modify original source image files.

## Deferred

- HEIC/HEIF thumbnails.
- Video poster-frame thumbnails.

These are deferred to optional, default-off backends because they require system libraries such as `libheif`, ffmpeg, or gstreamer.

## License

Licensed **GPL-3.0-or-later**. See [`LICENSE`](LICENSE).

Because libcosmic is a hard dependency and is GPL-3.0, distributed PhotoBrowser binaries are GPL-3.0.
