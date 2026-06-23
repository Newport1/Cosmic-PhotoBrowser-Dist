use cosmic::iced::{alignment, Background, Border, Color, Length};
use cosmic::widget;
use std::path::PathBuf;

use crate::app::{ellipsize_path, AppModel, Message};

const CURRENT_PATH_MAX_CHARS: usize = 28;
const SIDEBAR_W: f32 = 260.0;
const PANEL_PADDING: f32 = 9.0;
const SECTION_GAP: u16 = 5;
const ROW_H: f32 = 24.0;
const TREE_INDENT: f32 = 14.0;
const TOGGLE_W: f32 = 18.0;

const PANEL_BG: Color = Color::from_rgb(0.15, 0.15, 0.16); // #252526
const DIVIDER: Color = Color::from_rgb(0.10, 0.10, 0.11); // #1A1A1C
const PRIMARY_TEXT: Color = Color::from_rgb(0.88, 0.88, 0.88); // #E0E0E0
const SELECTED_TEXT: Color = Color::from_rgb(0.98, 0.98, 0.98); // #FAFAFA
const MUTED_TEXT: Color = Color::from_rgb(0.60, 0.60, 0.60); // #999999
const HEADER_TEXT: Color = Color::from_rgb(0.74, 0.74, 0.74); // #BDBDBD
const ACCENT_BLUE: Color = Color::from_rgb(0.04, 0.52, 1.0); // #0A84FF

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DriveEntry {
    pub label: String,
    pub mount_point: PathBuf,
}

pub(crate) fn read_drives() -> Vec<DriveEntry> {
    std::fs::read_to_string("/proc/self/mounts")
        .or_else(|_| std::fs::read_to_string("/proc/mounts"))
        .map(|contents| parse_mounts(&contents))
        .unwrap_or_default()
}

pub(crate) fn parse_mounts(contents: &str) -> Vec<DriveEntry> {
    let mut drives = Vec::new();
    for line in contents.lines() {
        let mut fields = line.split_whitespace();
        let Some(device) = fields.next().map(unescape_mount_field) else {
            continue;
        };
        let Some(mount_point_text) = fields.next().map(unescape_mount_field) else {
            continue;
        };
        let Some(fs_type) = fields.next() else {
            continue;
        };

        if !is_user_relevant_mount(&device, &mount_point_text, fs_type) {
            continue;
        }

        let mount_point = PathBuf::from(&mount_point_text);
        if drives
            .iter()
            .any(|drive: &DriveEntry| drive.mount_point == mount_point)
        {
            continue;
        }

        let label = mount_point
            .file_name()
            .and_then(|name| name.to_str())
            .filter(|name| !name.is_empty())
            .map(ToOwned::to_owned)
            .or_else(|| {
                PathBuf::from(&device)
                    .file_name()
                    .and_then(|name| name.to_str())
                    .map(ToOwned::to_owned)
            })
            .unwrap_or_else(|| mount_point_text.clone());
        drives.push(DriveEntry { label, mount_point });
    }
    drives
}

fn is_user_relevant_mount(device: &str, mount_point: &str, fs_type: &str) -> bool {
    const EXCLUDED_FS: &[&str] = &[
        "proc",
        "sysfs",
        "tmpfs",
        "devtmpfs",
        "devpts",
        "cgroup",
        "cgroup2",
        "overlay",
        "squashfs",
        "autofs",
        "mqueue",
        "debugfs",
        "tracefs",
        "fusectl",
        "configfs",
        "securityfs",
        "ramfs",
        "binfmt_misc",
    ];
    if EXCLUDED_FS.contains(&fs_type) || device.starts_with("/dev/loop") || device.contains(':') {
        return false;
    }

    is_real_block_device(device) || is_removable_media_mount(mount_point)
}

fn is_real_block_device(device: &str) -> bool {
    device.starts_with("/dev/sd")
        || device.starts_with("/dev/nvme")
        || device.starts_with("/dev/mmcblk")
}

fn is_removable_media_mount(mount_point: &str) -> bool {
    mount_point.starts_with("/run/media/")
        || mount_point.starts_with("/media/")
        || mount_point.starts_with("/mnt/")
}

fn unescape_mount_field(field: &str) -> String {
    field
        .replace("\\040", " ")
        .replace("\\011", "\t")
        .replace("\\012", "\n")
        .replace("\\134", "\\")
}

/// Sidebar with real XDG places, user favorites, and a path bar showing the current folder.
pub fn view(app: &AppModel) -> cosmic::Element<'_, Message> {
    let mut col =
        widget::column::with_capacity(app.nav.places.len() + app.config.favorites.len() + 8)
            .push(section_header_with_add())
            .spacing(SECTION_GAP);

    if app.config.favorites.is_empty() {
        col = col.push(empty_row("No favorites yet"));
    } else {
        for path in &app.config.favorites {
            let label = path
                .file_name()
                .and_then(|name| name.to_str())
                .map(ToOwned::to_owned)
                .unwrap_or_else(|| path.display().to_string());
            let short = ellipsize_path(&label, CURRENT_PATH_MAX_CHARS);
            let selected = app.browser.current_dir.as_deref() == Some(path.as_path());
            col = col.push(favorite_row(short, path.clone(), selected));
        }
    }

    col = col.push(section_divider()).push(section_header("Places"));

    for (label, path) in &app.nav.places {
        let selected = app.browser.current_dir.as_deref() == Some(path.as_path());
        col = col.push(nav_row(label, path.clone(), selected));
    }

    col = col.push(section_divider()).push(section_header("Drives"));

    if app.nav.drives.is_empty() {
        col = col.push(empty_row("No mounted drives"));
    } else {
        for drive in &app.nav.drives {
            let label = ellipsize_path(&drive.label, CURRENT_PATH_MAX_CHARS);
            let selected = app.browser.current_dir.as_deref() == Some(drive.mount_point.as_path());
            col = col.push(nav_row(label, drive.mount_point.clone(), selected));
        }
    }

    col = col
        .push(section_divider())
        .push(folders_header(app.config.tree_show_files));

    if app.nav.tree.roots.is_empty() {
        col = col.push(empty_row("No folder roots"));
    } else {
        for (path, depth) in app.nav.tree.visible_nodes() {
            col = col.push(tree_row(app, path, depth));
        }
    }

    // Current path bar.
    let path_label = app
        .browser
        .current_dir
        .as_deref()
        .map(|p| ellipsize_path(&p.display().to_string(), CURRENT_PATH_MAX_CHARS))
        .unwrap_or_else(|| "—".to_owned());

    col = col
        .push(section_divider())
        .push(section_header("Current folder"))
        .push(empty_row(path_label));

    widget::container(widget::scrollable(col).height(Length::Fill))
        .width(Length::Fixed(SIDEBAR_W))
        .height(Length::Fill)
        .padding(PANEL_PADDING)
        .style(panel_style)
        .into()
}

fn tree_row(app: &AppModel, path: PathBuf, depth: usize) -> cosmic::Element<'_, Message> {
    let is_expanded = app.nav.tree.is_expanded(&path);
    let has_loaded_children = app.nav.tree.has_loaded_children(&path);
    let is_file = app.nav.tree.is_file_leaf(&path);
    let can_toggle =
        !is_file && (depth == 0 || has_loaded_children || !app.nav.tree.has_loaded_node(&path));
    let toggle_label = if is_expanded { "▾" } else { "▸" };
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| path.to_str().unwrap_or("/"));
    let current = app.browser.current_dir.as_deref() == Some(path.as_path());
    let label = ellipsize_path(name, CURRENT_PATH_MAX_CHARS);

    let mut items: Vec<cosmic::Element<'_, Message>> = Vec::with_capacity(3);
    items.push(
        widget::container(widget::text(""))
            .width(Length::Fixed(depth as f32 * TREE_INDENT))
            .height(Length::Fixed(ROW_H))
            .into(),
    );
    if can_toggle {
        let toggle_content: cosmic::Element<'static, Message> =
            styled_text(toggle_label, MUTED_TEXT, 11);
        items.push(
            widget::button::custom(toggle_content)
                .on_press(Message::ToggleTreeNode(path.clone()))
                .width(Length::Fixed(TOGGLE_W))
                .height(Length::Fixed(ROW_H))
                .padding(0)
                .class(cosmic::theme::Button::Transparent)
                .into(),
        );
    } else {
        items.push(
            widget::container(widget::text(""))
                .width(Length::Fixed(TOGGLE_W))
                .height(Length::Fixed(ROW_H))
                .into(),
        );
    }
    let icon = if is_file { "📄" } else { "📁" };
    let nav_target = if is_file {
        path.parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| path.clone())
    } else {
        path.clone()
    };
    let node_button_content: cosmic::Element<'_, Message> = widget::row::with_children(vec![
        styled_text(icon, if current { PRIMARY_TEXT } else { MUTED_TEXT }, 12),
        styled_text(
            label,
            if current { SELECTED_TEXT } else { PRIMARY_TEXT },
            12,
        ),
    ])
    .spacing(5)
    .align_y(alignment::Vertical::Center)
    .into();
    items.push(
        widget::button::custom(node_button_content)
            .on_press(Message::NavigateTo(nav_target))
            .width(Length::Fill)
            .height(Length::Fixed(ROW_H))
            .padding(0)
            .class(cosmic::theme::Button::Transparent)
            .into(),
    );

    widget::container(
        widget::row::with_children(items)
            .spacing(0)
            .align_y(alignment::Vertical::Center),
    )
    .width(Length::Fill)
    .height(Length::Fixed(ROW_H))
    .style(move |_| row_style(current))
    .into()
}

fn section_header(label: &'static str) -> cosmic::Element<'static, Message> {
    widget::container(styled_text(label, HEADER_TEXT, 12))
        .width(Length::Fill)
        .height(Length::Fixed(ROW_H))
        .align_y(alignment::Vertical::Center)
        .into()
}

fn section_header_with_add() -> cosmic::Element<'static, Message> {
    widget::row::with_children(vec![
        widget::container(styled_text("Favorites", HEADER_TEXT, 12))
            .width(Length::Fill)
            .height(Length::Fixed(ROW_H))
            .align_y(alignment::Vertical::Center)
            .into(),
        widget::button::text("+")
            .on_press(Message::AddFavorite)
            .width(Length::Fixed(ROW_H))
            .height(Length::Fixed(ROW_H))
            .padding(0)
            .into(),
    ])
    .spacing(4)
    .align_y(alignment::Vertical::Center)
    .into()
}

fn folders_header(show_files: bool) -> cosmic::Element<'static, Message> {
    // Small toggle affordance in the Folders sidebar section header.
    // 📄 in muted when off (dirs-only, preserves pre-v0.7 behavior), accent when on (files shown as leaves).
    // Click flips config.tree_show_files (persisted), resets tree expansion so children reload under the new policy.
    let toggle_content: cosmic::Element<'static, Message> =
        styled_text("📄", if show_files { ACCENT_BLUE } else { MUTED_TEXT }, 11);
    widget::row::with_children(vec![
        widget::container(styled_text("Folders", HEADER_TEXT, 12))
            .width(Length::Fill)
            .height(Length::Fixed(ROW_H))
            .align_y(alignment::Vertical::Center)
            .into(),
        widget::button::custom(toggle_content)
            .on_press(Message::ToggleTreeShowFiles)
            .width(Length::Fixed(ROW_H))
            .height(Length::Fixed(ROW_H))
            .padding(0)
            .class(cosmic::theme::Button::Transparent)
            .into(),
    ])
    .spacing(4)
    .align_y(alignment::Vertical::Center)
    .into()
}

fn favorite_row(label: String, path: PathBuf, selected: bool) -> cosmic::Element<'static, Message> {
    let star = crate::view::grid::star_icon(true, 12);
    let label_el = widget::text(label).size(12).class(if selected {
        SELECTED_TEXT
    } else {
        PRIMARY_TEXT
    });
    let nav_btn = widget::button::custom(
        widget::container(
            widget::row::with_children(vec![star.into(), label_el.into()])
                .spacing(4)
                .align_y(alignment::Vertical::Center),
        )
        .width(Length::Fill)
        .height(Length::Fixed(ROW_H))
        .align_y(alignment::Vertical::Center),
    )
    .on_press(Message::NavigateTo(path.clone()))
    .width(Length::Fill)
    .height(Length::Fixed(ROW_H))
    .padding(0)
    .class(cosmic::theme::Button::Transparent);
    widget::container(
        widget::row::with_children(vec![
            nav_btn.into(),
            widget::button::text("×")
                .on_press(Message::RemoveFavorite(path))
                .width(Length::Fixed(ROW_H))
                .height(Length::Fixed(ROW_H))
                .padding(0)
                .into(),
        ])
        .spacing(2),
    )
    .width(Length::Fill)
    .height(Length::Fixed(ROW_H))
    .style(move |_| row_style(selected))
    .into()
}

fn nav_row(
    label: impl Into<String>,
    msg_path: PathBuf,
    selected: bool,
) -> cosmic::Element<'static, Message> {
    widget::container(row_button(
        label.into(),
        Message::NavigateTo(msg_path),
        selected,
    ))
    .width(Length::Fill)
    .height(Length::Fixed(ROW_H))
    .style(move |_| row_style(selected))
    .into()
}

fn empty_row(label: impl Into<String>) -> cosmic::Element<'static, Message> {
    widget::container(styled_text(label.into(), MUTED_TEXT, 12))
        .width(Length::Fill)
        .height(Length::Fixed(ROW_H))
        .align_y(alignment::Vertical::Center)
        .into()
}

fn row_button(
    label: String,
    message: Message,
    selected: bool,
) -> cosmic::widget::Button<'static, Message> {
    let content: cosmic::Element<'static, Message> = widget::container(styled_text(
        label,
        if selected {
            SELECTED_TEXT
        } else {
            PRIMARY_TEXT
        },
        12,
    ))
    .width(Length::Fill)
    .height(Length::Fixed(ROW_H))
    .align_y(alignment::Vertical::Center)
    .into();

    widget::button::custom(content)
        .on_press(message)
        .width(Length::Fill)
        .height(Length::Fixed(ROW_H))
        .padding(0)
        .class(cosmic::theme::Button::Transparent)
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

fn row_style(selected: bool) -> cosmic::iced::widget::container::Style {
    cosmic::iced::widget::container::Style {
        background: selected.then_some(Background::Color(ACCENT_BLUE)),
        border: Border {
            width: 0.0,
            radius: 0.0.into(),
            color: ACCENT_BLUE,
        },
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use super::parse_mounts;
    use std::path::PathBuf;

    #[test]
    fn parse_mounts_filters_to_user_relevant_drives_and_dedups() {
        let mounts = r#"proc /proc proc rw,nosuid,nodev,noexec,relatime 0 0
/dev/nvme0n1p2 / ext4 rw,relatime 0 0
tmpfs /run tmpfs rw,nosuid,nodev 0 0
/dev/sdb1 /run/media/alex/PHOTO\040DRIVE exfat rw,nosuid,nodev 0 0
/dev/sdc1 /media/Backup ext4 rw,relatime 0 0
/dev/sdd1 /mnt/card vfat rw,relatime 0 0
/dev/loop0 /snap/core squashfs ro,nodev 0 0
server:/share /mnt/network nfs rw,relatime 0 0
/dev/sdb1 /run/media/alex/PHOTO\040DRIVE exfat rw,nosuid,nodev 0 0
"#;

        let drives = parse_mounts(mounts);
        let mount_points: Vec<PathBuf> = drives
            .iter()
            .map(|drive| drive.mount_point.clone())
            .collect();
        let labels: Vec<&str> = drives.iter().map(|drive| drive.label.as_str()).collect();

        assert_eq!(
            mount_points,
            vec![
                PathBuf::from("/"),
                PathBuf::from("/run/media/alex/PHOTO DRIVE"),
                PathBuf::from("/media/Backup"),
                PathBuf::from("/mnt/card"),
            ]
        );
        assert_eq!(labels, vec!["nvme0n1p2", "PHOTO DRIVE", "Backup", "card"]);
    }
}
