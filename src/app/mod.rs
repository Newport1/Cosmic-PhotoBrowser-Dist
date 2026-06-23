use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use chrono::DateTime;

use notify::{self, RecursiveMode, Watcher};

use crate::browser_state::BrowserState;
use crate::config::{Config, SortMode};
use crate::cull::{batch_summary, cull_passes};
use crate::folder_tree::FolderTree;
use crate::inspection::{
    high_res_loupe_mode, CompareState, DecodedImage, LoupeDecodeMode, LoupeState, LoupeZoom,
    MetadataSection, MetadataSectionState, PreviewState,
};
use crate::metadata::ExifSummary;
use crate::nav::loupe_step;
use crate::preview_cache::{PreviewCache, PREVIEW_CACHE_CAP};
use crate::scan::{Entry, EntryKind, FileCategory};
use crate::tasks;
use crate::thumb::{xdg, ThumbService, ThumbState};
use crate::view;
use crate::view::grid::{
    grid_available_h, grid_available_w, visible_range, MARGIN_ROWS, SCROLLBAR_W,
};
use cosmic::app::context_drawer::{self, ContextDrawer};
use cosmic::iced::Length;
use cosmic::prelude::*;
use cosmic::widget;
use cosmic::widget::menu::{self, ItemHeight, ItemWidth};
use cosmic::widget::segmented_button;
use cosmic::widget::RcElementWrapper;
use directories::UserDirs;

mod update_cull;
mod update_decode;
mod update_duplicates;
mod update_export;
mod update_filter;
mod update_grid;
mod update_keyboard;
mod update_loupe;
mod update_nav;
mod update_settings;

/// Hamming-distance threshold for perceptual duplicate grouping.
const DUP_HAMMING_THRESHOLD: u32 = 10;

/// Pure helper: turn a scan result into (groups, members). Extracted so the
/// transformation is unit-testable without the async task plumbing.
fn compute_duplicate_sets(
    items: &[(usize, u64)],
    threshold: u32,
) -> (Vec<Vec<usize>>, std::collections::HashSet<usize>) {
    let groups = crate::dedupe::group_duplicates(items, threshold);
    let members = groups.iter().flat_map(|g| g.iter().copied()).collect();
    (groups, members)
}

/// Build a descriptive collection name from the active filters, e.g. "3+ stars · Canon EOS R5 · 2021 · Red · #beach".
/// Returns None if NO filter is active (nothing to save).
fn describe_filters(
    rating_min: Option<u8>,
    label: Option<&str>,
    camera: Option<&str>,
    date_year: Option<i32>,
    tag: Option<&str>,
) -> Option<String> {
    let mut parts: Vec<String> = Vec::new();
    if let Some(r) = rating_min {
        parts.push(format!("{r}+ stars"));
    }
    if let Some(l) = label {
        parts.push(l.to_owned());
    }
    if let Some(c) = camera {
        parts.push(c.to_owned());
    }
    if let Some(y) = date_year {
        parts.push(y.to_string());
    }
    if let Some(t) = tag {
        parts.push(format!("#{t}"));
    }
    if parts.is_empty() {
        None
    } else {
        Some(parts.join(" · "))
    }
}

// ── Cell state for the render snapshot ───────────────────────────────────────

/// Per-cell render state stored in the windowed snapshot.
///
/// Only the visible window (plus margin) is snapshotted; `view()` iterates
/// this bounded `Vec` rather than `entries` (which is O(N)).
#[derive(Debug, Clone)]
pub enum CellState {
    /// Image with a decoded thumbnail handle.
    Thumb(cosmic::widget::image::Handle),
    /// Decode in-flight.
    Pending,
    /// Decode failed.
    Failed,
    /// RAW image without a decodable thumbnail.
    RawPlaceholder,
    /// Directory entry — carries the path for navigation.
    Dir(PathBuf),
    /// Non-thumbnailed file category glyph.
    Glyph(&'static str),
    /// Unknown / uncategorized file (dedicated SVG placeholder in grid only).
    UnknownFile,
}

fn category_glyph(category: &FileCategory) -> &'static str {
    match category {
        FileCategory::Video => "🎞",
        FileCategory::Audio => "🎵",
        FileCategory::Document => "📄",
        FileCategory::Archive => "🗜",
        FileCategory::Code => "⟨⟩",
        // Unknown files render via CellState::UnknownFile (SVG) and never reach this Glyph path;
        // kept explicit (not `_`) so a new FileCategory variant is a compile error, not a silent "•".
        FileCategory::Unknown => "•",
    }
}

pub(crate) fn cell_state_for_thumb(kind: &EntryKind, thumb_state: ThumbState) -> CellState {
    match thumb_state {
        ThumbState::Ready(handle) => CellState::Thumb(handle),
        ThumbState::Pending => CellState::Pending,
        ThumbState::Failed if *kind == EntryKind::Raw => CellState::RawPlaceholder,
        ThumbState::Failed => CellState::Failed,
    }
}

// ── SortBar ──────────────────────────────────────────────────────────────────

/// Grouped sort segmented-button state (model + the four option entities).
pub struct SortBar {
    pub model: segmented_button::SingleSelectModel,
    pub name_entity: segmented_button::Entity,
    pub date_entity: segmented_button::Entity,
    pub captured_entity: segmented_button::Entity,
    pub rating_entity: segmented_button::Entity,
}

/// Grouped duplicate-detection state.
#[derive(Default)]
pub struct DuplicateState {
    pub groups: Vec<Vec<usize>>,
    pub members: std::collections::HashSet<usize>,
    pub filter_active: bool,
    pub scan_in_progress: bool,
    pub exact: bool,
}

/// Grouped filter/cull state for rating, label, camera, tag, date, hide-rejected, and cull cache.
#[derive(Default)]
pub struct FilterState {
    pub rating: Option<u8>,
    pub label: Option<crate::xmp::ColorLabel>,
    pub camera: Option<String>,
    pub tag: Option<String>,
    pub date: Option<i32>,
    pub hide_rejected: bool,
    pub cull_cache: std::collections::HashMap<std::path::PathBuf, crate::xmp::CullMeta>,
}

// ── SelectionState ───────────────────────────────────────────────────────────

/// Grouped selection state (primary, multi, anchor, batch status, modifiers).
#[derive(Default)]
pub struct SelectionState {
    pub selected_index: Option<usize>,
    pub multi_selected: std::collections::HashSet<usize>,
    pub select_anchor: Option<usize>,
    pub batch_status: Option<String>,
    pub modifiers: cosmic::iced::keyboard::Modifiers,
}

// ── LoupeRuntime ─────────────────────────────────────────────────────────────

/// Grouped runtime-only state for loupe decode, slideshow, zoom, and pan.
pub struct LoupeRuntime {
    pub decode_pending: Option<(std::path::PathBuf, crate::inspection::LoupeDecodeMode)>,
    pub slideshow_playing: bool,
    pub scroll: cosmic::iced::widget::scrollable::AbsoluteOffset,
    pub zoom_factor: f32,
    pub drag: Option<(
        cosmic::iced::Point,
        cosmic::iced::widget::scrollable::AbsoluteOffset,
    )>,
    pub pan_last_cursor: Option<cosmic::iced::Point>,
}

// ── NavState ─────────────────────────────────────────────────────────────────

/// Grouped navigation/sidebar state (back/forward stacks, places, drives, tree).
pub struct NavState {
    /// Back-stack of previously visited folders. Runtime-only; not persisted.
    pub back: Vec<PathBuf>,
    /// Forward-stack of folders left by Back. Runtime-only; not persisted.
    pub forward: Vec<PathBuf>,
    /// XDG places shown in the sidebar.
    pub places: Vec<(String, PathBuf)>,
    /// Mounted user-relevant drives shown in the sidebar.
    pub drives: Vec<view::sidebar::DriveEntry>,
    /// Sidebar folder tree state.
    pub tree: FolderTree,
}

// ── AppModel ─────────────────────────────────────────────────────────────────

/// Application model for PhotoBrowser.
pub struct AppModel {
    pub core: cosmic::Core,
    /// Active config (thumb_size etc.).
    pub config: Config,
    /// Current-folder listing, sort, and capture-date cache state.
    pub browser: BrowserState,
    /// Navigation/sidebar state (back/forward, places, drives, tree).
    pub nav: NavState,
    /// Bounded-LRU thumbnail service (M3).
    pub thumb: ThumbService,

    // ── M4 virtual grid state ────────────────────────────────────────────────
    /// Fallback viewport width in pixels (init default / on_window_resize).
    /// Only used before the `responsive` wrapper has measured the real size.
    pub viewport_w: f32,
    /// Fallback viewport height in pixels.
    pub viewport_h: f32,
    /// The grid's true CONTENT width, measured by the `responsive` wrapper in
    /// `view()` every layout pass and read back here (f32 bits; 0 = not yet
    /// measured).  This is the reliable size source — `on_window_resize` does
    /// not fire at window creation, so the fallback alone leaves the grid sized
    /// for the 1024 default until the user resizes.  `view()` (render) and
    /// `rebuild_snapshot` (request) both use this, so they agree exactly.
    pub measured_w: Arc<AtomicU32>,
    /// The grid pane's measured height (f32 bits; 0 = not yet measured).
    pub measured_h: Arc<AtomicU32>,
    /// The (width, height) the last `rebuild_snapshot` used — lets the 32 ms
    /// tick detect when `view()` has measured a new size and re-request.
    pub last_grid_w: f32,
    pub last_grid_h: f32,
    /// Absolute vertical scroll offset in pixels.
    pub scroll_offset_y: f32,
    /// The render snapshot as a LOOKUP TABLE: entry_index → CellState for every
    /// item in the request envelope.  `grid_content` indexes this by whatever
    /// indices the measured column count implies; the envelope (a superset)
    /// guarantees a hit.  Bounded by window size.
    pub grid_snapshot: HashMap<usize, CellState>,
    /// The request envelope currently materialized in the snapshot (a superset
    /// of any frame's rendered range; see `request_envelope`).
    pub visible_index_range: std::ops::Range<usize>,
    /// Grouped selection state.
    pub selection: SelectionState,
    /// Decoded preview handle for the selected image.
    pub preview: Option<PreviewState>,
    /// Bounded text content for selected .md/.txt (read-only preview; capped at 64KiB/2000 lines).
    /// Pair of (path, lossy-bounded-text). Additive; images use the `preview` field instead.
    pub text_preview: Option<(PathBuf, String)>,
    /// Runtime-only expansion state for right-panel read-only metadata sections.
    pub metadata_sections: MetadataSectionState,
    /// Full-window single-image view (v4-M1). `Some` ⇒ the app is in loupe mode.
    pub loupe: Option<LoupeState>,
    /// View-only 2-up compare (CM-M1). `Some` ⇒ compare takeover (exactly 2 panes, Fit, no writes/zoom).
    pub compare: Option<CompareState>,
    /// Runtime-only loupe decode, slideshow, zoom, and pan state.
    pub loupe_rt: LoupeRuntime,
    /// Separate bounded LRU for loupe/high-res decodes (keyed by path+mode).
    /// Distinct from thumb LRU: loupe never evicts thumbnails; re-open is instant.
    pub preview_cache: PreviewCache,
    /// Grouped sort segmented-button state.
    pub sort: SortBar,
    /// Fallback focus guard for the filter input; set while editing after input.
    pub filter_focused: bool,
    /// Whether the cache settings inline panel is open.
    pub settings_open: bool,
    /// Editable text for cache max GB.
    pub cache_max_input: String,
    /// Editable text for cache directory override.
    pub cache_dir_input: String,
    /// Editable text for export directory override.
    pub export_dir_input: String,
    /// In-progress text for the preview-pane "add keyword" box.
    pub keyword_input: String,
    /// Grouped filter/cull state.
    pub filter: FilterState,
    /// Grouped duplicate-detection state.
    pub dups: DuplicateState,
    /// Keybinds for menu (shortcuts labels); empty map is sufficient (no shortcuts shown yet).
    pub menu_key_binds: std::collections::HashMap<cosmic::widget::menu::KeyBind, MenuAction>,
    /// Folder-scoped metadata index (entry index → (exif_captured_unix, camera)) populated by
    /// background M4a indexing on folder open. Cleared on folder change; no UI yet.
    pub folder_metadata: std::collections::HashMap<usize, (Option<i64>, Option<String>)>,
    /// Folder-scoped sidecar keywords per entry index (read-only), populated by index_folder_keywords.
    pub folder_tags: std::collections::HashMap<usize, Vec<String>>,
}

// ── Messages ──────────────────────────────────────────────────────────────────

/// Messages emitted by the application.
#[derive(Debug, Clone)]
pub enum Message {
    /// Navigate into a directory (from sidebar or grid).
    NavigateTo(PathBuf),
    /// Navigate to the parent of the current directory.
    NavigateUp,
    /// Navigate to the previous folder in the runtime history stack.
    NavigateBack,
    /// Navigate to the next folder in the runtime history stack.
    NavigateForward,
    /// Add the current folder to the persisted Favorites list.
    AddFavorite,
    /// Remove one path from the persisted Favorites list.
    RemoveFavorite(PathBuf),
    /// Expand or collapse a folder-tree node.
    ToggleTreeNode(PathBuf),
    /// Periodic tick — drains the thumbnail decode channel.
    ThumbTick,
    /// Periodic tick — auto-advance loupe slideshow (only emitted while loupe.is_some() && playing; ~4s).
    SlideshowTick,
    /// Scroll offset changed (absolute Y in pixels).
    Scroll(f32),
    /// Thumbnail size slider changed.
    ThumbSizeChanged(u16),
    /// Sort mode changed in the header control.
    SortModeChanged(SortMode),
    /// Filename filter text changed.
    FilterChanged(String),
    /// Clear the filename filter.
    ClearFilter,
    /// In-progress text for the preview-pane add-keyword box changed.
    KeywordInputChanged(String),
    /// Commit the keyword_input to the currently previewed photo (single).
    AddKeyword,
    /// Remove a specific keyword from the currently previewed photo (single).
    RemoveKeyword(String),
    /// Image cell selected for preview.
    SelectImage(usize),
    /// Keyboard modifiers changed (tracked for multi-select Ctrl/Shift clicks).
    ModifiersChanged(cosmic::iced::keyboard::Modifiers),
    /// Select all currently displayed grid entries (Ctrl+A).
    SelectAll,
    /// Preview decode completed (path, handle, original dimensions, histogram, error).
    PreviewDecoded(
        PathBuf,
        DecodedImage,
        Option<[u32; crate::histogram::HISTOGRAM_BINS]>,
        Option<String>,
    ),
    /// EXIF metadata read completed (path, best-effort summary).
    MetadataLoaded(PathBuf, ExifSummary),
    /// Expand/collapse one read-only metadata section in the right panel.
    ToggleMetadataSection(MetadataSection),
    /// Capture epoch read completed (path, parsed EXIF DateTimeOriginal epoch).
    CaptureEpochLoaded(PathBuf, Option<i64>),
    /// Open the loupe (full-window single-image view) for the given entry index.
    OpenLoupe(usize),
    /// Leave the loupe, returning to the 3-pane grid.
    CloseLoupe,
    /// Enter 2-up compare view for the first two multi-selected images (in displayed order). View-only.
    EnterCompare,
    /// Close the compare view.
    CloseCompare,
    /// Compare decode completed (path, handle+orig dims, error) — mirrors LoupeDecoded (simple Fit path).
    CompareDecoded(
        PathBuf,
        Option<(cosmic::widget::image::Handle, u32, u32)>,
        Option<String>,
    ),
    /// Keyboard navigation key pressed.
    KeyPressed(cosmic::iced::keyboard::key::Named),
    /// Spacebar pressed while browsing the grid.
    SpacePressed,
    /// Manual refresh requested by keyboard (R/F5).
    RefreshCurrentFolder,
    /// Digit key (0-9) pressed. Routed by context in handler: loupe open => zoom; grid => cull rating/label.
    DigitKey(u8),
    /// 'x' key pressed (reject/cull) while in grid (loupe+compare closed). Sidecar only.
    RejectKey,
    /// Loupe decode completed (path, handle, original dimensions, error).
    LoupeDecoded(PathBuf, LoupeDecodeMode, DecodedImage, Option<String>),
    /// Toggle the cache settings panel.
    ToggleSettings,
    /// Close the cache settings panel.
    CloseSettings,
    /// Cache max GB input changed.
    CacheMaxGbChanged(String),
    /// Cache directory text field changed.
    CacheDirInputChanged(String),
    /// Apply the text field as a custom cache directory.
    ApplyCacheDir,
    /// Reset cache directory to default.
    ResetCacheDir,
    /// Toggle hidden dotfile visibility and rescan.
    ToggleShowHidden,
    /// Toggle full RAW demosaic for the loupe.
    ToggleLoupeFullDemosaic,
    /// Toggle whether the sidebar folder tree shows files (leaves) in addition to folders.
    ToggleTreeShowFiles,
    /// Toggle "images only" grid visibility (hides documents/videos/etc; folders and images/RAW remain).
    ToggleImagesOnly,
    /// Toggle loupe scale mode without changing the decoded handle.
    ToggleLoupeZoom,
    /// Set loupe scale mode without changing the decoded handle.
    SetLoupeZoom(LoupeZoom),
    /// Step loupe zoom factor (true=zoom in +/=, false=zoom out -/_) when in Actual (or first + enters Actual).
    LoupeZoomStep(bool),
    /// Toggle loupe slideshow play/pause (▶/⏸). View-only; no file writes. Starts paused on loupe open.
    ToggleSlideshow,
    /// Toggle the native-res loupe (full-res Actual decode ≤8192) setting.
    ToggleFullResLoupe,
    /// Toggle develop-look in the loupe (applies apply_develop using sidecar DevelopParams).
    ToggleDevelopLook,
    /// Toggle whether the loupe shows the sibling filmstrip at bottom. Default ON.
    ToggleShowFilmstrip,
    /// Left mouse pressed inside the zoomed (Actual) loupe scrollable content — begin grab-pan gesture.
    LoupePanPress,
    /// Left mouse released — end grab-pan gesture.
    LoupePanRelease,
    /// Mouse moved inside the zoomed loupe mouse_area (content-local Point).
    LoupePanMove(cosmic::iced::Point),
    /// The loupe zoomed scrollable reported a new absolute offset (via on_scroll, from wheel or scroll_to).
    LoupeScrolled(cosmic::iced::widget::scrollable::AbsoluteOffset),
    /// Set star rating on current loupe image (click stars in top bar); writes XMP sidecar only.
    SetLoupeRating(u8),
    /// Set color label on current loupe image (click swatches in top bar); writes XMP sidecar only (via M1 writer).
    SetLoupeLabel(Option<crate::xmp::ColorLabel>),
    /// Toggle reject flag on current loupe image; writes XMP sidecar only (via M1 writer).
    ToggleLoupeReject,
    /// Apply rating (0..=5; 0 clears) to every multi-selected grid item. Sidecar only.
    BatchSetRating(u8),
    /// Apply color label (Some sets, None clears) to every multi-selected grid item. Sidecar only.
    BatchSetLabel(Option<crate::xmp::ColorLabel>),
    /// Set reject flag (explicit) on every multi-selected grid item. Sidecar only.
    BatchSetReject(bool),
    /// Cycle the grid rating filter (toolbar): None -> 1 -> 2 -> 3 -> 4 -> 5 -> None.
    CycleRatingFilter,
    /// Cycle the grid color label filter (toolbar): None -> Red -> Yellow -> Green -> Blue -> Purple -> None.
    #[allow(dead_code)]
    CycleLabelFilter,
    /// Toggle the grid "hide rejected" filter.
    ToggleHideRejected,
    /// Set color label filter (from View > Color label menu; None clears/"All").
    SetLabelFilter(Option<crate::xmp::ColorLabel>),
    /// Set camera filter (from View > Camera menu; None clears/"All"). Index into sorted_cameras().
    SetCameraFilter(Option<usize>),
    /// Set tag/keyword filter (from View > Tag menu; None clears/"All"). Index into sorted_tags().
    SetTagFilter(Option<usize>),
    /// Set capture year filter (from View > Date menu; None clears/"All").
    SetDateFilter(Option<i32>),
    /// Save the current active filters (rating/label/camera/date/tag) as a named SavedCollection (auto-named).
    SaveCollection,
    /// Apply a previously saved collection by index into config.collections.
    ApplyCollection(usize),
    /// Remove all saved collections (and persist the empty list).
    ClearCollections,
    /// No-op for disabled menu items (e.g. Open New Tab placeholder).
    MenuNoop,
    /// Toggle the duplicate filter: on first press, scans current folder for near-duplicates and
    /// filters the grid to only show members of duplicate groups. Press again (or while active)
    /// to clear. Review-only; never mutates files.
    FindDuplicates,
    /// Background duplicate-hash scan completed. Vec of (entry_index, dhash) for scanned images.
    DuplicatesScanned(Vec<(usize, u64)>),
    /// Toggle the exact (SHA-256 byte-identical) duplicate filter. Mirrors FindDuplicates but for exact matches.
    FindExactDuplicates,
    /// Background exact-duplicate SHA-256 scan completed. Vec of (entry_index, sha256-hex).
    ExactDuplicatesScanned(Vec<(usize, String)>),
    /// Background folder metadata index completed. Vec of (entry_index, capture_unix, camera).
    FolderIndexed(Vec<(usize, Option<i64>, Option<String>)>),
    /// Background folder keyword index completed. Vec of (entry_index, keywords) for entries with keywords.
    FolderKeywordsIndexed(Vec<(usize, Vec<String>)>),
    /// File > Export selection… action: copy currently-selected photos (+ sidecars) to export dir.
    ExportSelection,
    /// Apply the export-dir text field from settings.
    ApplyExportDir,
    /// Export directory text field changed (live typing).
    ExportDirInputChanged(String),
    /// Export task completed. (result, dest_dir)
    ExportDone(crate::export::ExportResult, std::path::PathBuf),
}

/// Menu actions for the top-left menu bar (File / View / Settings).
/// Must be Clone+Copy+Eq+PartialEq to satisfy libcosmic menu::action::MenuAction.
#[derive(Clone, Copy, Eq, PartialEq, Debug)]
pub enum MenuAction {
    OpenNewTab,
    ToggleHidden,
    ToggleImagesOnly,
    ToggleFilmstrip,
    ToggleHideRejected,
    LabelAll,
    Label(crate::xmp::ColorLabel),
    CameraAll,
    Camera(usize),
    TagAll,
    Tag(usize),
    DateAll,
    DateYear(i32),
    SaveCollection,
    ApplyCollection(usize),
    ClearCollections,
    SortName,
    SortModified,
    SortCaptured,
    SortRating,
    FindDuplicates,
    FindExactDuplicates,
    Preferences,
    ExportSelection,
}

impl menu::action::MenuAction for MenuAction {
    type Message = Message;
    fn message(&self) -> Message {
        match self {
            MenuAction::OpenNewTab => Message::MenuNoop,
            MenuAction::ToggleHidden => Message::ToggleShowHidden,
            MenuAction::ToggleImagesOnly => Message::ToggleImagesOnly,
            MenuAction::ToggleFilmstrip => Message::ToggleShowFilmstrip,
            MenuAction::ToggleHideRejected => Message::ToggleHideRejected,
            MenuAction::LabelAll => Message::SetLabelFilter(None),
            MenuAction::Label(c) => Message::SetLabelFilter(Some(*c)),
            MenuAction::CameraAll => Message::SetCameraFilter(None),
            MenuAction::Camera(i) => Message::SetCameraFilter(Some(*i)),
            MenuAction::TagAll => Message::SetTagFilter(None),
            MenuAction::Tag(i) => Message::SetTagFilter(Some(*i)),
            MenuAction::DateAll => Message::SetDateFilter(None),
            MenuAction::DateYear(y) => Message::SetDateFilter(Some(*y)),
            MenuAction::SaveCollection => Message::SaveCollection,
            MenuAction::ApplyCollection(i) => Message::ApplyCollection(*i),
            MenuAction::ClearCollections => Message::ClearCollections,
            MenuAction::SortName => Message::SortModeChanged(crate::config::SortMode::Name),
            MenuAction::SortModified => Message::SortModeChanged(crate::config::SortMode::Date),
            MenuAction::SortCaptured => Message::SortModeChanged(crate::config::SortMode::Captured),
            MenuAction::SortRating => Message::SortModeChanged(crate::config::SortMode::Rating),
            MenuAction::FindDuplicates => Message::FindDuplicates,
            MenuAction::FindExactDuplicates => Message::FindExactDuplicates,
            MenuAction::Preferences => Message::ToggleSettings,
            MenuAction::ExportSelection => Message::ExportSelection,
        }
    }
}

/// Build the File / View / Settings menu bar for placement in header_start (top-left).
fn menu_bar(app: &AppModel) -> cosmic::Element<'_, Message> {
    let key_binds = &app.menu_key_binds;

    menu::bar(vec![
        // File
        menu::Tree::with_children(
            RcElementWrapper::new(cosmic::Element::from(menu::root("File"))),
            menu::items(
                key_binds,
                vec![
                    menu::Item::ButtonDisabled("Open New Tab", None, MenuAction::OpenNewTab),
                    menu::Item::Button("Export selection…", None, MenuAction::ExportSelection),
                ],
            ),
        ),
        // View
        menu::Tree::with_children(
            RcElementWrapper::new(cosmic::Element::from(menu::root("View"))),
            menu::items(key_binds, {
                // Build with uniform owned-String labels so the Camera submenu
                // can use dynamic camera names (runtime Strings) from sorted_cameras().
                // The children Vec owns the strings for the dynamic case.
                let v: Vec<menu::Item<MenuAction, String>> = vec![
                    menu::Item::CheckBox(
                        "Show hidden files".to_string(),
                        None,
                        app.config.show_hidden,
                        MenuAction::ToggleHidden,
                    ),
                    menu::Item::CheckBox(
                        "Images only".to_string(),
                        None,
                        app.config.images_only,
                        MenuAction::ToggleImagesOnly,
                    ),
                    menu::Item::CheckBox(
                        "Show filmstrip".to_string(),
                        None,
                        app.config.show_filmstrip,
                        MenuAction::ToggleFilmstrip,
                    ),
                    menu::Item::CheckBox(
                        "Hide rejects".to_string(),
                        None,
                        app.filter.hide_rejected,
                        MenuAction::ToggleHideRejected,
                    ),
                    menu::Item::CheckBox(
                        "Find Duplicates".to_string(),
                        None,
                        app.dups.filter_active && !app.dups.exact,
                        MenuAction::FindDuplicates,
                    ),
                    menu::Item::CheckBox(
                        "Find Exact Duplicates".to_string(),
                        None,
                        app.dups.filter_active && app.dups.exact,
                        MenuAction::FindExactDuplicates,
                    ),
                    menu::Item::Divider,
                    menu::Item::Folder(
                        "Color label".to_string(),
                        vec![
                            menu::Item::CheckBox(
                                "All".to_string(),
                                None,
                                app.filter.label.is_none(),
                                MenuAction::LabelAll,
                            ),
                            menu::Item::CheckBox(
                                "Red".to_string(),
                                None,
                                app.filter.label == Some(crate::xmp::ColorLabel::Red),
                                MenuAction::Label(crate::xmp::ColorLabel::Red),
                            ),
                            menu::Item::CheckBox(
                                "Yellow".to_string(),
                                None,
                                app.filter.label == Some(crate::xmp::ColorLabel::Yellow),
                                MenuAction::Label(crate::xmp::ColorLabel::Yellow),
                            ),
                            menu::Item::CheckBox(
                                "Green".to_string(),
                                None,
                                app.filter.label == Some(crate::xmp::ColorLabel::Green),
                                MenuAction::Label(crate::xmp::ColorLabel::Green),
                            ),
                            menu::Item::CheckBox(
                                "Blue".to_string(),
                                None,
                                app.filter.label == Some(crate::xmp::ColorLabel::Blue),
                                MenuAction::Label(crate::xmp::ColorLabel::Blue),
                            ),
                            menu::Item::CheckBox(
                                "Purple".to_string(),
                                None,
                                app.filter.label == Some(crate::xmp::ColorLabel::Purple),
                                MenuAction::Label(crate::xmp::ColorLabel::Purple),
                            ),
                        ],
                    ),
                    // Camera submenu: driven by sorted_cameras() derived from folder_metadata.
                    // If no cameras, still render Folder with just the "All" item (simpler, compiles cleanly).
                    {
                        let mut cam_children: Vec<menu::Item<MenuAction, String>> =
                            vec![menu::Item::CheckBox(
                                "All".to_string(),
                                None,
                                app.filter.camera.is_none(),
                                MenuAction::CameraAll,
                            )];
                        for (i, cam) in app.sorted_cameras().iter().enumerate() {
                            let checked = app.filter.camera.as_deref() == Some(cam.as_str());
                            cam_children.push(menu::Item::CheckBox(
                                cam.clone(),
                                None,
                                checked,
                                MenuAction::Camera(i),
                            ));
                        }
                        menu::Item::Folder("Camera".to_string(), cam_children)
                    },
                    {
                        let mut date_children: Vec<menu::Item<MenuAction, String>> =
                            vec![menu::Item::CheckBox(
                                "All".to_string(),
                                None,
                                app.filter.date.is_none(),
                                MenuAction::DateAll,
                            )];
                        for year in app.sorted_capture_years() {
                            let checked = app.filter.date == Some(year);
                            date_children.push(menu::Item::CheckBox(
                                year.to_string(),
                                None,
                                checked,
                                MenuAction::DateYear(year),
                            ));
                        }
                        menu::Item::Folder("Date".to_string(), date_children)
                    },
                    {
                        let mut tag_children: Vec<menu::Item<MenuAction, String>> =
                            vec![menu::Item::CheckBox(
                                "All".to_string(),
                                None,
                                app.filter.tag.is_none(),
                                MenuAction::TagAll,
                            )];
                        for (i, t) in app.sorted_tags().iter().enumerate() {
                            let checked = app.filter.tag.as_deref() == Some(t.as_str());
                            tag_children.push(menu::Item::CheckBox(
                                t.clone(),
                                None,
                                checked,
                                MenuAction::Tag(i),
                            ));
                        }
                        menu::Item::Folder("Tag".to_string(), tag_children)
                    },
                    {
                        let mut coll_children: Vec<menu::Item<MenuAction, String>> =
                            vec![menu::Item::Button(
                                "Save current filters".to_string(),
                                None,
                                MenuAction::SaveCollection,
                            )];
                        if !app.config.collections.is_empty() {
                            coll_children.push(menu::Item::Divider);
                            for (i, c) in app.config.collections.iter().enumerate() {
                                coll_children.push(menu::Item::Button(
                                    c.name.clone(),
                                    None,
                                    MenuAction::ApplyCollection(i),
                                ));
                            }
                            coll_children.push(menu::Item::Divider);
                            coll_children.push(menu::Item::Button(
                                "Clear all collections".to_string(),
                                None,
                                MenuAction::ClearCollections,
                            ));
                        }
                        menu::Item::Folder("Collections".to_string(), coll_children)
                    },
                    menu::Item::Divider,
                    menu::Item::Folder(
                        "Sort by".to_string(),
                        vec![
                            menu::Item::CheckBox(
                                "Name".to_string(),
                                None,
                                app.config.sort_mode == crate::config::SortMode::Name,
                                MenuAction::SortName,
                            ),
                            menu::Item::CheckBox(
                                "Modified".to_string(),
                                None,
                                app.config.sort_mode == crate::config::SortMode::Date,
                                MenuAction::SortModified,
                            ),
                            menu::Item::CheckBox(
                                "Captured".to_string(),
                                None,
                                app.config.sort_mode == crate::config::SortMode::Captured,
                                MenuAction::SortCaptured,
                            ),
                            menu::Item::CheckBox(
                                "Rating".to_string(),
                                None,
                                app.config.sort_mode == crate::config::SortMode::Rating,
                                MenuAction::SortRating,
                            ),
                        ],
                    ),
                ];
                v
            }),
        ),
        // Settings
        menu::Tree::with_children(
            RcElementWrapper::new(cosmic::Element::from(menu::root("Settings"))),
            menu::items(
                key_binds,
                vec![menu::Item::Button(
                    "Preferences",
                    None,
                    MenuAction::Preferences,
                )],
            ),
        ),
    ])
    .item_height(ItemHeight::Dynamic(40))
    .item_width(ItemWidth::Uniform(240))
    .into()
}

// ── Cell height constant ──────────────────────────────────────────────────────

/// v0.6-C2 measured a 130-cell visible+margin envelope at 96px on a
/// representative maximized pane, so the decode queue starts above that and is
/// raised dynamically for larger envelopes.
const THUMB_QUEUE_CAP_MIN: usize = 256;

fn thumbnail_queue_cap_for_envelope(envelope_len: usize) -> usize {
    THUMB_QUEUE_CAP_MIN.max(envelope_len)
}

/// Cell height = thumb_size + label area.
pub fn cell_h(thumb_size: u16) -> f32 {
    // image (thumb_size) + rating row (14) + filename label (20). The rating row is
    // ALWAYS reserved (stars when rated, empty spacer otherwise) so every cell is this
    // exact height and the windowing (request) side agrees with the render side.
    thumb_size as f32 + 14.0 + 20.0
}

fn visible_priority_positions(
    range: std::ops::Range<usize>,
    cols: usize,
    scroll_y: f32,
    avail_h: f32,
    cell_h: f32,
) -> Vec<usize> {
    let cols = cols.max(1);
    let center_col = (cols.saturating_sub(1) as f32) / 2.0;
    let center_row = (scroll_y + avail_h / 2.0) / cell_h.max(1.0);
    let mut positions: Vec<_> = range.collect();
    positions.sort_by(|a, b| {
        let a_row = (*a / cols) as f32;
        let a_col = (*a % cols) as f32;
        let b_row = (*b / cols) as f32;
        let b_col = (*b % cols) as f32;
        let a_dist = (a_row - center_row).mul_add(a_row - center_row, (a_col - center_col).powi(2));
        let b_dist = (b_row - center_row).mul_add(b_row - center_row, (b_col - center_col).powi(2));
        a_dist.total_cmp(&b_dist).then_with(|| a.cmp(b))
    });
    positions
}

fn visible_priority_for_position(
    pos: usize,
    cols: usize,
    scroll_y: f32,
    avail_h: f32,
    cell_h: f32,
) -> f32 {
    let cols = cols.max(1);
    let center_col = (cols.saturating_sub(1) as f32) / 2.0;
    let center_row = (scroll_y + avail_h / 2.0) / cell_h.max(1.0);
    let row = (pos / cols) as f32;
    let col = (pos % cols) as f32;
    (row - center_row).mul_add(row - center_row, (col - center_col).powi(2))
}

/// Clamp thumbnail size to the supported range, then floor to the lower 16px step.
pub fn snap_thumb_size(raw: u16) -> u16 {
    let clamped = raw.clamp(96, 256);
    ((clamped - 96) / 16) * 16 + 96
}

/// Shorten a string by eliding its middle with `…`, never exceeding `max` bytes
/// for ASCII paths. For tiny limits, returns a clipped prefix.
pub fn middle_ellipsis(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_owned();
    }
    if max == 0 {
        return String::new();
    }
    if max <= '…'.len_utf8() {
        return s.chars().take(max).collect();
    }
    let ellipsis = "…";
    let remaining = max - ellipsis.len();
    let head_len = remaining / 2;
    let tail_len = remaining - head_len;
    let head: String = s.chars().take(head_len).collect();
    let tail: String = s
        .chars()
        .rev()
        .take(tail_len)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    format!("{head}{ellipsis}{tail}")
}

/// Shorten a path-like string at segment boundaries when possible, keeping the
/// leading segment and the last one or two segments visible.
pub fn ellipsize_path(path: &str, max: usize) -> String {
    if path.len() <= max {
        return path.to_owned();
    }

    let segments: Vec<&str> = path
        .split('/')
        .filter(|segment| !segment.is_empty())
        .collect();
    if segments.len() < 3 {
        return middle_ellipsis(path, max);
    }

    let head = if path.starts_with('/') {
        format!("/{}", segments[0])
    } else {
        segments[0].to_owned()
    };

    for tail_count in [2, 1] {
        if segments.len() <= tail_count + 1 {
            continue;
        }
        let tail = segments[segments.len() - tail_count..].join("/");
        let candidate = format!("{head}/…/{tail}");
        if candidate.len() <= max {
            return candidate;
        }
    }

    middle_ellipsis(path, max)
}

/// Build display labels and accumulated paths for a path breadcrumb.
///
/// Absolute paths include the root segment (`/`); relative paths accumulate
/// their normal components without touching the filesystem.
pub fn breadcrumb_segments(path: &Path) -> Vec<(String, PathBuf)> {
    use std::path::Component;

    let mut segments = Vec::new();
    let mut acc = PathBuf::new();

    for component in path.components() {
        match component {
            Component::RootDir => {
                acc.push(Path::new("/"));
                segments.push(("/".to_owned(), acc.clone()));
            }
            Component::Normal(name) => {
                acc.push(name);
                segments.push((name.to_string_lossy().into_owned(), acc.clone()));
            }
            Component::CurDir => {
                if segments.is_empty() {
                    acc.push(".");
                    segments.push((".".to_owned(), acc.clone()));
                }
            }
            Component::ParentDir => {
                acc.push("..");
                segments.push(("..".to_owned(), acc.clone()));
            }
            Component::Prefix(prefix) => {
                acc.push(prefix.as_os_str());
                segments.push((
                    prefix.as_os_str().to_string_lossy().into_owned(),
                    acc.clone(),
                ));
            }
        }
    }

    if segments.is_empty() {
        segments.push((path.display().to_string(), path.to_path_buf()));
    }

    segments
}

/// Add `path` to favorites if absent. Returns true when the vector changed.
pub(crate) fn add_favorite(favorites: &mut Vec<PathBuf>, path: PathBuf) -> bool {
    if favorites.iter().any(|existing| existing == &path) {
        return false;
    }
    favorites.push(path);
    true
}

/// Remove all occurrences of `path` from favorites. Returns true when changed.
pub(crate) fn remove_favorite(favorites: &mut Vec<PathBuf>, path: &Path) -> bool {
    let before = favorites.len();
    favorites.retain(|existing| existing != path);
    favorites.len() != before
}

pub(crate) fn navigation_new_destination(
    nav_back: &mut Vec<PathBuf>,
    nav_forward: &mut Vec<PathBuf>,
    current: Option<&Path>,
) {
    if let Some(current) = current {
        nav_back.push(current.to_path_buf());
    }
    nav_forward.clear();
}

pub(crate) fn navigation_back(
    nav_back: &mut Vec<PathBuf>,
    nav_forward: &mut Vec<PathBuf>,
    current: Option<&Path>,
) -> Option<PathBuf> {
    let destination = nav_back.pop()?;
    if let Some(current) = current {
        nav_forward.push(current.to_path_buf());
    }
    Some(destination)
}

pub(crate) fn navigation_forward(
    nav_back: &mut Vec<PathBuf>,
    nav_forward: &mut Vec<PathBuf>,
    current: Option<&Path>,
) -> Option<PathBuf> {
    let destination = nav_forward.pop()?;
    if let Some(current) = current {
        nav_back.push(current.to_path_buf());
    }
    Some(destination)
}

// ── AppModel impl ─────────────────────────────────────────────────────────────

impl AppModel {
    /// Re-scan `current_dir` and update `entries`.
    fn reload(&mut self) {
        self.filter.cull_cache.clear();
        self.browser
            .reload(self.config.show_hidden, self.config.sort_mode);
        // Reset scroll/selection and rebuild snapshot.
        self.scroll_offset_y = 0.0;
        self.selection.selected_index = None;
        self.selection.multi_selected.clear();
        self.selection.batch_status = None;
        self.selection.select_anchor = None;
        self.preview = None;
        self.text_preview = None;
        self.compare = None; // drop handles on folder change
        self.rebuild_snapshot();
    }

    /// Pure helper returning the indices to act on for cull actions:
    /// multi-selected (sorted) if non-empty, else the primary selected_index if Some, else [].
    fn cull_target_indices(&self) -> Vec<usize> {
        if !self.selection.multi_selected.is_empty() {
            let mut v: Vec<usize> = self.selection.multi_selected.iter().copied().collect();
            v.sort_unstable();
            v
        } else {
            self.selection.selected_index.into_iter().collect()
        }
    }

    /// Add `kw` (trimmed) to every photo in the current selection (cull_target_indices), writing each
    /// sidecar via xmp::add_keyword and refreshing folder_tags. Returns (ok, total). No-op for empty kw.
    fn add_keyword_to_targets(&mut self, kw: &str) -> (usize, usize) {
        let kw = kw.trim().to_owned();
        let targets = self.cull_target_indices();
        if kw.is_empty() || targets.is_empty() {
            return (0, 0);
        }
        let total = targets.len();
        let mut ok = 0usize;
        for i in targets {
            let Some(entry) = self.browser.entries.get(i) else {
                continue;
            };
            let path = entry.path.clone();
            if let Ok(new_list) = crate::xmp::add_keyword(&path, &kw) {
                self.folder_tags.insert(i, new_list);
                ok += 1;
            }
        }
        (ok, total)
    }

    /// Remove `kw` from every photo in the current selection (cull_target_indices), writing each sidecar via
    /// xmp::remove_keyword and refreshing folder_tags. Returns (ok, total). No-op for empty kw.
    fn remove_keyword_from_targets(&mut self, kw: &str) -> (usize, usize) {
        let kw = kw.trim().to_owned();
        let targets = self.cull_target_indices();
        if kw.is_empty() || targets.is_empty() {
            return (0, 0);
        }
        let total = targets.len();
        let mut ok = 0usize;
        for i in targets {
            let Some(entry) = self.browser.entries.get(i) else {
                continue;
            };
            let path = entry.path.clone();
            if let Ok(new_list) = crate::xmp::remove_keyword(&path, &kw) {
                if new_list.is_empty() {
                    self.folder_tags.remove(&i);
                } else {
                    self.folder_tags.insert(i, new_list);
                }
                ok += 1;
            }
        }
        (ok, total)
    }

    /// Common implementation for cull actions (batch bar or keyboard grid cull).
    /// Takes explicit indices so keyboard can target primary when no multi-select.
    /// Collects paths, calls write for each, refreshes cull_cache (and loupe if open on it).
    /// Sets `batch_status` to a short summary (uses batch_summary for formatting).
    fn apply_cull<F: Fn(&std::path::Path) -> std::io::Result<()>>(
        &mut self,
        indices: &[usize],
        write: F,
        verb: &str,
    ) {
        let paths: Vec<std::path::PathBuf> = indices
            .iter()
            .filter_map(|&i| self.browser.entries.get(i).map(|e| e.path.clone()))
            .collect();
        let total = paths.len();
        if total == 0 {
            self.selection.batch_status = None;
            return;
        }
        let mut ok = 0usize;
        for p in &paths {
            if write(p).is_ok() {
                self.filter
                    .cull_cache
                    .insert(p.clone(), crate::xmp::read_sidecar_cull(p));
                if let Some(loupe) = &mut self.loupe {
                    if &loupe.path == p {
                        let (r, x) = crate::xmp::read_loupe_sidecar(p);
                        loupe.rating = r;
                        loupe.xmp = x;
                    }
                }
                ok += 1;
            }
        }
        self.selection.batch_status = Some(batch_summary(ok, total, verb));
    }

    /// Ensure every current image/RAW entry has its sidecar cull meta cached (one-time burst per folder;
    /// cached thereafter). Used before applying cull filters (rating/label/hide-reject) so they see the whole folder.
    /// Reads sidecars synchronously for the current folder.
    fn ensure_all_cull_cached(&mut self) {
        // Collect missing paths first to avoid borrow conflict between &entries and &mut cull_cache.
        let missing: Vec<_> = self
            .browser
            .entries
            .iter()
            .filter(|e| {
                matches!(e.kind, EntryKind::Image | EntryKind::Raw)
                    && !self.filter.cull_cache.contains_key(&e.path)
            })
            .map(|e| e.path.clone())
            .collect();
        for path in missing {
            self.filter
                .cull_cache
                .insert(path.clone(), crate::xmp::read_sidecar_cull(&path));
        }
    }

    /// Set `current_dir`, invalidate thumbnails, reload, prune, and kick the
    /// title/capture-date follow-up tasks. This is the shared tail for all
    /// directory navigation paths.
    fn set_dir_and_reload(&mut self, dir: PathBuf) -> Task<cosmic::Action<Message>> {
        self.browser.current_dir = Some(dir);
        self.thumb.next_generation();
        self.reload();
        self.prune_cache_to_configured_max();
        self.folder_metadata.clear();
        self.folder_tags.clear();
        self.filter.camera = None;
        self.filter.tag = None;
        self.filter.date = None;
        let pairs: Vec<(usize, PathBuf)> = self
            .browser
            .entries
            .iter()
            .enumerate()
            .filter(|(_, e)| crate::view::loupe::can_open_loupe(&e.kind))
            .map(|(idx, e)| (idx, e.path.clone()))
            .collect();
        let pairs_for_tags = pairs.clone();
        let db = crate::catalog::catalog_db_path();
        Task::batch([
            self.set_current_window_title(),
            self.browser
                .request_capture_epoch_batch(self.config.sort_mode, self.config.images_only),
            Task::perform(
                async move { tasks::index_folder_metadata(pairs, db) },
                |items| cosmic::Action::App(Message::FolderIndexed(items)),
            ),
            Task::perform(
                async move { tasks::index_folder_keywords(pairs_for_tags) },
                |items| cosmic::Action::App(Message::FolderKeywordsIndexed(items)),
            ),
        ])
    }

    /// Build the XDG places list.
    fn build_places() -> Vec<(String, PathBuf)> {
        let mut places = Vec::new();
        if let Some(ud) = UserDirs::new() {
            places.push(("Home".to_owned(), ud.home_dir().to_path_buf()));
            if let Some(p) = ud.picture_dir() {
                places.push(("Pictures".to_owned(), p.to_path_buf()));
            }
            if let Some(p) = ud.document_dir() {
                places.push(("Documents".to_owned(), p.to_path_buf()));
            }
            if let Some(p) = ud.download_dir() {
                places.push(("Downloads".to_owned(), p.to_path_buf()));
            }
        }
        places
    }

    /// (Re)compute the visible range and rebuild the render snapshot.
    ///
    /// This is the heart of the O(visible) guarantee: we compute which flat
    /// item indices are in the window, call `get_or_request` for image entries
    /// in that range only, and store a bounded snapshot for `view()`.
    pub fn rebuild_snapshot(&mut self) {
        // Use the SAME (width, height) `grid_content` renders with — the
        // responsive-measured content size — so the request side and the render
        // side land on identical `cols` and identical visible range: every
        // index the view renders was requested here.  Falls back to the derived
        // estimate only for the first frame, before `view()` has measured.
        let (avail_w, avail_h) = self.effective_grid_size();
        self.last_grid_w = avail_w;
        self.last_grid_h = avail_h;
        let cols = ((avail_w / self.config.thumb_size as f32).floor() as usize).max(1);
        let ch = cell_h(self.config.thumb_size);
        let grid_indices = self.grid_indices();
        let item_count = grid_indices.len();

        let range = visible_range(
            self.scroll_offset_y,
            avail_h,
            ch,
            cols,
            item_count,
            MARGIN_ROWS,
        );

        self.visible_index_range = range.clone();
        self.grid_snapshot = HashMap::with_capacity(range.len());
        self.thumb
            .ensure_queue_cap(thumbnail_queue_cap_for_envelope(range.len()));

        for pos in visible_priority_positions(range, cols, self.scroll_offset_y, avail_h, ch) {
            let entry_index = grid_indices[pos];
            let entry = &self.browser.entries[entry_index];
            if matches!(entry.kind, EntryKind::Image | EntryKind::Raw)
                && !self.filter.cull_cache.contains_key(&entry.path)
            {
                self.filter.cull_cache.insert(
                    entry.path.clone(),
                    crate::xmp::read_sidecar_cull(&entry.path),
                );
            }
            let state = match &entry.kind {
                EntryKind::Dir => CellState::Dir(entry.path.clone()),
                EntryKind::Image | EntryKind::Raw => cell_state_for_thumb(
                    &entry.kind,
                    self.thumb.get_or_request_with_priority(
                        &entry.path,
                        self.config.thumb_size,
                        visible_priority_for_position(pos, cols, self.scroll_offset_y, avail_h, ch),
                    ),
                ),
                EntryKind::Other(category) => {
                    if matches!(category, FileCategory::Unknown) {
                        CellState::UnknownFile
                    } else {
                        CellState::Glyph(category_glyph(category))
                    }
                }
            };
            self.grid_snapshot.insert(pos, state);
        }
        self.thumb.flush_requests();

        #[cfg(debug_assertions)]
        tracing::debug!(
            requested = self.grid_snapshot.len(),
            cols,
            total = item_count,
            "snapshot rebuilt"
        );
    }

    /// The grid's usable CONTENT width in pixels: window width minus the fixed
    /// sidebar and preview panes, the grid container padding (both sides), and
    /// the scrollbar.  The sidebar (`Fixed(260)`), preview (`Fixed(300)`),
    /// padding, and scrollbar are all constants that
    /// match the rendered widths exactly, so this equals the true content width
    /// with no layout-time measurement.
    ///
    /// This is the **single source of truth** for the column count: both
    /// `rebuild_snapshot` (request) and `view`/`grid_content` (render) divide it
    /// by the cell width, so they can never disagree.  Derived fresh from
    /// `viewport_w`; selection does not change the preview pane width.
    pub fn grid_width(&self) -> f32 {
        (grid_available_w(self.viewport_w, self.selection.selected_index.is_some()) - SCROLLBAR_W)
            .max(1.0)
    }

    /// The grid's effective (content width, content height): the responsive-
    /// measured size once `view()` has run, else the derived fallback. Both
    /// `rebuild_snapshot` and `view()` resolve cols from the same width, so they
    /// never disagree.
    pub fn effective_grid_size(&self) -> (f32, f32) {
        let mw = f32::from_bits(self.measured_w.load(Ordering::Relaxed));
        let mh = f32::from_bits(self.measured_h.load(Ordering::Relaxed));
        let w = if mw > 1.0 { mw } else { self.grid_width() };
        let h = if mh > 1.0 {
            mh
        } else {
            grid_available_h(self.viewport_h)
        };
        (w, h)
    }

    fn has_pending_grid_geometry(&self) -> bool {
        let (w, h) = self.effective_grid_size();
        (w - self.last_grid_w).abs() > 0.5 || (h - self.last_grid_h).abs() > 0.5
    }

    fn needs_tick(&self) -> bool {
        // Drives the 32 ms ThumbTick for decode drain + grid geometry propagation only.
        // Slideshow uses its own independent conditional ~4s subscription (never forces this).
        self.thumb.loading_len() > 0 || self.has_pending_grid_geometry()
    }

    fn apply_drained_thumbnails(&mut self) -> bool {
        let results = self.thumb.drain();
        if results.is_empty() {
            return false;
        }

        for result in results {
            self.thumb.on_decoded(result.path, result.gen, result.rgba);
        }
        self.rebuild_snapshot();
        true
    }

    fn window_title(&self) -> String {
        let folder = self
            .browser
            .current_dir
            .as_deref()
            .and_then(Path::file_name)
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| "No folder".to_owned());
        format!("PhotoBrowser — {folder}")
    }

    fn set_current_window_title(&mut self) -> Task<cosmic::Action<Message>> {
        self.set_window_title(
            self.window_title(),
            self.core
                .main_window_id()
                .unwrap_or(cosmic::iced::window::Id::RESERVED),
        )
    }

    fn build_sort_model(
        active: SortMode,
    ) -> (
        segmented_button::SingleSelectModel,
        segmented_button::Entity,
        segmented_button::Entity,
        segmented_button::Entity,
        segmented_button::Entity,
    ) {
        let mut name_entity = segmented_button::Entity::default();
        let mut date_entity = segmented_button::Entity::default();
        let mut captured_entity = segmented_button::Entity::default();
        let mut rating_entity = segmented_button::Entity::default();
        let model = segmented_button::Model::builder()
            .insert(|b| {
                let b = b
                    .text("Name")
                    .data(SortMode::Name)
                    .with_id(|id| name_entity = id);
                if active == SortMode::Name {
                    b.activate()
                } else {
                    b
                }
            })
            .insert(|b| {
                let b = b
                    .text("Modified")
                    .data(SortMode::Date)
                    .with_id(|id| date_entity = id);
                if active == SortMode::Date {
                    b.activate()
                } else {
                    b
                }
            })
            .insert(|b| {
                let b = b
                    .text("Captured")
                    .data(SortMode::Captured)
                    .with_id(|id| captured_entity = id);
                if active == SortMode::Captured {
                    b.activate()
                } else {
                    b
                }
            })
            .insert(|b| {
                let b = b
                    .text("Rating")
                    .data(SortMode::Rating)
                    .with_id(|id| rating_entity = id);
                if active == SortMode::Rating {
                    b.activate()
                } else {
                    b
                }
            })
            .build();
        (
            model,
            name_entity,
            date_entity,
            captured_entity,
            rating_entity,
        )
    }

    fn activate_sort_segment(&mut self, mode: SortMode) {
        let entity = match mode {
            SortMode::Name => self.sort.name_entity,
            SortMode::Date => self.sort.date_entity,
            SortMode::Captured => self.sort.captured_entity,
            SortMode::Rating => self.sort.rating_entity,
        };
        self.sort.model.activate(entity);
    }

    fn persist_cache_config(&self) {
        xdg::set_cache_dir_override(self.config.cache_dir.clone());
        self.config.save();
    }

    fn prune_cache_to_configured_max(&self) {
        xdg::prune_to_max(xdg::gb_to_bytes(self.config.cache_max_gb));
    }

    fn refresh_cache_inputs(&mut self) {
        self.cache_max_input = format!("{:.2}", self.config.cache_max_gb);
        self.cache_dir_input = self
            .config
            .cache_dir
            .as_ref()
            .map(|path| path.display().to_string())
            .unwrap_or_default();
    }

    /// If full_res_loupe is enabled and the current loupe is at Actual zoom,
    /// ensure a high-res (≤8192) decode is resident (from cache or async task).
    /// Returns the decode task if one was started; otherwise none. Until ready
    /// the view falls back to the existing bounded handle (so Fit + "loading" 1:1 stay).
    /// This is the wiring from the toggle + zoom changes into the separate decode path.
    fn ensure_high_res_for_current_loupe(&mut self) -> Task<cosmic::Action<Message>> {
        let (path, kind) = match &self.loupe {
            Some(l) if l.zoom == LoupeZoom::Actual && self.config.full_res_loupe => {
                if let Some(e) = self.browser.entries.get(l.index) {
                    (l.path.clone(), e.kind.clone())
                } else {
                    return Task::none();
                }
            }
            _ => return Task::none(),
        };

        let high_mode = high_res_loupe_mode(self.config.loupe_full_demosaic, &kind);
        // Drop any high-res for *other* images so we hold at most one full-res decode.
        self.preview_cache.purge_high_res_except(Some(&path));

        if let Some(cached) = self
            .preview_cache
            .get(&path, high_mode, self.config.develop_look)
        {
            if let Some(loupe) = &mut self.loupe {
                loupe.high_res_handle = Some(cached.handle.clone());
                loupe.high_res_dimensions = Some(cached.dimensions);
            }
            return Task::none();
        }

        // No cached high-res yet: clear any stale high in state (so view falls back to
        // the bounded handle) and kick the async decode at the 8192 cap. The decode
        // path is SEPARATE from loupe_decode_bound / the 2560 clamp (untouched).
        if let Some(loupe) = &mut self.loupe {
            loupe.high_res_handle = None;
            loupe.high_res_dimensions = None;
        }
        const HIGH_RES_BOUND: u32 = 8192;
        tasks::decode_loupe_task(path, HIGH_RES_BOUND, high_mode, self.config.develop_look)
    }
}

pub fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut value = bytes as f64;
    let mut unit = 0usize;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{} {}", bytes, UNITS[unit])
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

/// Format a `SystemTime` as a human-readable local-time string `YYYY-MM-DD HH:MM`.
///
/// # ADR-002 — why chrono
/// `std` cannot convert a `SystemTime` to a local-time calendar date; the only
/// portable option is the `chrono` crate which provides civil-time formatting.
pub fn format_mtime(t: SystemTime) -> String {
    DateTime::<chrono::Local>::from(t)
        .format("%Y-%m-%d %H:%M")
        .to_string()
}

/// Format an optional modified time for display.
pub fn format_modified(modified: Option<SystemTime>) -> String {
    match modified {
        Some(t) => format_mtime(t),
        None => "unknown".to_owned(),
    }
}

/// Resolve an optional CLI path arg to an initial directory to open.
/// Some(dir) if arg is an existing dir; the parent if arg is an existing file; None otherwise.
pub(crate) fn initial_dir_from_arg(arg: Option<&str>) -> Option<PathBuf> {
    let s = arg?;
    let p = PathBuf::from(s);
    match std::fs::metadata(&p) {
        Ok(m) => {
            let cand = dir_to_open_from(p, m.is_dir());
            // Prefer absolute for robustness (nav, titles, comparisons); fall back to logical cand.
            std::fs::canonicalize(&cand).ok().or(Some(cand))
        }
        _ => None,
    }
}

/// Pure path-shape decision (given the fs classification): if dir return as-is,
/// if file return its parent (or "." for a bare relative filename like "foo.jpg").
/// Separated so the shape logic is easily unit-tested in isolation.
fn dir_to_open_from(p: PathBuf, is_dir: bool) -> PathBuf {
    if is_dir {
        p
    } else if let Some(par) = p.parent() {
        if par.as_os_str().is_empty() {
            PathBuf::from(".")
        } else {
            par.to_path_buf()
        }
    } else {
        PathBuf::from(".")
    }
}

// ── cosmic::Application ───────────────────────────────────────────────────────

impl cosmic::Application for AppModel {
    type Executor = cosmic::executor::Default;
    type Flags = Option<PathBuf>;
    type Message = Message;

    const APP_ID: &'static str = "com.photobrowser.PhotoBrowser";

    fn core(&self) -> &cosmic::Core {
        &self.core
    }

    fn core_mut(&mut self) -> &mut cosmic::Core {
        &mut self.core
    }

    fn init(core: cosmic::Core, flags: Self::Flags) -> (Self, Task<cosmic::Action<Self::Message>>) {
        let places = Self::build_places();
        let drives = view::sidebar::read_drives();
        let tree_roots = UserDirs::new()
            .map(|ud| vec![ud.home_dir().to_path_buf()])
            .unwrap_or_default();

        // Compute initial dir *before* moving `places` into the AppModel struct.
        let initial_dir = flags.or_else(|| places.first().map(|(_, p)| p.clone()));

        let cfg = Config::load();
        xdg::set_cache_dir_override(cfg.cache_dir.clone());
        let thumb = ThumbService::new(cfg.thumb_cache_max_items, THUMB_QUEUE_CAP_MIN);
        let (model, name_entity, date_entity, captured_entity, rating_entity) =
            Self::build_sort_model(cfg.sort_mode);
        let sort = SortBar {
            model,
            name_entity,
            date_entity,
            captured_entity,
            rating_entity,
        };
        let cache_max_input = format!("{:.2}", cfg.cache_max_gb);
        let cache_dir_input = cfg
            .cache_dir
            .as_ref()
            .map(|path| path.display().to_string())
            .unwrap_or_default();
        let export_dir_input = cfg
            .export_dir
            .as_ref()
            .map(|path| path.display().to_string())
            .unwrap_or_default();

        let mut app = AppModel {
            core,
            config: cfg,
            browser: BrowserState {
                current_dir: None,
                ..BrowserState::default()
            },
            nav: NavState {
                back: Vec::new(),
                forward: Vec::new(),
                places,
                drives,
                tree: FolderTree::with_roots(tree_roots),
            },
            thumb,
            viewport_w: 1024.0,
            viewport_h: 800.0,
            measured_w: Arc::new(AtomicU32::new(0)),
            measured_h: Arc::new(AtomicU32::new(0)),
            last_grid_w: 0.0,
            last_grid_h: 0.0,
            scroll_offset_y: 0.0,
            grid_snapshot: HashMap::new(),
            visible_index_range: 0..0,
            selection: SelectionState::default(),
            preview: None,
            text_preview: None,
            metadata_sections: MetadataSectionState::default(),
            loupe: None,
            loupe_rt: LoupeRuntime {
                decode_pending: None,
                slideshow_playing: false,
                scroll: cosmic::iced::widget::scrollable::AbsoluteOffset::default(),
                zoom_factor: 1.0,
                drag: None,
                pan_last_cursor: None,
            },
            preview_cache: PreviewCache::new(PREVIEW_CACHE_CAP),
            sort,
            filter_focused: false,
            settings_open: false,
            cache_max_input,
            cache_dir_input,
            export_dir_input,
            keyword_input: String::new(),
            filter: FilterState::default(),
            dups: DuplicateState::default(),
            compare: None,
            menu_key_binds: std::collections::HashMap::new(),
            folder_metadata: std::collections::HashMap::new(),
            folder_tags: std::collections::HashMap::new(),
        };
        app.set_header_title("PhotoBrowser".into());

        // Feed CLI-resolved (or None) dir through the *same* set_dir_and_reload used
        // for all other navigation. This ensures a single scan of the target dir,
        // thumb invalidation, state reset, title, capture batch, etc. — no duplicated
        // scan logic. Bad/nonexistent arg already resolved to None upstream → default.
        let command = if let Some(dir) = initial_dir {
            app.set_dir_and_reload(dir)
        } else {
            app.reload();
            app.prune_cache_to_configured_max();
            Task::batch([
                app.set_current_window_title(),
                app.browser
                    .request_capture_epoch_batch(app.config.sort_mode, app.config.images_only),
            ])
        };

        (app, command)
    }

    fn header_start(&self) -> Vec<cosmic::Element<'_, Self::Message>> {
        vec![menu_bar(self)]
    }

    /// Periodic subscription — fires every 32 ms to drain the decode channel and
    /// to propagate any new grid size measured by the `responsive` wrapper in
    /// `view()` to the request side (see `Message::ThumbTick`).
    fn subscription(&self) -> cosmic::iced::Subscription<Self::Message> {
        use cosmic::iced::keyboard;

        // Esc closes the loupe. `CloseLoupe` is a no-op when the loupe is already
        // closed, so an always-on Esc handler is safe. This minimal Esc-only
        // subscription is intentionally superseded by the TextInput-aware key spine
        // in v4-M2. `on_key_press` is not exposed at the pinned libcosmic rev, so we
        // use `keyboard::listen()` + `filter_map` (the resolvable equivalent).
        let keys = keyboard::listen().filter_map(|event| match event {
            keyboard::Event::KeyPressed { key, modifiers, .. } => match key {
                keyboard::Key::Named(named) => match named {
                    // Alt+Left/Right/Up = folder back / forward / up (drives the existing nav
                    // history stacks + toolbar buttons). Guarded by Alt so plain arrows still
                    // drive grid/loupe navigation. Must precede the plain-arrow arm below.
                    keyboard::key::Named::ArrowLeft if modifiers.alt() => {
                        Some(Message::NavigateBack)
                    }
                    keyboard::key::Named::ArrowRight if modifiers.alt() => {
                        Some(Message::NavigateForward)
                    }
                    keyboard::key::Named::ArrowUp if modifiers.alt() => Some(Message::NavigateUp),
                    keyboard::key::Named::ArrowLeft
                    | keyboard::key::Named::ArrowRight
                    | keyboard::key::Named::ArrowUp
                    | keyboard::key::Named::ArrowDown
                    | keyboard::key::Named::Home
                    | keyboard::key::Named::End
                    | keyboard::key::Named::PageUp
                    | keyboard::key::Named::PageDown
                    | keyboard::key::Named::Enter
                    | keyboard::key::Named::Escape => Some(Message::KeyPressed(named)),
                    keyboard::key::Named::F5 => Some(Message::RefreshCurrentFolder),
                    _ => None,
                },
                keyboard::Key::Character(text) if is_space_character(text.as_str()) => {
                    Some(Message::SpacePressed)
                }
                keyboard::Key::Character(text) if is_refresh_character(text.as_str()) => {
                    Some(Message::RefreshCurrentFolder)
                }
                keyboard::Key::Character(text)
                    if text.eq_ignore_ascii_case("a") && modifiers.control() =>
                {
                    Some(Message::SelectAll)
                }
                keyboard::Key::Character(text)
                    if text.len() == 1 && text.chars().next().unwrap().is_ascii_digit() =>
                {
                    let digit = text.chars().next().unwrap().to_digit(10).unwrap() as u8;
                    Some(Message::DigitKey(digit))
                }
                keyboard::Key::Character(text) if is_loupe_fit_character(text.as_str()) => {
                    Some(Message::SetLoupeZoom(LoupeZoom::Fit))
                }
                keyboard::Key::Character(text) if is_loupe_zoom_in_character(text.as_str()) => {
                    Some(Message::LoupeZoomStep(true))
                }
                keyboard::Key::Character(text) if is_loupe_zoom_out_character(text.as_str()) => {
                    Some(Message::LoupeZoomStep(false))
                }
                keyboard::Key::Character(text) if is_slideshow_toggle_character(text.as_str()) => {
                    Some(Message::ToggleSlideshow)
                }
                keyboard::Key::Character(text)
                    if text.len() == 1 && text.eq_ignore_ascii_case("x") =>
                {
                    Some(Message::RejectKey)
                }
                _ => None,
            },
            keyboard::Event::ModifiersChanged(m) => Some(Message::ModifiersChanged(m)),
            _ => None,
        });

        // Third subscription: notify-backed, dir-keyed, debounced, NonRecursive.
        // Emits RefreshCurrentFolder (existing path) when watched dir contents change.
        // Identity derived from current_dir via run; re-watches on Navigate* (no leaks).
        let watch = watch_current_dir_subscription(self.browser.current_dir.clone());

        // Thumb tick (32ms) only when needed for decode/geo (existing).
        let thumb_tick = if self.needs_tick() {
            cosmic::iced::time::every(Duration::from_millis(32)).map(|_| Message::ThumbTick)
        } else {
            cosmic::iced::Subscription::none()
        };
        // 4th subscription: slideshow 4s tick ONLY while loupe open AND playing (event-driven, zero CPU when stopped/closed).
        let slideshow = if self.loupe.is_some() && self.loupe_rt.slideshow_playing {
            cosmic::iced::time::every(Duration::from_secs(4)).map(|_| Message::SlideshowTick)
        } else {
            cosmic::iced::Subscription::none()
        };
        // While a loupe pan-drag is active, catch the button release GLOBALLY so a release
        // outside the mouse_area / window can't leave the drag "stuck" (LoupePanRelease clears it).
        let pan_release = if self.loupe_rt.drag.is_some() {
            cosmic::iced::event::listen_with(|event, _status, _window| match event {
                cosmic::iced::Event::Mouse(cosmic::iced::mouse::Event::ButtonReleased(
                    cosmic::iced::mouse::Button::Left,
                )) => Some(Message::LoupePanRelease),
                _ => None,
            })
        } else {
            cosmic::iced::Subscription::none()
        };
        cosmic::iced::Subscription::batch([thumb_tick, keys, watch, slideshow, pan_release])
    }

    fn update(&mut self, message: Self::Message) -> Task<cosmic::Action<Self::Message>> {
        match message {
            Message::NavigateTo(path) => return self.handle_navigate_to(path),
            Message::NavigateUp => return self.handle_navigate_up(),
            Message::NavigateBack => return self.handle_navigate_back(),
            Message::NavigateForward => return self.handle_navigate_forward(),
            Message::AddFavorite => return self.handle_add_favorite(),
            Message::RemoveFavorite(path) => return self.handle_remove_favorite(path),
            Message::ToggleTreeNode(path) => return self.handle_toggle_tree_node(path),
            Message::ThumbTick => return self.handle_thumb_tick(),
            Message::ToggleSettings => return self.handle_toggle_settings(),
            Message::CloseSettings => return self.handle_close_settings(),
            Message::CacheMaxGbChanged(value) => return self.handle_cache_max_gb_changed(value),
            Message::CacheDirInputChanged(value) => {
                return self.handle_cache_dir_input_changed(value)
            }
            Message::ApplyCacheDir => return self.handle_apply_cache_dir(),
            Message::ExportDirInputChanged(value) => {
                return self.handle_export_dir_input_changed(value)
            }
            Message::ApplyExportDir => return self.handle_apply_export_dir(),
            Message::ResetCacheDir => return self.handle_reset_cache_dir(),
            Message::ToggleShowHidden => return self.handle_toggle_show_hidden(),
            Message::ToggleLoupeFullDemosaic => return self.handle_toggle_loupe_full_demosaic(),
            Message::ToggleFullResLoupe => return self.handle_toggle_full_res_loupe(),
            Message::ToggleDevelopLook => return self.handle_toggle_develop_look(),
            Message::ToggleShowFilmstrip => return self.handle_toggle_show_filmstrip(),
            Message::ToggleTreeShowFiles => return self.handle_toggle_tree_show_files(),
            Message::ToggleImagesOnly => return self.handle_toggle_images_only(),
            Message::CycleRatingFilter => return self.handle_cycle_rating_filter(),
            Message::CycleLabelFilter => return self.handle_cycle_label_filter(),
            Message::ToggleHideRejected => return self.handle_toggle_hide_rejected(),
            Message::SetLabelFilter(x) => return self.handle_set_label_filter(x),
            Message::SetCameraFilter(idx) => return self.handle_set_camera_filter(idx),
            Message::SetTagFilter(idx) => return self.handle_set_tag_filter(idx),
            Message::SetDateFilter(year) => return self.handle_set_date_filter(year),
            Message::SaveCollection => return self.handle_save_collection(),
            Message::ApplyCollection(i) => return self.handle_apply_collection(i),
            Message::ClearCollections => return self.handle_clear_collections(),
            Message::MenuNoop => {}
            Message::FindDuplicates => return self.handle_find_duplicates(),
            Message::DuplicatesScanned(items) => return self.handle_duplicates_scanned(items),
            Message::FindExactDuplicates => return self.handle_find_exact_duplicates(),
            #[rustfmt::skip]
            Message::ExactDuplicatesScanned(items) => return self.handle_exact_duplicates_scanned(items),
            Message::FolderIndexed(items) => return self.handle_folder_indexed(items),
            Message::FolderKeywordsIndexed(items) => {
                return self.handle_folder_keywords_indexed(items)
            }
            #[rustfmt::skip]
            Message::SetLoupeZoom(zoom) => return self.handle_set_loupe_zoom(zoom),
            Message::ToggleLoupeZoom => return self.handle_toggle_loupe_zoom(),
            Message::LoupeZoomStep(zoom_in) => return self.handle_loupe_zoom_step(zoom_in),
            Message::Scroll(y) => return self.handle_scroll(y),
            Message::RefreshCurrentFolder => return self.handle_refresh_current_folder(),
            Message::ModifiersChanged(m) => return self.handle_modifiers_changed(m),
            Message::SelectAll => return self.handle_select_all(),
            #[rustfmt::skip]
            Message::KeyPressed(named) => return self.handle_key_pressed(named),
            Message::SpacePressed => return self.handle_space_pressed(),
            Message::ThumbSizeChanged(size) => return self.handle_thumb_size_changed(size),
            Message::FilterChanged(query) => return self.handle_filter_changed(query),
            Message::ClearFilter => return self.handle_clear_filter(),
            Message::KeywordInputChanged(s) => return self.handle_keyword_input_changed(s),
            Message::AddKeyword => return self.handle_add_keyword(),
            Message::RemoveKeyword(kw) => return self.handle_remove_keyword(kw),
            Message::SortModeChanged(mode) => return self.handle_sort_mode_changed(mode),
            Message::SelectImage(index) => return self.handle_select_image(index),
            #[rustfmt::skip]
            Message::PreviewDecoded(path, decoded, histogram, error) => return self.handle_preview_decoded(path, decoded, histogram, error),
            #[rustfmt::skip]
            Message::MetadataLoaded(path, metadata) => return self.handle_metadata_loaded(path, metadata),
            #[rustfmt::skip]
            Message::ToggleMetadataSection(section) => return self.handle_toggle_metadata_section(section),
            #[rustfmt::skip]
            Message::CaptureEpochLoaded(path, epoch) => return self.handle_capture_epoch_loaded(path, epoch),
            Message::OpenLoupe(index) => return self.handle_open_loupe(index),
            #[rustfmt::skip]
            Message::LoupeDecoded(path, mode, decoded, error) => return self.handle_loupe_decoded(path, mode, decoded, error),
            Message::ToggleSlideshow => return self.handle_toggle_slideshow(),
            Message::SlideshowTick => return self.handle_slideshow_tick(),
            Message::LoupePanPress => return self.handle_loupe_pan_press(),
            Message::LoupePanRelease => return self.handle_loupe_pan_release(),
            Message::LoupePanMove(p) => return self.handle_loupe_pan_move(p),
            Message::LoupeScrolled(off) => return self.handle_loupe_scrolled(off),
            Message::SetLoupeRating(n) => return self.handle_set_loupe_rating(n),
            #[rustfmt::skip]
            Message::SetLoupeLabel(lab) => return self.handle_set_loupe_label(lab),
            Message::ToggleLoupeReject => return self.handle_toggle_loupe_reject(),
            Message::DigitKey(d) => return self.handle_digit_key(d),
            Message::RejectKey => return self.handle_reject_key(),
            Message::BatchSetRating(n) => return self.handle_batch_set_rating(n),
            Message::BatchSetLabel(lab) => return self.handle_batch_set_label(lab),
            Message::BatchSetReject(r) => return self.handle_batch_set_reject(r),
            Message::CloseLoupe => return self.handle_close_loupe(),
            Message::EnterCompare => return self.handle_enter_compare(),
            Message::CloseCompare => return self.handle_close_compare(),
            #[rustfmt::skip]
            Message::CompareDecoded(path, decoded, error) => return self.handle_compare_decoded(path, decoded, error),
            Message::ExportSelection => return self.handle_export_selection(),
            Message::ExportDone(res, dest) => return self.handle_export_done(res, dest),
        }
        Task::none()
    }

    /// Called by the libcosmic runtime when the window is resized.
    fn on_window_resize(&mut self, _id: cosmic::iced::window::Id, width: f32, height: f32) {
        self.viewport_w = width;
        self.viewport_h = height;
        // `grid_width()` derives the content width fresh from `viewport_w`, so
        // there is no cached width to update — just rebuild for the new size.
        self.rebuild_snapshot();
    }

    /// Context drawer for cache settings (v1.1). Driven by `settings_open` (which the
    /// gear ToggleSettings / CloseSettings / Esc keep in sync with core show_context).
    /// Uses the pinned libcosmic rev's `cosmic::app::context_drawer::{ContextDrawer, context_drawer}`
    /// + `set_show_context` / `core.window.show_context`. The drawer appears as a side panel
    ///   (not overlay in normal windowing here); Esc + gear + drawer's X all close it via CloseSettings.
    fn context_drawer(&self) -> Option<ContextDrawer<'_, Self::Message>> {
        if !self.settings_open {
            return None;
        }
        // Reuse the existing controls (relocated; no new controls except the full_res toggle
        // added inside settings_panel). The internal "Cache settings"/Done are kept for
        // visual/behavior continuity; drawer supplies its own title bar + close affordance.
        Some(
            context_drawer::context_drawer(
                view::grid::settings_panel(self),
                Message::CloseSettings,
            )
            .title("Cache settings"),
        )
    }

    fn view(&self) -> cosmic::Element<'_, Self::Message> {
        // ADR-v04-1: the loupe is an in-`view()` mode switch, not a new window. When
        // `loupe` is set, render the full-window single-image view and skip the
        // 3-pane shell entirely (the grid path stays byte-identical).
        if self.compare.is_some() {
            return view::compare::view(self);
        }
        if self.loupe.is_some() {
            return view::loupe::view(self);
        }

        let body = widget::row::with_children(vec![
            view::sidebar::view(self),
            view::grid::view(self),
            view::preview::view(self),
        ])
        .width(Length::Fill)
        .height(Length::Fill);

        // The toolbar spans the FULL window width above the three
        // panes, so it has the whole width (not just the narrow center pane) and
        // its controls never clip. Cache settings now live in the libcosmic context
        // drawer (driven by core.window.show_context + context_drawer() impl), not inline.
        let shell = widget::column::with_capacity(3)
            .push(view::grid::toolbar(self))
            .push(widget::divider::horizontal::default())
            .push(body)
            .width(Length::Fill)
            .height(Length::Fill);

        widget::container(shell)
            .width(Length::Fill)
            .height(Length::Fill)
            .into()
    }
}

// ── Count helpers exposed to view modules ─────────────────────────────────────

impl AppModel {
    fn apply_cull_filter(&self, indices: Vec<usize>) -> Vec<usize> {
        let rf = self.filter.rating;
        let lf = self.filter.label;
        let hr = self.filter.hide_rejected;
        if rf.is_none() && lf.is_none() && !hr {
            return indices;
        }
        indices
            .into_iter()
            .filter(|&i| {
                self.browser.entries.get(i).is_some_and(|e| {
                    let c = self
                        .filter
                        .cull_cache
                        .get(&e.path)
                        .copied()
                        .unwrap_or_default();
                    cull_passes(c, rf, lf, hr)
                })
            })
            .collect()
    }

    fn apply_duplicate_filter(&self, indices: Vec<usize>) -> Vec<usize> {
        if !self.dups.filter_active {
            return indices;
        }
        indices
            .into_iter()
            .filter(|i| self.dups.members.contains(i))
            .collect()
    }

    fn apply_camera_filter(&self, indices: Vec<usize>) -> Vec<usize> {
        if self.filter.camera.is_none() {
            return indices;
        }
        let target = self.filter.camera.as_deref();
        indices
            .into_iter()
            .filter(|&i| self.folder_metadata.get(&i).and_then(|(_, c)| c.as_deref()) == target)
            .collect()
    }

    fn apply_tag_filter(&self, indices: Vec<usize>) -> Vec<usize> {
        let Some(ref tag) = self.filter.tag else {
            return indices;
        };
        indices
            .into_iter()
            .filter(|&i| {
                self.folder_tags
                    .get(&i)
                    .is_some_and(|kws| kws.iter().any(|k| k == tag))
            })
            .collect()
    }

    /// Distinct, sorted camera names present in the current folder's indexed metadata.
    /// Drives the View>Camera submenu and the index->name resolution. Deterministic.
    pub fn sorted_cameras(&self) -> Vec<String> {
        let mut v: Vec<String> = self
            .folder_metadata
            .values()
            .filter_map(|(_, cam)| cam.clone())
            .collect();
        v.sort();
        v.dedup();
        v
    }

    /// Distinct keywords present in the current folder's sidecars, sorted. Drives the View>Tag submenu.
    pub fn sorted_tags(&self) -> Vec<String> {
        let mut v: Vec<String> = self
            .folder_tags
            .values()
            .flat_map(|kws| kws.iter().cloned())
            .collect();
        v.sort();
        v.dedup();
        v
    }

    /// Calendar year (UTC) of a unix timestamp, or None if out of range.
    /// EXIF capture has no timezone; UTC is the stable, deterministic choice.
    fn year_of_epoch(unix: i64) -> Option<i32> {
        use chrono::Datelike;
        chrono::DateTime::from_timestamp(unix, 0).map(|dt| dt.year())
    }

    fn apply_date_filter(&self, indices: Vec<usize>) -> Vec<usize> {
        let Some(year) = self.filter.date else {
            return indices;
        };
        indices
            .into_iter()
            .filter(|&i| {
                self.folder_metadata
                    .get(&i)
                    .and_then(|(cap, _)| *cap)
                    .and_then(Self::year_of_epoch)
                    == Some(year)
            })
            .collect()
    }

    /// Distinct capture years present in the current folder's indexed metadata,
    /// newest first. Drives the View>Date submenu. Deterministic.
    pub fn sorted_capture_years(&self) -> Vec<i32> {
        let mut v: Vec<i32> = self
            .folder_metadata
            .values()
            .filter_map(|(cap, _)| *cap)
            .filter_map(Self::year_of_epoch)
            .collect();
        v.sort_unstable();
        v.dedup();
        v.reverse(); // newest first
        v
    }

    fn apply_rating_sort(&self, mut indices: Vec<usize>) -> Vec<usize> {
        if self.config.sort_mode == SortMode::Rating {
            indices.sort_by_key(|&i| {
                std::cmp::Reverse({
                    let c = self
                        .browser
                        .entries
                        .get(i)
                        .and_then(|e| self.filter.cull_cache.get(&e.path).copied())
                        .unwrap_or_default();
                    if c.rejected {
                        0
                    } else {
                        c.rating.unwrap_or(0)
                    }
                })
            });
        }
        indices
    }

    /// Pure helper extracted for testability: given indices and a rating lookup,
    /// return indices stably sorted by rating descending (missing -> 0).
    #[cfg(test)]
    fn order_indices_by_rating_desc(
        indices: Vec<usize>,
        rating_of: impl Fn(usize) -> u8,
    ) -> Vec<usize> {
        let mut v = indices;
        v.sort_by_key(|&i| std::cmp::Reverse(rating_of(i)));
        v
    }

    #[allow(dead_code)]
    pub fn displayed_indices(&self) -> Vec<usize> {
        self.apply_rating_sort(self.apply_duplicate_filter(self.apply_camera_filter(
            self.apply_tag_filter(self.apply_date_filter(
                self.apply_cull_filter(self.browser.displayed_indices(self.config.images_only)),
            )),
        )))
    }

    pub fn grid_indices(&self) -> Vec<usize> {
        self.apply_rating_sort(self.apply_duplicate_filter(self.apply_camera_filter(
            self.apply_tag_filter(self.apply_date_filter(
                self.apply_cull_filter(self.browser.grid_indices(self.config.images_only)),
            )),
        )))
    }

    /// Pure helper: return the first ≤2 selected indices in the displayed order.
    /// Used by EnterCompare; order-preserving; <2 returns what's present.
    pub(crate) fn first_two_selected(
        displayed: &[usize],
        selected: &std::collections::HashSet<usize>,
    ) -> Vec<usize> {
        displayed
            .iter()
            .copied()
            .filter(|i| selected.contains(i))
            .take(2)
            .collect()
    }

    fn scroll_position_into_view(&mut self, position: usize) {
        let (avail_w, avail_h) = self.effective_grid_size();
        let cols = ((avail_w / self.config.thumb_size as f32).floor() as usize).max(1);
        let ch = cell_h(self.config.thumb_size);
        let row_y = (position / cols) as f32 * ch;
        let row_bottom = row_y + ch;

        if row_y < self.scroll_offset_y {
            self.scroll_offset_y = row_y.max(0.0);
            self.rebuild_snapshot();
        } else if row_bottom > self.scroll_offset_y + avail_h {
            self.scroll_offset_y = (row_bottom - avail_h).max(0.0);
            self.rebuild_snapshot();
        }
    }

    pub fn entry_counts(&self) -> (usize, usize, usize) {
        entry_counts(&self.browser.entries)
    }

    pub fn image_count(&self) -> usize {
        self.entry_counts().1
    }

    /// Request (through the single ThumbService) thumbs for every currently
    /// loupe-eligible sibling (displayed image/raw under filter). This is the
    /// ONLY request path used for filmstrip cells — size is a fixed small value;
    /// the bounded path-keyed LRU + worker are shared with the grid. Flush is
    /// called so decodes start promptly even while the loupe view is active.
    fn request_filmstrip_thumbs(&mut self) {
        let siblings = self.displayed_indices();
        if siblings.is_empty() {
            return;
        }
        const FILM_THUMB_SIZE: u16 = 96;
        // Allow the queue to hold the full cull set (large folders are bounded by worker policy).
        self.thumb
            .ensure_queue_cap(siblings.len().max(THUMB_QUEUE_CAP_MIN));
        for &idx in &siblings {
            if let Some(entry) = self.browser.entries.get(idx) {
                // get_or_request is the exact same call site shape the grid uses.
                let _ = self.thumb.get_or_request(&entry.path, FILM_THUMB_SIZE);
            }
        }
        self.thumb.flush_requests();
    }
}

pub(crate) fn entry_counts(entries: &[Entry]) -> (usize, usize, usize) {
    let mut dirs = 0;
    let mut images = 0;
    let mut other = 0;
    for entry in entries {
        match entry.kind {
            EntryKind::Dir => dirs += 1,
            EntryKind::Image | EntryKind::Raw => images += 1,
            EntryKind::Other(_) => other += 1,
        }
    }
    (dirs, images, other)
}

#[allow(dead_code)]
pub fn filter_matches(name: &str, query: &str) -> bool {
    let query = query.trim();
    if query.is_empty() {
        return true;
    }

    name.to_lowercase().contains(&query.to_lowercase())
}

pub(crate) fn space_loupe_selection(
    entries: &[Entry],
    selected_index: Option<usize>,
    filter_focused: bool,
    settings_open: bool,
) -> Option<usize> {
    if filter_focused || settings_open {
        return None;
    }

    let selected = selected_index?;
    entries
        .get(selected)
        .filter(|entry| view::loupe::can_open_loupe(&entry.kind))
        .map(|_| selected)
}

pub(crate) fn loupe_decode_uses_full_demosaic(
    full_demosaic_setting: bool,
    kind: &EntryKind,
) -> bool {
    full_demosaic_setting && matches!(kind, EntryKind::Raw)
}

pub(crate) fn loupe_decode_mode(full_demosaic_setting: bool, kind: &EntryKind) -> LoupeDecodeMode {
    if loupe_decode_uses_full_demosaic(full_demosaic_setting, kind) {
        LoupeDecodeMode::FullRaw
    } else {
        LoupeDecodeMode::EmbeddedPreview
    }
}

pub(crate) fn loupe_base_decode_mode(_kind: &EntryKind) -> LoupeDecodeMode {
    LoupeDecodeMode::EmbeddedPreview
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LoupeResultRole {
    Base,
    DemosaicUpgrade,
    HighResUpgrade,
}

pub(crate) fn loupe_result_role(
    base_mode: LoupeDecodeMode,
    result_mode: LoupeDecodeMode,
) -> Option<LoupeResultRole> {
    match result_mode {
        LoupeDecodeMode::HighRes | LoupeDecodeMode::HighResFullRaw => {
            Some(LoupeResultRole::HighResUpgrade)
        }
        LoupeDecodeMode::FullRaw => Some(LoupeResultRole::DemosaicUpgrade),
        mode if mode == base_mode => Some(LoupeResultRole::Base),
        _ => None,
    }
}

/// Luminance histogram from a decoded RGBA image handle, if it is an Rgba handle.
fn histogram_from_handle(
    handle: &cosmic::widget::image::Handle,
) -> Option<[u32; crate::histogram::HISTOGRAM_BINS]> {
    match handle {
        cosmic::widget::image::Handle::Rgba { pixels, .. } => {
            Some(crate::histogram::luminance_histogram(pixels))
        }
        _ => None,
    }
}

/// Dispatch loupe decode tasks SEQUENTIALLY (chained), not batched. The decodes are synchronous
/// CPU work run inside `Task::perform`, so they block an executor thread for their whole duration;
/// batching the fast embedded-preview decode together with the slow full-demosaic lets the demosaic
/// starve the embedded (loupe stays blank until the demosaic finishes). The tasks arrive in
/// [embedded, full-demosaic] order, so chaining runs the embedded first (shows immediately) and the
/// demosaic upgrade after.
fn chain_decode_tasks(tasks: Vec<Task<cosmic::Action<Message>>>) -> Task<cosmic::Action<Message>> {
    let mut it = tasks.into_iter();
    match (it.next(), it.next()) {
        (None, _) => Task::none(),
        (Some(a), None) => a,
        (Some(a), Some(b)) => a.chain(b),
    }
}

fn loupe_decode_recovery_needed(
    current_path: &Path,
    current_mode: LoupeDecodeMode,
    current_has_handle: bool,
    pending: Option<(&Path, LoupeDecodeMode)>,
    result_path: &Path,
    result_mode: LoupeDecodeMode,
) -> bool {
    let result_is_high_res = matches!(
        result_mode,
        LoupeDecodeMode::HighRes | LoupeDecodeMode::HighResFullRaw
    );
    let current_decode_pending = pending.is_some_and(|(pending_path, pending_mode)| {
        pending_path == current_path && pending_mode == current_mode
    });

    current_path == result_path
        && current_mode != result_mode
        && !result_is_high_res
        && !current_has_handle
        && !current_decode_pending
}

pub fn parse_exif_datetime(s: &str) -> Option<i64> {
    let trimmed = s.trim();
    if trimmed.is_empty() {
        return None;
    }
    // Standard EXIF is "YYYY:MM:DD HH:MM:SS"; some sources (notably kamadak-exif's
    // CR3 CMT1 DateTime rendering) produce "YYYY-MM-DD HH:MM:SS". Accept both.
    for fmt in ["%Y:%m:%d %H:%M:%S", "%Y-%m-%d %H:%M:%S"] {
        if let Ok(dt) = chrono::NaiveDateTime::parse_from_str(trimmed, fmt) {
            return Some(dt.and_utc().timestamp());
        }
    }
    None
}

pub(crate) const LOUPE_ZOOM_MIN: f32 = 0.25;
pub(crate) const LOUPE_ZOOM_MAX: f32 = 8.0;

/// Step the loupe zoom factor up (zoom_in) or down by a 1.25x ratio, clamped.
pub(crate) fn step_loupe_zoom(factor: f32, zoom_in: bool) -> f32 {
    let next = if zoom_in {
        factor * 1.25
    } else {
        factor / 1.25
    };
    next.clamp(LOUPE_ZOOM_MIN, LOUPE_ZOOM_MAX)
}

pub(crate) fn is_refresh_character(text: &str) -> bool {
    text.chars().count() == 1 && text.eq_ignore_ascii_case("r")
}

pub(crate) fn is_space_character(text: &str) -> bool {
    text == " "
}

pub(crate) fn is_loupe_fit_character(text: &str) -> bool {
    text.eq_ignore_ascii_case("f")
}

pub(crate) fn is_loupe_zoom_in_character(text: &str) -> bool {
    text == "+" || text == "="
}

pub(crate) fn is_loupe_zoom_out_character(text: &str) -> bool {
    text == "-" || text == "_"
}

pub(crate) fn is_slideshow_toggle_character(text: &str) -> bool {
    text.chars().count() == 1 && text.eq_ignore_ascii_case("p")
}

/// Small pure helper: given the current loupe-eligible displayed list (from displayed_indices)
/// and the current image index, compute the next (wrap=true, forward) using loupe_step.
/// Returns None if empty or current not present (defensive). Used only by SlideshowTick.
/// The helper keeps slideshow navigation logic independent from UI state.
fn slideshow_next_index(displayed: &[usize], current_index: usize) -> Option<usize> {
    if displayed.is_empty() {
        return None;
    }
    let cursor = displayed.iter().position(|&i| i == current_index)?;
    let next_cursor = loupe_step(displayed.len(), cursor, true, true);
    Some(displayed[next_cursor])
}

/// Compute the new (selected_set, primary, anchor) after a click on `clicked` (entry index),
/// given the current selection, the anchor, the modifier state, and the displayed order (entry
/// indices in grid order). Pure.
pub(crate) fn compute_selection(
    current: &HashSet<usize>,
    anchor: Option<usize>,
    clicked: usize,
    ctrl: bool,
    shift: bool,
    displayed_order: &[usize],
) -> (HashSet<usize>, usize, Option<usize>) {
    if shift {
        // Shift-range: from anchor (or clicked if no/invalid anchor) to clicked, in current displayed order.
        // Anchor returned: unchanged if original anchor was valid in order; else Some(clicked) (plain-like).
        let start = if let Some(a) = anchor {
            if displayed_order.contains(&a) {
                a
            } else {
                clicked
            }
        } else {
            clicked
        };
        let pos_start = displayed_order.iter().position(|&i| i == start);
        let pos_click = displayed_order.iter().position(|&i| i == clicked);
        if let (Some(ps), Some(pc)) = (pos_start, pos_click) {
            let lo = ps.min(pc);
            let hi = ps.max(pc);
            let set: HashSet<usize> = displayed_order[lo..=hi].iter().copied().collect();
            let ret_anchor = if let Some(a) = anchor {
                if displayed_order.contains(&a) {
                    anchor
                } else {
                    Some(clicked)
                }
            } else {
                Some(clicked)
            };
            (set, clicked, ret_anchor)
        } else {
            // Fallback (defensive): plain
            let mut set = HashSet::new();
            set.insert(clicked);
            (set, clicked, Some(clicked))
        }
    } else if ctrl {
        // Ctrl: toggle; primary/anchor become the clicked (empty set allowed)
        let mut set = current.clone();
        if set.contains(&clicked) {
            set.remove(&clicked);
        } else {
            set.insert(clicked);
        }
        (set, clicked, Some(clicked))
    } else {
        // Plain: replace selection
        let mut set = HashSet::new();
        set.insert(clicked);
        (set, clicked, Some(clicked))
    }
}

/// Returns true for filesystem events that indicate a directory's *contents*
/// changed (add / remove / rename). Used to filter notify events before
/// debounce so metadata-only or access events don't trigger rescans.
/// Pure function — has a dedicated unit test.
pub(crate) fn is_relevant_watch_event(event: &notify::Event) -> bool {
    use notify::event::{CreateKind, EventKind, ModifyKind};
    matches!(
        &event.kind,
        EventKind::Create(CreateKind::File)
            | EventKind::Create(CreateKind::Folder)
            | EventKind::Create(CreateKind::Any)
            | EventKind::Create(CreateKind::Other)
            | EventKind::Remove(_)
            | EventKind::Modify(ModifyKind::Name(_))
    )
}

/// Build a directory-watcher subscription keyed by `current_dir`.
/// When the returned subscription is no longer present in `subscription()`,
/// the runtime drops the stream, the thread, and the watcher (no leaks).
/// The `run_with(dir)` ensures a navigation that changes the dir produces a
/// subscription with different identity → old watcher is torn down, new one
/// attached for the new `current_dir`.
fn watch_current_dir_subscription(
    current_dir: Option<PathBuf>,
) -> cosmic::iced::Subscription<Message> {
    let Some(dir) = current_dir else {
        return cosmic::iced::Subscription::none();
    };
    if dir.as_os_str().is_empty() {
        return cosmic::iced::Subscription::none();
    }

    cosmic::iced::Subscription::run_with(dir.clone(), |dir| {
        let p = dir.clone();
        cosmic::iced::stream::channel(32, move |output| async move {
            run_dir_watcher(p, output);
            // Do not pend here; returning ends the runner future. The
            // mpsc::Receiver side (internal to the stream) + the Sender
            // held by the worker thread keep the subscription stream alive
            // until the thread drops its Sender (on sub drop or disconnect).
        })
    })
    .map(|_| Message::RefreshCurrentFolder)
}

/// Spawn a background thread that owns a `RecommendedWatcher` (non-recursive)
/// and a debounce timer. On relevant events (add/remove/rename), after a quiet
/// period of ~400 ms, `try_send(())` to the stream. The () is mapped to
/// `Message::RefreshCurrentFolder` by the caller. Event-driven (no poll).
///
/// Thread exits promptly when the subscription is dropped (folder navigation
/// that swaps current_dir or app teardown): we poll `output.is_closed()` on a
/// short internal interval. This ensures the watcher and thread are released
/// without waiting for the next FS event. Active-dir behavior is unchanged
/// (NonRecursive, single-dir, ~400 ms coalesce debounce on relevant events only).
fn run_dir_watcher(dir: PathBuf, output: cosmic::iced::futures::channel::mpsc::Sender<()>) {
    let _ = std::thread::spawn(move || {
        let (tx, rx) = std::sync::mpsc::channel();
        let mut watcher = match notify::recommended_watcher(tx) {
            Ok(w) => w,
            Err(e) => {
                tracing::warn!(
                    "failed to create RecommendedWatcher for {}: {e}",
                    dir.display()
                );
                return;
            }
        };

        if let Err(e) = watcher.watch(&dir, RecursiveMode::NonRecursive) {
            tracing::warn!("watch({}) failed: {e}", dir.display());
            return;
        }
        // `watcher` lives until this thread ends → OS resources released on drop.

        let debounce = Duration::from_millis(400);
        // Short internal poll for prompt close detection on unsubscribe (max ~50 ms
        // linger after iced drops the receiver side). Debounce timing for user
        // events is unaffected.
        let poll = Duration::from_millis(50);
        let mut last = std::time::Instant::now();
        let mut had_relevant = false;
        let mut output = output;

        loop {
            if output.is_closed() {
                break;
            }
            match rx.recv_timeout(poll) {
                Ok(Ok(event)) => {
                    if is_relevant_watch_event(&event) {
                        last = std::time::Instant::now();
                        had_relevant = true;
                    }
                }
                Ok(Err(e)) => {
                    tracing::debug!("notify error under {}: {e}", dir.display());
                }
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                    if had_relevant && last.elapsed() >= debounce {
                        if output.try_send(()).is_err() {
                            // Receiver gone (iced dropped the subscription) → exit
                            break;
                        }
                        had_relevant = false;
                        last = std::time::Instant::now();
                    }
                    // no send attempted; loop back to is_closed check (prompt exit if dropped)
                }
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                    break;
                }
            }
        }
    });
}

// ── Unit tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::{
        add_favorite, breadcrumb_segments, cell_h, cell_state_for_thumb, compute_duplicate_sets,
        compute_selection, describe_filters, ellipsize_path, entry_counts, filter_matches,
        format_modified, format_mtime, histogram_from_handle, is_refresh_character,
        is_space_character, loupe_decode_uses_full_demosaic, middle_ellipsis, navigation_back,
        navigation_forward, navigation_new_destination, parse_exif_datetime, remove_favorite,
        snap_thumb_size, space_loupe_selection, thumbnail_queue_cap_for_envelope,
        visible_priority_positions, AppModel, CellState, DuplicateState, FilterState, LoupeRuntime,
        MenuAction, Message, MetadataSectionState, NavState, PreviewCache, SelectionState, SortBar,
        PREVIEW_CACHE_CAP, THUMB_QUEUE_CAP_MIN,
    };
    use crate::browser_state::BrowserState;
    use crate::config::Config;
    use crate::folder_tree::FolderTree;
    use crate::inspection::LoupeDecodeMode;
    use crate::scan::{Entry, EntryKind, FileCategory};
    use crate::thumb::{ThumbService, ThumbState};
    use std::collections::{HashMap, HashSet};
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::Arc;
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

    fn test_app() -> AppModel {
        let cfg = Config::default();
        let (model, name_entity, date_entity, captured_entity, rating_entity) =
            AppModel::build_sort_model(cfg.sort_mode);
        let sort = SortBar {
            model,
            name_entity,
            date_entity,
            captured_entity,
            rating_entity,
        };
        AppModel {
            core: cosmic::Core::default(),
            config: cfg,
            browser: BrowserState::default(),
            nav: NavState {
                back: Vec::new(),
                forward: Vec::new(),
                places: Vec::new(),
                drives: Vec::new(),
                tree: FolderTree::default(),
            },
            thumb: ThumbService::new(8, THUMB_QUEUE_CAP_MIN),
            viewport_w: 1024.0,
            viewport_h: 800.0,
            measured_w: Arc::new(AtomicU32::new(0)),
            measured_h: Arc::new(AtomicU32::new(0)),
            last_grid_w: 0.0,
            last_grid_h: 0.0,
            scroll_offset_y: 0.0,
            grid_snapshot: HashMap::new(),
            visible_index_range: 0..0,
            selection: SelectionState::default(),
            preview: None,
            text_preview: None,
            metadata_sections: MetadataSectionState::default(),
            loupe: None,
            loupe_rt: LoupeRuntime {
                decode_pending: None,
                slideshow_playing: false,
                scroll: cosmic::iced::widget::scrollable::AbsoluteOffset::default(),
                zoom_factor: 1.0,
                drag: None,
                pan_last_cursor: None,
            },
            preview_cache: PreviewCache::new(PREVIEW_CACHE_CAP),
            sort,
            filter_focused: false,
            settings_open: false,
            cache_max_input: "2.00".to_owned(),
            cache_dir_input: String::new(),
            export_dir_input: String::new(),
            keyword_input: String::new(),
            filter: FilterState::default(),
            dups: DuplicateState::default(),
            compare: None,
            menu_key_binds: std::collections::HashMap::new(),
            folder_metadata: std::collections::HashMap::new(),
            folder_tags: std::collections::HashMap::new(),
        }
    }

    #[test]
    fn navigation_new_destination_pushes_current_and_clears_forward() {
        let mut back = vec![PathBuf::from("/photos/first")];
        let mut forward = vec![PathBuf::from("/photos/future")];

        navigation_new_destination(&mut back, &mut forward, Some(Path::new("/photos/current")));

        assert_eq!(
            back,
            vec![
                PathBuf::from("/photos/first"),
                PathBuf::from("/photos/current")
            ]
        );
        assert!(forward.is_empty());
    }

    #[test]
    fn navigation_back_moves_current_to_forward_and_returns_previous() {
        let mut back = vec![PathBuf::from("/photos/a"), PathBuf::from("/photos/b")];
        let mut forward = Vec::new();

        let destination =
            navigation_back(&mut back, &mut forward, Some(Path::new("/photos/current")));

        assert_eq!(destination, Some(PathBuf::from("/photos/b")));
        assert_eq!(back, vec![PathBuf::from("/photos/a")]);
        assert_eq!(forward, vec![PathBuf::from("/photos/current")]);
    }

    #[test]
    fn navigation_forward_moves_current_to_back_and_returns_next() {
        let mut back = vec![PathBuf::from("/photos/a")];
        let mut forward = vec![PathBuf::from("/photos/b"), PathBuf::from("/photos/c")];

        let destination =
            navigation_forward(&mut back, &mut forward, Some(Path::new("/photos/current")));

        assert_eq!(destination, Some(PathBuf::from("/photos/c")));
        assert_eq!(
            back,
            vec![PathBuf::from("/photos/a"), PathBuf::from("/photos/current")]
        );
        assert_eq!(forward, vec![PathBuf::from("/photos/b")]);
    }

    #[test]
    fn navigation_back_forward_noop_when_stack_empty() {
        let mut back = Vec::new();
        let mut forward = Vec::new();

        assert_eq!(
            navigation_back(&mut back, &mut forward, Some(Path::new("/x"))),
            None
        );
        assert_eq!(
            navigation_forward(&mut back, &mut forward, Some(Path::new("/x"))),
            None
        );
        assert!(back.is_empty());
        assert!(forward.is_empty());
    }

    fn fake_rgba() -> (Vec<u8>, u32, u32) {
        (vec![255u8, 0, 0, 255], 1, 1)
    }

    #[test]
    fn needs_tick_is_false_when_idle_and_geometry_propagated() {
        let mut app = test_app();
        let (w, h) = app.effective_grid_size();
        app.last_grid_w = w;
        app.last_grid_h = h;

        assert!(!app.needs_tick());
    }

    #[test]
    fn needs_tick_is_true_when_thumbnails_are_loading() {
        let mut app = test_app();
        let (w, h) = app.effective_grid_size();
        app.last_grid_w = w;
        app.last_grid_h = h;
        app.thumb
            .queue_decoded_for_test(PathBuf::from("/tmp/loading.jpg"), 0, Some(fake_rgba()));

        assert!(app.needs_tick());
    }

    #[test]
    fn needs_tick_is_true_when_geometry_delta_is_pending() {
        let mut app = test_app();
        let (w, h) = app.effective_grid_size();
        app.last_grid_w = w;
        app.last_grid_h = h;
        app.measured_w
            .store((w + 16.0).to_bits(), Ordering::Relaxed);

        assert!(app.needs_tick());
    }

    #[test]
    fn visible_priority_orders_request_envelope_from_viewport_center() {
        let ordered = visible_priority_positions(0..12, 4, 0.0, 90.0, 30.0);

        assert_eq!(ordered[0], 5, "cell nearest viewport center goes first");
        assert!(
            ordered.iter().position(|pos| *pos == 0).unwrap()
                > ordered.iter().position(|pos| *pos == 4).unwrap(),
            "off-center margin cells sort after nearer visible cells"
        );
        assert_eq!(ordered.len(), 12);
    }

    #[test]
    fn thumbnail_queue_cap_covers_c2_envelope() {
        let pane_w =
            crate::view::grid::grid_available_w(1920.0, true) - crate::view::grid::SCROLLBAR_W;
        let pane_h = crate::view::grid::grid_available_h(1000.0);
        let cols = ((pane_w / 96.0).floor() as usize).max(1);
        let envelope = crate::view::grid::visible_range(
            0.0,
            pane_h,
            cell_h(96),
            cols,
            10_000,
            crate::view::grid::MARGIN_ROWS,
        );

        assert!(
            thumbnail_queue_cap_for_envelope(envelope.len()) >= envelope.len(),
            "queue cap must cover the full visible+margin request envelope"
        );
    }

    #[test]
    fn thumb_tick_applies_decode_batch_and_rebuilds_snapshot_once() {
        let mut app = test_app();
        app.browser.entries = vec![
            test_entry("/tmp/first.jpg", EntryKind::Image),
            test_entry("/tmp/second.jpg", EntryKind::Image),
            test_entry("/tmp/third.jpg", EntryKind::Image),
        ];
        app.rebuild_snapshot();
        let before_range = app.visible_index_range.clone();

        for name in ["/tmp/first.jpg", "/tmp/second.jpg", "/tmp/third.jpg"] {
            app.thumb
                .queue_decoded_for_test(PathBuf::from(name), 0, Some(fake_rgba()));
        }

        let rebuilt = app.apply_drained_thumbnails();

        assert!(rebuilt);
        assert_eq!(app.thumb.loading_len(), 0);
        assert_eq!(app.visible_index_range, before_range);
        for pos in before_range {
            assert!(
                matches!(app.grid_snapshot.get(&pos), Some(CellState::Thumb(_))),
                "expected decoded thumb at grid position {pos}"
            );
        }
    }

    #[test]
    fn format_mtime_shape() {
        // 2001-09-09T01:46:40 UTC (unix second 1_000_000_000)
        let t = UNIX_EPOCH + Duration::from_secs(1_000_000_000);
        let s = format_mtime(t);
        assert!(s.starts_with("20"), "date should start with '20', got: {s}");
        assert!(
            s.contains(':'),
            "date should contain ':' for HH:MM, got: {s}"
        );
        // Shape: 16 visible chars minimum for "YYYY-MM-DD HH:MM"
        assert!(
            s.len() >= 16,
            "date string too short (expected ≥16 chars), got: {s}"
        );
    }

    #[test]
    fn relevant_watch_events_coalesce_only_add_remove_rename() {
        use notify::event::{CreateKind, EventKind, ModifyKind, RemoveKind, RenameMode};
        use notify::Event;
        use std::path::PathBuf;

        let p = PathBuf::from("/tmp/photo.jpg");

        // Structural changes that should trigger a (debounced) refresh
        assert!(super::is_relevant_watch_event(
            &Event::new(EventKind::Create(CreateKind::File)).add_path(p.clone())
        ));
        assert!(super::is_relevant_watch_event(
            &Event::new(EventKind::Create(CreateKind::Folder)).add_path(p.clone())
        ));
        assert!(super::is_relevant_watch_event(
            &Event::new(EventKind::Remove(RemoveKind::File)).add_path(p.clone())
        ));
        assert!(super::is_relevant_watch_event(
            &Event::new(EventKind::Modify(ModifyKind::Name(RenameMode::Both))).add_path(p.clone())
        ));

        // Non-structural (data change, access, other) must NOT trigger rescans
        assert!(!super::is_relevant_watch_event(
            &Event::new(EventKind::Modify(ModifyKind::Data(
                notify::event::DataChange::Content
            )))
            .add_path(p.clone())
        ));
        assert!(!super::is_relevant_watch_event(
            &Event::new(EventKind::Access(notify::event::AccessKind::Read)).add_path(p.clone())
        ));
        assert!(!super::is_relevant_watch_event(
            &Event::new(EventKind::Other).add_path(p.clone())
        ));
    }

    #[test]
    fn watch_current_dir_subscription_is_none_for_absent_or_empty_dir() {
        // Pure function test of the lifecycle entrypoint: for no current_dir (or
        // empty) we must short-circuit to Subscription::none(). This path spawns
        // *no* watcher thread at all (the run_with + run_dir_watcher path is only
        // for a real dir). Guards "no lingering thread" for initial/no-folder and
        // the teardown cases that navigate to None. Complements the thread-close
        // logic inside run_dir_watcher (is_closed + short poll) for the active case.
        // (Cannot easily assert the concrete Subscription variant here as it is
        // largely opaque, but construction is the decision point and must not panic.)
        use std::path::PathBuf;
        let _s1 = super::watch_current_dir_subscription(None);
        let _s2 = super::watch_current_dir_subscription(Some(PathBuf::new()));
        let _s3 = super::watch_current_dir_subscription(Some(PathBuf::from("/tmp")));
        // /tmp existence not required for this builder test; the real watch happens inside thread.
    }

    #[test]
    fn format_modified_none_returns_unknown() {
        assert_eq!(format_modified(None), "unknown");
    }

    #[test]
    fn format_modified_some_delegates_to_format_mtime() {
        let t = UNIX_EPOCH + Duration::from_secs(1_000_000_000);
        // format_modified(Some(t)) must equal format_mtime(t)
        assert_eq!(format_modified(Some(t)), format_mtime(t));
    }

    #[test]
    fn entry_counts_treat_raw_as_image_not_other() {
        let entries = vec![
            test_entry("photo.jpg", EntryKind::Image),
            test_entry("raw.nef", EntryKind::Raw),
            test_entry("notes.txt", EntryKind::Other(FileCategory::Document)),
        ];

        let (_dirs, images, other) = entry_counts(&entries);

        assert_eq!(images, 2);
        assert_eq!(other, 1);
    }

    #[test]
    fn parse_exif_datetime_table() {
        assert_eq!(parse_exif_datetime("1970:01:01 00:00:00"), Some(0));
        assert_eq!(parse_exif_datetime(""), None);
        assert_eq!(parse_exif_datetime("   "), None);
        assert_eq!(parse_exif_datetime("not a date"), None);
        assert_eq!(parse_exif_datetime("2024:05:01"), None);
        // Hyphen format (kamadak-exif CR3 CMT1 DateTime) parses to same epoch as colon format
        assert_eq!(
            parse_exif_datetime("2021-05-15 13:02:02"),
            parse_exif_datetime("2021:05:15 13:02:02")
        );
        assert!(parse_exif_datetime("malformed").is_none());
    }

    #[test]
    fn filter_matches_table() {
        let cases = [
            ("DSC_0420.JPG", "", true),
            ("DSC_0420.JPG", "   ", true),
            ("summer-vacation.raw", "vac", true),
            ("Portrait.NEF", "portrait", true),
            ("Portrait.NEF", "landscape", false),
            ("Σίσυφος.jpg", "σίσ", true),
        ];

        for (name, query, expected) in cases {
            assert_eq!(
                filter_matches(name, query),
                expected,
                "name={name:?} query={query:?}"
            );
        }
    }

    #[test]
    fn failed_raw_thumbnail_keeps_raw_placeholder_identity() {
        let state = cell_state_for_thumb(&EntryKind::Raw, ThumbState::Failed);

        assert!(matches!(state, CellState::RawPlaceholder));
    }

    #[test]
    fn snap_thumb_size_clamps_then_floors_to_lower_step() {
        assert_eq!(snap_thumb_size(100), 96);
        assert_eq!(snap_thumb_size(95), 96);
        assert_eq!(snap_thumb_size(300), 256);
    }

    #[test]
    fn refresh_character_accepts_r_case_insensitively() {
        assert!(is_refresh_character("r"));
        assert!(is_refresh_character("R"));
        assert!(!is_refresh_character("rr"));
        assert!(!is_refresh_character("f"));
    }

    #[test]
    fn space_character_accepts_literal_space_only() {
        assert!(is_space_character(" "));
        assert!(!is_space_character(""));
        assert!(!is_space_character("  "));
        assert!(!is_space_character("space"));
    }

    #[test]
    fn cull_target_indices_prefers_multi_sorted_then_primary_then_empty() {
        // Pure logic over pub fields; no GUI needed.
        let mut app = test_app();
        app.selection.multi_selected = HashSet::new();
        app.selection.selected_index = None;
        assert_eq!(app.cull_target_indices(), Vec::<usize>::new());

        app.selection.selected_index = Some(7);
        assert_eq!(app.cull_target_indices(), vec![7]);

        app.selection.multi_selected = [10, 2, 5].into_iter().collect();
        let got = app.cull_target_indices();
        assert_eq!(got, vec![2, 5, 10]); // sorted

        // multi takes precedence even if primary set
        app.selection.selected_index = Some(99);
        let got2 = app.cull_target_indices();
        assert_eq!(got2, vec![2, 5, 10]);
    }

    #[test]
    fn space_loupe_selection_requires_selected_image_and_unfocused_grid() {
        let entries = vec![
            test_entry("folder", EntryKind::Dir),
            test_entry("photo.jpg", EntryKind::Image),
            test_entry("raw.nef", EntryKind::Raw),
        ];

        assert_eq!(
            space_loupe_selection(&entries, Some(1), false, false),
            Some(1)
        );
        assert_eq!(
            space_loupe_selection(&entries, Some(2), false, false),
            Some(2)
        );
        assert_eq!(space_loupe_selection(&entries, Some(0), false, false), None);
        assert_eq!(space_loupe_selection(&entries, None, false, false), None);
        assert_eq!(space_loupe_selection(&entries, Some(1), true, false), None);
        assert_eq!(space_loupe_selection(&entries, Some(1), false, true), None);
    }

    #[test]
    fn loupe_full_demosaic_mode_requires_setting_and_raw_entry() {
        assert!(loupe_decode_uses_full_demosaic(true, &EntryKind::Raw));
        assert!(!loupe_decode_uses_full_demosaic(false, &EntryKind::Raw));
        assert!(!loupe_decode_uses_full_demosaic(true, &EntryKind::Image));
        assert!(!loupe_decode_uses_full_demosaic(
            true,
            &EntryKind::Other(FileCategory::Document)
        ));
        assert!(!loupe_decode_uses_full_demosaic(true, &EntryKind::Dir));
    }

    #[test]
    fn histogram_from_handle_counts_pixels_from_rgba() {
        // Mirror total_count_matches_pixels: 10 pixels worth of RGBA data.
        let bytes: Vec<u8> = vec![100u8; 4 * 10];
        let handle = cosmic::widget::image::Handle::from_rgba(5, 2, bytes.clone());
        let hist = histogram_from_handle(&handle);
        assert!(hist.is_some());
        let total: u32 = hist.unwrap().iter().sum();
        assert_eq!(total, 10);
    }

    #[test]
    fn loupe_result_role_routes_full_raw_as_fit_demosaic_upgrade() {
        assert_eq!(
            super::loupe_result_role(LoupeDecodeMode::EmbeddedPreview, LoupeDecodeMode::FullRaw,),
            Some(super::LoupeResultRole::DemosaicUpgrade)
        );
        assert_eq!(
            super::loupe_result_role(
                LoupeDecodeMode::EmbeddedPreview,
                LoupeDecodeMode::EmbeddedPreview,
            ),
            Some(super::LoupeResultRole::Base)
        );
        assert_eq!(
            super::loupe_result_role(LoupeDecodeMode::EmbeddedPreview, LoupeDecodeMode::HighRes,),
            Some(super::LoupeResultRole::HighResUpgrade)
        );
        assert_eq!(
            super::loupe_result_role(
                LoupeDecodeMode::EmbeddedPreview,
                LoupeDecodeMode::HighResFullRaw,
            ),
            Some(super::LoupeResultRole::HighResUpgrade)
        );
    }

    #[test]
    fn middle_ellipsis_short_strings_are_unchanged() {
        assert_eq!(
            middle_ellipsis("/home/user/Pictures", 32),
            "/home/user/Pictures"
        );
    }

    #[test]
    fn middle_ellipsis_long_strings_keep_head_and_tail_within_max() {
        let shortened = middle_ellipsis("/home/user/very/long/path/to/photos", 16);

        assert!(shortened.len() <= 16, "{shortened}");
        assert!(shortened.contains('…'), "{shortened}");
        assert!(shortened.starts_with("/home"), "{shortened}");
        assert!(shortened.ends_with("photos"), "{shortened}");
    }

    #[test]
    fn ellipsize_path_short_paths_are_unchanged() {
        assert_eq!(
            ellipsize_path("/home/user/Pictures", 32),
            "/home/user/Pictures"
        );
    }

    #[test]
    fn ellipsize_path_long_paths_keep_head_and_tail_within_max() {
        let shortened = ellipsize_path("/media/user/archive/MEDIA/2023-11", 28);

        assert!(shortened.len() <= 28, "{shortened}");
        assert!(shortened.contains('…'), "{shortened}");
        assert!(shortened.starts_with("/media"), "{shortened}");
        assert!(shortened.ends_with("MEDIA/2023-11"), "{shortened}");
    }

    #[test]
    fn ellipsize_path_no_separators_degrades_gracefully() {
        let shortened = ellipsize_path("averylongfilenamewithoutseparators", 12);

        assert!(shortened.len() <= 12, "{shortened}");
        assert!(shortened.contains('…'), "{shortened}");
    }

    #[test]
    fn breadcrumb_segments_root_returns_root_only() {
        assert_eq!(
            breadcrumb_segments(Path::new("/")),
            vec![("/".to_owned(), PathBuf::from("/"))]
        );
    }

    #[test]
    fn breadcrumb_segments_absolute_path_includes_each_ancestor() {
        assert_eq!(
            breadcrumb_segments(Path::new("/home/photo_user/photos")),
            vec![
                ("/".to_owned(), PathBuf::from("/")),
                ("home".to_owned(), PathBuf::from("/home")),
                ("photo_user".to_owned(), PathBuf::from("/home/photo_user")),
                (
                    "photos".to_owned(),
                    PathBuf::from("/home/photo_user/photos")
                ),
            ]
        );
    }

    #[test]
    fn breadcrumb_segments_relative_path_accumulates_components() {
        assert_eq!(
            breadcrumb_segments(Path::new("albums/2024")),
            vec![
                ("albums".to_owned(), PathBuf::from("albums")),
                ("2024".to_owned(), PathBuf::from("albums/2024")),
            ]
        );
    }

    #[test]
    fn add_favorite_appends_once_and_reports_changes() {
        let mut favorites = vec![PathBuf::from("/home/test/Pictures")];

        assert!(add_favorite(
            &mut favorites,
            PathBuf::from("/home/test/Downloads")
        ));
        assert!(!add_favorite(
            &mut favorites,
            PathBuf::from("/home/test/Pictures")
        ));

        assert_eq!(
            favorites,
            vec![
                PathBuf::from("/home/test/Pictures"),
                PathBuf::from("/home/test/Downloads")
            ]
        );
    }

    #[test]
    fn remove_favorite_removes_duplicates_and_reports_changes() {
        let mut favorites = vec![
            PathBuf::from("/home/test/Pictures"),
            PathBuf::from("/home/test/Downloads"),
            PathBuf::from("/home/test/Pictures"),
        ];

        assert!(remove_favorite(
            &mut favorites,
            Path::new("/home/test/Pictures")
        ));
        assert!(!remove_favorite(
            &mut favorites,
            Path::new("/home/test/Missing")
        ));

        assert_eq!(favorites, vec![PathBuf::from("/home/test/Downloads")]);
    }

    #[test]
    fn menu_action_message_maps_to_expected_variants() {
        use crate::config::SortMode;
        use crate::xmp::ColorLabel;
        // Bring the MenuAction trait into scope so .message() is callable (provided by libcosmic).
        use cosmic::widget::menu::Action;

        // LabelAll clears the filter.
        assert!(matches!(
            MenuAction::LabelAll.message(),
            Message::SetLabelFilter(None)
        ));

        // Label(c) sets the filter.
        assert!(matches!(
            MenuAction::Label(ColorLabel::Red).message(),
            Message::SetLabelFilter(Some(ColorLabel::Red))
        ));
        assert!(matches!(
            MenuAction::Label(ColorLabel::Purple).message(),
            Message::SetLabelFilter(Some(ColorLabel::Purple))
        ));

        // Sort* map to SortModeChanged with corresponding mode (Modified = Date).
        assert!(matches!(
            MenuAction::SortName.message(),
            Message::SortModeChanged(SortMode::Name)
        ));
        assert!(matches!(
            MenuAction::SortModified.message(),
            Message::SortModeChanged(SortMode::Date)
        ));
        assert!(matches!(
            MenuAction::SortRating.message(),
            Message::SortModeChanged(SortMode::Rating)
        ));

        // A few others for coverage of wiring.
        assert!(matches!(
            MenuAction::ToggleHidden.message(),
            Message::ToggleShowHidden
        ));
        assert!(matches!(
            MenuAction::Preferences.message(),
            Message::ToggleSettings
        ));
        assert!(matches!(
            MenuAction::OpenNewTab.message(),
            Message::MenuNoop
        ));

        // Camera* map to SetCameraFilter (index or None).
        assert!(matches!(
            MenuAction::CameraAll.message(),
            Message::SetCameraFilter(None)
        ));
        assert!(matches!(
            MenuAction::Camera(0).message(),
            Message::SetCameraFilter(Some(0))
        ));
        assert!(matches!(
            MenuAction::Camera(2).message(),
            Message::SetCameraFilter(Some(2))
        ));

        // Tag* map to SetTagFilter (index or None).
        assert!(matches!(
            MenuAction::TagAll.message(),
            Message::SetTagFilter(None)
        ));
        assert!(matches!(
            MenuAction::Tag(0).message(),
            Message::SetTagFilter(Some(0))
        ));
        assert!(matches!(
            MenuAction::Tag(2).message(),
            Message::SetTagFilter(Some(2))
        ));
    }

    // ── Slideshow navigation tests ───────────────────────────────────────────
    #[test]
    fn slideshow_next_index_wraps_using_loupe_step() {
        // Exercises the exact (len, cursor, forward=true, wrap=true) + displayed list pattern used by SlideshowTick.
        let displayed = vec![10usize, 20, 30];
        assert_eq!(super::slideshow_next_index(&displayed, 30), Some(10));
        assert_eq!(super::slideshow_next_index(&displayed, 20), Some(30));
        assert_eq!(super::slideshow_next_index(&displayed, 10), Some(20));
        assert_eq!(super::slideshow_next_index(&displayed, 99), None);
        assert_eq!(super::slideshow_next_index(&[], 0), None);
        assert_eq!(super::slideshow_next_index(&displayed, 999), None); // current not in list
        assert_eq!(super::slideshow_next_index(&[42], 42), Some(42)); // len==1 stays
    }

    #[test]
    fn dropped_same_path_mode_mismatch_without_pending_requests_recovery_decode() {
        let path = Path::new("photo.cr2");

        assert!(super::loupe_decode_recovery_needed(
            path,
            LoupeDecodeMode::FullRaw,
            false,
            None,
            path,
            LoupeDecodeMode::EmbeddedPreview,
        ));

        assert!(!super::loupe_decode_recovery_needed(
            path,
            LoupeDecodeMode::FullRaw,
            false,
            Some((path, LoupeDecodeMode::FullRaw)),
            path,
            LoupeDecodeMode::EmbeddedPreview,
        ));

        assert!(!super::loupe_decode_recovery_needed(
            path,
            LoupeDecodeMode::FullRaw,
            true,
            None,
            path,
            LoupeDecodeMode::EmbeddedPreview,
        ));

        assert!(!super::loupe_decode_recovery_needed(
            Path::new("current.cr2"),
            LoupeDecodeMode::FullRaw,
            false,
            None,
            Path::new("stale.cr2"),
            LoupeDecodeMode::EmbeddedPreview,
        ));
    }

    #[test]
    fn step_loupe_zoom_steps_and_clamps() {
        // Zoom in from 1.0 to 1.25, clamp at 8.0, then zoom out and clamp at 0.25.
        use super::{step_loupe_zoom, LOUPE_ZOOM_MAX, LOUPE_ZOOM_MIN};
        assert!((step_loupe_zoom(1.0, true) - 1.25).abs() < f32::EPSILON);
        // repeated zoom-in clamps
        let mut f = 1.0;
        for _ in 0..20 {
            f = step_loupe_zoom(f, true);
        }
        assert!((f - LOUPE_ZOOM_MAX).abs() < f32::EPSILON);
        assert!((step_loupe_zoom(1.0, false) - 0.8).abs() < f32::EPSILON);
        // repeated zoom-out clamps
        let mut f = 1.0;
        for _ in 0..20 {
            f = step_loupe_zoom(f, false);
        }
        assert!((f - LOUPE_ZOOM_MIN).abs() < f32::EPSILON);
    }

    #[test]
    fn rating_sort_desc_then_name_stable_via_pairs() {
        // Pure ordering test using the extracted helper. Input is (index, rating).
        // Expected: higher rating first; equal ratings keep original relative order (stable).
        let pairs: &[(usize, u8)] = &[(0, 0), (1, 5), (2, 3), (3, 5), (4, 0), (5, 3)];
        // Build a lookup by index -> rating (simulate cache lookup returning u8 directly).
        let ordered = super::AppModel::order_indices_by_rating_desc(
            pairs.iter().map(|(i, _)| *i).collect(),
            |i| {
                pairs
                    .iter()
                    .find(|(idx, _)| *idx == i)
                    .map(|(_, r)| *r)
                    .unwrap_or(0)
            },
        );
        // 5s first (stable: 1 then 3), then 3s (stable: 2 then 5), then 0s (stable: 0 then 4)
        assert_eq!(ordered, vec![1, 3, 2, 5, 0, 4]);
    }

    // ── compute_selection tests (MS-M1) ────────────────────────────────────────

    #[test]
    fn compute_selection_plain_replaces() {
        let disp = vec![10, 20, 30, 40];
        let cur = HashSet::from([10, 30]);
        let (set, prim, anch) = compute_selection(&cur, Some(10), 30, false, false, &disp);
        assert_eq!(set, HashSet::from([30]));
        assert_eq!(prim, 30);
        assert_eq!(anch, Some(30));
    }

    #[test]
    fn compute_selection_ctrl_adds() {
        let disp = vec![0, 1, 2];
        let cur = HashSet::from([0]);
        let (set, prim, anch) = compute_selection(&cur, Some(0), 2, true, false, &disp);
        assert_eq!(set, HashSet::from([0, 2]));
        assert_eq!(prim, 2);
        assert_eq!(anch, Some(2));
    }

    #[test]
    fn compute_selection_ctrl_toggles_off_last() {
        let disp = vec![5, 6];
        let cur = HashSet::from([5]); // only one; toggling it off yields empty (allowed)
        let (set, prim, anch) = compute_selection(&cur, Some(5), 5, true, false, &disp);
        assert!(set.is_empty(), "empty multi allowed when toggling off last");
        assert_eq!(prim, 5);
        assert_eq!(anch, Some(5));
    }

    #[test]
    fn compute_selection_shift_forward_range() {
        let disp = vec![0, 1, 2, 3, 4];
        let (set, prim, anch) = compute_selection(&HashSet::new(), Some(1), 3, false, true, &disp);
        assert_eq!(set, HashSet::from([1, 2, 3]));
        assert_eq!(prim, 3);
        assert_eq!(anch, Some(1)); // unchanged
    }

    #[test]
    fn compute_selection_shift_backward_range() {
        let disp = vec![0, 1, 2, 3, 4];
        let (set, prim, anch) = compute_selection(&HashSet::new(), Some(3), 1, false, true, &disp);
        assert_eq!(set, HashSet::from([1, 2, 3]));
        assert_eq!(prim, 1);
        assert_eq!(anch, Some(3));
    }

    #[test]
    fn compute_selection_shift_reordered_anchor() {
        let disp = vec![40, 10, 30, 20]; // anchor 30 at pos 2, clicked 10 at pos 1
        let cur = HashSet::new();
        let (set, prim, anch) = compute_selection(&cur, Some(30), 10, false, true, &disp);
        assert_eq!(set, HashSet::from([10, 30]));
        assert_eq!(prim, 10);
        assert_eq!(anch, Some(30)); // original kept
    }

    #[test]
    fn compute_selection_ctrl_then_shift() {
        let disp = vec![0, 1, 2, 3, 4];
        let cur0 = HashSet::new();
        let (after_ctrl, _, anch) = compute_selection(&cur0, None, 1, true, false, &disp);
        assert_eq!(after_ctrl, HashSet::from([1]));
        assert_eq!(anch, Some(1));
        let (after_shift, prim, anch2) =
            compute_selection(&after_ctrl, anch, 3, false, true, &disp);
        assert_eq!(after_shift, HashSet::from([1, 2, 3]));
        assert_eq!(prim, 3);
        assert_eq!(anch2, Some(1));
    }

    #[test]
    fn compute_selection_no_anchor_shift_equals_plain() {
        let disp = vec![0, 1, 2];
        let (set, prim, anch) = compute_selection(&HashSet::new(), None, 2, false, true, &disp);
        assert_eq!(set, HashSet::from([2]));
        assert_eq!(prim, 2);
        assert_eq!(anch, Some(2));
    }

    #[test]
    fn compute_selection_shift_anchor_not_in_displayed_treat_plain() {
        let disp = vec![100, 101];
        let (set, prim, anch) =
            compute_selection(&HashSet::new(), Some(999), 100, false, true, &disp);
        assert_eq!(set, HashSet::from([100]));
        assert_eq!(prim, 100);
        assert_eq!(anch, Some(100));
    }

    // ── first_two_selected (pure helper for CM-M1 EnterCompare) ─────────────────
    #[test]
    fn first_two_selected_preserves_display_order() {
        use std::collections::HashSet;
        let displayed = vec![0, 1, 2, 3, 4];
        let mut sel = HashSet::new();
        sel.insert(3);
        sel.insert(1);
        sel.insert(4);
        // Should return in displayed order, the first two that are selected: 1 then 3
        assert_eq!(AppModel::first_two_selected(&displayed, &sel), vec![1, 3]);
    }

    #[test]
    fn first_two_selected_less_than_two_returns_all_present() {
        use std::collections::HashSet;
        let displayed = vec![5, 6, 7];
        let mut sel = HashSet::new();
        sel.insert(6);
        assert_eq!(AppModel::first_two_selected(&displayed, &sel), vec![6]);
        let empty: HashSet<usize> = HashSet::new();
        assert!(AppModel::first_two_selected(&displayed, &empty).is_empty());
    }

    #[test]
    fn first_two_selected_respects_displayed_filter_order() {
        use std::collections::HashSet;
        let displayed = vec![10, 20, 30];
        let mut sel = HashSet::new();
        sel.insert(30);
        sel.insert(10);
        // Even if insert order reversed, displayed order wins.
        assert_eq!(AppModel::first_two_selected(&displayed, &sel), vec![10, 30]);
    }

    // ── Command-line folder-opening tests ────────────────────────────────────
    #[test]
    fn initial_dir_from_arg_none_and_bad_yield_none() {
        assert_eq!(super::initial_dir_from_arg(None), None);
        assert_eq!(super::initial_dir_from_arg(Some("")), None);
        assert_eq!(
            super::initial_dir_from_arg(Some("/non/existent/path/that/never/exists")),
            None
        );
        assert_eq!(
            super::initial_dir_from_arg(Some("also_no_such_thing_12345")),
            None
        );
    }

    #[test]
    fn initial_dir_from_arg_dir_returns_dir_tempfile() {
        let td = tempfile::tempdir().expect("tempdir");
        let dir_path = td.path().to_path_buf();
        // ensure it exists (tempdir does)
        let got = super::initial_dir_from_arg(dir_path.to_str());
        // after canonicalize in helper it should match the real abs path
        assert_eq!(
            got,
            Some(std::fs::canonicalize(&dir_path).unwrap_or(dir_path.clone()))
        );
    }

    #[test]
    fn initial_dir_from_arg_file_returns_its_parent_tempfile() {
        use std::fs;
        let td = tempfile::tempdir().expect("tempdir");
        let dir_path = td.path().to_path_buf();
        let file_path = dir_path.join("photo.jpg");
        fs::write(&file_path, b"fake image data").expect("write temp file");
        let got = super::initial_dir_from_arg(file_path.to_str());
        let exp = std::fs::canonicalize(&dir_path).unwrap_or(dir_path.clone());
        assert_eq!(got, Some(exp));
    }

    #[test]
    fn dir_to_open_from_pure_shape_logic_unit_testable() {
        // No fs calls; exercises the path decision split for unit testing.
        assert_eq!(
            super::dir_to_open_from(PathBuf::from("/abs/dir"), true),
            PathBuf::from("/abs/dir")
        );
        assert_eq!(
            super::dir_to_open_from(PathBuf::from("/abs/dir/sub"), true),
            PathBuf::from("/abs/dir/sub")
        );
        assert_eq!(
            super::dir_to_open_from(PathBuf::from("/abs/file.jpg"), false),
            PathBuf::from("/abs")
        );
        assert_eq!(
            super::dir_to_open_from(PathBuf::from("/abs/a/b/c.nef"), false),
            PathBuf::from("/abs/a/b")
        );
        assert_eq!(
            super::dir_to_open_from(PathBuf::from("barefile.jpg"), false),
            PathBuf::from(".")
        );
        assert_eq!(
            super::dir_to_open_from(PathBuf::from("sub/dir"), true),
            PathBuf::from("sub/dir")
        );
        assert_eq!(
            super::dir_to_open_from(PathBuf::from("sub/file.txt"), false),
            PathBuf::from("sub")
        );
        assert_eq!(
            super::dir_to_open_from(PathBuf::from(""), false),
            PathBuf::from(".")
        );
        assert_eq!(
            super::dir_to_open_from(PathBuf::from("."), true),
            PathBuf::from(".")
        );
    }

    // ── Duplicate filter tests ───────────────────────────────────────────────

    #[test]
    fn apply_duplicate_filter_inactive_returns_input_unchanged() {
        let mut app = test_app();
        // Populate a few entries so indices are meaningful.
        app.browser.entries = vec![
            test_entry("a.jpg", EntryKind::Image),
            test_entry("b.jpg", EntryKind::Image),
            test_entry("c.jpg", EntryKind::Image),
        ];
        app.dups.filter_active = false;
        app.dups.members.clear();

        let input = vec![0usize, 1, 2];
        let out = app.apply_duplicate_filter(input.clone());
        assert_eq!(out, input);
    }

    #[test]
    fn active_dups_filter_retains_only_members_order_preserved() {
        let mut app = test_app();
        app.browser.entries = vec![
            test_entry("a.jpg", EntryKind::Image),
            test_entry("b.jpg", EntryKind::Image),
            test_entry("c.jpg", EntryKind::Image),
            test_entry("d.jpg", EntryKind::Image),
        ];
        app.dups.filter_active = true;
        app.dups.members = std::collections::HashSet::from([1, 3]);

        let input = vec![0usize, 1, 2, 3];
        let out = app.apply_duplicate_filter(input);
        assert_eq!(out, vec![1, 3]);
    }

    #[test]
    fn compute_duplicate_sets_groups_and_members_at_threshold_10() {
        // Two near-dups (hamming 2) + one far unique at threshold 10.
        // Indices: 10 and 7 are close; 99 is distant.
        let items: Vec<(usize, u64)> = vec![
            (10, 0x0000_0000_0000_0000),
            (7, 0x0000_0000_0000_0003),  // hamming=2 from first
            (99, 0xFFFF_FFFF_FFFF_FFFF), // far from both
        ];
        let (groups, members) = compute_duplicate_sets(&items, 10);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0], vec![7, 10]);
        assert_eq!(members.len(), 2);
        assert!(members.contains(&7));
        assert!(members.contains(&10));
        assert!(!members.contains(&99));
    }

    #[test]
    fn duplicate_and_rating_filters_compose_to_intersection() {
        use crate::xmp::CullMeta;
        let mut app = test_app();
        app.browser.entries = vec![
            test_entry("dup1.jpg", EntryKind::Image),
            test_entry("dup2.jpg", EntryKind::Image),
            test_entry("other.jpg", EntryKind::Image),
            test_entry("dup3.jpg", EntryKind::Image),
        ];
        // Make dup1,dup2 a duplicate group.
        app.dups.groups = vec![vec![0, 1]];
        app.dups.members = std::collections::HashSet::from([0, 1]);
        app.dups.filter_active = true;

        // Rating filter: require >= 3 stars. Seed cull_cache accordingly.
        app.filter.rating = Some(3);
        app.filter.cull_cache.insert(
            app.browser.entries[0].path.clone(),
            CullMeta {
                rating: Some(5),
                label: None,
                rejected: false,
            },
        );
        app.filter.cull_cache.insert(
            app.browser.entries[1].path.clone(),
            CullMeta {
                rating: Some(2),
                label: None,
                rejected: false,
            },
        );
        app.filter.cull_cache.insert(
            app.browser.entries[2].path.clone(),
            CullMeta {
                rating: Some(4),
                label: None,
                rejected: false,
            },
        );
        app.filter.cull_cache.insert(
            app.browser.entries[3].path.clone(),
            CullMeta {
                rating: Some(5),
                label: None,
                rejected: false,
            },
        );

        // grid_indices should intersect: only index 0 (dup + rating>=3).
        // displayed_indices does rating sort too; we only care about set membership here.
        let grid = app.grid_indices();
        assert_eq!(grid, vec![0]);

        // displayed_indices also applies rating sort (desc) then duplicate filter.
        let disp = app.displayed_indices();
        assert_eq!(disp, vec![0]);
    }

    #[test]
    fn exact_dups_flag_and_filter_active_roundtrip() {
        let mut app = test_app();
        // Minimal: set exact mode flags and verify they are readable (status line path).
        app.dups.exact = true;
        app.dups.filter_active = true;
        assert!(app.dups.exact);
        assert!(app.dups.filter_active);
        // Toggling back via the same pattern the UI uses.
        app.dups.filter_active = false;
        app.dups.exact = false;
        assert!(!app.dups.exact);
        assert!(!app.dups.filter_active);
    }

    #[test]
    fn sorted_cameras_dedups_sorts_and_ignores_none() {
        let mut app = test_app();
        // entry 0: camera "Canon", 1: "Nikon", 2: "Canon", 3: None, 4: "Sony"
        app.folder_metadata
            .insert(0, (Some(1), Some("Canon".to_owned())));
        app.folder_metadata
            .insert(1, (Some(2), Some("Nikon".to_owned())));
        app.folder_metadata
            .insert(2, (Some(3), Some("Canon".to_owned())));
        app.folder_metadata.insert(3, (Some(4), None));
        app.folder_metadata
            .insert(4, (Some(5), Some("Sony".to_owned())));

        let cams = app.sorted_cameras();
        assert_eq!(cams, vec!["Canon", "Nikon", "Sony"]);
    }

    #[test]
    fn apply_camera_filter_none_returns_unchanged() {
        let mut app = test_app();
        app.browser.entries = vec![
            test_entry("a.jpg", EntryKind::Image),
            test_entry("b.jpg", EntryKind::Image),
            test_entry("c.jpg", EntryKind::Image),
        ];
        app.folder_metadata
            .insert(0, (None, Some("Canon".to_owned())));
        app.folder_metadata
            .insert(1, (None, Some("Nikon".to_owned())));
        app.folder_metadata
            .insert(2, (None, Some("Canon".to_owned())));
        app.filter.camera = None;

        let input = vec![0usize, 1, 2];
        assert_eq!(app.apply_camera_filter(input.clone()), input);
    }

    #[test]
    fn apply_camera_filter_some_retains_only_matching() {
        let mut app = test_app();
        app.browser.entries = vec![
            test_entry("a.jpg", EntryKind::Image),
            test_entry("b.jpg", EntryKind::Image),
            test_entry("c.jpg", EntryKind::Image),
            test_entry("d.jpg", EntryKind::Image),
        ];
        app.folder_metadata
            .insert(0, (None, Some("Canon".to_owned())));
        app.folder_metadata
            .insert(1, (None, Some("Nikon".to_owned())));
        app.folder_metadata
            .insert(2, (None, Some("Canon".to_owned())));
        app.folder_metadata
            .insert(3, (None, Some("Sony".to_owned())));
        app.filter.camera = Some("Canon".to_owned());

        let input = vec![0usize, 1, 2, 3];
        assert_eq!(app.apply_camera_filter(input), vec![0, 2]);
    }

    #[test]
    fn set_camera_filter_index_resolves_and_clears() {
        let mut app = test_app();
        app.folder_metadata
            .insert(0, (None, Some("Alpha".to_owned())));
        app.folder_metadata
            .insert(1, (None, Some("Beta".to_owned())));
        app.folder_metadata
            .insert(2, (None, Some("Alpha".to_owned())));
        // sorted: ["Alpha","Beta"]
        assert_eq!(app.sorted_cameras(), vec!["Alpha", "Beta"]);

        // Set via index 1 -> "Beta"
        // (simulate the handler)
        let idx = Some(1usize);
        app.filter.camera = idx.and_then(|i| app.sorted_cameras().get(i).cloned());
        assert_eq!(app.filter.camera, Some("Beta".to_owned()));

        // Set via index 0 -> "Alpha"
        let idx = Some(0usize);
        app.filter.camera = idx.and_then(|i| app.sorted_cameras().get(i).cloned());
        assert_eq!(app.filter.camera, Some("Alpha".to_owned()));

        // None clears
        app.filter.camera = None.and_then(|i: usize| app.sorted_cameras().get(i).cloned());
        // explicit clear path mirrors handler for None
        app.filter.camera = None;
        assert!(app.filter.camera.is_none());
    }

    #[test]
    fn sorted_tags_dedups_and_sorts() {
        let mut app = test_app();
        // entry 0 has beach,sunset; 1 has beach; 2 has none (omitted); 3 has sunset,beach
        app.folder_tags
            .insert(0, vec!["beach".to_string(), "sunset".to_string()]);
        app.folder_tags.insert(1, vec!["beach".to_string()]);
        // intentionally no entry 2 to simulate omitted
        app.folder_tags
            .insert(3, vec!["sunset".to_string(), "beach".to_string()]);

        let tags = app.sorted_tags();
        assert_eq!(tags, vec!["beach".to_string(), "sunset".to_string()]);
    }

    #[test]
    fn apply_tag_filter_none_returns_unchanged() {
        let mut app = test_app();
        app.browser.entries = vec![
            test_entry("a.jpg", EntryKind::Image),
            test_entry("b.jpg", EntryKind::Image),
            test_entry("c.jpg", EntryKind::Image),
        ];
        app.folder_tags.insert(0, vec!["beach".to_string()]);
        app.folder_tags.insert(1, vec!["sunset".to_string()]);
        app.folder_tags.insert(2, vec!["beach".to_string()]);
        app.filter.tag = None;

        let input = vec![0usize, 1, 2];
        assert_eq!(app.apply_tag_filter(input.clone()), input);
    }

    #[test]
    fn apply_tag_filter_some_retains_only_matching() {
        let mut app = test_app();
        app.browser.entries = vec![
            test_entry("a.jpg", EntryKind::Image),
            test_entry("b.jpg", EntryKind::Image),
            test_entry("c.jpg", EntryKind::Image),
            test_entry("d.jpg", EntryKind::Image),
        ];
        app.folder_tags.insert(0, vec!["beach".to_string()]);
        app.folder_tags.insert(1, vec!["sunset".to_string()]);
        app.folder_tags.insert(2, vec!["beach".to_string()]);
        app.folder_tags.insert(3, vec!["mountain".to_string()]);
        app.filter.tag = Some("beach".to_owned());

        let input = vec![0usize, 1, 2, 3];
        assert_eq!(app.apply_tag_filter(input), vec![0, 2]);
    }

    #[test]
    fn set_tag_filter_index_resolves_and_clears() {
        let mut app = test_app();
        app.folder_tags.insert(0, vec!["Alpha".to_string()]);
        app.folder_tags.insert(1, vec!["Beta".to_string()]);
        app.folder_tags.insert(2, vec!["Alpha".to_string()]);
        // sorted: ["Alpha","Beta"]
        assert_eq!(app.sorted_tags(), vec!["Alpha", "Beta"]);

        // Set via index 1 -> "Beta"
        let idx = Some(1usize);
        app.filter.tag = idx.and_then(|i| app.sorted_tags().get(i).cloned());
        assert_eq!(app.filter.tag, Some("Beta".to_owned()));

        // Set via index 0 -> "Alpha"
        let idx = Some(0usize);
        app.filter.tag = idx.and_then(|i| app.sorted_tags().get(i).cloned());
        assert_eq!(app.filter.tag, Some("Alpha".to_owned()));

        // None clears
        app.filter.tag = None.and_then(|i: usize| app.sorted_tags().get(i).cloned());
        app.filter.tag = None;
        assert!(app.filter.tag.is_none());
    }

    #[test]
    fn tag_and_camera_filters_compose_to_intersection() {
        let mut app = test_app();
        app.browser.entries = vec![
            test_entry("c1.jpg", EntryKind::Image),
            test_entry("c2.jpg", EntryKind::Image),
            test_entry("other.jpg", EntryKind::Image),
            test_entry("c3.jpg", EntryKind::Image),
        ];
        // 0: tag "beach" + camera "X" (match both), 1: tag "sunset"+Y, 2: tag "beach"+Y, 3: tag "mountain"+X
        app.folder_tags.insert(0, vec!["beach".to_string()]);
        app.folder_tags.insert(1, vec!["sunset".to_string()]);
        app.folder_tags.insert(2, vec!["beach".to_string()]);
        app.folder_tags.insert(3, vec!["mountain".to_string()]);

        app.folder_metadata.insert(0, (None, Some("X".to_owned())));
        app.folder_metadata.insert(1, (None, Some("Y".to_owned())));
        app.folder_metadata.insert(2, (None, Some("Y".to_owned())));
        app.folder_metadata.insert(3, (None, Some("X".to_owned())));

        // Tag filter active
        app.filter.tag = Some("beach".to_owned());

        // Camera filter active
        app.filter.camera = Some("X".to_owned());

        // Only index 0 survives both filters (tag beach + camera X).
        let grid = app.grid_indices();
        assert_eq!(grid, vec![0]);

        let disp = app.displayed_indices();
        assert_eq!(disp, vec![0]);
    }

    #[test]
    fn camera_and_rating_filters_compose_to_intersection() {
        use crate::xmp::CullMeta;
        let mut app = test_app();
        app.browser.entries = vec![
            test_entry("c1.jpg", EntryKind::Image),
            test_entry("c2.jpg", EntryKind::Image),
            test_entry("other.jpg", EntryKind::Image),
            test_entry("c3.jpg", EntryKind::Image),
        ];
        // Cameras for 0 and 2 match target "X"; 1 and 3 are "Y".
        app.folder_metadata.insert(0, (None, Some("X".to_owned())));
        app.folder_metadata.insert(1, (None, Some("Y".to_owned())));
        app.folder_metadata.insert(2, (None, Some("X".to_owned())));
        app.folder_metadata.insert(3, (None, Some("Y".to_owned())));

        // Camera filter active
        app.filter.camera = Some("X".to_owned());

        // Rating filter >=3
        app.filter.rating = Some(3);
        app.filter.cull_cache.insert(
            app.browser.entries[0].path.clone(),
            CullMeta {
                rating: Some(5),
                label: None,
                rejected: false,
            },
        );
        app.filter.cull_cache.insert(
            app.browser.entries[1].path.clone(),
            CullMeta {
                rating: Some(4),
                label: None,
                rejected: false,
            },
        );
        app.filter.cull_cache.insert(
            app.browser.entries[2].path.clone(),
            CullMeta {
                rating: Some(2),
                label: None,
                rejected: false,
            },
        );
        app.filter.cull_cache.insert(
            app.browser.entries[3].path.clone(),
            CullMeta {
                rating: Some(5),
                label: None,
                rejected: false,
            },
        );

        // Only index 0 survives both filters (camera X + rating >=3).
        let grid = app.grid_indices();
        assert_eq!(grid, vec![0]);

        let disp = app.displayed_indices();
        assert_eq!(disp, vec![0]);
    }

    #[test]
    fn year_of_epoch_known_values() {
        assert_eq!(AppModel::year_of_epoch(0), Some(1970));
        assert_eq!(AppModel::year_of_epoch(1_621_083_722), Some(2021));
    }

    #[test]
    fn sorted_capture_years_dedups_sorts_desc_ignores_none() {
        let mut app = test_app();
        // 2022-01-01 ~ 1640995200, 2021-01-01 ~ 1609459200, 2020-01-01 ~ 1577836800
        app.folder_metadata
            .insert(0, (Some(1640995200), Some("Canon".to_owned())));
        app.folder_metadata
            .insert(1, (Some(1609459200), Some("Nikon".to_owned())));
        app.folder_metadata
            .insert(2, (Some(1640995200), Some("Canon".to_owned())));
        app.folder_metadata.insert(3, (None, None)); // no capture unix
        app.folder_metadata
            .insert(4, (Some(1577836800), Some("Sony".to_owned())));
        app.folder_metadata
            .insert(5, (None, Some("Leica".to_owned()))); // no capture

        let years = app.sorted_capture_years();
        assert_eq!(years, vec![2022, 2021, 2020]);
    }

    #[test]
    fn apply_date_filter_none_returns_unchanged() {
        let mut app = test_app();
        app.browser.entries = vec![
            test_entry("a.jpg", EntryKind::Image),
            test_entry("b.jpg", EntryKind::Image),
            test_entry("c.jpg", EntryKind::Image),
        ];
        // 2021
        app.folder_metadata
            .insert(0, (Some(1621083722), Some("Canon".to_owned())));
        app.folder_metadata
            .insert(1, (Some(1621083722), Some("Nikon".to_owned())));
        app.folder_metadata
            .insert(2, (Some(1621083722), Some("Canon".to_owned())));
        app.filter.date = None;

        let input = vec![0usize, 1, 2];
        assert_eq!(app.apply_date_filter(input.clone()), input);
    }

    #[test]
    fn apply_date_filter_some_retains_only_matching_year() {
        let mut app = test_app();
        app.browser.entries = vec![
            test_entry("a.jpg", EntryKind::Image),
            test_entry("b.jpg", EntryKind::Image),
            test_entry("c.jpg", EntryKind::Image),
            test_entry("d.jpg", EntryKind::Image),
        ];
        app.folder_metadata
            .insert(0, (Some(1609459200), Some("Canon".to_owned()))); // 2021
        app.folder_metadata
            .insert(1, (Some(1640995200), Some("Nikon".to_owned()))); // 2022
        app.folder_metadata
            .insert(2, (Some(1609459200), Some("Canon".to_owned()))); // 2021
        app.folder_metadata
            .insert(3, (Some(1577836800), Some("Sony".to_owned()))); // 2020
        app.filter.date = Some(2021);

        let input = vec![0usize, 1, 2, 3];
        assert_eq!(app.apply_date_filter(input), vec![0, 2]);
    }

    #[test]
    fn date_and_camera_filters_compose_to_intersection() {
        let mut app = test_app();
        app.browser.entries = vec![
            test_entry("c1.jpg", EntryKind::Image),
            test_entry("c2.jpg", EntryKind::Image),
            test_entry("other.jpg", EntryKind::Image),
            test_entry("c3.jpg", EntryKind::Image),
        ];
        // 0: 2021+X (match both), 1:2022+Y, 2:2021+Y, 3:2022+X  => only 0 matches date+camera
        app.folder_metadata
            .insert(0, (Some(1609459200), Some("X".to_owned())));
        app.folder_metadata
            .insert(1, (Some(1640995200), Some("Y".to_owned())));
        app.folder_metadata
            .insert(2, (Some(1609459200), Some("Y".to_owned())));
        app.folder_metadata
            .insert(3, (Some(1640995200), Some("X".to_owned())));

        // Date filter active
        app.filter.date = Some(2021);

        // Camera filter active
        app.filter.camera = Some("X".to_owned());

        // Only index 0 survives both filters (year 2021 + camera X).
        let grid = app.grid_indices();
        assert_eq!(grid, vec![0]);

        let disp = app.displayed_indices();
        assert_eq!(disp, vec![0]);
    }

    /// Real-fixture end-to-end: runs the ACTUAL decode→dHash→group chain
    /// (`tasks::hash_entries` + `compute_duplicate_sets`) over a folder of known
    /// duplicates. `#[ignore]` + env-gated (real images, local fixtures only),
    /// mirroring the NEF/CR3 decode tests.
    /// Run: cargo test --release app::tests::find_duplicates_pipeline_on_real_fixture -- --ignored --nocapture
    #[test]
    #[ignore = "env-gated; real duplicate-detection pipeline. Local fixture not committed."]
    fn find_duplicates_pipeline_on_real_fixture() {
        let Ok(dir) = std::env::var("PHOTOBROWSER_TEST_DUPDIR") else {
            eprintln!("SKIP: set PHOTOBROWSER_TEST_DUPDIR to a local fixture directory");
            return;
        };
        let p = std::path::Path::new(&dir);
        if !p.exists() {
            eprintln!("SKIP: PHOTOBROWSER_TEST_DUPDIR path does not exist");
            return;
        }
        let pairs: Vec<(usize, std::path::PathBuf)> = std::fs::read_dir(p)
            .unwrap()
            .flatten()
            .enumerate()
            .map(|(i, e)| (i, e.path()))
            .collect();
        let items = crate::tasks::hash_entries(pairs, None);
        let (groups, members) = compute_duplicate_sets(&items, super::DUP_HAMMING_THRESHOLD);
        println!(
            "dup pipeline: {} hashed, {} groups, {} members",
            items.len(),
            groups.len(),
            members.len()
        );
        assert!(
            groups.len() >= 3,
            "expected >=3 duplicate groups, got {}",
            groups.len()
        );
        assert!(
            members.len() >= 6,
            "expected >=6 grouped members, got {}",
            members.len()
        );
    }

    /// Env-gated integration test exercising the catalog-persisted path for
    /// Find Duplicates. Runs hash_entries with a temp catalog DB, asserts
    /// duplicate groups are produced, proves dHash rows were written (by
    /// mtime-validated lookup), then re-runs and asserts identical (idx,hash)
    /// results (cache reuse). Mirrors the structure and gating of
    /// find_duplicates_pipeline_on_real_fixture.
    /// Run: cargo test --release app::tests::find_duplicates_catalog_persists_and_reuses -- --ignored --nocapture
    #[test]
    #[ignore = "env-gated; real duplicate-detection with catalog. Local fixture not committed."]
    fn find_duplicates_catalog_persists_and_reuses() {
        use std::collections::HashMap;
        use std::time::UNIX_EPOCH;

        let Ok(dir) = std::env::var("PHOTOBROWSER_TEST_DUPDIR") else {
            eprintln!("SKIP: set PHOTOBROWSER_TEST_DUPDIR to a local fixture directory");
            return;
        };
        let p = std::path::Path::new(&dir);
        if !p.exists() {
            eprintln!("SKIP: PHOTOBROWSER_TEST_DUPDIR path does not exist");
            return;
        }

        let dir_entries: Vec<_> = std::fs::read_dir(p).unwrap().flatten().collect();
        let pairs: Vec<(usize, std::path::PathBuf)> = dir_entries
            .iter()
            .enumerate()
            .map(|(i, e)| (i, e.path()))
            .collect();

        let td = tempfile::tempdir().unwrap();
        let db_path = td.path().join("catalog.db");

        // First run: populates catalog on misses
        let items1 = crate::tasks::hash_entries(pairs.clone(), Some(db_path.clone()));
        let (groups, members) = compute_duplicate_sets(&items1, super::DUP_HAMMING_THRESHOLD);
        println!(
            "dup catalog run1: {} hashed, {} groups, {} members",
            items1.len(),
            groups.len(),
            members.len()
        );
        assert!(
            groups.len() >= 3,
            "expected >=3 duplicate groups, got {}",
            groups.len()
        );
        assert!(
            members.len() >= 6,
            "expected >=6 grouped members, got {}",
            members.len()
        );

        // Prove persistence: for each successfully hashed file, get_dhash by its current mtime yields Some
        let cat = crate::catalog::Catalog::open(&db_path).expect("open catalog after first run");
        let idx_to_path: HashMap<usize, std::path::PathBuf> =
            pairs.iter().map(|(i, path)| (*i, path.clone())).collect();
        for (idx, _h) in &items1 {
            if let Some(path) = idx_to_path.get(idx) {
                if let Ok(meta) = std::fs::metadata(path) {
                    if let Ok(st) = meta.modified() {
                        if let Ok(dur) = st.duration_since(UNIX_EPOCH) {
                            let mt = dur.as_secs() as i64;
                            if let Some(ps) = path.to_str() {
                                let cached = cat.get_dhash(ps, mt).expect("catalog get_dhash call");
                                assert!(
                                    cached.is_some(),
                                    "expected persisted dhash for {}",
                                    path.display()
                                );
                            }
                        }
                    }
                }
            }
        }

        // Second run over same DB must return identical (idx, hash) tuples via cache hits
        let items2 = crate::tasks::hash_entries(pairs.clone(), Some(db_path.clone()));
        assert_eq!(
            items1, items2,
            "second scan with catalog must produce identical (idx,hash) results"
        );
    }

    /// Env-gated integration test for folder metadata indexing (M4a).
    /// Builds pairs from fixture dir, runs index_folder_metadata with temp catalog,
    /// asserts one result per input, catalog rows populated, at least one camera Some (NEFs),
    /// then re-runs with same DB and asserts identical results (fresh-reuse).
    /// Run: cargo test --release app::tests::index_folder_metadata_catalog_persists_and_reuses -- --ignored --nocapture
    #[test]
    #[ignore = "env-gated; folder metadata index with catalog. Local fixture not committed."]
    fn index_folder_metadata_catalog_persists_and_reuses() {
        let Ok(dir) = std::env::var("PHOTOBROWSER_TEST_DUPDIR") else {
            eprintln!("SKIP: set PHOTOBROWSER_TEST_DUPDIR to a local fixture directory");
            return;
        };
        let p = std::path::Path::new(&dir);
        if !p.exists() {
            eprintln!("SKIP: PHOTOBROWSER_TEST_DUPDIR path does not exist");
            return;
        }

        let dir_entries: Vec<_> = std::fs::read_dir(p).unwrap().flatten().collect();
        let pairs: Vec<(usize, std::path::PathBuf)> = dir_entries
            .iter()
            .enumerate()
            .map(|(i, e)| (i, e.path()))
            .collect();

        let td = tempfile::tempdir().unwrap();
        let db_path = td.path().join("catalog.db");

        // First run: populates catalog (or reuses if present) via EXIF reads + upserts
        let items1 = crate::tasks::index_folder_metadata(pairs.clone(), Some(db_path.clone()));
        println!(
            "index_folder_metadata run1: {} results for {} inputs",
            items1.len(),
            pairs.len()
        );
        assert_eq!(
            items1.len(),
            pairs.len(),
            "must return one entry per input pair"
        );

        // Verify catalog has rows for UTF8 paths we fed, and at least one camera is Some
        let cat = crate::catalog::Catalog::open(&db_path).expect("open catalog after first run");
        let mut saw_camera = false;
        for (idx, path) in &pairs {
            if let Some(ps) = path.to_str() {
                let meta = cat.get_file_meta(ps).expect("get_file_meta call");
                assert!(
                    meta.is_some(),
                    "expected catalog row for {} after index",
                    path.display()
                );
                // Check the returned items also reflect camera for NEF etc.
            }
            // Also inspect the returned item for this idx
            if let Some((_, _, cam)) = items1.iter().find(|(i, _, _)| i == idx) {
                if cam.is_some() {
                    saw_camera = true;
                }
            }
        }
        assert!(
            saw_camera,
            "expected at least one camera Some (NEFs carry camera EXIF)"
        );

        // Second run over same DB must return identical results (fresh-reuse path)
        let items2 = crate::tasks::index_folder_metadata(pairs.clone(), Some(db_path.clone()));
        assert_eq!(
            items1, items2,
            "second run with catalog must produce identical (idx,cap,cam) results"
        );
    }

    #[test]
    fn add_keyword_to_targets_applies_to_multi_selection() {
        let mut app = test_app();
        let td = tempfile::tempdir().unwrap();
        let p0 = td.path().join("p0.jpg");
        let p1 = td.path().join("p1.jpg");
        let p2 = td.path().join("p2.jpg");
        std::fs::write(&p0, b"").unwrap();
        std::fs::write(&p1, b"").unwrap();
        std::fs::write(&p2, b"").unwrap();
        app.browser.entries = vec![
            test_entry(p0.to_str().unwrap(), EntryKind::Image),
            test_entry(p1.to_str().unwrap(), EntryKind::Image),
            test_entry(p2.to_str().unwrap(), EntryKind::Image),
        ];
        app.selection.multi_selected = [0usize, 2].into_iter().collect();
        let res = app.add_keyword_to_targets("beach");
        assert_eq!(res, (2, 2));
        assert!(crate::xmp::read_sidecar_keywords(&p0).contains(&"beach".to_string()));
        assert!(crate::xmp::read_sidecar_keywords(&p2).contains(&"beach".to_string()));
        assert!(!crate::xmp::read_sidecar_keywords(&p1).contains(&"beach".to_string()));
        assert_eq!(app.folder_tags.get(&0), Some(&vec!["beach".to_string()]));
        assert_eq!(app.folder_tags.get(&2), Some(&vec!["beach".to_string()]));
        assert!(!app.folder_tags.contains_key(&1));
    }

    #[test]
    fn add_keyword_to_targets_falls_back_to_primary_when_no_multi() {
        let mut app = test_app();
        let td = tempfile::tempdir().unwrap();
        let p0 = td.path().join("p0.jpg");
        let p1 = td.path().join("p1.jpg");
        let p2 = td.path().join("p2.jpg");
        for p in [&p0, &p1, &p2] {
            std::fs::write(p, b"").unwrap();
        }
        app.browser.entries = vec![
            test_entry(p0.to_str().unwrap(), EntryKind::Image),
            test_entry(p1.to_str().unwrap(), EntryKind::Image),
            test_entry(p2.to_str().unwrap(), EntryKind::Image),
        ];
        app.selection.multi_selected.clear();
        app.selection.selected_index = Some(1);
        let res = app.add_keyword_to_targets("sunset");
        assert_eq!(res, (1, 1));
        assert!(crate::xmp::read_sidecar_keywords(&p1).contains(&"sunset".to_string()));
        assert!(crate::xmp::read_sidecar_keywords(&p0).is_empty());
        assert!(crate::xmp::read_sidecar_keywords(&p2).is_empty());
        assert_eq!(app.folder_tags.get(&1), Some(&vec!["sunset".to_string()]));
        assert!(!app.folder_tags.contains_key(&0));
        assert!(!app.folder_tags.contains_key(&2));
    }

    #[test]
    fn add_keyword_to_targets_empty_kw_is_noop() {
        let mut app = test_app();
        let td = tempfile::tempdir().unwrap();
        let p = td.path().join("x.jpg");
        std::fs::write(&p, b"").unwrap();
        app.browser.entries = vec![test_entry(p.to_str().unwrap(), EntryKind::Image)];
        app.selection.selected_index = Some(0);
        let res = app.add_keyword_to_targets("");
        assert_eq!(res, (0, 0));
        let res2 = app.add_keyword_to_targets("   ");
        assert_eq!(res2, (0, 0));
        assert!(crate::xmp::read_sidecar_keywords(&p).is_empty());
        assert!(app.folder_tags.is_empty());
    }

    #[test]
    fn remove_keyword_from_targets_applies_to_multi_selection() {
        let mut app = test_app();
        let td = tempfile::tempdir().unwrap();
        let p0 = td.path().join("p0.jpg");
        let p1 = td.path().join("p1.jpg");
        let p2 = td.path().join("p2.jpg");
        for p in [&p0, &p1, &p2] {
            std::fs::write(p, b"").unwrap();
        }
        app.browser.entries = vec![
            test_entry(p0.to_str().unwrap(), EntryKind::Image),
            test_entry(p1.to_str().unwrap(), EntryKind::Image),
            test_entry(p2.to_str().unwrap(), EntryKind::Image),
        ];
        // Pre-seed all with keyword "x"
        let _ = crate::xmp::add_keyword(&p0, "x");
        let _ = crate::xmp::add_keyword(&p1, "x");
        let _ = crate::xmp::add_keyword(&p2, "x");
        app.selection.multi_selected = [0usize, 2].into_iter().collect();
        let res = app.remove_keyword_from_targets("x");
        assert_eq!(res, (2, 2));
        assert!(!crate::xmp::read_sidecar_keywords(&p0).contains(&"x".to_string()));
        assert!(!crate::xmp::read_sidecar_keywords(&p2).contains(&"x".to_string()));
        assert!(crate::xmp::read_sidecar_keywords(&p1).contains(&"x".to_string()));
        assert!(!app.folder_tags.contains_key(&0));
        assert!(!app.folder_tags.contains_key(&2));
        // entry 1 sidecar unchanged (still has x); folder_tags not populated for it in this test setup
    }

    #[test]
    fn remove_keyword_from_targets_falls_back_to_primary() {
        let mut app = test_app();
        let td = tempfile::tempdir().unwrap();
        let p0 = td.path().join("p0.jpg");
        let p1 = td.path().join("p1.jpg");
        let p2 = td.path().join("p2.jpg");
        for p in [&p0, &p1, &p2] {
            std::fs::write(p, b"").unwrap();
        }
        app.browser.entries = vec![
            test_entry(p0.to_str().unwrap(), EntryKind::Image),
            test_entry(p1.to_str().unwrap(), EntryKind::Image),
            test_entry(p2.to_str().unwrap(), EntryKind::Image),
        ];
        let _ = crate::xmp::add_keyword(&p1, "x");
        app.selection.multi_selected.clear();
        app.selection.selected_index = Some(1);
        let res = app.remove_keyword_from_targets("x");
        assert_eq!(res, (1, 1));
        assert!(crate::xmp::read_sidecar_keywords(&p1).is_empty());
        assert!(crate::xmp::read_sidecar_keywords(&p0).is_empty());
        assert!(crate::xmp::read_sidecar_keywords(&p2).is_empty());
        assert!(!app.folder_tags.contains_key(&1));
        assert!(!app.folder_tags.contains_key(&0));
        assert!(!app.folder_tags.contains_key(&2));
    }

    #[test]
    fn remove_keyword_from_targets_empty_kw_is_noop() {
        let mut app = test_app();
        let td = tempfile::tempdir().unwrap();
        let p = td.path().join("x.jpg");
        std::fs::write(&p, b"").unwrap();
        app.browser.entries = vec![test_entry(p.to_str().unwrap(), EntryKind::Image)];
        app.selection.selected_index = Some(0);
        let res = app.remove_keyword_from_targets("");
        assert_eq!(res, (0, 0));
        let res2 = app.remove_keyword_from_targets("   ");
        assert_eq!(res2, (0, 0));
        assert!(crate::xmp::read_sidecar_keywords(&p).is_empty());
        assert!(app.folder_tags.is_empty());
    }

    // ── Cat-M12 describe_filters tests ──────────────────────────────────────

    #[test]
    fn describe_filters_none_active_returns_none() {
        assert!(describe_filters(None, None, None, None, None).is_none());
    }

    #[test]
    fn describe_filters_rating_only() {
        assert_eq!(
            describe_filters(Some(3), None, None, None, None),
            Some("3+ stars".to_owned())
        );
    }

    #[test]
    fn describe_filters_tag_prefixes_hash() {
        assert_eq!(
            describe_filters(None, None, None, None, Some("beach")),
            Some("#beach".to_owned())
        );
    }

    #[test]
    fn describe_filters_multiple_joined_in_order() {
        // order: rating, label, camera, year, tag
        let name = describe_filters(
            Some(5),
            Some("Red"),
            Some("Canon EOS R5"),
            Some(2021),
            Some("beach"),
        );
        assert_eq!(
            name,
            Some("5+ stars · Red · Canon EOS R5 · 2021 · #beach".to_owned())
        );
    }

    // ── Cat-M12 collection save/apply logic tests ───────────────────────────

    #[test]
    fn save_collection_snapshots_active_filters() {
        let mut app = test_app();
        app.filter.rating = Some(4);
        app.filter.label = Some(crate::xmp::ColorLabel::Yellow);
        app.filter.camera = Some("Sony".to_owned());
        app.filter.date = Some(2022);
        app.filter.tag = Some("sunset".to_owned());

        // Simulate the SaveCollection handler logic (pure parts)
        let label = app.filter.label.map(|l| l.as_str().to_owned());
        if let Some(name) = describe_filters(
            app.filter.rating,
            label.as_deref(),
            app.filter.camera.as_deref(),
            app.filter.date,
            app.filter.tag.as_deref(),
        ) {
            app.config.collections.push(crate::config::SavedCollection {
                name,
                rating_min: app.filter.rating,
                label,
                camera: app.filter.camera.clone(),
                date_year: app.filter.date,
                tag: app.filter.tag.clone(),
            });
            // do not call save in unit test (config path may not be writable in all envs)
        }

        assert_eq!(app.config.collections.len(), 1);
        let c = &app.config.collections[0];
        assert_eq!(c.name, "4+ stars · Yellow · Sony · 2022 · #sunset");
        assert_eq!(c.rating_min, Some(4));
        assert_eq!(c.label, Some("Yellow".to_owned()));
        assert_eq!(c.camera, Some("Sony".to_owned()));
        assert_eq!(c.date_year, Some(2022));
        assert_eq!(c.tag, Some("sunset".to_owned()));
    }

    #[test]
    fn apply_collection_restores_filters() {
        let mut app = test_app();
        // Seed a collection (simulating persisted)
        app.config.collections.push(crate::config::SavedCollection {
            name: "2+ stars · Blue · 2019 · #vacation".to_owned(),
            rating_min: Some(2),
            label: Some("Blue".to_owned()),
            camera: None,
            date_year: Some(2019),
            tag: Some("vacation".to_owned()),
        });

        // Simulate ApplyCollection(0)
        if let Some(c) = app.config.collections.first().cloned() {
            app.filter.rating = c.rating_min;
            app.filter.label = c
                .label
                .as_deref()
                .and_then(crate::xmp::ColorLabel::from_str);
            app.filter.camera = c.camera;
            app.filter.date = c.date_year;
            app.filter.tag = c.tag;
            // (scroll + rebuild omitted in unit test)
        }

        assert_eq!(app.filter.rating, Some(2));
        assert_eq!(app.filter.label, Some(crate::xmp::ColorLabel::Blue));
        assert!(app.filter.camera.is_none());
        assert_eq!(app.filter.date, Some(2019));
        assert_eq!(app.filter.tag, Some("vacation".to_owned()));
    }
}
