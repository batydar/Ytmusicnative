use directories::{BaseDirs, UserDirs};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppSettings {
    #[serde(default = "default_download_dir")]
    pub download_dir: PathBuf,
    #[serde(default = "default_eq_preset")]
    pub eq_preset: String,
    #[serde(default)]
    pub loudness_normalization: bool,
    #[serde(default = "default_volume")]
    pub volume: f64,
    #[serde(default = "default_true")]
    pub sponsorblock_enabled: bool,
}

fn default_eq_preset() -> String {
    "Flat".to_string()
}

fn default_volume() -> f64 {
    80.0
}

fn default_true() -> bool {
    true
}

pub fn default_download_dir() -> PathBuf {
    if let Ok(user_profile) = std::env::var("USERPROFILE") {
        let mut path = PathBuf::from(user_profile);
        path.push("Music");
        path.push("YTMusicDownloads");
        return path;
    }

    if let Some(user_dirs) = UserDirs::new() {
        if let Some(audio_dir) = user_dirs.audio_dir() {
            return audio_dir.join("YTMusicDownloads");
        }
        return user_dirs
            .home_dir()
            .join("Music")
            .join("YTMusicDownloads");
    }

    PathBuf::from("YTMusicDownloads")
}

pub fn app_data_dir() -> PathBuf {
    if let Ok(appdata) = std::env::var("APPDATA") {
        return PathBuf::from(appdata).join("YTMusicNative");
    }

    if let Some(base_dirs) = BaseDirs::new() {
        return base_dirs.config_dir().join("YTMusicNative");
    }

    PathBuf::from("YTMusicNative")
}

pub fn settings_file_path() -> PathBuf {
    app_data_dir().join("settings.json")
}

pub fn cookies_file_path() -> PathBuf {
    app_data_dir().join("cookies.txt")
}

pub fn browser_auth_profile_dir() -> PathBuf {
    app_data_dir().join("BrowserAuthProfile")
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            download_dir: default_download_dir(),
            eq_preset: default_eq_preset(),
            loudness_normalization: false,
            volume: default_volume(),
            sponsorblock_enabled: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CachedTrackInfo {
    pub video_id: String,
    pub title: String,
    pub artist: String,
    pub file_path: PathBuf,
}

impl AppSettings {
    /// Load settings from `%APPDATA%/YTMusicNative/settings.json`.
    /// Creates default settings file and download directory if not present.
    pub fn load() -> Self {
        let path = settings_file_path();
        let settings = if path.exists() {
            match fs::read_to_string(&path) {
                Ok(content) => serde_json::from_str::<AppSettings>(&content).unwrap_or_default(),
                Err(_) => AppSettings::default(),
            }
        } else {
            let default_settings = AppSettings::default();
            let _ = default_settings.save();
            default_settings
        };

        let _ = fs::create_dir_all(&settings.download_dir);
        settings
    }

    /// Persist settings immediately to `%APPDATA%/YTMusicNative/settings.json`.
    pub fn save(&self) -> Result<(), String> {
        let dir = app_data_dir();
        fs::create_dir_all(&dir).map_err(|e| format!("Failed to create app data dir: {}", e))?;
        let _ = fs::create_dir_all(&self.download_dir);

        let path = settings_file_path();
        let json = serde_json::to_string_pretty(self)
            .map_err(|e| format!("Failed to serialize settings: {}", e))?;
        fs::write(path, json).map_err(|e| format!("Failed to write settings.json: {}", e))
    }

    /// Open native Windows folder picker asynchronously via `rfd::AsyncFileDialog`
    /// and immediately persist the chosen directory to `settings.json`.
    pub async fn pick_and_save_download_dir(&mut self) -> Option<PathBuf> {
        let dialog = rfd::AsyncFileDialog::new()
            .set_title("İndirme Klasörünü Seç")
            .set_directory(&self.download_dir);

        if let Some(folder_handle) = dialog.pick_folder().await {
            let new_dir = folder_handle.path().to_path_buf();
            let _ = fs::create_dir_all(&new_dir);
            self.download_dir = new_dir.clone();
            let _ = self.save();
            Some(new_dir)
        } else {
            None
        }
    }

    /// Scan `<CUSTOM_DOWNLOAD_DIR>` for any `.opus` file whose filename contains `[<video_id>]`.
    /// Returns the absolute local path if found.
    pub fn find_cached_opus(&self, video_id: &str) -> Option<PathBuf> {
        if video_id.trim().is_empty() || !self.download_dir.exists() {
            return None;
        }

        let needle = format!("[{}]", video_id);
        for entry in WalkDir::new(&self.download_dir)
            .max_depth(2)
            .into_iter()
            .filter_map(Result::ok)
        {
            let path = entry.path();
            if !path.is_file() {
                continue;
            }
            let is_opus = path
                .extension()
                .and_then(|ext| ext.to_str())
                .map(|ext| ext.eq_ignore_ascii_case("opus"))
                .unwrap_or(false);

            if !is_opus {
                continue;
            }

            if let Some(file_name) = path.file_name().and_then(|n| n.to_str()) {
                if file_name.contains(&needle) {
                    return Some(
                        fs::canonicalize(path)
                            .map(strip_unc_prefix)
                            .unwrap_or_else(|_| path.to_path_buf()),
                    );
                }
            }
        }

        None
    }

    /// List all downloaded `.opus` tracks in `<CUSTOM_DOWNLOAD_DIR>`.
    /// Parses `<Artist> - <Title> [<id>].opus` filenames.
    pub fn list_downloaded_tracks(&self) -> Vec<CachedTrackInfo> {
        let mut tracks = Vec::new();
        if !self.download_dir.exists() {
            return tracks;
        }

        for entry in WalkDir::new(&self.download_dir)
            .max_depth(2)
            .into_iter()
            .filter_map(Result::ok)
        {
            let path = entry.path();
            if !path.is_file() {
                continue;
            }
            let is_opus = path
                .extension()
                .and_then(|ext| ext.to_str())
                .map(|ext| ext.eq_ignore_ascii_case("opus"))
                .unwrap_or(false);
            if !is_opus {
                continue;
            }

            if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
                let (artist_title, video_id) = if let (Some(open_idx), Some(close_idx)) =
                    (stem.rfind('['), stem.rfind(']'))
                {
                    if open_idx < close_idx {
                        let id = stem[open_idx + 1..close_idx].trim().to_string();
                        let prefix = stem[..open_idx].trim();
                        (prefix, id)
                    } else {
                        (stem, String::new())
                    }
                } else {
                    (stem, String::new())
                };

                let (artist, title) = if let Some((a, t)) = artist_title.split_once(" - ") {
                    (a.trim().to_string(), t.trim().to_string())
                } else {
                    ("Unknown Artist".to_string(), artist_title.to_string())
                };

                let abs_path = fs::canonicalize(path)
                    .map(strip_unc_prefix)
                    .unwrap_or_else(|_| path.to_path_buf());

                tracks.push(CachedTrackInfo {
                    video_id,
                    title,
                    artist,
                    file_path: abs_path,
                });
            }
        }

        tracks
    }
}

pub fn strip_unc_prefix(path: PathBuf) -> PathBuf {
    if let Some(s) = path.to_str() {
        if let Some(stripped) = s.strip_prefix(r"\\?\") {
            return PathBuf::from(stripped);
        }
    }
    path
}

pub fn resolve_binary_path(bin_name: &str) -> PathBuf {
    let mut candidates = Vec::new();

    if let Ok(cwd) = std::env::current_dir() {
        candidates.push(cwd.join("bin").join(bin_name));
    }

    if let Ok(exe_path) = std::env::current_exe() {
        if let Some(exe_dir) = exe_path.parent() {
            candidates.push(exe_dir.join("bin").join(bin_name));
            candidates.push(exe_dir.join(bin_name));
            if let Some(parent1) = exe_dir.parent() {
                candidates.push(parent1.join("bin").join(bin_name));
                if let Some(parent2) = parent1.parent() {
                    candidates.push(parent2.join("bin").join(bin_name));
                }
            }
        }
    }

    for candidate in candidates {
        if candidate.exists() {
            return fs::canonicalize(&candidate)
                .map(strip_unc_prefix)
                .unwrap_or(candidate);
        }
    }

    Path::new("bin").join(bin_name)
}
