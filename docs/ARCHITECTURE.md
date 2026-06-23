# PhotoBrowser Architecture

PhotoBrowser is a Rust photo browser for the COSMIC desktop, built with libcosmic. The codebase is organized around a clear split between portable application logic and COSMIC/libcosmic UI integration.

The main design goal is to keep photo browsing, decoding, metadata, catalog, filtering, culling, duplicate detection, and export behavior separate from toolkit-specific UI code.

## Architecture Overview

PhotoBrowser is structured in two broad layers:

- **Core layer** — portable photo-browser logic with no direct dependency on COSMIC UI types.
- **UI layer** — COSMIC/libcosmic application state, views, async tasks, and display handles.

This separation keeps the core behavior easier to test, reuse, and port while allowing the current app to remain a native COSMIC desktop application.

## Core Layer

The core layer handles the main photo-browser behavior:

- **Folder scanning** — lists the current folder, classifies entries, and applies sorting.
- **Image decoding** — decodes standard image formats and supported RAW previews.
- **RAW develop-look preview** — applies approximate develop-look rendering from XMP/ACR-style settings.
- **Metadata extraction** — reads EXIF and camera/capture-date metadata, including supported CR3 metadata.
- **Catalog cache** — stores rebuildable metadata and duplicate-scan cache data in SQLite.
- **Duplicate detection** — supports perceptual near-duplicate detection and exact byte-for-byte matching.
- **Histograms** — computes luminance histogram data for preview and loupe display.
- **Culling state** — manages rating, color-label, reject, and filter behavior.
- **Navigation state** — handles grid and loupe cursor movement.
- **Folder tree state** — manages expandable folder navigation.
- **Configuration** — stores app settings and paths.
- **Thumbnail cache support** — handles thumbnail cache behavior and XDG thumbnail-cache integration.
- **XMP sidecars** — reads and writes ratings, labels, reject marks, and keywords to sidecar files.

## UI Layer

The UI layer connects the core behavior to the COSMIC desktop interface:

- **Application model** — owns top-level app state and message handling.
- **View builders** — render the sidebar, toolbar, grid, preview panel, loupe, compare view, menus, settings, and status areas.
- **Async tasks** — run decode, export, scan, and cache operations without blocking the UI.
- **Display handles** — convert decoded image data into libcosmic/iced image handles for rendering.
- **Thumbnail service** — manages display-ready thumbnail handles and in-memory thumbnail reuse.
- **Preview cache** — stores display-ready preview/loupe image handles.
- **Inspection state** — tracks preview, loupe, compare, histogram, and selected-image display state.
- **Browser state** — tracks the active folder, visible entries, filters, selections, and live folder updates.

## Core-to-UI Boundary

Decoded image data is produced by the core layer as normal image data or RGBA bytes. The UI layer converts that decoded data into libcosmic/iced image handles for display.

This boundary keeps toolkit-specific rendering types out of the portable photo-browser logic. It also makes the application easier to maintain because image decoding and UI display remain separate responsibilities.

## Data and Write Behavior

PhotoBrowser treats the original image folder as the source of truth.

PhotoBrowser may write:

- app configuration
- thumbnail and preview cache files
- SQLite catalog cache files
- XMP sidecar files next to photos
- user-selected export copies

PhotoBrowser does **not** write into, move, delete, or modify original source image files.

## XMP Sidecars

PhotoBrowser stores photo culling metadata in XMP sidecars:

- star ratings
- color labels
- reject marks
- keywords/tags

Sidecar files are separate `.xmp` files next to the original photo. The original image or RAW file is not modified.

## Catalog

The SQLite catalog is a rebuildable cache/index. It may store:

- file metadata
- camera information
- capture dates
- perceptual duplicate hashes
- exact duplicate hashes
- tag/index data

The catalog improves browsing, filtering, and duplicate-scan performance. It is not the source of truth; it can be rebuilt by scanning folders again.

## Export

Export is copy-only:

- selected photos are copied to the chosen export destination
- matching XMP sidecars are copied when available
- existing destination files are skipped
- original files are not moved, deleted, or modified

## Portability Notes

The current application targets COSMIC/libcosmic. The core/UI split is intended to keep the non-UI photo-browser logic portable enough to support future frontends.

A future frontend would primarily need to replace:

- application shell
- view rendering
- input/event handling
- image-handle/display conversion
- platform-specific file dialogs and settings integration

The core folder scanning, decode, metadata, catalog, duplicate detection, culling, filtering, tags, and export logic can remain conceptually separate from those UI concerns.
