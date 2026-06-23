use cosmic::iced::{alignment, Alignment, Background, Border, Color, Length};
use cosmic::widget;

use crate::app::{format_modified, human_size, middle_ellipsis, AppModel, Message};
use crate::histogram::HISTOGRAM_BINS;
use crate::inspection::MetadataSection;
use crate::metadata::ExifSummary;
use crate::view::grid::PREVIEW_W;

const PANEL_PADDING: f32 = 9.0;
const PREVIEW_AREA_H: f32 = 224.0;
const PREVIEW_MARGIN: f32 = 10.0;
const SECTION_HEADER_H: f32 = 24.0;
const LABEL_W: f32 = 86.0;
const PATH_MAX_CHARS: usize = 96;

const HISTO_HEIGHT: f32 = 72.0;
const HISTO_FILL: Color = Color::from_rgb(0.72, 0.72, 0.75);
const HISTO_TRACK: Color = Color::from_rgb(0.10, 0.10, 0.11);

const PANEL_BG: Color = Color::from_rgb(0.15, 0.15, 0.16); // #252526
const PREVIEW_BG: Color = Color::from_rgb(0.12, 0.12, 0.13); // #1F1F22
const DIVIDER: Color = Color::from_rgb(0.10, 0.10, 0.11); // #1A1A1C
const PRIMARY_TEXT: Color = Color::from_rgb(0.88, 0.88, 0.88); // #E0E0E0
const MUTED_TEXT: Color = Color::from_rgb(0.60, 0.60, 0.60); // #999999
const HEADER_TEXT: Color = Color::from_rgb(0.74, 0.74, 0.74); // #BDBDBD

/// Right preview pane for the selected image.
pub fn view(app: &AppModel) -> cosmic::Element<'_, Message> {
    let Some(selected_index) = app.selection.selected_index else {
        return no_selection_view();
    };

    let Some(entry) = app.browser.entries.get(selected_index) else {
        return no_selection_view();
    };

    let mut content = widget::column::with_capacity(10).spacing(0);
    content = content.push(preview_section(app, selected_index, entry.name.clone()));
    content = content.push(section_divider());

    let file_rows = vec![
        ("Filename", entry.name.clone()),
        ("File size", human_size(entry.size)),
        ("Modified date", format_modified(entry.modified)),
        (
            "Path",
            middle_ellipsis(&entry.path.display().to_string(), PATH_MAX_CHARS),
        ),
        ("Format", image_format(&entry.path)),
    ];
    content = content.push(metadata_section(
        MetadataSection::FileProperties,
        "File Properties",
        app.metadata_sections
            .is_expanded(MetadataSection::FileProperties),
        file_rows,
    ));

    if let Some(dimensions) = app
        .preview
        .as_ref()
        .and_then(|preview| preview.dimensions)
        .map(|(w, h)| format!("{w} × {h}"))
    {
        content = content.push(metadata_section(
            MetadataSection::Dimensions,
            "Dimensions",
            app.metadata_sections
                .is_expanded(MetadataSection::Dimensions),
            vec![("Dimensions", dimensions)],
        ));
    }

    if let Some(metadata) = app
        .preview
        .as_ref()
        .and_then(|preview| preview.metadata.as_ref())
    {
        let camera_rows = camera_rows(metadata);
        if !camera_rows.is_empty() {
            content = content.push(metadata_section(
                MetadataSection::CameraExif,
                "Camera / EXIF",
                app.metadata_sections
                    .is_expanded(MetadataSection::CameraExif),
                camera_rows,
            ));
        }

        if let Some(captured_date) = metadata.captured_date.clone() {
            content = content.push(metadata_section(
                MetadataSection::CaptureDate,
                "Capture Date",
                app.metadata_sections
                    .is_expanded(MetadataSection::CaptureDate),
                vec![("Captured date", captured_date)],
            ));
        }
    }

    content = content.push(keywords_section(app, selected_index));

    if let Some(hist) = app
        .preview
        .as_ref()
        .and_then(|preview| preview.histogram.as_ref())
    {
        content = content.push(histogram_section(hist));
    }

    widget::container(widget::scrollable(content).height(Length::Fill))
        .width(Length::Fixed(PREVIEW_W))
        .height(Length::Fill)
        .padding(PANEL_PADDING)
        .style(panel_style)
        .into()
}

fn preview_section(
    app: &AppModel,
    selected_index: usize,
    filename: String,
) -> cosmic::Element<'static, Message> {
    let mut section = widget::column::with_capacity(5).spacing(6);
    section = section.push(styled_text("Preview", HEADER_TEXT, 12));

    let is_text = app
        .browser
        .entries
        .get(selected_index)
        .map(|e| crate::scan::is_text_previewable(&e.name))
        .unwrap_or(false);

    let preview_area_content: cosmic::Element<'static, Message> = if is_text {
        let txt_el: cosmic::Element<'static, Message> = if let Some((p, txt)) = &app.text_preview {
            let current_path = app.browser.entries.get(selected_index).map(|e| &e.path);
            if current_path == Some(p) {
                widget::text(txt.clone())
                    .size(10)
                    .font(cosmic::iced::Font::MONOSPACE)
                    .into()
            } else {
                widget::text("Loading text preview…")
                    .size(11)
                    .class(MUTED_TEXT)
                    .center()
                    .into()
            }
        } else {
            widget::text("Loading text preview…")
                .size(11)
                .class(MUTED_TEXT)
                .center()
                .into()
        };
        widget::scrollable(widget::container(txt_el).width(Length::Fill).padding(4))
            .height(Length::Fill)
            .width(Length::Fill)
            .into()
    } else {
        if let Some(preview) = &app.preview {
            if let Some(handle) = &preview.handle {
                widget::image(handle.clone())
                    .width(Length::Fill)
                    .height(Length::Fill)
                    .into()
            } else if let Some(err) = &preview.error {
                widget::text(format!("Preview failed: {err}"))
                    .size(11)
                    .class(MUTED_TEXT)
                    .center()
                    .into()
            } else {
                widget::text("Loading preview…")
                    .size(11)
                    .class(MUTED_TEXT)
                    .center()
                    .into()
            }
        } else {
            widget::text("Loading preview…")
                .size(11)
                .class(MUTED_TEXT)
                .center()
                .into()
        }
    };

    section = section.push(
        widget::container(preview_area_content)
            .width(Length::Fill)
            .height(Length::Fixed(PREVIEW_AREA_H))
            .padding(PREVIEW_MARGIN)
            .align_x(alignment::Horizontal::Center)
            .align_y(alignment::Vertical::Center)
            .style(preview_area_style),
    );
    section = section.push(
        widget::text(middle_ellipsis(&filename, 32))
            .size(11)
            .class(MUTED_TEXT)
            .width(Length::Fill)
            .center(),
    );

    if app.preview.is_some() {
        let loupe_label: cosmic::Element<'static, Message> = widget::text("⤢ View large")
            .size(11)
            .class(PRIMARY_TEXT)
            .center()
            .into();
        section = section.push(
            widget::button::custom(loupe_label)
                .on_press(Message::OpenLoupe(selected_index))
                .width(Length::Fill)
                .padding(0)
                .class(cosmic::theme::Button::Transparent),
        );
    }

    widget::container(section)
        .width(Length::Fill)
        .padding([0.0, 0.0, 8.0, 0.0])
        .into()
}

fn metadata_section(
    section: MetadataSection,
    title: &'static str,
    expanded: bool,
    rows: Vec<(&'static str, String)>,
) -> cosmic::Element<'static, Message> {
    let chevron = if expanded { "▾" } else { "▸" };
    let header_content: cosmic::Element<'static, Message> = widget::row::with_children(vec![
        styled_text(chevron, MUTED_TEXT, 11),
        styled_text(title, HEADER_TEXT, 12),
    ])
    .spacing(6)
    .align_y(Alignment::Center)
    .into();

    let header = widget::button::custom(header_content)
        .on_press(Message::ToggleMetadataSection(section))
        .width(Length::Fill)
        .height(Length::Fixed(SECTION_HEADER_H))
        .padding(0)
        .class(cosmic::theme::Button::Transparent);

    let mut group = widget::column::with_capacity(rows.len() + 2)
        .spacing(0)
        .push(header)
        .push(section_divider());

    if expanded {
        let mut body = widget::column::with_capacity(rows.len()).spacing(3);
        for (label, value) in rows {
            body = body.push(info_row(label, value));
        }
        group = group.push(widget::container(body).padding([6.0, 0.0, 8.0, 0.0]));
    }

    widget::container(group).width(Length::Fill).into()
}

fn info_row(label: &'static str, value: String) -> cosmic::Element<'static, Message> {
    widget::row::with_children(vec![
        widget::container(styled_text(label, MUTED_TEXT, 11))
            .width(Length::Fixed(LABEL_W))
            .into(),
        styled_text(value, PRIMARY_TEXT, 11),
    ])
    .spacing(8)
    .align_y(Alignment::Center)
    .into()
}

fn camera_rows(metadata: &ExifSummary) -> Vec<(&'static str, String)> {
    [
        ("Camera", metadata.camera.as_ref()),
        ("Lens", metadata.lens.as_ref()),
        ("ISO", metadata.iso.as_ref()),
        ("Shutter", metadata.shutter.as_ref()),
        ("Aperture", metadata.aperture.as_ref()),
        ("Focal length", metadata.focal_length.as_ref()),
        ("Color profile", metadata.color_profile.as_ref()),
        ("Orientation", metadata.orientation.as_ref()),
    ]
    .into_iter()
    .filter_map(|(label, value)| value.map(|value| (label, value.clone())))
    .collect()
}

fn no_selection_view() -> cosmic::Element<'static, Message> {
    let message = widget::column::with_children(vec![
        styled_text("No selection", PRIMARY_TEXT, 16),
        styled_text("Select a photo to preview image details.", MUTED_TEXT, 12),
    ])
    .spacing(6)
    .align_x(Alignment::Center);

    widget::container(message)
        .width(Length::Fixed(PREVIEW_W))
        .height(Length::Fill)
        .align_x(Alignment::Center)
        .align_y(Alignment::Center)
        .padding(PANEL_PADDING)
        .style(panel_style)
        .into()
}

fn styled_text(
    label: impl Into<String>,
    color: Color,
    size: u16,
) -> cosmic::Element<'static, Message> {
    widget::text(label.into()).size(size).class(color).into()
}

fn section_divider() -> cosmic::Element<'static, Message> {
    widget::container(widget::text(""))
        .width(Length::Fill)
        .height(Length::Fixed(1.0))
        .style(|_| cosmic::iced::widget::container::Style {
            background: Some(Background::Color(DIVIDER)),
            ..Default::default()
        })
        .into()
}

fn panel_style(_theme: &cosmic::Theme) -> cosmic::iced::widget::container::Style {
    cosmic::iced::widget::container::Style {
        background: Some(Background::Color(PANEL_BG)),
        border: Border {
            width: 0.0,
            radius: 0.0.into(),
            color: DIVIDER,
        },
        ..Default::default()
    }
}

fn preview_area_style(_theme: &cosmic::Theme) -> cosmic::iced::widget::container::Style {
    cosmic::iced::widget::container::Style {
        background: Some(Background::Color(PREVIEW_BG)),
        border: Border {
            width: 0.0,
            radius: 0.0.into(),
            color: DIVIDER,
        },
        ..Default::default()
    }
}

pub(crate) fn image_format(path: &std::path::Path) -> String {
    path.extension()
        .and_then(|ext| ext.to_str())
        .map(str::to_uppercase)
        .unwrap_or_else(|| "unknown".to_owned())
}

pub(crate) fn histogram_section(hist: &[u32; HISTOGRAM_BINS]) -> cosmic::Element<'static, Message> {
    let label_row: cosmic::Element<'static, Message> = widget::row::with_children(vec![
        styled_text("▾", MUTED_TEXT, 11),
        styled_text("Histogram", HEADER_TEXT, 12),
    ])
    .spacing(6)
    .align_y(Alignment::Center)
    .into();

    let max = hist.iter().copied().max().unwrap_or(0);

    let mut bar_slots: Vec<cosmic::Element<'static, Message>> = Vec::with_capacity(HISTOGRAM_BINS);
    for &count in hist {
        let bar_h = if max == 0 || count == 0 {
            0.0
        } else {
            let f = (count as f32 / max as f32).sqrt();
            (f * HISTO_HEIGHT).clamp(1.0, HISTO_HEIGHT)
        };
        let bar = widget::container(widget::Space::new())
            .width(Length::Fill)
            .height(Length::Fixed(bar_h))
            .style(|_| cosmic::iced::widget::container::Style {
                background: Some(Background::Color(HISTO_FILL)),
                ..Default::default()
            });
        let col = widget::column::with_children(vec![
            widget::Space::new()
                .height(Length::Fixed(HISTO_HEIGHT - bar_h))
                .into(),
            bar.into(),
        ])
        .width(Length::FillPortion(1))
        .height(Length::Fixed(HISTO_HEIGHT));
        bar_slots.push(col.into());
    }

    let bars_row = widget::row::with_children(bar_slots)
        .spacing(0)
        .width(Length::Fill)
        .height(Length::Fixed(HISTO_HEIGHT));

    let bars = widget::container(bars_row)
        .width(Length::Fill)
        .height(Length::Fixed(HISTO_HEIGHT))
        .style(|_| cosmic::iced::widget::container::Style {
            background: Some(Background::Color(HISTO_TRACK)),
            ..Default::default()
        });

    let group = widget::column::with_capacity(3)
        .spacing(0)
        .push(label_row)
        .push(section_divider())
        .push(widget::container(bars).padding([4.0, 0.0, 6.0, 0.0]));

    widget::container(group).width(Length::Fill).into()
}

fn keywords_section<'a>(app: &'a AppModel, selected_index: usize) -> cosmic::Element<'a, Message> {
    let kws: &[String] = app
        .folder_tags
        .get(&selected_index)
        .map(|v| v.as_slice())
        .unwrap_or(&[]);

    let mut body = widget::column::with_capacity(kws.len() + 2).spacing(4);
    body = body.push(styled_text("Keywords", HEADER_TEXT, 12));
    body = body.push(section_divider());

    if app.selection.multi_selected.len() > 1 {
        let n = app.selection.multi_selected.len();
        body = body.push(styled_text(
            format!("Remove applies to {n} selected"),
            MUTED_TEXT,
            11,
        ));
    }

    for kw in kws {
        let row = widget::row::with_children(vec![
            widget::text(kw.clone()).size(12).width(Length::Fill).into(),
            // Glyph-safe: a plain text label button (COSMIC font lacks ✕). Mirror existing button style.
            widget::button::text("Remove")
                .on_press(Message::RemoveKeyword(kw.clone()))
                .into(),
        ])
        .align_y(Alignment::Center)
        .spacing(8);
        body = body.push(row);
    }

    // Add box — mirror the filename filter text_input (grid.rs ~76): on_input + on_submit.
    let target_n = if !app.selection.multi_selected.is_empty() {
        app.selection.multi_selected.len()
    } else {
        1
    };
    let ph = if target_n > 1 {
        format!("Add keyword to {target_n} photos…")
    } else {
        "Add keyword…".to_string()
    };
    let value = &app.keyword_input;
    let add = widget::text_input(ph, value)
        .on_input(Message::KeywordInputChanged)
        .on_submit(|_| Message::AddKeyword);
    body = body.push(add);

    widget::container(body).padding([6.0, 0.0, 8.0, 0.0]).into()
}

#[cfg(test)]
mod tests {
    use super::image_format;
    use std::path::Path;

    #[test]
    fn image_format_uppercases_extension() {
        assert_eq!(image_format(Path::new("photo.nef")), "NEF");
        assert_eq!(image_format(Path::new("photo.JpEg")), "JPEG");
    }

    #[test]
    fn image_format_without_extension_is_unknown() {
        assert_eq!(image_format(Path::new("photo")), "unknown");
    }
}
