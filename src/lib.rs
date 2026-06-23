//! PhotoBrowser — a libcosmic photo browser.
//!
//! The crate is split into a reusable core library and a thin binary entry point.
//! Core modules contain photo browsing, decoding, metadata, catalog, culling,
//! duplicate detection, export, and XMP behavior. UI modules contain COSMIC
//! application state, tasks, and views.

// Core modules.
pub mod catalog;
pub mod config;
pub mod cull;
pub mod decode;
pub mod decoded_image;
pub mod dedupe;
pub mod develop;
pub mod export;
pub mod folder_tree;
pub mod histogram;
pub mod metadata;
pub mod nav;
pub mod scan;
pub mod xmp;

// UI and toolkit integration.
mod app;
mod browser_state;
mod cosmic_adapter;
mod inspection;
mod preview_cache;
mod tasks;
mod thumb;
mod view;

use config::Config;
use tracing_subscriber::{filter::Targets, prelude::*};

/// Launch the PhotoBrowser COSMIC application.
pub fn run() -> cosmic::iced::Result {
    let targets = std::env::var("RUST_LOG")
        .ok()
        .and_then(|value| value.parse::<Targets>().ok())
        .unwrap_or_else(|| Targets::new().with_default(tracing::Level::INFO))
        .with_target("rawler", tracing::Level::ERROR);

    tracing_subscriber::registry()
        .with(tracing_subscriber::fmt::layer())
        .with(targets)
        .init();

    let cfg = Config::default();
    // Log startup config; also ensures SortMode variants are reachable.
    let _sort_label = cfg.sort_mode.label();
    let _date_label = config::SortMode::Date.label();
    let _captured_label = config::SortMode::Captured.label();
    if let Some(dirs) = Config::project_dirs() {
        tracing::debug!(
            thumb_size = cfg.thumb_size,
            cache_max = cfg.thumb_cache_max_items,
            sort_mode = _sort_label,
            data_dir = %dirs.data_dir().display(),
            "photobrowser starting"
        );
    }

    // CLI: optional path arg for startup dir (dir or file's parent). Resolved
    // via the pure+thin-fs helper so bad paths fall back silently to default.
    let cli_arg = std::env::args().nth(1);
    let flags = app::initial_dir_from_arg(cli_arg.as_deref());

    // Open at a usable default size rather than a tiny window — the small default
    // is what made the toolbar feel cramped. A size_limits backstop keeps the top
    // toolbar (filter + sort + gear etc.) from clipping its end buttons on
    // user-resized narrow windows. Users can still resize / maximize.
    let settings = cosmic::app::Settings::default()
        .size(cosmic::iced::Size::new(1280.0, 800.0))
        .size_limits(
            cosmic::iced::core::layout::Limits::NONE
                .min_width(920.0)
                .min_height(620.0),
        );
    cosmic::app::run::<app::AppModel>(settings, flags)
}
