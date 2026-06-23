use std::path::{Path, PathBuf};

use directories::ProjectDirs;
use serde::{Deserialize, Serialize};

/// Sort mode for the photo browser.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SortMode {
    Name,
    Date,
    Captured,
    Rating,
}

/// Application configuration.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Config {
    /// Thumbnail size in pixels (width and height).
    pub thumb_size: u16,
    /// Maximum number of items in the thumbnail LRU cache.
    pub thumb_cache_max_items: usize,
    /// Sort mode for the photo list.
    pub sort_mode: SortMode,
    /// Maximum thumbnail disk cache size in GB; 0.0 means unlimited.
    pub cache_max_gb: f64,
    /// Optional thumbnail cache directory override.
    pub cache_dir: Option<PathBuf>,
    /// User-pinned sidebar favorites.
    #[serde(default)]
    pub favorites: Vec<PathBuf>,
    /// Saved filter sets (smart collections). Config-only; never writes originals.
    #[serde(default)]
    pub collections: Vec<SavedCollection>,
    /// Whether dotfiles and dot-directories are included in folder scans.
    #[serde(default)]
    pub show_hidden: bool,
    /// Whether RAW files use a full demosaic for the loupe view.
    #[serde(default)]
    pub loupe_full_demosaic: bool,
    /// Whether the sidebar folder tree shows files (leaves) in addition to subfolders. Default OFF.
    #[serde(default)]
    pub tree_show_files: bool,
    /// Whether the grid shows only image/RAW files (folders remain visible for navigation). Default OFF.
    #[serde(default)]
    pub images_only: bool,
    /// When true, Actual (1:1) loupe zoom for the current image asynchronously re-decodes
    /// at native resolution capped at 8192 px on the long edge (VRAM). Default OFF (uses the
    /// existing ≤2560 display-bounded decode for 100% view). Stored under distinct cache mode.
    #[serde(default)]
    pub full_res_loupe: bool,
    /// Whether the loupe shows the bottom filmstrip of sibling images. Default ON.
    /// Uses explicit default_true (not bare #[serde(default)]) so legacy JSON without the key
    /// continues to show the filmstrip.
    #[serde(default = "default_true")]
    pub show_filmstrip: bool,
    /// Optional export directory for "File > Export selection…". When None, a default under
    /// XDG Pictures (or home) is used at runtime. Never modifies originals.
    #[serde(default)]
    pub export_dir: Option<PathBuf>,
    /// Whether the loupe applies develop-look (via apply_develop on the bounded decoded image).
    /// When on, reads sidecar XMP (read-only) and applies develop::apply_develop. Default OFF.
    #[serde(default)]
    pub develop_look: bool,
}

fn default_true() -> bool {
    true
}

impl Default for Config {
    fn default() -> Self {
        Self {
            thumb_size: 160,
            thumb_cache_max_items: 600,
            sort_mode: SortMode::Name,
            cache_max_gb: 2.0,
            cache_dir: None,
            favorites: Vec::new(),
            collections: Vec::new(),
            show_hidden: false,
            loupe_full_demosaic: false,
            tree_show_files: false,
            images_only: false,
            full_res_loupe: false,
            show_filmstrip: true,
            export_dir: None,
            develop_look: false,
        }
    }
}

/// A saved filter set ("smart collection"). All fields serde-trivial (label stored as its color name).
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SavedCollection {
    pub name: String,
    pub rating_min: Option<u8>,
    pub label: Option<String>, // ColorLabel name (Red/Yellow/…); convert via ColorLabel::from_str on apply
    pub camera: Option<String>,
    pub date_year: Option<i32>,
    pub tag: Option<String>,
}

impl SortMode {
    /// Returns the display label for this sort mode.
    pub fn label(&self) -> &'static str {
        match self {
            SortMode::Name => "name",
            SortMode::Date => "modified date",
            SortMode::Captured => "captured date",
            SortMode::Rating => "rating",
        }
    }
}

impl std::fmt::Display for SortMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            SortMode::Name => "Name",
            SortMode::Date => "Modified date",
            SortMode::Captured => "Capture date",
            SortMode::Rating => "Rating",
        })
    }
}

impl Config {
    /// Returns the project directories for PhotoBrowser, if determinable.
    pub fn project_dirs() -> Option<ProjectDirs> {
        ProjectDirs::from("com", "photobrowser", "PhotoBrowser")
    }

    /// Path to the JSON config file, if a platform config directory is available.
    pub fn config_path() -> Option<PathBuf> {
        Some(Self::project_dirs()?.config_dir().join("config.json"))
    }

    /// Load persisted configuration. Missing/unreadable/invalid config falls back
    /// to defaults and never panics.
    pub fn load() -> Self {
        let Some(path) = Self::config_path() else {
            tracing::warn!("project config directory unavailable; using default config");
            return Self::default();
        };
        Self::load_from_path(&path)
    }

    fn load_from_path(path: &Path) -> Self {
        match std::fs::read_to_string(path) {
            Ok(contents) => match serde_json::from_str(&contents) {
                Ok(config) => config,
                Err(err) => {
                    tracing::warn!(path = %path.display(), %err, "failed to parse config; using defaults");
                    Self::default()
                }
            },
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Self::default(),
            Err(err) => {
                tracing::warn!(path = %path.display(), %err, "failed to read config; using defaults");
                Self::default()
            }
        }
    }

    /// Best-effort atomic save to the platform config file.
    pub fn save(&self) {
        let Some(path) = Self::config_path() else {
            tracing::warn!("project config directory unavailable; skipping config save");
            return;
        };
        self.save_to_path(&path);
    }

    fn save_to_path(&self, path: &Path) {
        let Some(parent) = path.parent() else {
            tracing::warn!(path = %path.display(), "config path has no parent; skipping save");
            return;
        };
        if let Err(err) = std::fs::create_dir_all(parent) {
            tracing::warn!(path = %parent.display(), %err, "failed to create config dir");
            return;
        }
        let tmp = path.with_extension(format!("json.tmp-{}", std::process::id()));
        let encoded = match serde_json::to_vec_pretty(self) {
            Ok(encoded) => encoded,
            Err(err) => {
                tracing::warn!(%err, "failed to serialize config");
                return;
            }
        };
        if let Err(err) = std::fs::write(&tmp, encoded) {
            tracing::warn!(path = %tmp.display(), %err, "failed to write temp config");
            return;
        }
        if let Err(err) = std::fs::rename(&tmp, path) {
            tracing::warn!(from = %tmp.display(), to = %path.display(), %err, "failed to replace config");
            let _ = std::fs::remove_file(&tmp);
        }
    }
}

/// Default export destination: <XDG Pictures>/PhotoBrowser Exports, else <home>/PhotoBrowser Exports. None if neither
/// resolves. Used when `export_dir` is unset.
pub fn default_export_dir() -> Option<PathBuf> {
    let base = directories::UserDirs::new().and_then(|u| {
        u.picture_dir()
            .map(Path::to_path_buf)
            .or_else(|| Some(u.home_dir().to_path_buf()))
    })?;
    Some(base.join("PhotoBrowser Exports"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_values() {
        let cfg = Config::default();
        assert_eq!(cfg.thumb_size, 160);
        assert_eq!(cfg.thumb_cache_max_items, 600);
        assert_eq!(cfg.sort_mode, SortMode::Name);
        assert!(!cfg.show_hidden);
        assert!(!cfg.loupe_full_demosaic);
        assert!(!cfg.tree_show_files);
        assert!(!cfg.images_only);
        assert!(!cfg.full_res_loupe);
        assert!(!cfg.develop_look);
    }

    #[test]
    fn default_cache_settings_values() {
        let cfg = Config::default();
        assert_eq!(cfg.cache_max_gb, 2.0);
        assert_eq!(cfg.cache_dir, None);
        assert!(cfg.favorites.is_empty());
        assert!(cfg.collections.is_empty());
    }

    #[test]
    fn default_export_directory_uses_photobrowser_name() {
        let export_dir = default_export_dir().expect("a home directory should be available");
        assert!(
            export_dir.ends_with("PhotoBrowser Exports"),
            "unexpected export directory: {}",
            export_dir.display()
        );
    }

    #[test]
    fn saved_collection_serde_roundtrip() {
        let sc = SavedCollection {
            name: "3+ stars · Red · #beach".to_owned(),
            rating_min: Some(3),
            label: Some("Red".to_owned()),
            camera: None,
            date_year: None,
            tag: Some("beach".to_owned()),
        };
        let encoded = serde_json::to_string(&sc).unwrap();
        let decoded: SavedCollection = serde_json::from_str(&encoded).unwrap();
        assert_eq!(decoded, sc);
    }

    #[test]
    fn sort_mode_variants() {
        // Ensure all variants are reachable.
        let modes = [
            SortMode::Name,
            SortMode::Date,
            SortMode::Captured,
            SortMode::Rating,
        ];
        for mode in &modes {
            let label = match mode {
                SortMode::Name => "name",
                SortMode::Date => "date",
                SortMode::Captured => "captured",
                SortMode::Rating => "rating",
            };
            assert!(!label.is_empty());
        }
    }

    #[test]
    fn config_json_roundtrips() {
        let cfg = Config {
            thumb_size: 192,
            thumb_cache_max_items: 42,
            sort_mode: SortMode::Captured,
            cache_max_gb: 3.5,
            cache_dir: Some(PathBuf::from("/tmp/photobrowser-thumbs")),
            favorites: vec![PathBuf::from("/home/test/Pictures")],
            show_hidden: true,
            loupe_full_demosaic: true,
            tree_show_files: true,
            images_only: true,
            full_res_loupe: true,
            show_filmstrip: true,
            export_dir: None,
            develop_look: true,
            collections: Vec::new(),
        };

        let encoded = serde_json::to_string(&cfg).unwrap();
        let decoded: Config = serde_json::from_str(&encoded).unwrap();

        assert_eq!(decoded, cfg);
    }

    #[test]
    fn config_json_without_favorites_loads_default_empty_favorites() {
        let encoded = r#"{
            "thumb_size": 192,
            "thumb_cache_max_items": 42,
            "sort_mode": "Captured",
            "cache_max_gb": 3.5,
            "cache_dir": "/tmp/photobrowser-thumbs"
        }"#;

        let decoded: Config = serde_json::from_str(encoded).unwrap();

        assert!(decoded.favorites.is_empty());
    }

    #[test]
    fn config_json_without_show_hidden_loads_default_false() {
        let encoded = r#"{
            "thumb_size": 192,
            "thumb_cache_max_items": 42,
            "sort_mode": "Captured",
            "cache_max_gb": 3.5,
            "cache_dir": "/tmp/photobrowser-thumbs",
            "favorites": ["/home/test/Pictures"]
        }"#;

        let decoded: Config = serde_json::from_str(encoded).unwrap();

        assert!(!decoded.show_hidden);
    }

    #[test]
    fn config_json_without_loupe_full_demosaic_loads_default_false() {
        let encoded = r#"{
            "thumb_size": 192,
            "thumb_cache_max_items": 42,
            "sort_mode": "Captured",
            "cache_max_gb": 3.5,
            "cache_dir": "/tmp/photobrowser-thumbs",
            "favorites": ["/home/test/Pictures"],
            "show_hidden": true
        }"#;

        let decoded: Config = serde_json::from_str(encoded).unwrap();

        assert!(!decoded.loupe_full_demosaic);
    }

    #[test]
    fn config_json_without_tree_show_files_loads_default_false() {
        let encoded = r#"{
            "thumb_size": 192,
            "thumb_cache_max_items": 42,
            "sort_mode": "Captured",
            "cache_max_gb": 3.5,
            "cache_dir": "/tmp/photobrowser-thumbs",
            "favorites": ["/home/test/Pictures"],
            "show_hidden": true,
            "loupe_full_demosaic": true
        }"#;

        let decoded: Config = serde_json::from_str(encoded).unwrap();

        assert!(!decoded.tree_show_files);
    }

    #[test]
    fn config_json_without_full_res_loupe_loads_default_false() {
        let encoded = r#"{
            "thumb_size": 192,
            "thumb_cache_max_items": 42,
            "sort_mode": "Captured",
            "cache_max_gb": 3.5,
            "cache_dir": "/tmp/photobrowser-thumbs",
            "favorites": ["/home/test/Pictures"],
            "show_hidden": true,
            "loupe_full_demosaic": true,
            "tree_show_files": true,
            "images_only": true
        }"#;

        let decoded: Config = serde_json::from_str(encoded).unwrap();

        assert!(!decoded.full_res_loupe);
    }

    #[test]
    fn config_json_without_develop_look_loads_default_false() {
        let encoded = r#"{
            "thumb_size": 192,
            "thumb_cache_max_items": 42,
            "sort_mode": "Captured",
            "cache_max_gb": 3.5,
            "cache_dir": "/tmp/photobrowser-thumbs",
            "favorites": ["/home/test/Pictures"],
            "show_hidden": true,
            "loupe_full_demosaic": true,
            "tree_show_files": true,
            "images_only": true,
            "full_res_loupe": true
        }"#;

        let decoded: Config = serde_json::from_str(encoded).unwrap();

        assert!(!decoded.develop_look);
    }

    #[test]
    fn config_default_show_filmstrip_true_and_json_omitting_it_loads_true() {
        // Additive test only (no existing test modified).
        assert!(Config::default().show_filmstrip);

        let encoded = r#"{
            "thumb_size": 192,
            "thumb_cache_max_items": 42,
            "sort_mode": "Captured",
            "cache_max_gb": 3.5,
            "cache_dir": "/tmp/photobrowser-thumbs",
            "favorites": ["/home/test/Pictures"],
            "show_hidden": true,
            "loupe_full_demosaic": true,
            "tree_show_files": true,
            "images_only": true,
            "full_res_loupe": true
        }"#;

        let decoded: Config = serde_json::from_str(encoded).unwrap();
        assert!(decoded.show_filmstrip);
    }

    #[test]
    fn load_missing_config_returns_default() {
        let _env = ConfigEnvGuard::new(tempfile::tempdir().unwrap());

        assert_eq!(Config::load(), Config::default());
    }

    struct ConfigEnvGuard {
        _dir: tempfile::TempDir,
        old_xdg_config_home: Option<std::ffi::OsString>,
    }

    impl ConfigEnvGuard {
        fn new(dir: tempfile::TempDir) -> Self {
            let old_xdg_config_home = std::env::var_os("XDG_CONFIG_HOME");
            std::env::set_var("XDG_CONFIG_HOME", dir.path());
            Self {
                _dir: dir,
                old_xdg_config_home,
            }
        }
    }

    impl Drop for ConfigEnvGuard {
        fn drop(&mut self) {
            match &self.old_xdg_config_home {
                Some(value) => std::env::set_var("XDG_CONFIG_HOME", value),
                None => std::env::remove_var("XDG_CONFIG_HOME"),
            }
        }
    }
}
