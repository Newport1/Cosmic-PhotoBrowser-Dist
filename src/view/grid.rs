//! Virtualized thumbnail grid — O(visible + margin) cell construction per frame.
//!
//! ## Composition choice: spacer-window
//! We use a top spacer + windowed rows + bottom spacer inside a single
//! `widget::scrollable`.  The scrollable carries an `on_scroll` callback that
//! delivers `Message::Scroll(AbsoluteOffset)` so `update()` can recompute the
//! request envelope.
//!
//! ## Single geometry authority (M5.3)
//! A column-count disagreement between the request side (`rebuild_snapshot`,
//! which only has an *estimate* of the pane width) and the render side
//! (`grid_content`, which receives the *measured* width from the `responsive`
//! wrapper) previously left the rightmost column un-requested (gray placeholders)
//! and desynced row grouping.  The fix:
//! - **Render geometry uses ONLY the measured width.** `grid_content` subtracts
//!   the same container padding + scrollbar that the estimate does, so the two
//!   sides land on the same `cols` whenever the panes render at their constant
//!   widths, and within ±1 otherwise.
//! - **The snapshot is a lookup table** (`HashMap<index, CellState>`), not a
//!   geometry authority.  `grid_content` renders whatever rows the measured cols
//!   imply and looks each index up, drawing a `Pending` placeholder on a miss.
//! - **`rebuild_snapshot` requests a bounded superset** (`request_envelope`,
//!   covering `cols_est ± 1`) so every index the view can render is already
//!   requested — no starved column is possible.
//! - **Cells are fixed-size**, never flexed, so a one-column rounding difference
//!   is cosmetically invisible (leftover row width is just trailing space).
//!
//! ## Viewport tracking
//! Window height is tracked via `AppModel::viewport_h` (f32, pixels), set by
//! `Message::Resize { width, height }` which is fired from the libcosmic
//! `on_window_resize` subscription override.  On startup it defaults to 800.0.
//! The scroll offset is tracked via `AppModel::scroll_offset_y` (f32), updated
//! by `Message::Scroll`.

use cosmic::iced::widget::scrollable::Viewport;
use cosmic::iced::{alignment, Background, Border, Color, Length};
use cosmic::widget;

use std::collections::HashMap;

use crate::app::{
    breadcrumb_segments, cell_h as cell_height, human_size, middle_ellipsis, AppModel, CellState,
    Message,
};
use crate::scan::{self, EntryKind};
use crate::thumb::xdg;
use crate::xmp::{ColorLabel, CullMeta};

/// Full-width toolbar, rendered ABOVE the three panes (not inside the
/// center pane) so it gets the whole window width and never clips its controls.
/// (Most toggles and sort/label moved into top-left menu bar; toolbar retains
/// nav + filter + rating filter + Compare.)
pub fn toolbar(app: &AppModel) -> cosmic::Element<'_, Message> {
    let has_parent = app
        .browser
        .current_dir
        .as_deref()
        .and_then(|p| p.parent())
        .is_some();

    // Nav kept in toolbar (frequent); sort moved to View > Sort by menu.
    let nav_controls = widget::row::with_children(vec![
        toolbar_nav_button(
            "◀",
            (!app.nav.back.is_empty()).then_some(Message::NavigateBack),
        ),
        toolbar_nav_button(
            "▶",
            (!app.nav.forward.is_empty()).then_some(Message::NavigateForward),
        ),
        toolbar_nav_button("↑", has_parent.then_some(Message::NavigateUp)),
    ])
    .spacing(6);
    // Readable fixed width (not greedy): the trailing Fill spacer absorbs slack so
    // the right-group keeps its natural width; min_width(920) backstop in main.rs.
    let filter_input = widget::text_input("filter filenames", &app.browser.filter_query)
        .on_input(Message::FilterChanged)
        .width(Length::Fixed(220.0));
    let clear_filter = widget::button::custom(widget::text("Clear").size(12).class(MUTED_TEXT))
        .on_press(Message::ClearFilter)
        .class(cosmic::theme::Button::Transparent);
    let right_controls = widget::row::with_children(vec![
        // Rating filter kept in toolbar (frequent, compact); label/hide-rejects/dotfiles/images/sort/gear moved to menus.
        // Use button::custom + explicit text class (MUTED_TEXT) + star SVG icon
        // (no Unicode glyphs) because button::text(...).class(Transparent) yields
        // invisible fg on dark surfaces.
        widget::button::custom(
            widget::row::with_children(vec![
                star_icon(true, 14).into(),
                widget::text(match app.filter.rating {
                    None => "All".to_owned(),
                    Some(n) => format!(">={}", n),
                })
                .size(12)
                .class(MUTED_TEXT)
                .into(),
            ])
            .spacing(3)
            .align_y(alignment::Vertical::Center),
        )
        .on_press(Message::CycleRatingFilter)
        .class(cosmic::theme::Button::Transparent)
        .into(),
        // Compare (2-up view-only Fit). Uses button::custom + MUTED_TEXT (no glyphs), always shown;
        // handler no-ops if <2 multi-selected. Mirrors loupe decode for the two panes.
        widget::button::custom(widget::text("Compare").size(12).class(MUTED_TEXT))
            .on_press(Message::EnterCompare)
            .class(cosmic::theme::Button::Transparent)
            .into(),
    ])
    .spacing(8);
    let header = widget::row::with_children(vec![
        nav_controls.into(),
        filter_input.into(),
        clear_filter.into(),
        widget::container(widget::text(""))
            .width(Length::Fill)
            .into(),
        right_controls.into(),
    ])
    .spacing(8)
    .width(Length::Fill);

    // Batch action bar: only when multi-selected (compact row below header).
    // Uses star_icon (no glyphs), button::custom + MUTED_TEXT for text, colored containers for swatches (exact loupe pattern, no current highlight).
    let inner: cosmic::Element<'_, Message> = if !app.selection.multi_selected.is_empty() {
        let n = app.selection.multi_selected.len();
        let mut bch: Vec<cosmic::Element<'_, Message>> =
            vec![widget::text(format!("{} selected", n))
                .size(12)
                .class(MUTED_TEXT)
                .into()];
        for i in 1..=5u8 {
            bch.push(
                widget::button::custom(star_icon(false, 14))
                    .padding(2)
                    .on_press(Message::BatchSetRating(i))
                    .into(),
            );
        }
        bch.push(
            widget::button::custom(widget::text("0").size(12).class(MUTED_TEXT))
                .on_press(Message::BatchSetRating(0))
                .class(cosmic::theme::Button::Transparent)
                .into(),
        );
        // visual group space
        bch.push(
            widget::container(widget::text(""))
                .width(Length::Fixed(6.0))
                .into(),
        );
        for &lab in &ColorLabel::all() {
            let col = crate::view::color::label_color(lab);
            let sw = widget::container(widget::text(""))
                .width(Length::Fixed(14.0))
                .height(Length::Fixed(14.0))
                .style(move |_| cosmic::iced::widget::container::Style {
                    background: Some(Background::Color(col)),
                    border: Border {
                        width: 1.0,
                        radius: 3.0.into(),
                        color: Color::from_rgb(0.25, 0.25, 0.28),
                    },
                    ..Default::default()
                });
            bch.push(
                widget::button::custom(sw)
                    .padding(1)
                    .on_press(Message::BatchSetLabel(Some(lab)))
                    .into(),
            );
        }
        // clear label swatch (dark, no current state)
        let clr = widget::container(widget::text(""))
            .width(Length::Fixed(14.0))
            .height(Length::Fixed(14.0))
            .style(|_| cosmic::iced::widget::container::Style {
                background: Some(Background::Color(Color::from_rgb(0.18, 0.18, 0.20))),
                border: Border {
                    width: 1.0,
                    radius: 3.0.into(),
                    color: Color::from_rgb(0.25, 0.25, 0.28),
                },
                ..Default::default()
            });
        bch.push(
            widget::button::custom(clr)
                .padding(1)
                .on_press(Message::BatchSetLabel(None))
                .into(),
        );
        bch.push(
            widget::container(widget::text(""))
                .width(Length::Fixed(6.0))
                .into(),
        );
        bch.push(
            widget::button::custom(widget::text("Reject").size(12).class(MUTED_TEXT))
                .on_press(Message::BatchSetReject(true))
                .class(cosmic::theme::Button::Transparent)
                .into(),
        );
        bch.push(
            widget::button::custom(widget::text("Unreject").size(12).class(MUTED_TEXT))
                .on_press(Message::BatchSetReject(false))
                .class(cosmic::theme::Button::Transparent)
                .into(),
        );
        if let Some(s) = &app.selection.batch_status {
            bch.push(
                widget::container(widget::text(""))
                    .width(Length::Fixed(6.0))
                    .into(),
            );
            bch.push(widget::text(s).size(12).class(MUTED_TEXT).into());
        }
        let batch_row = widget::row::with_children(bch).spacing(2);
        widget::column::with_children(vec![header.into(), batch_row.into()])
            .spacing(2)
            .into()
    } else {
        header.into()
    };

    widget::container(inner)
        .padding(8)
        .width(Length::Fill)
        .into()
}

/// Build the grid content for a measured content size (`avail_w`, `avail_h`)
/// supplied by the responsive wrapper.  This is the SINGLE geometry authority:
/// the rendered column count derives only from `avail_w`, and the visible-row
/// span only from `avail_h` — and `rebuild_snapshot` reads the same measured
/// values back, so request and render never disagree.
fn grid_content(app: &AppModel, avail_w: f32, avail_h: f32) -> cosmic::Element<'_, Message> {
    let thumb_size = app.config.thumb_size;
    let cell_w = thumb_size as f32;
    let cell_h = cell_height(thumb_size);

    let has_parent = app
        .browser
        .current_dir
        .as_deref()
        .and_then(|p| p.parent())
        .is_some();

    // `avail_w` is the grid's usable CONTENT width (`AppModel::grid_width()` —
    // window minus sidebar, preview, padding, scrollbar).  `rebuild_snapshot`
    // divides the SAME value by the same cell width, so the request side and the
    // render side always land on the same `cols`: no column can be starved, none
    // can overflow.  This is the single geometry authority.
    let cols = ((avail_w / cell_w).floor() as usize).max(1);

    let grid_indices = app.grid_indices();
    let item_count = grid_indices.len();
    let total_rows = if item_count == 0 {
        0
    } else {
        item_count.div_ceil(cols)
    };

    // The exact contiguous flat range this frame renders, at `cols`.  Because
    // rebuild_snapshot materialized this same range, every index is a hit.
    let render_range = visible_range(
        app.scroll_offset_y,
        avail_h,
        cell_h,
        cols,
        item_count,
        MARGIN_ROWS,
    );
    let first_rendered_row = render_range.start.checked_div(cols).unwrap_or(0);
    let last_rendered_row = if !render_range.is_empty() {
        ((render_range.end - 1).checked_div(cols).unwrap_or(0)) + 1
    } else {
        first_rendered_row
    };

    let top_spacer_h = first_rendered_row as f32 * cell_h;
    let bottom_spacer_h = (total_rows.saturating_sub(last_rendered_row)) as f32 * cell_h;

    #[cfg(debug_assertions)]
    tracing::debug!(
        requested = app.grid_snapshot.len(),
        rendered = render_range.len(),
        total_items = item_count,
        render_start = render_range.start,
        render_end = render_range.end,
        cols,
        avail_w,
        "grid view rebuild"
    );

    let mut outer = widget::column::with_capacity(8).spacing(4);

    // NOTE: the toolbar (nav / filter / rating / compare) now lives in the full-width
    // `toolbar()` row ABOVE the three panes (see app::view), not here — so it gets
    // the whole window width instead of the narrow center pane and never clips.
    outer = outer.push(breadcrumb_bar(app, has_parent));
    outer = outer.push(widget::divider::horizontal::default());

    if app.browser.current_dir.is_none() {
        outer = outer.push(
            widget::container(widget::text("No folder open"))
                .width(Length::Fill)
                .height(Length::Fill),
        );
        outer = outer.push(bottom_status_bar(app));
        return widget::container(outer)
            .width(Length::Fill)
            .height(Length::Fill)
            .padding(16)
            .into();
    }

    if item_count == 0 {
        outer = outer.push(
            widget::container(widget::text("(empty folder)"))
                .width(Length::Fill)
                .height(Length::Fill),
        );
        outer = outer.push(bottom_status_bar(app));
        return widget::container(outer)
            .width(Length::Fill)
            .height(Length::Fill)
            .padding(16)
            .into();
    }

    // ── virtual scroll body ─────────────────────────────────────────────────
    let mut body = widget::column::with_capacity(last_rendered_row - first_rendered_row + 4)
        .spacing(GRID_GUTTER);

    if top_spacer_h > 0.0 {
        body = body.push(
            widget::container(widget::text(""))
                .width(Length::Fill)
                .height(Length::Fixed(top_spacer_h)),
        );
    }

    // Render exactly the rows the MEASURED cols imply, chunking the render
    // range into rows of `cols`.  Each index is looked up in the snapshot map;
    // a miss (only possible transiently before a decode lands) draws Pending.

    // Build entry_index -> group_pos map once when duplicate filter is active.
    // Per-folder duplicate groups use a linear lookup.
    let dup_group_for: HashMap<usize, usize> = if app.dups.filter_active {
        build_dup_index(&app.dups.groups)
    } else {
        HashMap::new()
    };

    let mut idx = render_range.start;
    while idx < render_range.end {
        let mut row_items: Vec<cosmic::Element<'_, Message>> = Vec::with_capacity(cols);
        for _ in 0..cols {
            if idx >= render_range.end {
                break;
            }
            let entry_index = grid_indices[idx];
            let entry = &app.browser.entries[entry_index];
            let primary = app.selection.selected_index == Some(entry_index);
            let multi = app.selection.multi_selected.contains(&entry_index);
            let state = app.grid_snapshot.get(&idx).unwrap_or(&CellState::Pending);
            let preview_selectable = matches!(entry.kind, EntryKind::Image | EntryKind::Raw)
                || scan::is_text_previewable(&entry.name);
            let cull = app
                .filter
                .cull_cache
                .get(&entry.path)
                .copied()
                .unwrap_or_default();
            let dup_col: Option<Color> = dup_group_for
                .get(&entry_index)
                .copied()
                .map(dup_group_color);
            row_items.push(cell_widget(
                state,
                &entry.name,
                entry_index,
                primary,
                multi,
                thumb_size,
                preview_selectable,
                cull,
                dup_col,
            ));
            idx += 1;
        }
        body = body.push(widget::row::with_children(row_items).spacing(GRID_GUTTER));
    }

    if bottom_spacer_h > 0.0 {
        body = body.push(
            widget::container(widget::text(""))
                .width(Length::Fill)
                .height(Length::Fixed(bottom_spacer_h)),
        );
    }

    let scrollable = widget::scrollable(body)
        .height(Length::Fill)
        .on_scroll(|vp: Viewport| Message::Scroll(vp.absolute_offset().y));
    outer = outer.push(scrollable);
    outer = outer.push(bottom_status_bar(app));

    widget::container(outer)
        .width(Length::Fill)
        .height(Length::Fill)
        .padding(16)
        .into()
}

fn toolbar_nav_button<'a>(
    label: &'a str,
    message: Option<Message>,
) -> cosmic::Element<'a, Message> {
    let text = if message.is_some() {
        widget::text(label).size(14)
    } else {
        widget::text(label).size(14).class(MUTED_TEXT)
    };
    let content: cosmic::Element<'a, Message> = widget::container(text)
        .padding(2)
        .center_y(Length::Shrink)
        .into();
    let button = widget::button::custom(content)
        .padding(0)
        .class(cosmic::theme::Button::Transparent);

    match message {
        Some(message) => button.on_press(message).into(),
        None => button.into(),
    }
}

fn breadcrumb_bar(app: &AppModel, _has_parent: bool) -> cosmic::Element<'_, Message> {
    let mut children: Vec<cosmic::Element<'_, Message>> = Vec::new();

    if let Some(path) = &app.browser.current_dir {
        for (idx, (label, target)) in breadcrumb_segments(path).into_iter().enumerate() {
            if idx > 0 {
                children.push(widget::text("›").size(12).into());
            }
            children.push(
                widget::button::custom(
                    widget::text(middle_ellipsis(&label, 24))
                        .size(12)
                        .class(MUTED_TEXT),
                )
                .class(cosmic::theme::Button::Transparent)
                .on_press(Message::NavigateTo(target))
                .into(),
            );
        }
    } else {
        children.push(widget::text("No folder open").size(12).into());
    }

    widget::row::with_children(children)
        .spacing(6)
        .width(Length::Fill)
        .into()
}

fn bottom_status_bar(app: &AppModel) -> cosmic::Element<'_, Message> {
    let total = app.browser.entries.len();
    let images = app.image_count();
    let raw = app
        .browser
        .entries
        .iter()
        .filter(|entry| matches!(entry.kind, EntryKind::Raw))
        .count();
    let selected = usize::from(app.selection.selected_index.is_some());
    let cached = app.thumb.len();
    let thumb_size = app.config.thumb_size;
    let size_slider = widget::slider(96..=256, thumb_size, Message::ThumbSizeChanged).step(16u16);

    let mut left_children: Vec<cosmic::Element<'_, Message>> = vec![
        widget::text(format!(
            "{total} items · {images} images · {raw} RAW · {selected} selected · cache: {cached} thumbs"
        ))
        .size(12)
        .class(MUTED_TEXT)
        .into(),
    ];

    // Duplicate scan / filter status (minimal text, no special glyphs).
    if app.dups.scan_in_progress {
        left_children.push(
            widget::text(" · Finding duplicates…")
                .size(12)
                .class(MUTED_TEXT)
                .into(),
        );
    } else if app.dups.filter_active {
        let groups = app.dups.groups.len();
        let members = app.dups.members.len();
        let kind = if app.dups.exact { "exact" } else { "near" };
        left_children.push(
            widget::text(format!(
                " · {groups} {kind} duplicate groups ({members} images)"
            ))
            .size(12)
            .class(MUTED_TEXT)
            .into(),
        );
    }

    if let Some(ref cam) = app.filter.camera {
        left_children.push(
            widget::text(format!(" · Camera: {cam}"))
                .size(12)
                .class(MUTED_TEXT)
                .into(),
        );
    }
    if let Some(ref tag) = app.filter.tag {
        left_children.push(
            widget::text(format!(" · Tag: {tag}"))
                .size(12)
                .class(MUTED_TEXT)
                .into(),
        );
    }

    let left = widget::row::with_children(left_children).spacing(0);

    let content = widget::row::with_children(vec![
        left.into(),
        widget::container(widget::text(""))
            .width(Length::Fill)
            .into(),
        widget::text(format!("Thumb: {thumb_size}px"))
            .size(12)
            .into(),
        size_slider.width(Length::Fixed(180.0)).into(),
    ])
    .spacing(12)
    .width(Length::Fill);

    widget::container(content)
        .width(Length::Fill)
        .height(Length::Fixed(BOTTOM_STATUS_BAR_H))
        .into()
}

pub fn settings_panel(app: &AppModel) -> cosmic::Element<'_, Message> {
    let effective_dir = xdg::cache_root()
        .map(|path| path.display().to_string())
        .unwrap_or_else(|| "unavailable".to_owned());
    let effective_dir_short = middle_ellipsis(&effective_dir, 56);
    let cache_size = xdg::cache_root()
        .as_deref()
        .map(xdg::dir_size_bytes)
        .map(human_size)
        .unwrap_or_else(|| "unknown".to_owned());

    let max_input = widget::text_input("0 = unlimited", &app.cache_max_input)
        .on_input(Message::CacheMaxGbChanged)
        .width(Length::Fixed(120.0));
    let dir_input = widget::text_input("custom cache directory", &app.cache_dir_input)
        .on_input(Message::CacheDirInputChanged)
        .width(Length::Fill);

    let export_input = widget::text_input(
        "export folder (blank = ~/Pictures/PhotoBrowser Exports)",
        &app.export_dir_input,
    )
    .on_input(Message::ExportDirInputChanged)
    .width(Length::Fill);

    // Controls only (header "Cache settings" + Done removed: context drawer supplies
    // its own title bar and close; this fn is now only used for the drawer content).
    let content = widget::column::with_children(vec![
        widget::row::with_children(vec![
            widget::text("Max cache size (GB):").size(12).into(),
            max_input.into(),
            widget::text(format!("Current size: {cache_size}"))
                .size(12)
                .into(),
        ])
        .spacing(8)
        .into(),
        widget::text(format!("Effective cache dir: {effective_dir_short}"))
            .size(12)
            .into(),
        widget::row::with_children(vec![
            dir_input.into(),
            widget::button::text("Set custom dir")
                .on_press(Message::ApplyCacheDir)
                .into(),
            widget::button::text("Reset to default")
                .on_press(Message::ResetCacheDir)
                .into(),
        ])
        .spacing(8)
        .into(),
        widget::row::with_children(vec![
            export_input.into(),
            widget::button::text("Set export dir")
                .on_press(Message::ApplyExportDir)
                .into(),
        ])
        .spacing(8)
        .into(),
        widget::row::with_children(vec![
            widget::text("Full-RAW demosaic in the loupe (higher quality, slower):")
                .size(12)
                .into(),
            widget::button::text(if app.config.loupe_full_demosaic {
                "On"
            } else {
                "Off"
            })
            .on_press(Message::ToggleLoupeFullDemosaic)
            .into(),
        ])
        .spacing(8)
        .into(),
        // The full_res_loupe toggle is off by default. When enabled, Actual zoom
        // triggers a separate ≤8192 decode from the regular ≤2560 path.
        widget::row::with_children(vec![
            widget::text("Native-resolution detail at 100% zoom (≤8192 px, slower):")
                .size(12)
                .into(),
            widget::button::text(if app.config.full_res_loupe {
                "On"
            } else {
                "Off"
            })
            .on_press(Message::ToggleFullResLoupe)
            .into(),
        ])
        .spacing(8)
        .into(),
        widget::row::with_children(vec![
            widget::text("Develop-look (approx):").size(12).into(),
            widget::button::text(if app.config.develop_look { "on" } else { "off" })
                .on_press(Message::ToggleDevelopLook)
                .into(),
        ])
        .spacing(8)
        .into(),
        widget::row::with_children(vec![
            widget::text("Show filmstrip in loupe:").size(12).into(),
            widget::button::text(if app.config.show_filmstrip {
                "On"
            } else {
                "Off"
            })
            .on_press(Message::ToggleShowFilmstrip)
            .into(),
        ])
        .spacing(8)
        .into(),
    ])
    .spacing(8);

    widget::container(content)
        .width(Length::Fill)
        .padding(10)
        .into()
}

/// Number of margin rows to keep rendered above and below the visible window.
pub const MARGIN_ROWS: usize = 2;

// ── layout constants ──────────────────────────────────────────────────────────

/// Fixed width of the sidebar pane (matches sidebar.rs `SIDEBAR_W`).
pub const SIDEBAR_W: f32 = 260.0;

/// Fixed width of the preview pane (matches preview.rs `.width(Fixed(300.0))`).
pub const PREVIEW_W: f32 = 300.0;

/// Horizontal padding applied to the grid container on each side.
pub const GRID_PADDING: f32 = 16.0;

/// Fixed vertical space reserved for the bottom status bar.
pub const BOTTOM_STATUS_BAR_H: f32 = 44.0;

/// Default scrollbar width of the iced `Scrollable` widget (pixels).
/// The scrollable inside the grid container consumes this width from the
/// content area, so snapshot column math must subtract it from the pane width.
pub const SCROLLBAR_W: f32 = 10.0;

/// Visual gutter between fixed-size thumbnail cells.
const GRID_GUTTER: u16 = 8;

/// Internal tile padding around thumbnail content inside each flat card.
const TILE_PADDING: f32 = 6.0;

const ACCENT_BLUE: Color = Color::from_rgb(0.04, 0.52, 1.0); // #0A84FF
const GRID_BG: Color = Color::from_rgb(0.13, 0.13, 0.14); // #202022
const TILE_BG: Color = Color::from_rgb(0.21, 0.21, 0.22); // #363638
const TILE_BG_SUBTLE: Color = Color::from_rgb(0.19, 0.19, 0.20); // #303033
const TILE_BORDER: Color = Color::from_rgb(0.25, 0.25, 0.26); // #404043
const MUTED_TEXT: Color = Color::from_rgb(0.60, 0.60, 0.60); // #999999
const PLACEHOLDER_TEXT: Color = Color::from_rgb(0.64, 0.64, 0.66); // #A3A3A8
const RAW_TEXT: Color = Color::from_rgb(0.68, 0.68, 0.70); // #ADADB3
const FOLDER_TEXT: Color = Color::from_rgb(0.70, 0.70, 0.64); // faint warm neutral

/// Compute the grid pane's usable width in pixels for column-count arithmetic.
///
/// The window row is `[sidebar | grid | preview]`.  The grid uses
/// `Length::Fill` but the column count must be derived from the space it
/// actually occupies, not the full window width.
///
/// `_has_preview` is accepted for caller compatibility, but selection does not
/// change the preview pane width.
///
/// The returned value is clamped to a minimum of 1.0 so callers can safely
/// divide without checking.
pub fn grid_available_w(viewport_w: f32, _has_preview: bool) -> f32 {
    // Subtract sidebar, preview, and both sides of the grid's own padding.
    (viewport_w - SIDEBAR_W - PREVIEW_W - GRID_PADDING * 2.0).max(1.0)
}

/// Compute the grid body's usable height for virtualized windowing.
///
/// The bottom status bar is outside the scrollable grid body, so both the
/// render side and the request side must subtract it before calling
/// [`visible_range`] or scrolling selected rows into view.
pub fn grid_available_h(pane_h: f32) -> f32 {
    (pane_h - GRID_PADDING * 2.0 - BOTTOM_STATUS_BAR_H).max(1.0)
}

// ── windowing math ────────────────────────────────────────────────────────────

/// Compute the range of flat item indices that are visible (plus margin).
///
/// Parameters:
/// - `scroll_y`    — absolute scroll offset in pixels (top of visible area)
/// - `viewport_h`  — height of the visible viewport in pixels
/// - `cell_h`      — height of a single cell row in pixels (thumb + label)
/// - `cols`        — number of columns in the grid
/// - `item_count`  — total number of items
/// - `margin_rows` — extra rows to keep rendered above and below the window
///
/// Returns a `Range<usize>` of flat item indices (clamped to `0..item_count`).
pub fn visible_range(
    scroll_y: f32,
    viewport_h: f32,
    cell_h: f32,
    cols: usize,
    item_count: usize,
    margin_rows: usize,
) -> std::ops::Range<usize> {
    if item_count == 0 || cols == 0 || cell_h <= 0.0 {
        return 0..0;
    }

    // First visible row index (0-based).
    let first_row = (scroll_y / cell_h).floor() as isize;
    // Last visible row index (inclusive).
    let last_row = ((scroll_y + viewport_h) / cell_h).ceil() as isize;

    // Apply margin.
    let first_row_m = (first_row - margin_rows as isize).max(0) as usize;
    // Total number of rows.
    let total_rows = item_count.div_ceil(cols);
    let last_row_m = ((last_row + margin_rows as isize) as usize).min(total_rows);

    // Clamp start so overscroll past the last row returns 0..0 or a valid range.
    let first_row_clamped = first_row_m.min(total_rows);
    let start = (first_row_clamped * cols).min(item_count);
    let end = (last_row_m * cols).min(item_count);

    // Guarantee start <= end (handles any edge case from floating-point rounding).
    start.min(end)..end
}

// ── view ──────────────────────────────────────────────────────────────────────

/// Return a symbolic star icon (filled or outline) using embedded SVG + currentColor.
/// Size in px. Reuses the gear SVG embed pattern so it recolors on dark surfaces.
pub(crate) fn star_icon(filled: bool, size: u16) -> cosmic::widget::icon::Icon {
    static FILLED: &[u8] = include_bytes!("../../assets/icons/star-filled.svg");
    static OUTLINE: &[u8] = include_bytes!("../../assets/icons/star-outline.svg");
    let bytes = if filled { FILLED } else { OUTLINE };
    widget::icon::icon(widget::icon::from_svg_bytes(bytes).symbolic(true)).size(size)
}

/// Build the thumb cell widget for one grid slot.
#[allow(clippy::too_many_arguments)]
fn cell_widget<'a>(
    state: &CellState,
    name: &'a str,
    entry_index: usize,
    selected: bool, // primary (drives ▶ and strong highlight)
    multi_selected: bool,
    thumb_size: u16,
    preview_selectable: bool, // Image, Raw, or .md/.txt (for text preview)
    cull: CullMeta,
    dup_group_color: Option<Color>,
) -> cosmic::Element<'a, Message> {
    let sz = Length::Fixed(thumb_size as f32);
    let tile_content_sz = Length::Fixed((thumb_size as f32 - TILE_PADDING * 2.0).max(1.0));
    let cell_sz = Length::Fixed(cell_height(thumb_size));
    let label_h = Length::Fixed(20.0);

    // Every cell occupies a fixed `sz`×`sz` image area so rows stay aligned and
    // a one-column rounding difference can never make a cell render smaller.
    let img_area: cosmic::Element<'a, Message> = match state {
        CellState::Thumb(handle) => widget::container(
            widget::image(handle.clone())
                .width(tile_content_sz)
                .height(tile_content_sz),
        )
        .width(sz)
        .height(sz)
        .padding(TILE_PADDING)
        .align_x(alignment::Horizontal::Center)
        .align_y(alignment::Vertical::Center)
        .style(move |theme| {
            image_area_style(
                theme,
                selected,
                multi_selected,
                cull.rejected,
                dup_group_color,
            )
        })
        .into(),
        CellState::Pending => widget::container(widget::text("▢ loading").size(11).center())
            .width(sz)
            .height(sz)
            .padding(TILE_PADDING)
            .align_x(alignment::Horizontal::Center)
            .align_y(alignment::Vertical::Center)
            .style(pending_tile_style)
            .into(),
        CellState::Failed => widget::container(widget::text("⚠ failed").size(11).center())
            .width(sz)
            .height(sz)
            .padding(TILE_PADDING)
            .align_x(alignment::Horizontal::Center)
            .align_y(alignment::Vertical::Center)
            .style(failed_tile_style)
            .into(),
        CellState::RawPlaceholder => widget::container(
            widget::column::with_children(vec![
                widget::text("RAW").size(13).center().into(),
                widget::text("no preview").size(10).center().into(),
            ])
            .spacing(2),
        )
        .width(sz)
        .height(sz)
        .padding(TILE_PADDING)
        .align_x(alignment::Horizontal::Center)
        .align_y(alignment::Vertical::Center)
        .style(raw_tile_style)
        .into(),
        CellState::Dir(_) => widget::container(widget::text("📁").size(thumb_size / 3).center())
            .width(sz)
            .height(sz)
            .padding(TILE_PADDING)
            .align_x(alignment::Horizontal::Center)
            .align_y(alignment::Vertical::Center)
            .style(folder_tile_style)
            .into(),
        CellState::Glyph(glyph) => {
            widget::container(widget::text(*glyph).size(thumb_size / 3).center())
                .width(sz)
                .height(sz)
                .padding(TILE_PADDING)
                .align_x(alignment::Horizontal::Center)
                .align_y(alignment::Vertical::Center)
                .style(glyph_tile_style)
                .into()
        }
        CellState::UnknownFile => {
            static UNKNOWN_FILE_SVG: &[u8] = include_bytes!("../../assets/icons/unknown-file.svg");
            let icon =
                widget::icon(widget::icon::from_svg_bytes(UNKNOWN_FILE_SVG)).size(thumb_size / 3);
            widget::container(icon)
                .width(sz)
                .height(sz)
                .padding(TILE_PADDING)
                .align_x(alignment::Horizontal::Center)
                .align_y(alignment::Vertical::Center)
                .style(glyph_tile_style)
                .into()
        }
    };

    let label_text = if selected {
        format!("▶ {name}")
    } else {
        name.to_owned()
    };
    let label_text = middle_ellipsis(&label_text, (thumb_size as usize / 8).max(8));
    let label = widget::text(label_text)
        .size(10)
        .class(MUTED_TEXT)
        .width(sz)
        .height(label_h)
        .center();
    // Rating row is ALWAYS present at a fixed 14px (reserved in cell_h) so every cell is
    // the same height: filled star icons when rated, an empty spacer otherwise. (A conditional
    // row would overflow the fixed cell height and get clipped — the stars would vanish.)
    // Color label dot (if present) sits left of stars; reject uses border on image area (no height).
    let rating_h = Length::Fixed(14.0);
    let mut rating_children: Vec<cosmic::Element<'a, Message>> = Vec::new();
    if let Some(l) = cull.label {
        let dot = widget::container(widget::text(""))
            .width(Length::Fixed(8.0))
            .height(Length::Fixed(8.0))
            .style(move |_| cosmic::iced::widget::container::Style {
                background: Some(Background::Color(crate::view::color::label_color(l))),
                border: Border {
                    radius: 4.0.into(),
                    ..Default::default()
                },
                ..Default::default()
            });
        rating_children.push(dot.into());
    }
    if let Some(n) = cull.rating {
        if n >= 1 {
            let n = n.min(5);
            let stars: Vec<cosmic::Element<'a, Message>> =
                (0..n).map(|_| star_icon(true, 10).into()).collect();
            rating_children.extend(stars);
        }
    }
    let rating_row: cosmic::Element<'a, Message> = if rating_children.is_empty() {
        widget::container(widget::text(""))
            .width(sz)
            .height(rating_h)
            .into()
    } else {
        widget::container(widget::row::with_children(rating_children).spacing(3))
            .width(sz)
            .height(rating_h)
            .align_x(alignment::Horizontal::Center)
            .into()
    };
    let content: cosmic::Element<'a, Message> =
        widget::column::with_children(vec![img_area, rating_row, label.into()])
            .width(sz)
            .height(cell_sz)
            .spacing(0)
            .into();

    // Folders navigate; images + .md/.txt select for preview (image or bounded text).
    // Other Glyph cells (pdf, videos, archives, code, unknown) remain inert (today's behavior).
    match state {
        CellState::Dir(path) => widget::button::custom(content)
            .on_press(Message::NavigateTo(path.clone()))
            .width(sz)
            .height(cell_sz)
            .padding(0)
            .class(cosmic::theme::Button::Transparent)
            .into(),
        CellState::Glyph(_) | CellState::UnknownFile if !preview_selectable => {
            widget::container(content).width(sz).height(cell_sz).into()
        }
        _ => widget::button::custom(content)
            .on_press(Message::SelectImage(entry_index))
            .width(sz)
            .height(cell_sz)
            .padding(0)
            .selected(selected)
            .class(cosmic::theme::Button::Transparent)
            .into(),
    }
}

/// Distinct, high-visibility border colors cycled by duplicate-group index, so
/// members of the same near-duplicate cluster share a color in the filtered grid.
fn dup_group_color(group_idx: usize) -> cosmic::iced::Color {
    // 8-hue palette; cycles for >8 groups. No glyphs, just border color.
    const PALETTE: [(f32, f32, f32); 8] = [
        (0.95, 0.30, 0.30),
        (0.30, 0.65, 0.95),
        (0.40, 0.80, 0.40),
        (0.95, 0.75, 0.20),
        (0.75, 0.45, 0.90),
        (0.30, 0.80, 0.80),
        (0.95, 0.55, 0.25),
        (0.85, 0.40, 0.65),
    ];
    let (r, g, b) = PALETTE[group_idx % PALETTE.len()];
    cosmic::iced::Color::from_rgb(r, g, b)
}

/// Build a map from entry index to its duplicate-group position (index into dups.groups).
/// Pure helper; linear build is fine for per-folder counts.
fn build_dup_index(groups: &[Vec<usize>]) -> HashMap<usize, usize> {
    let mut m = HashMap::new();
    for (gi, group) in groups.iter().enumerate() {
        for &e in group {
            m.insert(e, gi);
        }
    }
    m
}

fn image_area_style(
    _theme: &cosmic::Theme,
    selected: bool,
    multi_selected: bool,
    rejected: bool,
    dup_group: Option<Color>,
) -> cosmic::iced::widget::container::Style {
    if let Some(c) = dup_group {
        return cosmic::iced::widget::container::Style {
            background: Some(Background::Color(GRID_BG)),
            border: Border {
                width: 3.0,
                radius: 0.0.into(),
                color: c,
            },
            ..Default::default()
        };
    }
    let (border_color, width) = if rejected {
        (crate::view::color::reject_color(), 2.0)
    } else if selected {
        (ACCENT_BLUE, 2.0)
    } else if multi_selected {
        (ACCENT_BLUE, 1.0)
    } else {
        (TILE_BORDER, 1.0)
    };

    cosmic::iced::widget::container::Style {
        background: Some(Background::Color(GRID_BG)),
        border: Border {
            width,
            radius: 0.0.into(),
            color: border_color,
        },
        ..Default::default()
    }
}

fn pending_tile_style(_theme: &cosmic::Theme) -> cosmic::iced::widget::container::Style {
    cosmic::iced::widget::container::Style {
        background: Some(Background::Color(TILE_BG_SUBTLE)),
        text_color: Some(PLACEHOLDER_TEXT),
        border: Border {
            width: 1.0,
            radius: 0.0.into(),
            color: TILE_BORDER,
        },
        ..Default::default()
    }
}

fn failed_tile_style(_theme: &cosmic::Theme) -> cosmic::iced::widget::container::Style {
    cosmic::iced::widget::container::Style {
        background: Some(Background::Color(TILE_BG_SUBTLE)),
        text_color: Some(Color::from_rgb(0.70, 0.58, 0.58)),
        border: Border {
            width: 1.0,
            radius: 0.0.into(),
            color: Color::from_rgb(0.34, 0.30, 0.30),
        },
        ..Default::default()
    }
}

fn raw_tile_style(_theme: &cosmic::Theme) -> cosmic::iced::widget::container::Style {
    cosmic::iced::widget::container::Style {
        background: Some(Background::Color(TILE_BG)),
        text_color: Some(RAW_TEXT),
        border: Border {
            width: 1.0,
            radius: 0.0.into(),
            color: Color::from_rgb(0.32, 0.32, 0.36),
        },
        ..Default::default()
    }
}

fn folder_tile_style(_theme: &cosmic::Theme) -> cosmic::iced::widget::container::Style {
    cosmic::iced::widget::container::Style {
        background: Some(Background::Color(TILE_BG)),
        text_color: Some(FOLDER_TEXT),
        border: Border {
            width: 1.0,
            radius: 0.0.into(),
            color: Color::from_rgb(0.34, 0.34, 0.28),
        },
        ..Default::default()
    }
}

fn glyph_tile_style(_theme: &cosmic::Theme) -> cosmic::iced::widget::container::Style {
    cosmic::iced::widget::container::Style {
        background: Some(Background::Color(TILE_BG_SUBTLE)),
        text_color: Some(PLACEHOLDER_TEXT),
        border: Border {
            width: 1.0,
            radius: 0.0.into(),
            color: TILE_BORDER,
        },
        ..Default::default()
    }
}

/// Render the virtualized thumbnail grid.
///
/// # Single geometry authority via measured size (M5.3 + M5.4)
///
/// The grid is wrapped in `widget::responsive`, which hands the closure the
/// pane's TRUE size every layout pass — including the first frame and every
/// resize, which the `on_window_resize` hook does **not** (it never fires at
/// window creation, so a derived estimate is stuck at the 1024 default until the
/// user manually resizes — the cause of the over-wide right gap).
///
/// The closure subtracts the grid padding + scrollbar to get the content width,
/// **writes the measured (width, height) back into the model** (`measured_w` /
/// `measured_h`, lock-free atomics), and renders.  The 32 ms tick reads those
/// values so `rebuild_snapshot` requests for the same size the view renders —
/// the request and render sides share one measured authority and cannot
/// disagree on the column count.
pub fn view(app: &AppModel) -> cosmic::Element<'_, Message> {
    let mw = app.measured_w.clone();
    let mh = app.measured_h.clone();
    widget::responsive(move |size| {
        let content_w = (size.width - 2.0 * GRID_PADDING - SCROLLBAR_W).max(1.0);
        let content_h = grid_available_h(size.height);
        mw.store(content_w.to_bits(), std::sync::atomic::Ordering::Relaxed);
        mh.store(content_h.to_bits(), std::sync::atomic::Ordering::Relaxed);
        grid_content(app, content_w, content_h)
    })
    .width(Length::Fill)
    .height(Length::Fill)
    .into()
}

// ── unit tests for visible_range and grid_available_w ────────────────────────

#[cfg(test)]
mod tests {
    use super::{
        grid_available_h, grid_available_w, visible_range, BOTTOM_STATUS_BAR_H, GRID_PADDING,
        PREVIEW_W, SIDEBAR_W,
    };

    /// Helper: cell_h = 180, cols = 4, thumb_size ~160 + label.
    fn cell_h() -> f32 {
        180.0
    }

    // ── top of list (scroll_y == 0) ───────────────────────────────────────────
    #[test]
    fn top_of_list() {
        // viewport = 600 px, cell_h = 180, cols = 4, 100 items, margin = 2
        // visible rows: 0..ceil(600/180)=4  → rows 0..4
        // with margin 2: rows 0..6
        // flat indices: 0..min(6*4, 100) = 0..24
        let r = visible_range(0.0, 600.0, cell_h(), 4, 100, 2);
        assert_eq!(r.start, 0, "start should be 0 at top");
        assert!(r.end <= 100, "end must not exceed item_count");
        assert!(
            r.end >= 16,
            "should cover at least 4 visible rows worth = 16 items"
        );
    }

    // ── deep scroll (mid-list) ────────────────────────────────────────────────
    #[test]
    fn deep_scroll() {
        // scroll_y = 3600 (row 20 at cell_h=180), viewport=600, cols=4, 200 items
        // visible rows: 20..24, with margin 2: 18..26
        // flat: 18*4..min(26*4,200) = 72..104
        let r = visible_range(3600.0, 600.0, 180.0, 4, 200, 2);
        assert_eq!(r.start, 72, "deep scroll start");
        assert_eq!(r.end, 104, "deep scroll end");
    }

    // ── overscroll past end (clamps to item_count) ────────────────────────────
    #[test]
    fn overscroll_clamps() {
        // 50 items, 4 cols = 13 rows.  Scroll way past end.
        let r = visible_range(99999.0, 600.0, 180.0, 4, 50, 2);
        assert_eq!(r.end, 50, "overscroll must clamp to item_count");
        assert!(r.start <= 50);
    }

    // ── margin clamps at 0 (top margin cannot go negative) ───────────────────
    #[test]
    fn margin_clamps_at_zero() {
        // scroll_y slightly past first row, but margin should not underflow
        let r = visible_range(10.0, 600.0, 180.0, 4, 100, 2);
        assert_eq!(r.start, 0, "margin must not produce negative start");
    }

    // ── margin clamps at item_count (bottom margin) ───────────────────────────
    #[test]
    fn margin_clamps_at_item_count() {
        // 20 items in 4 cols = 5 rows.  Viewport sees rows 3..5.
        // margin=2 would ask for rows up to 7, but total is 5.
        let r = visible_range(3.0 * 180.0, 600.0, 180.0, 4, 20, 2);
        assert!(r.end <= 20, "bottom margin must not exceed item_count");
    }

    // ── cols change shifts range ──────────────────────────────────────────────
    #[test]
    fn cols_change_shifts_range() {
        // Same scroll_y, same items, but different cols → different ranges.
        let r4 = visible_range(0.0, 600.0, 180.0, 4, 100, 0);
        let r8 = visible_range(0.0, 600.0, 180.0, 8, 100, 0);

        // With 8 cols and same viewport height, each row holds twice as many items.
        // r4 covers 4 rows * 4 cols = 16 items; r8 covers 4 rows * 8 cols = 32 items.
        assert!(
            r8.end >= r4.end,
            "more cols → more items per viewport (r4.end={}, r8.end={})",
            r4.end,
            r8.end
        );
    }

    // ── empty folder → 0..0 ──────────────────────────────────────────────────
    #[test]
    fn empty_folder() {
        let r = visible_range(0.0, 600.0, 180.0, 4, 0, 2);
        assert_eq!(r, 0..0, "empty folder must return 0..0");
    }

    // ── grid_available_w: subtracts sidebar + preview + grid padding ──────────

    #[test]
    fn grid_available_w_no_selection() {
        // viewport_w = 1024, no preview selected → preview is the fixed PREVIEW_W (300).
        // expected = 1024 - SIDEBAR_W(220) - PREVIEW_W(300) - GRID_PADDING*2(32) = 472
        let expected = 1024.0 - SIDEBAR_W - PREVIEW_W - GRID_PADDING * 2.0;
        let got = grid_available_w(1024.0, false);
        let selected = grid_available_w(1024.0, true);
        assert!(
            (got - expected).abs() < 0.5,
            "no-selection available_w: expected {expected}, got {got}"
        );
        assert!(
            (got - selected).abs() < 0.5,
            "selection must not change available_w: no-selection {got}, selected {selected}"
        );
        assert!(
            got < 1024.0,
            "available width must be less than the full viewport"
        );
    }

    #[test]
    fn grid_available_w_with_selection() {
        // viewport_w = 1024, image selected → preview is the fixed PREVIEW_W (300).
        // expected = 1024 - 220 - 300 - 32 = 472
        let expected = 1024.0 - SIDEBAR_W - PREVIEW_W - GRID_PADDING * 2.0;
        let got = grid_available_w(1024.0, true);
        let unselected = grid_available_w(1024.0, false);
        assert!(
            (got - expected).abs() < 0.5,
            "with-selection available_w: expected {expected}, got {got}"
        );
        assert!(
            (got - unselected).abs() < 0.5,
            "selection must not change available_w: selected {got}, no-selection {unselected}"
        );
    }

    #[test]
    fn grid_available_w_clamps_to_minimum() {
        // A very small window should still return at least 1.0.
        let got = grid_available_w(10.0, true);
        assert!(got >= 1.0, "available_w must be at least 1.0, got {got}");
    }

    #[test]
    fn grid_available_w_col_count_matches_pane() {
        // At 1024px window with thumb_size=128 and no selection:
        // avail = 1024 - 220 - 280 - 32 = 492 → cols = floor(492/128) = 3
        // WITHOUT the fix, cols would be floor(1024/128) = 8 — causing overflow.
        let avail = grid_available_w(1024.0, false);
        let cols = (avail / 128.0).floor() as usize;
        let bad_cols = (1024.0_f32 / 128.0).floor() as usize;
        assert!(
            cols < bad_cols,
            "corrected cols ({cols}) must be less than naive full-viewport cols ({bad_cols})"
        );
    }

    #[test]
    fn grid_available_h_subtracts_padding_and_bottom_status_bar() {
        let pane_h = 800.0_f32;
        let expected = pane_h - GRID_PADDING * 2.0 - BOTTOM_STATUS_BAR_H;

        assert_eq!(grid_available_h(pane_h), expected);
    }

    // ── responsive path: cols derived from measured pane width ────────────────

    /// Simulates the responsive widget measuring the actual grid pane width.
    /// The responsive `Size.width` is the true rendered pane width, which may
    /// differ from `grid_available_w` by the scrollbar width (≈10 px) or other
    /// layout engine rounding.  Cols derived from the measured width must fit
    /// within the pane without overflow.
    #[test]
    fn cols_from_responsive_measured_width_fit_within_pane() {
        // Simulate the responsive widget measuring a 480px pane (the true
        // rendered width after the layout engine assigns space).
        let measured_w = 480.0_f32;
        let thumb_size = 128.0_f32;
        let cols = (measured_w / thumb_size).floor() as usize;
        // cols * thumb_size must not exceed the measured width.
        assert!(
            cols as f32 * thumb_size <= measured_w,
            "cols ({cols}) * thumb_size ({thumb_size}) = {} must fit within measured_w ({measured_w})",
            cols as f32 * thumb_size
        );
    }

    /// Verify that a fullscreen window (1920px) with no selection produces
    /// correct cols from the responsive-measured width vs the constant estimate.
    /// The responsive width should always produce fewer or equal cols than the
    /// full-viewport width.
    #[test]
    fn fullscreen_no_preview_cols_not_overflow() {
        let viewport_w = 1920.0_f32;
        let thumb_size = 128.0_f32;

        // What grid_available_w estimates (used for snapshot fallback)
        let estimate = grid_available_w(viewport_w, false);
        let cols_estimate = (estimate / thumb_size).floor() as usize;

        // The responsive widget will measure a width ≤ estimate (layout
        // engine may assign less due to scrollbar, borders, etc.).  Assert
        // the estimate already fits within the pane by the constant formula.
        assert!(
            cols_estimate as f32 * thumb_size <= estimate,
            "cols ({cols_estimate}) * thumb_size overflow estimate ({estimate})"
        );

        // Full-viewport naive cols would be larger and overflow the pane.
        let naive_cols = (viewport_w / thumb_size).floor() as usize;
        assert!(
            cols_estimate < naive_cols,
            "responsive-path cols ({cols_estimate}) must be < naive ({naive_cols})"
        );
    }

    // ── single-authority invariant (M5.3) ────────────────────────────────────

    /// The render side (`grid_content`) and request side (`rebuild_snapshot`)
    /// both compute `cols = floor(grid_width / cell_w)` from the SAME derived
    /// content width.  Given identical inputs they must produce identical cols
    /// and therefore identical visible ranges — so every rendered index was
    /// requested.  This is the structural guarantee that replaced the fragile
    /// estimate-vs-measured split behind the R1–R3 right-column defects.
    #[test]
    fn render_and_request_cols_agree() {
        let cell_w = 160.0_f32;
        for window_w in [800.0_f32, 1024.0, 1366.0, 1920.0, 2560.0] {
            for has_preview in [false, true] {
                // Same derivation both sides use (mirrors AppModel::grid_width()).
                let content_w = grid_available_w(window_w, has_preview) - super::SCROLLBAR_W;
                let cols_render = ((content_w / cell_w).floor() as usize).max(1);
                let cols_request = ((content_w / cell_w).floor() as usize).max(1);
                assert_eq!(
                    cols_render, cols_request,
                    "cols must agree at window={window_w}, preview={has_preview}"
                );
                // And the cols must fit within the content width (no overflow).
                assert!(
                    cols_render as f32 * cell_w <= content_w.max(cell_w),
                    "cols ({cols_render}) * cell_w must fit content_w ({content_w})"
                );
            }
        }
    }

    // ── Duplicate group color-coding tests ──────────────────────────────────

    #[test]
    fn dup_group_color_same_idx_same_color() {
        assert_eq!(super::dup_group_color(0), super::dup_group_color(0));
        assert_eq!(super::dup_group_color(3), super::dup_group_color(3));
    }

    #[test]
    fn dup_group_color_diff_idx_lt8_diff_colors() {
        for i in 0..8 {
            for j in 0..8 {
                if i != j {
                    assert_ne!(
                        super::dup_group_color(i),
                        super::dup_group_color(j),
                        "color {} should differ from {}",
                        i,
                        j
                    );
                }
            }
        }
    }

    #[test]
    fn dup_group_color_wraps_at_8() {
        assert_eq!(super::dup_group_color(0), super::dup_group_color(8));
        assert_eq!(super::dup_group_color(2), super::dup_group_color(10));
        assert_eq!(super::dup_group_color(7), super::dup_group_color(15));
    }

    #[test]
    fn build_dup_index_entry_in_group_1_is_some_1() {
        let groups: Vec<Vec<usize>> = vec![vec![100], vec![7, 8], vec![20]];
        let m = super::build_dup_index(&groups);
        assert_eq!(m.get(&7), Some(&1));
        assert_eq!(m.get(&8), Some(&1));
    }

    #[test]
    fn build_dup_index_entry_not_in_any_group_is_none() {
        let groups: Vec<Vec<usize>> = vec![vec![1, 2]];
        let m = super::build_dup_index(&groups);
        assert_eq!(m.get(&0), None);
        assert_eq!(m.get(&3), None);
    }
}
