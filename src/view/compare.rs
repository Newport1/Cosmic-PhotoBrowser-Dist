//! Compare — view-only 2-up side-by-side Fit view (CM-M1).
//!
//! Exactly two panes. Reuses the loupe decode machinery (decode_compare_task + load_image + thumbnail(bound))
//! and the same Fit rendering: `widget::image(handle).content_fit(Contain).width(Fill).height(Fill)`.
//! No writes, no zoom/pan/sync (deferred). Top bar shows the two filenames + Close.

use cosmic::iced::{Alignment, ContentFit, Length};
use cosmic::widget;

use crate::app::{AppModel, Message};

/// Full-window 2-up compare. Dark container like loupe; top bar with filenames + Close, then
/// equal-width row of two panes.
pub fn view(app: &AppModel) -> cosmic::Element<'_, Message> {
    let (name0, name1, p0, p1) = match &app.compare {
        Some(c) if c.panes.len() == 2 => {
            let n0 = c.panes[0]
                .path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("")
                .to_owned();
            let n1 = c.panes[1]
                .path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("")
                .to_owned();
            (n0, n1, &c.panes[0], &c.panes[1])
        }
        _ => return widget::text("").into(),
    };

    let top_bar = widget::row::with_children(vec![
        widget::container(widget::text(name0).size(14))
            .width(Length::Fill)
            .into(),
        widget::container(widget::text(name1).size(14))
            .width(Length::Fill)
            .into(),
        widget::button::custom(widget::text("✕ Close"))
            .on_press(Message::CloseCompare)
            .into(),
    ])
    .align_y(Alignment::Center)
    .padding(8)
    .spacing(8)
    .width(Length::Fill);

    let left = pane_content(p0);
    let right = pane_content(p1);

    // Thin visual separation between panes (a 1px vertical bar + spacing).
    let divider = widget::container(widget::text(""))
        .width(Length::Fixed(1.0))
        .height(Length::Fill)
        .style(|_| cosmic::iced::widget::container::Style {
            background: Some(cosmic::iced::Background::Color(
                cosmic::iced::Color::from_rgb(0.15, 0.15, 0.16),
            )),
            ..Default::default()
        });
    let panes = widget::row::with_children(vec![
        widget::container(left)
            .width(Length::Fill)
            .height(Length::Fill)
            .into(),
        divider.into(),
        widget::container(right)
            .width(Length::Fill)
            .height(Length::Fill)
            .into(),
    ])
    .spacing(4)
    .width(Length::Fill)
    .height(Length::Fill);

    let center = widget::container(panes)
        .width(Length::Fill)
        .height(Length::Fill)
        .align_x(Alignment::Center)
        .align_y(Alignment::Center);

    let body = widget::column::with_children(vec![top_bar.into(), center.into()])
        .width(Length::Fill)
        .height(Length::Fill);

    widget::container(body)
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
}

fn pane_content(p: &crate::inspection::ComparePane) -> cosmic::Element<'_, Message> {
    if let Some(handle) = &p.handle {
        let image = widget::image(handle.clone())
            .content_fit(ContentFit::Contain)
            .width(Length::Fill)
            .height(Length::Fill);
        widget::container(image)
            .width(Length::Fill)
            .height(Length::Fill)
            .into()
    } else if let Some(err) = &p.error {
        widget::text(format!("Failed to open image: {err}"))
            .size(14)
            .into()
    } else {
        widget::text("Loading…").size(14).into()
    }
}
