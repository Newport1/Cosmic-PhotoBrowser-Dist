//! Loupe — full-window single-image view.
//!
//! The loupe is a *bigger look at one image*: an in-`view()` mode switch gated on
//! `AppModel::loupe`, not a new window. It writes only XMP sidecars for ratings;
//! never the source image. It holds exactly ONE display-bounded decoded handle,
//! reusing the `decode::load_image` + `img.thumbnail(bound, bound)` spine.

use cosmic::iced::widget::scrollable::{Direction, Scrollbar};
use cosmic::iced::{Alignment, Background, Border, Color, ContentFit, Length, Point};
use cosmic::widget;
use std::sync::LazyLock;

use crate::app::{filter_matches, AppModel, Message};
use crate::inspection::LoupeZoom;
use crate::scan::{Entry, EntryKind};
use crate::thumb::ThumbState;

/// Lowest display-bounded decode edge — never decode the loupe smaller than the
/// 800px preview bound.
const LOUPE_MIN_BOUND: u32 = 800;
/// Highest display-bounded decode edge — cap to protect 4 GB VRAM (Risk #1).
const LOUPE_MAX_BOUND: u32 = 2560;

/// Highest edge for the *separate* full-res (native) Actual decode when
/// `full_res_loupe` is enabled. This path is distinct from the regular
/// LOUPE_MAX_BOUND + loupe_decode_bound path. Downscale only if the source
/// long edge exceeds the cap; thumbnail(8192,8192) implements it.
#[allow(dead_code)]
pub const LOUPE_HIGHRES_MAX_BOUND: u32 = 8192;

/// Pure math helper for the 8192 ceiling (used by tests; decode calls pass the
/// literal cap to thumbnail which does the box-fit downscale).
#[allow(dead_code)]
pub fn high_res_loupe_decode_bound() -> u32 {
    LOUPE_HIGHRES_MAX_BOUND
}

/// Stable Id for the zoomed (Actual / full-res) loupe scrollable. Enables
/// programmatic scroll_to for drag-to-pan from update().
pub(crate) static LOUPE_SCROLL_ID: LazyLock<cosmic::iced::widget::Id> =
    LazyLock::new(|| cosmic::iced::widget::Id::new("loupe-zoom"));

/// Whether the loupe can be opened for an entry of this kind. Only decodable
/// stills (`Image` / `Raw`) qualify — never directories or non-image files.
pub fn can_open_loupe(kind: &EntryKind) -> bool {
    matches!(kind, EntryKind::Image | EntryKind::Raw)
}

/// List the indices (into `entries`) of loupe-eligible siblings: entries for which
/// `can_open_loupe(kind)` is true AND the name matches the current filter query.
/// Returned in the order they appear in `entries` (the sorted/filtered document order).
/// Pure function; no side effects, no cache or decode interaction.
pub(crate) fn loupe_eligible_indices(entries: &[Entry], filter_query: &str) -> Vec<usize> {
    entries
        .iter()
        .enumerate()
        .filter_map(|(idx, entry)| {
            if can_open_loupe(&entry.kind) && filter_matches(&entry.name, filter_query) {
                Some(idx)
            } else {
                None
            }
        })
        .collect()
}

// ── filmstrip (L2) constants & helpers ────────────────────────────────────────

const FILM_THUMB: u16 = 96;
const FILMSTRIP_H: f32 = 108.0;
const FILM_GUTTER: u16 = 4;
const FILM_PADDING: f32 = 4.0;

const ACCENT_BLUE: Color = Color::from_rgb(0.04, 0.52, 1.0);
const FILM_BG: Color = Color::from_rgb(0.11, 0.11, 0.12);
const FILM_PLACEHOLDER_BG: Color = Color::from_rgb(0.17, 0.17, 0.18);
const FILM_PLACEHOLDER_FG: Color = Color::from_rgb(0.58, 0.58, 0.60);

fn film_cell_style(is_current: bool) -> cosmic::iced::widget::container::Style {
    cosmic::iced::widget::container::Style {
        background: Some(Background::Color(FILM_BG)),
        border: Border {
            width: if is_current { 2.0 } else { 1.0 },
            radius: 0.0.into(),
            color: if is_current {
                ACCENT_BLUE
            } else {
                Color::from_rgb(0.22, 0.22, 0.23)
            },
        },
        ..Default::default()
    }
}

fn filmstrip(app: &AppModel) -> cosmic::Element<'_, Message> {
    let current = app.loupe.as_ref().map(|l| l.index);
    // Use the (tested) pure helper for the list of loupe-eligible siblings under filter.
    let siblings = loupe_eligible_indices(&app.browser.entries, &app.browser.filter_query);
    if siblings.is_empty() {
        return widget::container(widget::text(""))
            .width(Length::Fill)
            .height(Length::Fixed(FILMSTRIP_H))
            .into();
    }

    let mut cells: Vec<cosmic::Element<'_, Message>> = Vec::with_capacity(siblings.len());
    let cell_sz = Length::Fixed(FILM_THUMB as f32);

    for &sidx in &siblings {
        let is_cur = Some(sidx) == current;
        let tstate = app
            .browser
            .entries
            .get(sidx)
            .map(|e| app.thumb.peek_state(&e.path))
            .unwrap_or(ThumbState::Pending);

        let thumb_el: cosmic::Element<'_, Message> = match &tstate {
            ThumbState::Ready(handle) => widget::image(handle.clone())
                .width(cell_sz)
                .height(cell_sz)
                .into(),
            ThumbState::Pending | ThumbState::Failed => widget::container(
                widget::text("▢")
                    .size(11)
                    .class(FILM_PLACEHOLDER_FG)
                    .center(),
            )
            .width(cell_sz)
            .height(cell_sz)
            .align_x(Alignment::Center)
            .align_y(Alignment::Center)
            .style(|_| cosmic::iced::widget::container::Style {
                background: Some(Background::Color(FILM_PLACEHOLDER_BG)),
                ..Default::default()
            })
            .into(),
        };

        let framed: cosmic::Element<'_, Message> = widget::container(thumb_el)
            .width(cell_sz)
            .height(cell_sz)
            .padding(2.0)
            .style(move |_theme| film_cell_style(is_cur))
            .into();

        let btn = widget::button::custom(framed)
            .on_press(Message::OpenLoupe(sidx))
            .padding(0)
            .class(cosmic::theme::Button::Transparent)
            .into();
        cells.push(btn);
    }

    let row = widget::row::with_children(cells)
        .spacing(FILM_GUTTER)
        .align_y(Alignment::Center);

    let scroller = widget::scrollable(row)
        .direction(Direction::Horizontal(Scrollbar::default()))
        .height(Length::Fixed(FILMSTRIP_H - FILM_PADDING * 2.0))
        .width(Length::Fill);

    widget::container(scroller)
        .width(Length::Fill)
        .height(Length::Fixed(FILMSTRIP_H))
        .padding(FILM_PADDING)
        .style(|_| cosmic::iced::widget::container::Style {
            background: Some(Background::Color(FILM_BG)),
            ..Default::default()
        })
        .into()
}

/// Display-bounded longest-edge cap for the loupe decode.
///
/// Takes the larger of the two viewport dimensions, rounds to `u32`, and clamps to
/// `[LOUPE_MIN_BOUND, LOUPE_MAX_BOUND]` (never below the 800px preview bound, never
/// above 2560 to protect VRAM). Non-finite / zero / negative input clamps to the
/// minimum bound — no panic, no NaN propagation.
pub fn loupe_decode_bound(viewport_w: f32, viewport_h: f32) -> u32 {
    let longest = viewport_w.max(viewport_h);
    if !longest.is_finite() || longest <= 0.0 {
        return LOUPE_MIN_BOUND;
    }
    let rounded = longest.round();
    // `f32::round` of a finite positive value is finite & ≥ 0; clamp the f32 to the
    // bound range first so the `as u32` cast cannot saturate/truncate surprisingly.
    let clamped = rounded.clamp(LOUPE_MIN_BOUND as f32, LOUPE_MAX_BOUND as f32);
    (clamped as u32).clamp(LOUPE_MIN_BOUND, LOUPE_MAX_BOUND)
}

/// New scroll offset (one axis) that keeps the content point currently under
/// `focal` (a viewport-relative coordinate, e.g. viewport_centre) stationary
/// when the zoom factor changes from `old` to `new`. Clamped to [0, max_off].
pub fn focal_zoom_offset(old_off: f32, focal: f32, old: f32, new: f32, max_off: f32) -> f32 {
    if old <= 0.0 || !old.is_finite() || !new.is_finite() {
        return old_off.clamp(0.0, max_off.max(0.0));
    }
    let content = old_off + focal; // content coord under focal at old zoom
    let scaled = content * (new / old) - focal; // its new offset to stay under focal
    scaled.clamp(0.0, max_off.max(0.0))
}

/// Full-window single-image view. Renders a dark container with a top bar (a
/// Close button + the filename) and the centered, display-fit image.
pub fn view(app: &AppModel) -> cosmic::Element<'_, Message> {
    let filename = app
        .loupe
        .as_ref()
        .and_then(|loupe| loupe.path.file_name())
        .and_then(|name| name.to_str())
        .unwrap_or("")
        .to_owned();

    let badge = app.loupe.as_ref().and_then(|loupe| {
        app.browser
            .entries
            .get(loupe.index)
            .filter(|entry| entry.kind == EntryKind::Raw)
            .map(|_| {
                if loupe.want_full_demosaic {
                    crate::inspection::LoupeDecodeMode::FullRaw.label()
                } else {
                    loupe.decode_mode.label()
                }
            })
    });

    let mut top_bar_children: Vec<cosmic::Element<'_, Message>> =
        vec![widget::container(widget::text(filename).size(14))
            .width(Length::Fill)
            .into()];
    if let Some(label) = badge {
        top_bar_children.push(widget::text(label).size(12).into());
    }
    if let Some(loupe) = &app.loupe {
        let zoom_label = match loupe.zoom {
            LoupeZoom::Fit => "100%",
            LoupeZoom::Actual => "Fit",
        };
        top_bar_children.push(
            widget::button::text(zoom_label)
                .on_press(Message::ToggleLoupeZoom)
                .into(),
        );
        // Develop-look (approx) badge when enabled and the image has develop edits in its sidecar.
        if app.config.develop_look && loupe.xmp.as_ref().is_some_and(|x| x.has_develop_edits) {
            top_bar_children.push(widget::text("Develop-look (approx)").size(12).into());
        }
        // Play/pause for slideshow (view-only auto-advance). ▶ when paused, ⏸ when playing.
        let slideshow_label = if app.loupe_rt.slideshow_playing {
            "⏸ Pause"
        } else {
            "▶ Play"
        };
        top_bar_children.push(
            widget::button::text(slideshow_label)
                .on_press(Message::ToggleSlideshow)
                .into(),
        );
        // Star rating (click to set/clear; 1 key remains zoom). Writes only XMP sidecar.
        // Use SVG icons via grid::star_icon (currentColor symbolic) + button::custom
        // (button::text glyph was blank; Transparent text fg was invisible).
        let rating = loupe.rating.unwrap_or(0);
        let mut stars: Vec<cosmic::Element<'_, Message>> = Vec::with_capacity(5);
        for i in 1..=5u8 {
            let filled = rating >= i;
            stars.push(
                widget::button::custom(crate::view::grid::star_icon(filled, 16))
                    .padding(2)
                    .on_press(Message::SetLoupeRating(i))
                    .into(),
            );
        }
        top_bar_children.push(widget::row::with_children(stars).spacing(2).into());

        // 5 color swatches + clear (after stars). Use colored containers (no Unicode glyphs).
        // Current label gets white-ish outline highlight.
        let cur_label = loupe.xmp.as_ref().and_then(|x| x.label);
        for &lab in &crate::xmp::ColorLabel::all() {
            let is_cur = cur_label == Some(lab);
            let col = crate::view::color::label_color(lab);
            let sw = widget::container(widget::text(""))
                .width(Length::Fixed(16.0))
                .height(Length::Fixed(16.0))
                .style(move |_| cosmic::iced::widget::container::Style {
                    background: Some(Background::Color(col)),
                    border: Border {
                        width: if is_cur { 2.0 } else { 1.0 },
                        radius: 3.0.into(),
                        color: if is_cur {
                            Color::from_rgb(0.9, 0.9, 0.95)
                        } else {
                            Color::from_rgb(0.25, 0.25, 0.28)
                        },
                    },
                    ..Default::default()
                });
            top_bar_children.push(
                widget::button::custom(sw)
                    .padding(1)
                    .on_press(Message::SetLoupeLabel(Some(lab)))
                    .into(),
            );
        }
        // Clear label (dark swatch; highlighted when none)
        let clear_is_cur = cur_label.is_none();
        let clear_sw = widget::container(widget::text(""))
            .width(Length::Fixed(16.0))
            .height(Length::Fixed(16.0))
            .style(move |_| cosmic::iced::widget::container::Style {
                background: Some(Background::Color(Color::from_rgb(0.18, 0.18, 0.20))),
                border: Border {
                    width: if clear_is_cur { 2.0 } else { 1.0 },
                    radius: 3.0.into(),
                    color: if clear_is_cur {
                        Color::from_rgb(0.7, 0.7, 0.75)
                    } else {
                        Color::from_rgb(0.25, 0.25, 0.28)
                    },
                },
                ..Default::default()
            });
        top_bar_children.push(
            widget::button::custom(clear_sw)
                .padding(1)
                .on_press(Message::SetLoupeLabel(None))
                .into(),
        );

        // Reject toggle: red border; filled bg when active (no glyph).
        let is_rej = loupe.xmp.as_ref().is_some_and(|x| x.rejected);
        let rej_col = crate::view::color::reject_color();
        let rej_content = widget::container(widget::text(""))
            .width(Length::Fixed(16.0))
            .height(Length::Fixed(16.0))
            .style(move |_| cosmic::iced::widget::container::Style {
                background: if is_rej {
                    Some(Background::Color(rej_col))
                } else {
                    None
                },
                border: Border {
                    width: 2.0,
                    radius: 3.0.into(),
                    color: rej_col,
                },
                ..Default::default()
            });
        top_bar_children.push(
            widget::button::custom(rej_content)
                .padding(1)
                .on_press(Message::ToggleLoupeReject)
                .into(),
        );
    }
    top_bar_children.push(
        widget::button::text("✕ Close")
            .on_press(Message::CloseLoupe)
            .into(),
    );

    // The filename label takes the remaining width so the Close button is pushed to
    // the right edge of the bar.
    let top_bar = widget::row::with_children(top_bar_children)
        .align_y(Alignment::Center)
        .padding(8)
        .spacing(8)
        .width(Length::Fill);

    let center: cosmic::Element<'_, Message> = match app.loupe.as_ref() {
        Some(loupe) => {
            // Choose the source handle for rendering:
            // - Fit zoom: always the bounded (≤2560 via normal decode_mode) handle
            // - Actual + full_res_loupe ON: prefer high_res_handle (≤8192) if present;
            //   until the async re-decode lands we fall back to the existing bounded handle
            //   (so 1:1 shows the Fit-decode pixels, then swaps to native res).
            // - Otherwise (flag off or no high yet): use the main handle 1:1.
            let handle_for_render = if loupe.zoom == LoupeZoom::Actual && app.config.full_res_loupe
            {
                // Actual: prefer native-res high_res, then the Fit demosaic upgrade, then the bounded base.
                loupe
                    .high_res_handle
                    .as_ref()
                    .or(loupe.demosaic_handle.as_ref())
                    .or(loupe.handle.as_ref())
            } else {
                // Fit: full-demosaic upgrade once it lands, else the fast embedded base, else the
                // previous image as a transitional placeholder (so nav never flashes blank gray).
                loupe
                    .demosaic_handle
                    .as_ref()
                    .or(loupe.handle.as_ref())
                    .or(loupe.placeholder_handle.as_ref())
            };
            if let Some(handle) = handle_for_render {
                // Fit renders a bare container(image.expand(true)) — no mouse_area (pan is
                // Actual-only). expand(true) makes the loupe paint on first open (#8); the
                // remaining arrow-nav repaint gap is a content-change/damage issue tracked in #8.
                match loupe.zoom {
                    LoupeZoom::Fit => {
                        // Fit mode uses expand(true) so a new image handle fills the available
                        // space. Pan controls remain limited to Actual zoom.
                        let image = widget::image(handle.clone())
                            .content_fit(ContentFit::Contain)
                            .expand(true);
                        widget::container(image)
                            .width(Length::Fill)
                            .height(Length::Fill)
                            // #8 fix (live-verified v1.32.1): a per-image Id changes this node's
                            // identity when the image (handle) changes, so iced 0.14 — which has no
                            // widget key() API and diffs by tree shape — rebuilds the subtree on
                            // arrow-nav, forcing wgpu to repaint (previously stale until a resize).
                            .id(cosmic::iced::widget::Id::new(format!(
                                "loupe-fit-image:{}",
                                loupe.path.display()
                            )))
                            .into()
                    }
                    LoupeZoom::Actual => {
                        let (w, h) = decoded_handle_dimensions(handle).unwrap_or((1, 1));
                        let f = app.loupe_rt.zoom_factor;
                        let image = widget::image(handle.clone())
                            .content_fit(ContentFit::Contain)
                            .width(Length::Fixed(w as f32 * f))
                            .height(Length::Fixed(h as f32 * f));
                        let inner = widget::container(image)
                            .width(Length::Shrink)
                            .height(Length::Shrink)
                            // #13: per-(image, handle-dims) Id so iced rebuilds this subtree both on
                            // arrow-nav (path changes) AND when the native full-RAW high-res handle swaps
                            // in async for the SAME image (w/h change) — forces wgpu to repaint instead of
                            // staying black until a window resize. Mirrors the #8 Fit-container fix.
                            .id(cosmic::iced::widget::Id::new(format!(
                                "loupe-actual:{}:{}x{}",
                                loupe.path.display(),
                                w,
                                h
                            )));
                        // Use Both so horizontal offset/scroll is supported (default is
                        // Vertical only). Wheel still primarily drives V; drag drives both.
                        let scroll = widget::scrollable(inner)
                            .id(LOUPE_SCROLL_ID.clone())
                            .direction(Direction::Both {
                                vertical: Scrollbar::default(),
                                horizontal: Scrollbar::default(),
                            })
                            .on_scroll(|vp| Message::LoupeScrolled(vp.absolute_offset()))
                            .width(Length::Fill)
                            .height(Length::Fill);
                        // mouse_area wraps the *scrollable* (not the inner content) so
                        // on_move cursor coords stay in the stable viewport frame; otherwise
                        // each scroll_to shifts the content under the cursor and feeds back
                        // into the next move → pan jitter. Drag is only built on this zoomed
                        // (Actual) arm, so it's active only when there's something to pan.
                        widget::mouse_area(scroll)
                            .on_press(Message::LoupePanPress)
                            .on_release(Message::LoupePanRelease)
                            .on_move(|p: Point| Message::LoupePanMove(p))
                            .into()
                    }
                }
            } else if let Some(err) = &loupe.error {
                widget::text(format!("Failed to open image: {err}"))
                    .size(14)
                    .into()
            } else {
                widget::text("Loading…").size(14).into()
            }
        }
        None => widget::text("").into(),
    };

    let center = widget::container(center)
        .width(Length::Fill)
        .height(Length::Fill)
        .align_x(Alignment::Center)
        .align_y(Alignment::Center);

    let mut children = vec![top_bar.into(), center.into()];
    if let Some(loupe) = &app.loupe {
        if let Some(hist) = loupe.histogram.as_ref() {
            children.push(crate::view::preview::histogram_section(hist));
        }
        if let Some(x) = &loupe.xmp {
            let mut parts: Vec<String> = Vec::new();
            if let Some(m) = &x.model {
                parts.push(m.clone());
            }
            if let Some(e) = &x.exposure_time {
                parts.push(e.clone());
            }
            if let Some(f) = &x.f_number {
                parts.push(format!("f {}", f));
            }
            if let Some(iso) = &x.iso {
                parts.push(format!("ISO {}", iso));
            }
            if x.has_develop_edits {
                parts.push("edited".to_owned());
            }
            if !parts.is_empty() {
                children.insert(
                    1,
                    widget::container(widget::text(parts.join("  ·  ")).size(11))
                        .padding([0, 8])
                        .into(),
                );
            }
        }
    }
    if app.config.show_filmstrip {
        children.push(filmstrip(app));
    }
    let body = widget::column::with_children(children)
        .width(Length::Fill)
        .height(Length::Fill);

    widget::container(body)
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
}

pub(crate) fn decoded_handle_dimensions(
    handle: &cosmic::widget::image::Handle,
) -> Option<(u32, u32)> {
    match handle {
        cosmic::widget::image::Handle::Rgba { width, height, .. } => Some((*width, *height)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scan::FileCategory;

    #[test]
    fn loupe_decode_bound_clamps_small_window_up_to_min() {
        // A tiny window must never decode below the 800px preview bound.
        assert_eq!(loupe_decode_bound(320.0, 200.0), 800);
        assert_eq!(loupe_decode_bound(800.0, 600.0), 800);
    }

    #[test]
    fn loupe_decode_bound_clamps_huge_window_down_to_max() {
        // A 4K+ window must cap at 2560 to protect VRAM.
        assert_eq!(loupe_decode_bound(3840.0, 2160.0), 2560);
        assert_eq!(loupe_decode_bound(5000.0, 9000.0), 2560);
    }

    #[test]
    fn loupe_decode_bound_passes_through_mid_value() {
        // A value inside the range passes through (longest edge, rounded).
        assert_eq!(loupe_decode_bound(1600.0, 900.0), 1600);
        assert_eq!(loupe_decode_bound(1280.4, 1000.0), 1280);
    }

    #[test]
    fn loupe_decode_bound_nonfinite_or_zero_is_min() {
        // Non-finite / zero / negative input clamps to the minimum, never panics.
        assert_eq!(loupe_decode_bound(f32::NAN, f32::NAN), 800);
        assert_eq!(loupe_decode_bound(f32::INFINITY, 100.0), 800);
        assert_eq!(loupe_decode_bound(0.0, 0.0), 800);
        assert_eq!(loupe_decode_bound(-100.0, -50.0), 800);
    }

    #[test]
    fn can_open_loupe_true_for_image_and_raw() {
        assert!(can_open_loupe(&EntryKind::Image));
        assert!(can_open_loupe(&EntryKind::Raw));
    }

    #[test]
    fn can_open_loupe_false_for_dir_and_other() {
        assert!(!can_open_loupe(&EntryKind::Dir));
        assert!(!can_open_loupe(&EntryKind::Other(FileCategory::Video)));
        assert!(!can_open_loupe(&EntryKind::Other(FileCategory::Document)));
    }

    // ── Loupe-eligible index tests ───────────────────────────────────────────

    fn mk_entry(name: &str, kind: crate::scan::EntryKind) -> crate::scan::Entry {
        crate::scan::Entry {
            path: std::path::PathBuf::from(name),
            name: name.to_owned(),
            kind,
            modified: None,
            size: 0,
        }
    }

    #[test]
    fn loupe_eligible_indices_filters_images_raws_and_query() {
        use crate::scan::FileCategory;
        let entries = vec![
            mk_entry("folder", crate::scan::EntryKind::Dir),
            mk_entry("photo.jpg", crate::scan::EntryKind::Image),
            mk_entry(
                "doc.txt",
                crate::scan::EntryKind::Other(FileCategory::Document),
            ),
            mk_entry("raw.CR2", crate::scan::EntryKind::Raw),
            mk_entry(
                "notes.md",
                crate::scan::EntryKind::Other(FileCategory::Code),
            ),
            mk_entry("vacation.png", crate::scan::EntryKind::Image),
        ];

        // No filter: only the two images + one raw, preserving order.
        assert_eq!(loupe_eligible_indices(&entries, ""), vec![1, 3, 5]);

        // Filter narrows to matching names only.
        assert_eq!(loupe_eligible_indices(&entries, "vac"), vec![5]);
        assert_eq!(loupe_eligible_indices(&entries, "RAW"), vec![3]); // case-insensitive inside filter_matches
    }

    // ── Full-resolution loupe tests ──────────────────────────────────────────

    #[test]
    fn high_res_loupe_decode_bound_caps_at_8192() {
        // The separate high-res path caps decode long-edge at 8192 (VRAM ceiling for native 1:1).
        assert_eq!(high_res_loupe_decode_bound(), 8192);
    }

    #[test]
    fn high_res_loupe_mode_selection_for_cache() {
        // high_res_loupe_mode (in inspection) produces distinct variants so high-res
        // co-resides in preview_cache with the bounded decode for same path+demosaic policy.
        use crate::inspection::{high_res_loupe_mode, LoupeDecodeMode};
        use crate::scan::EntryKind;
        assert_eq!(
            high_res_loupe_mode(false, &EntryKind::Image),
            LoupeDecodeMode::HighRes
        );
        assert_eq!(
            high_res_loupe_mode(true, &EntryKind::Raw),
            LoupeDecodeMode::HighResFullRaw
        );
        assert_eq!(
            high_res_loupe_mode(false, &EntryKind::Raw),
            LoupeDecodeMode::HighRes
        );
    }

    // ── Pan-offset math for drag-to-pan in zoomed loupe ───────────────────────
    // Tests the exact formula used by LoupePanMove: target = start + (start_cursor - cursor).
    // Sign chosen for natural grab (mouse right => content shifts right => offsets decrease).
    // The test covers the drag state machine's pure offset calculation.
    #[test]
    fn loupe_pan_offset_math_grab_direction() {
        use cosmic::iced::widget::scrollable::AbsoluteOffset;
        use cosmic::iced::Point;

        // The pure math extracted from the drag state machine (no clamping here;
        // scrollable::scroll_to + internal state clamp the result to valid range).
        fn pan_target(
            start_offset: AbsoluteOffset,
            start_cursor: Point,
            cursor: Point,
        ) -> AbsoluteOffset {
            AbsoluteOffset {
                x: start_offset.x + (start_cursor.x - cursor.x),
                y: start_offset.y + (start_cursor.y - cursor.y),
            }
        }

        let start_off = AbsoluteOffset { x: 320.0, y: 180.0 };
        let start_c = Point::new(410.0, 275.0);

        // Drag mouse right+down (cursor increases): deltas negative => target offset decreases.
        // This shifts the *content* right/down under the viewport = natural "grab the photo" feel.
        let cur = Point::new(440.0, 295.0);
        let t = pan_target(start_off, start_c, cur);
        assert!((t.x - 290.0).abs() < f32::EPSILON);
        assert!((t.y - 160.0).abs() < f32::EPSILON);

        // Drag mouse left+up (cursor decreases): offsets increase => content shifts left/up.
        let cur2 = Point::new(380.0, 250.0);
        let t2 = pan_target(start_off, start_c, cur2);
        assert!((t2.x - 350.0).abs() < f32::EPSILON);
        assert!((t2.y - 205.0).abs() < f32::EPSILON);

        // Zero delta leaves offset unchanged.
        let t3 = pan_target(start_off, start_c, start_c);
        assert!((t3.x - start_off.x).abs() < f32::EPSILON);
        assert!((t3.y - start_off.y).abs() < f32::EPSILON);
    }

    // ── focal_zoom_offset tests (keyboard +/- recenter in Actual loupe) ─────────

    #[test]
    fn focal_zoom_offset_zoom_in_grows_offset_to_keep_center_fixed() {
        // Centred point: focal at viewport/2, content centre under it.
        // Zoom in doubles the scale (old=1, new=2): offset should grow by content_half.
        let old_off = 100.0;
        let focal = 200.0; // viewport centre for 400-wide view
                           // At 2x, to keep 300 under focal=200: new_off = 300*2 - 200 = 400
        let new_off = focal_zoom_offset(old_off, focal, 1.0, 2.0, 1000.0);
        assert!((new_off - 400.0).abs() < f32::EPSILON);
    }

    #[test]
    fn focal_zoom_offset_zoom_out_shrinks_offset() {
        let old_off = 400.0;
        let focal = 200.0;
        // old=2, new=1: content=600; scaled = 600*(1/2)-200 = 100
        let new_off = focal_zoom_offset(old_off, focal, 2.0, 1.0, 1000.0);
        assert!((new_off - 100.0).abs() < f32::EPSILON);
    }

    #[test]
    fn focal_zoom_offset_old_le_zero_returns_clamped_old() {
        // old zoom factor <=0: return clamped old_off (per spec)
        assert_eq!(focal_zoom_offset(50.0, 100.0, 0.0, 2.0, 200.0), 50.0);
        assert_eq!(focal_zoom_offset(-5.0, 10.0, -1.0, 2.0, 100.0), 0.0);
        // old_off > max_off with valid zoom: result clamped to max_off
        assert_eq!(focal_zoom_offset(300.0, 100.0, 1.0, 2.0, 50.0), 50.0);
    }

    #[test]
    fn focal_zoom_offset_never_negative_and_clamped_to_max_off() {
        // Large focal with small old_off can produce negative before clamp; must clamp >=0
        let r = focal_zoom_offset(0.0, 10.0, 1.0, 0.25, 5.0);
        assert!(r >= 0.0);
        assert!(r <= 5.0);
        // Must never exceed max_off
        let r2 = focal_zoom_offset(0.0, 1000.0, 1.0, 8.0, 10.0);
        assert!(r2 <= 10.0);
        assert!(r2 >= 0.0);
    }

    #[test]
    fn focal_zoom_offset_doubling_with_focal_viewport_half_moves_by_content_half() {
        // Explicit case: viewport=400, focal=200, old_off=0 (image top-left at origin)
        // content under focal at old=1 is 200.
        // new=2: scaled = 200*2 - 200 = 200. offset moves from 0 to 200.
        let off = focal_zoom_offset(0.0, 200.0, 1.0, 2.0, 10000.0);
        assert!((off - 200.0).abs() < f32::EPSILON);
    }
}
