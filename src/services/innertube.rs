use crate::config::{browser_auth_profile_dir, cookies_file_path, resolve_binary_path};
use crate::mpv::CREATE_NO_WINDOW;
use reqwest::header::{
    HeaderMap, HeaderValue, AUTHORIZATION, CONTENT_TYPE, COOKIE, ORIGIN, USER_AGENT,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha1::{Digest, Sha1};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

#[cfg(target_os = "windows")]
use std::os::windows::process::CommandExt;

pub const INNERTUBE_CLIENT_NAME: &str = "WEB_REMIX";
pub const INNERTUBE_CLIENT_VERSION: &str = "1.20240101.01.00";
pub const YTMUSIC_ORIGIN: &str = "https://music.youtube.com";
pub const BROWSE_ENDPOINT: &str = "https://music.youtube.com/youtubei/v1/browse";
pub const SEARCH_ENDPOINT: &str = "https://music.youtube.com/youtubei/v1/search";
pub const NEXT_ENDPOINT: &str = "https://music.youtube.com/youtubei/v1/next";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrackItem {
    pub video_id: String,
    pub title: String,
    pub artist: String,
    pub album: String,
    pub duration_text: String,
    pub duration_secs: u64,
    pub thumbnail_url: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlaylistItem {
    pub browse_id: String,
    pub title: String,
    pub subtitle: String,
    pub track_count: String,
}

#[derive(Clone)]
pub struct InnerTubeClient {
    http: reqwest::Client,
}

impl InnerTubeClient {
    pub fn new() -> Self {
        let http = reqwest::Client::builder()
            .user_agent("Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/124.0.0.0 Safari/537.36")
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());

        Self { http }
    }

    fn build_context() -> Value {
        json!({
            "client": {
                "clientName": INNERTUBE_CLIENT_NAME,
                "clientVersion": INNERTUBE_CLIENT_VERSION,
                "hl": "tr",
                "gl": "TR",
                "platform": "DESKTOP"
            }
        })
    }

    pub fn is_logged_in() -> bool {
        let path = cookies_file_path();
        if !path.exists() {
            return false;
        }
        if let Ok(content) = fs::read_to_string(&path) {
            content.contains("SAPISID")
                || content.contains("__Secure-3PAPISID")
                || content.contains("__Secure-1PAPISID")
                || content.contains("LOGIN_INFO")
        } else {
            false
        }
    }

    /// Generate `SAPISIDHASH` (and `SAPISID1PHASH` / `SAPISID3PHASH`) via SHA-1:
    /// `sha1("{timestamp} {sapisid} https://music.youtube.com")`
    pub fn generate_sapisidhash(sapisid: &str) -> String {
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);

        let input = format!("{} {} {}", timestamp, sapisid, YTMUSIC_ORIGIN);
        let mut hasher = Sha1::new();
        hasher.update(input.as_bytes());
        let digest = hasher.finalize();
        let hex_hash: String = digest.iter().map(|b| format!("{:02x}", b)).collect();

        format!(
            "SAPISIDHASH {ts}_{hash} SAPISID1PHASH {ts}_{hash} SAPISID3PHASH {ts}_{hash}",
            ts = timestamp,
            hash = hex_hash
        )
    }

    /// Find installed Chromium-based browser (`msedge.exe` or `chrome.exe`) for dedicated App-mode Login Window.
    fn find_login_browser() -> Option<(&'static str, PathBuf)> {
        let candidates: &[(&str, &str)] = &[
            (
                "edge",
                r"C:\Program Files (x86)\Microsoft\Edge\Application\msedge.exe",
            ),
            (
                "edge",
                r"C:\Program Files\Microsoft\Edge\Application\msedge.exe",
            ),
            (
                "chrome",
                r"C:\Program Files\Google\Chrome\Application\chrome.exe",
            ),
            (
                "chrome",
                r"C:\Program Files (x86)\Google\Chrome\Application\chrome.exe",
            ),
        ];

        for (browser_name, path_str) in candidates {
            let p = PathBuf::from(path_str);
            if p.exists() {
                return Some((*browser_name, p));
            }
        }
        None
    }

    /// Gracefully close any running browser window using our isolated `BrowserAuthProfile` directory
    /// so Chromium flushes its SQLite cookie database cleanly to disk and unlocks it before extraction.
    pub fn close_auth_browser_window() {
        #[cfg(target_os = "windows")]
        {
            // Step 1: Send graceful WM_CLOSE via taskkill (without /F) so Edge flushes cookies to disk
            let ps_graceful = "Get-CimInstance Win32_Process | Where-Object { $_.CommandLine -like '*BrowserAuthProfile*' } | ForEach-Object { taskkill /PID $_.ProcessId 2>&1 | Out-Null }";
            let _ = Command::new("powershell")
                .args(["-NoProfile", "-WindowStyle", "Hidden", "-Command", ps_graceful])
                .creation_flags(CREATE_NO_WINDOW)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();

            std::thread::sleep(std::time::Duration::from_millis(650));

            // Step 2: Force-close any leftover background helper processes holding the lock
            let ps_force = "Get-CimInstance Win32_Process | Where-Object { $_.CommandLine -like '*BrowserAuthProfile*' } | ForEach-Object { Stop-Process -Id $_.ProcessId -Force -ErrorAction SilentlyContinue }";
            let _ = Command::new("powershell")
                .args(["-NoProfile", "-WindowStyle", "Hidden", "-Command", ps_force])
                .creation_flags(CREATE_NO_WINDOW)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();

            std::thread::sleep(std::time::Duration::from_millis(250));
        }
    }

    /// Extract Netscape `cookies.txt` from our isolated `BrowserAuthProfile` using `bin/yt-dlp.exe` (`CREATE_NO_WINDOW`).
    pub fn extract_cookies_from_auth_profile() -> Result<bool, String> {
        Self::close_auth_browser_window();

        let profile_dir = browser_auth_profile_dir();
        let cookies_out = cookies_file_path();
        if let Some(parent) = cookies_out.parent() {
            let _ = fs::create_dir_all(parent);
        }

        let ytdlp_bin = resolve_binary_path("yt-dlp.exe");
        let browser_type = Self::find_login_browser()
            .map(|(b, _)| b)
            .unwrap_or("edge");

        if profile_dir.exists() {
            let browser_arg = format!("{}:{}", browser_type, profile_dir.to_string_lossy());
            let mut cmd = Command::new(&ytdlp_bin);
            cmd.arg("--cookies-from-browser")
                .arg(&browser_arg)
                .arg("--cookies")
                .arg(&cookies_out)
                .arg("--skip-download")
                .arg("--flat-playlist")
                .arg("--playlist-items")
                .arg("1")
                .arg("https://music.youtube.com")
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null());

            #[cfg(target_os = "windows")]
            {
                cmd.creation_flags(CREATE_NO_WINDOW);
            }

            let _ = cmd.status();
            if Self::is_logged_in() {
                return Ok(true);
            }
        }

        // Fallback: Try extracting from unlocked browsers (e.g. Firefox or closed Edge/Chrome/Brave)
        for fallback_browser in ["firefox", "edge", "chrome", "brave"] {
            let mut cmd = Command::new(&ytdlp_bin);
            cmd.arg("--cookies-from-browser")
                .arg(fallback_browser)
                .arg("--cookies")
                .arg(&cookies_out)
                .arg("--skip-download")
                .arg("--flat-playlist")
                .arg("--playlist-items")
                .arg("1")
                .arg("https://music.youtube.com")
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null());

            #[cfg(target_os = "windows")]
            {
                cmd.creation_flags(CREATE_NO_WINDOW);
            }

            let _ = cmd.status();
            if Self::is_logged_in() {
                return Ok(true);
            }
        }

        Ok(Self::is_logged_in())
    }

    /// Launch a dedicated, isolated Google / QR / Passkey login popup window, wait for it to close,
    /// and automatically extract the session cookies to `cookies.txt`.
    pub fn launch_interactive_login_window_blocking() -> Result<bool, String> {
        Self::close_auth_browser_window();

        let (_, browser_exe) = Self::find_login_browser().ok_or_else(|| {
            "Microsoft Edge veya Google Chrome bulunamadı.".to_string()
        })?;

        let profile_dir = browser_auth_profile_dir();
        let _ = fs::create_dir_all(&profile_dir);

        // Google ServiceLogin URL that supports Email, Passkey, and Phone QR Code login, then redirects to YouTube Music
        let login_url = "https://accounts.google.com/ServiceLogin?service=youtube&uilel=3&passive=true&hl=tr&continue=https%3A%2F%2Fmusic.youtube.com%2F";

        let mut child = Command::new(&browser_exe)
            .arg(format!("--app={}", login_url))
            .arg(format!("--user-data-dir={}", profile_dir.to_string_lossy()))
            .arg("--window-size=520,720")
            .arg("--no-first-run")
            .arg("--no-default-browser-check")
            .arg("--disable-extensions")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| format!("Giriş penceresi açılamadı: {}", e))?;

        // Wait until the user closes the popup window OR clicks "Girişi Tamamla" (which terminates it)
        let _ = child.wait();
        std::thread::sleep(std::time::Duration::from_millis(350));

        Self::extract_cookies_from_auth_profile()
    }

    /// Clear saved cookies and auth profile to log out or switch accounts.
    pub fn logout() {
        Self::close_auth_browser_window();
        let cookies_path = cookies_file_path();
        if cookies_path.exists() {
            let _ = fs::remove_file(&cookies_path);
        }
        let profile_dir = browser_auth_profile_dir();
        if profile_dir.exists() {
            let _ = fs::remove_dir_all(&profile_dir);
        }
    }

    fn load_cookies_and_sapisid() -> Option<(String, Option<String>)> {
        let path = cookies_file_path();
        if !path.exists() {
            return None;
        }

        let content = fs::read_to_string(&path).ok()?;
        let mut cookie_map: HashMap<String, String> = HashMap::new();
        let mut sapisid: Option<String> = None;

        for raw_line in content.lines() {
            let line = raw_line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }

            if line.contains('\t') {
                let cols: Vec<&str> = line.split('\t').collect();
                if cols.len() >= 7 {
                    let domain = cols[0].trim();
                    let name = cols[5].trim();
                    let val = cols[6].trim();
                    if !name.is_empty()
                        && (domain.contains("youtube.com") || domain.contains("google.com"))
                    {
                        if name == "SAPISID" {
                            sapisid = Some(val.to_string());
                        } else if sapisid.is_none()
                            && (name == "__Secure-3PAPISID" || name == "__Secure-1PAPISID")
                        {
                            sapisid = Some(val.to_string());
                        }
                        // Prefer youtube.com cookies over google.com when names collide
                        if domain.contains("youtube.com") || !cookie_map.contains_key(name) {
                            cookie_map.insert(name.to_string(), val.to_string());
                        }
                    }
                }
            } else {
                for part in line.split(';') {
                    let kv = part.trim();
                    if let Some((k, v)) = kv.split_once('=') {
                        let name = k.trim();
                        let val = v.trim();
                        if name == "SAPISID" {
                            sapisid = Some(val.to_string());
                        } else if sapisid.is_none()
                            && (name == "__Secure-3PAPISID" || name == "__Secure-1PAPISID")
                        {
                            sapisid = Some(val.to_string());
                        }
                        cookie_map.insert(name.to_string(), val.to_string());
                    }
                }
            }
        }

        if cookie_map.is_empty() {
            None
        } else {
            // Ensure SAPISID is also set if only __Secure-3PAPISID was present
            if let Some(ref sap) = sapisid {
                cookie_map
                    .entry("SAPISID".to_string())
                    .or_insert_with(|| sap.clone());
            }
            let pairs: Vec<String> = cookie_map
                .into_iter()
                .map(|(k, v)| format!("{}={}", k, v))
                .collect();
            Some((pairs.join("; "), sapisid))
        }
    }

    fn build_headers() -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        headers.insert(ORIGIN, HeaderValue::from_static(YTMUSIC_ORIGIN));
        headers.insert("X-Origin", HeaderValue::from_static(YTMUSIC_ORIGIN));
        headers.insert("X-Goog-AuthUser", HeaderValue::from_static("0"));
        headers.insert("X-Youtube-Client-Name", HeaderValue::from_static("67"));
        headers.insert(
            "X-Youtube-Client-Version",
            HeaderValue::from_static(INNERTUBE_CLIENT_VERSION),
        );
        headers.insert(
            USER_AGENT,
            HeaderValue::from_static(
                "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/124.0.0.0 Safari/537.36",
            ),
        );

        if let Some((cookie_str, maybe_sapisid)) = Self::load_cookies_and_sapisid() {
            if let Ok(cookie_val) = HeaderValue::from_str(&cookie_str) {
                headers.insert(COOKIE, cookie_val);
            }
            if let Some(sapisid) = maybe_sapisid {
                let auth_str = Self::generate_sapisidhash(&sapisid);
                if let Ok(auth_val) = HeaderValue::from_str(&auth_str) {
                    headers.insert(AUTHORIZATION, auth_val);
                }
            }
        }

        headers
    }

    /// Fetch Home Recommendations via `POST https://music.youtube.com/youtubei/v1/browse` (`{"browseId": "FEmusic_home"}`).
    /// Follows continuations and expands personal mixes so logged-in users see their true personalized feed.
    pub async fn fetch_home_recommendations(&self) -> Result<Vec<TrackItem>, String> {
        let body = json!({
            "context": Self::build_context(),
            "browseId": "FEmusic_home"
        });

        let response = self
            .http
            .post(BROWSE_ENDPOINT)
            .headers(Self::build_headers())
            .json(&body)
            .send()
            .await
            .map_err(|e| format!("InnerTube browse request failed: {}", e))?;

        let mut json_val: Value = response
            .json()
            .await
            .map_err(|e| format!("Failed to parse InnerTube browse JSON: {}", e))?;

        let mut tracks = Vec::new();
        let mut seen = HashSet::new();
        let mut home_playlists = Vec::new();
        let mut seen_pl = HashSet::new();

        extract_tracks_recursive(&json_val, &mut tracks, &mut seen);
        extract_playlists_recursive(&json_val, &mut home_playlists, &mut seen_pl);

        // Follow up to 2 continuation pages on FEmusic_home to get more personal shelves (Quick Picks, Listen Again, Forgotten Favorites)
        for _ in 0..2 {
            let Some(ctoken) = find_continuation_token(&json_val) else {
                break;
            };
            let cont_url = format!(
                "{}?ctoken={}&continuation={}&type=next",
                BROWSE_ENDPOINT, ctoken, ctoken
            );
            let cont_body = json!({
                "context": Self::build_context()
            });
            if let Ok(res) = self
                .http
                .post(&cont_url)
                .headers(Self::build_headers())
                .json(&cont_body)
                .send()
                .await
            {
                if let Ok(next_val) = res.json::<Value>().await {
                    extract_tracks_recursive(&next_val, &mut tracks, &mut seen);
                    extract_playlists_recursive(&next_val, &mut home_playlists, &mut seen_pl);
                    json_val = next_val;
                } else {
                    break;
                }
            } else {
                break;
            }
        }

        // If FEmusic_home mainly returned personal mix cards (e.g., My Supermix, Liked Music, Discover Mix),
        // pull tracks from the user's Liked Music (VLLM) and top personal home mixes so the Home screen is rich with their music.
        if tracks.len() < 15 && Self::is_logged_in() {
            if let Ok(liked) = self.fetch_playlist_tracks("VLLM").await {
                for t in liked.into_iter().take(20) {
                    if seen.insert(t.video_id.clone()) {
                        tracks.push(t);
                    }
                }
            }
        }

        if tracks.len() < 15 {
            for pl in home_playlists.iter().take(2) {
                if let Ok(pl_tracks) = self.fetch_playlist_tracks(&pl.browse_id).await {
                    for t in pl_tracks.into_iter().take(15) {
                        if seen.insert(t.video_id.clone()) {
                            tracks.push(t);
                        }
                    }
                }
                if tracks.len() >= 30 {
                    break;
                }
            }
        }

        // Anonymous fallback if still < 10 tracks
        if tracks.len() < 10 {
            for query in ["Türkiye Top 50", "Pop Hits Mix", "Trending Music"] {
                if let Ok(extra) = self.search(query).await {
                    for t in extra {
                        if seen.insert(t.video_id.clone()) {
                            tracks.push(t);
                        }
                    }
                }
            }
        }

        Ok(tracks)
    }

    /// Search tracks via `POST https://music.youtube.com/youtubei/v1/search`.
    pub async fn search(&self, query: &str) -> Result<Vec<TrackItem>, String> {
        if query.trim().is_empty() {
            return Ok(Vec::new());
        }

        let body = json!({
            "context": Self::build_context(),
            "query": query.trim()
        });

        let response = self
            .http
            .post(SEARCH_ENDPOINT)
            .headers(Self::build_headers())
            .json(&body)
            .send()
            .await
            .map_err(|e| format!("InnerTube search request failed: {}", e))?;

        let json_val: Value = response
            .json()
            .await
            .map_err(|e| format!("Failed to parse InnerTube search JSON: {}", e))?;

        let mut tracks = Vec::new();
        let mut seen = HashSet::new();
        extract_tracks_recursive(&json_val, &mut tracks, &mut seen);

        Ok(tracks)
    }

    /// Fetch ALL Playlists saved and created by the user (plus Liked Music and personal mixes).
    pub async fn fetch_playlists(&self) -> Result<Vec<PlaylistItem>, String> {
        let mut playlists = Vec::new();
        let mut seen_ids = HashSet::new();

        let logged_in = Self::is_logged_in();

        if logged_in {
            // 1. Always include user's Liked Music (VLLM) at the top
            seen_ids.insert("VLLM".to_string());
            playlists.push(PlaylistItem {
                browse_id: "VLLM".to_string(),
                title: "Beğenilen Şarkılar (Liked Music)".to_string(),
                subtitle: "Senin Kitaplığın • Beğendiğin tüm müzikler".to_string(),
                track_count: "Otomatik".to_string(),
            });

            // 2. Fetch user's saved & created playlists from Library endpoints:
            //    - `FEmusic_liked_playlists` (Saved & Created Playlists)
            //    - `FEmusic_library_landing` (Main Library view in WEB_REMIX)
            //    - `FEmusic_liked_albums` (Saved Albums)
            for browse_id in [
                "FEmusic_liked_playlists",
                "FEmusic_library_landing",
                "FEmusic_liked_albums",
            ] {
                let lib_body = json!({
                    "context": Self::build_context(),
                    "browseId": browse_id
                });

                if let Ok(res) = self
                    .http
                    .post(BROWSE_ENDPOINT)
                    .headers(Self::build_headers())
                    .json(&lib_body)
                    .send()
                    .await
                {
                    if let Ok(mut val) = res.json::<Value>().await {
                        extract_playlists_recursive(&val, &mut playlists, &mut seen_ids);

                        // Follow up to 4 continuation pages so large playlist libraries load completely
                        for _ in 0..4 {
                            let Some(ctoken) = find_continuation_token(&val) else {
                                break;
                            };
                            let cont_url = format!(
                                "{}?ctoken={}&continuation={}&type=next",
                                BROWSE_ENDPOINT, ctoken, ctoken
                            );
                            let cont_body = json!({
                                "context": Self::build_context()
                            });
                            if let Ok(c_res) = self
                                .http
                                .post(&cont_url)
                                .headers(Self::build_headers())
                                .json(&cont_body)
                                .send()
                                .await
                            {
                                if let Ok(next_val) = c_res.json::<Value>().await {
                                    extract_playlists_recursive(
                                        &next_val,
                                        &mut playlists,
                                        &mut seen_ids,
                                    );
                                    val = next_val;
                                } else {
                                    break;
                                }
                            } else {
                                break;
                            }
                        }
                    }
                }
            }

            // 3. Also pull personal mixes from `FEmusic_home` (e.g. My Supermix, Discover Mix, Archive Mix)
            let home_body = json!({
                "context": Self::build_context(),
                "browseId": "FEmusic_home"
            });
            if let Ok(res) = self
                .http
                .post(BROWSE_ENDPOINT)
                .headers(Self::build_headers())
                .json(&home_body)
                .send()
                .await
            {
                if let Ok(val) = res.json::<Value>().await {
                    extract_playlists_recursive(&val, &mut playlists, &mut seen_ids);
                }
            }
        }

        // 4. Fallback for anonymous users (or if library is completely empty)
        if playlists.len() <= 1 {
            for q in [
                "Türkçe Pop Çalma Listesi",
                "Global Top Hits Playlist",
                "Rock Classics Playlist",
                "Chill Lofi Study Beats",
            ] {
                let body = json!({
                    "context": Self::build_context(),
                    "query": q
                });

                if let Ok(res) = self
                    .http
                    .post(SEARCH_ENDPOINT)
                    .headers(Self::build_headers())
                    .json(&body)
                    .send()
                    .await
                {
                    if let Ok(val) = res.json::<Value>().await {
                        extract_playlists_recursive(&val, &mut playlists, &mut seen_ids);
                    }
                }
                if playlists.len() >= 18 {
                    break;
                }
            }
        }

        Ok(playlists)
    }

    /// Fetch all tracks inside a playlist, album, or personal radio mix given its `browseId`
    pub async fn fetch_playlist_tracks(&self, browse_id: &str) -> Result<Vec<TrackItem>, String> {
        let trimmed = browse_id.trim();
        let target_id = if trimmed.starts_with("VL") || trimmed.starts_with("MPREb_") {
            trimmed.to_string()
        } else if trimmed == "FEmusic_liked_videos" || trimmed == "LM" {
            "VLLM".to_string()
        } else if trimmed.starts_with("PL")
            || trimmed.starts_with("RD")
            || trimmed.starts_with("OLAK")
        {
            format!("VL{}", trimmed)
        } else {
            trimmed.to_string()
        };

        let body = json!({
            "context": Self::build_context(),
            "browseId": target_id
        });

        let mut tracks = Vec::new();
        let mut seen = HashSet::new();

        if let Ok(response) = self
            .http
            .post(BROWSE_ENDPOINT)
            .headers(Self::build_headers())
            .json(&body)
            .send()
            .await
        {
            if let Ok(mut json_val) = response.json::<Value>().await {
                extract_tracks_recursive(&json_val, &mut tracks, &mut seen);

                // Follow up to 3 continuations for playlists with >100 songs
                for _ in 0..3 {
                    let Some(ctoken) = find_continuation_token(&json_val) else {
                        break;
                    };
                    let cont_url = format!(
                        "{}?ctoken={}&continuation={}&type=next",
                        BROWSE_ENDPOINT, ctoken, ctoken
                    );
                    let cont_body = json!({
                        "context": Self::build_context()
                    });
                    if let Ok(c_res) = self
                        .http
                        .post(&cont_url)
                        .headers(Self::build_headers())
                        .json(&cont_body)
                        .send()
                        .await
                    {
                        if let Ok(next_val) = c_res.json::<Value>().await {
                            extract_tracks_recursive(&next_val, &mut tracks, &mut seen);
                            json_val = next_val;
                        } else {
                            break;
                        }
                    } else {
                        break;
                    }
                }
            }
        }

        // Fallback: If this is a dynamic Mix / Radio playlist (e.g. `VLRD...` or `RDCLAK...`) where `browse` returns 0 tracks,
        // query `/youtubei/v1/next` with the raw `playlistId` to get the generated radio/mix queue!
        if tracks.is_empty() {
            let raw_playlist_id = target_id.strip_prefix("VL").unwrap_or(&target_id);
            let next_body = json!({
                "context": Self::build_context(),
                "playlistId": raw_playlist_id
            });

            if let Ok(res) = self
                .http
                .post(NEXT_ENDPOINT)
                .headers(Self::build_headers())
                .json(&next_body)
                .send()
                .await
            {
                if let Ok(val) = res.json::<Value>().await {
                    extract_tracks_recursive(&val, &mut tracks, &mut seen);
                }
            }
        }

        Ok(tracks)
    }

    /// Fetch dynamically generated similar radio tracks (autoplay queue) for a song via `/youtubei/v1/next`.
    pub async fn fetch_radio_queue(&self, video_id: &str) -> Result<Vec<TrackItem>, String> {
        let body = json!({
            "context": Self::build_context(),
            "videoId": video_id,
            "playlistId": format!("RDAMVM{}", video_id),
            "isAudioOnly": true
        });

        let response = self
            .http
            .post(NEXT_ENDPOINT)
            .headers(Self::build_headers())
            .json(&body)
            .send()
            .await
            .map_err(|e| format!("InnerTube radio queue request failed: {}", e))?;

        let json_val: Value = response
            .json()
            .await
            .map_err(|e| format!("Failed to parse radio queue JSON: {}", e))?;

        let mut tracks = Vec::new();
        let mut seen = HashSet::new();
        extract_tracks_recursive(&json_val, &mut tracks, &mut seen);

        Ok(tracks)
    }
}

fn find_continuation_token(node: &Value) -> Option<String> {
    if let Some(cont) = node
        .pointer("/nextContinuationData/continuation")
        .and_then(|v| v.as_str())
    {
        if !cont.is_empty() {
            return Some(cont.to_string());
        }
    }
    if let Some(cont) = node
        .pointer("/continuationEndpoint/continuationCommand/token")
        .and_then(|v| v.as_str())
    {
        if !cont.is_empty() {
            return Some(cont.to_string());
        }
    }
    match node {
        Value::Object(map) => {
            for val in map.values() {
                if let Some(found) = find_continuation_token(val) {
                    return Some(found);
                }
            }
            None
        }
        Value::Array(arr) => {
            for val in arr {
                if let Some(found) = find_continuation_token(val) {
                    return Some(found);
                }
            }
            None
        }
        _ => None,
    }
}

fn extract_playlists_recursive(
    node: &Value,
    out: &mut Vec<PlaylistItem>,
    seen: &mut HashSet<String>,
) {
    match node {
        Value::Object(map) => {
            if let Some(renderer) = map.get("musicTwoRowItemRenderer") {
                if let Some(pl) = parse_two_row_playlist_item(renderer) {
                    if seen.insert(pl.browse_id.clone()) {
                        out.push(pl);
                    }
                }
            }
            if let Some(renderer) = map.get("musicResponsiveListItemRenderer") {
                if let Some(pl) = parse_responsive_playlist_item(renderer) {
                    if seen.insert(pl.browse_id.clone()) {
                        out.push(pl);
                    }
                }
            }
            for val in map.values() {
                extract_playlists_recursive(val, out, seen);
            }
        }
        Value::Array(arr) => {
            for val in arr {
                extract_playlists_recursive(val, out, seen);
            }
        }
        _ => {}
    }
}

fn normalize_playlist_browse_id(raw_id: &str) -> Option<String> {
    let id = raw_id.trim();
    if id.is_empty()
        || id == "FEmusic_liked_playlists"
        || id == "FEmusic_library_landing"
        || id == "FEmusic_home"
        || id == "FEmusic_explore"
        || id == "FEmusic_new_playlist"
        || id.starts_with("UC")
    {
        return None;
    }

    if id == "FEmusic_liked_videos" || id == "LM" || id == "VLLM" {
        return Some("VLLM".to_string());
    }

    if id.starts_with("VL") || id.starts_with("MPREb_") {
        return Some(id.to_string());
    }

    if id.starts_with("PL") || id.starts_with("RD") || id.starts_with("OLAK") {
        return Some(format!("VL{}", id));
    }

    None
}

fn parse_two_row_playlist_item(renderer: &Value) -> Option<PlaylistItem> {
    // If it has a direct watchEndpoint with a videoId and no browseEndpoint, it's a single track/video, not a playlist
    if renderer
        .pointer("/navigationEndpoint/watchEndpoint/videoId")
        .is_some()
        && renderer
            .pointer("/navigationEndpoint/browseEndpoint/browseId")
            .is_none()
    {
        return None;
    }

    let raw_id = renderer
        .pointer("/navigationEndpoint/browseEndpoint/browseId")
        .or_else(|| renderer.pointer("/title/runs/0/navigationEndpoint/browseEndpoint/browseId"))
        .or_else(|| renderer.pointer("/navigationEndpoint/watchPlaylistEndpoint/playlistId"))
        .or_else(|| {
            renderer.pointer(
                "/thumbnailOverlay/musicItemThumbnailOverlayRenderer/content/musicPlayButtonRenderer/playNavigationEndpoint/watchPlaylistEndpoint/playlistId",
            )
        })
        .or_else(|| {
            renderer.pointer(
                "/thumbnailOverlay/musicItemThumbnailOverlayRenderer/content/musicPlayButtonRenderer/playNavigationEndpoint/watchEndpoint/playlistId",
            )
        })
        .and_then(|v| v.as_str())?;

    let browse_id = normalize_playlist_browse_id(raw_id)?;

    let title = renderer
        .pointer("/title/runs")
        .and_then(|r| r.as_array())
        .map(|runs| join_runs(runs))?;

    if title.is_empty()
        || title.eq_ignore_ascii_case("New playlist")
        || title.eq_ignore_ascii_case("Yeni çalma listesi")
    {
        return None;
    }

    let subtitle = renderer
        .pointer("/subtitle/runs")
        .and_then(|r| r.as_array())
        .map(|runs| {
            runs.iter()
                .filter_map(|r| r.get("text").and_then(|t| t.as_str()))
                .map(|s| s.trim())
                .filter(|st| !st.is_empty() && *st != "•")
                .collect::<Vec<_>>()
                .join(" • ")
        })
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "YouTube Music Çalma Listesi".to_string());

    Some(PlaylistItem {
        browse_id,
        title,
        subtitle,
        track_count: String::new(),
    })
}

fn parse_responsive_playlist_item(renderer: &Value) -> Option<PlaylistItem> {
    // Skip single song rows
    if renderer.pointer("/playlistItemData/videoId").is_some()
        || renderer
            .pointer("/navigationEndpoint/watchEndpoint/videoId")
            .is_some()
    {
        return None;
    }

    let raw_id = renderer
        .pointer("/navigationEndpoint/browseEndpoint/browseId")
        .or_else(|| {
            renderer.pointer(
                "/flexColumns/0/musicResponsiveListItemFlexColumnRenderer/text/runs/0/navigationEndpoint/browseEndpoint/browseId",
            )
        })
        .or_else(|| renderer.pointer("/navigationEndpoint/watchPlaylistEndpoint/playlistId"))
        .or_else(|| {
            renderer.pointer(
                "/overlay/musicItemThumbnailOverlayRenderer/content/musicPlayButtonRenderer/playNavigationEndpoint/watchPlaylistEndpoint/playlistId",
            )
        })
        .and_then(|v| v.as_str())?;

    let browse_id = normalize_playlist_browse_id(raw_id)?;

    let flex_columns = renderer.get("flexColumns")?.as_array()?;
    let title = flex_columns
        .first()
        .and_then(|col| {
            col.pointer("/musicResponsiveListItemFlexColumnRenderer/text/runs")
                .and_then(|r| r.as_array())
        })
        .map(|runs| join_runs(runs))?;

    if title.is_empty()
        || title.eq_ignore_ascii_case("New playlist")
        || title.eq_ignore_ascii_case("Yeni çalma listesi")
    {
        return None;
    }

    let subtitle = flex_columns
        .get(1)
        .and_then(|col| {
            col.pointer("/musicResponsiveListItemFlexColumnRenderer/text/runs")
                .and_then(|r| r.as_array())
        })
        .map(|runs| {
            runs.iter()
                .filter_map(|r| r.get("text").and_then(|t| t.as_str()))
                .map(|s| s.trim())
                .filter(|st| !st.is_empty() && *st != "•")
                .collect::<Vec<_>>()
                .join(" • ")
        })
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "Kütüphane Çalma Listesi".to_string());

    Some(PlaylistItem {
        browse_id,
        title,
        subtitle,
        track_count: String::new(),
    })
}

fn extract_tracks_recursive(
    node: &Value,
    out: &mut Vec<TrackItem>,
    seen: &mut HashSet<String>,
) {
    match node {
        Value::Object(map) => {
            if let Some(renderer) = map.get("musicResponsiveListItemRenderer") {
                if let Some(item) = parse_responsive_list_item(renderer) {
                    if seen.insert(item.video_id.clone()) {
                        out.push(item);
                    }
                }
            }
            if let Some(renderer) = map.get("musicTwoRowItemRenderer") {
                if let Some(item) = parse_two_row_item(renderer) {
                    if seen.insert(item.video_id.clone()) {
                        out.push(item);
                    }
                }
            }
            if let Some(renderer) = map.get("playlistPanelVideoRenderer") {
                if let Some(item) = parse_playlist_panel_video_item(renderer) {
                    if seen.insert(item.video_id.clone()) {
                        out.push(item);
                    }
                }
            }

            for val in map.values() {
                extract_tracks_recursive(val, out, seen);
            }
        }
        Value::Array(arr) => {
            for val in arr {
                extract_tracks_recursive(val, out, seen);
            }
        }
        _ => {}
    }
}

fn parse_playlist_panel_video_item(renderer: &Value) -> Option<TrackItem> {
    let video_id = renderer
        .get("videoId")
        .and_then(|v| v.as_str())
        .or_else(|| {
            renderer
                .pointer("/navigationEndpoint/watchEndpoint/videoId")
                .and_then(|v| v.as_str())
        })?
        .to_string();

    if video_id.is_empty() {
        return None;
    }

    let title = renderer
        .pointer("/title/runs")
        .and_then(|r| r.as_array())
        .map(|runs| join_runs(runs))
        .unwrap_or_else(|| "Unknown Title".to_string());

    let artist = renderer
        .pointer("/longBylineText/runs")
        .or_else(|| renderer.pointer("/shortBylineText/runs"))
        .and_then(|r| r.as_array())
        .map(|runs| {
            runs.iter()
                .filter_map(|r| r.get("text").and_then(|t| t.as_str()))
                .map(|s| s.trim())
                .filter(|s| !s.is_empty() && *s != "•")
                .collect::<Vec<_>>()
                .join(" • ")
        })
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "YouTube Music".to_string());

    let duration_text = renderer
        .pointer("/lengthText/runs")
        .and_then(|r| r.as_array())
        .map(|runs| join_runs(runs))
        .filter(|d| is_duration_format(d))
        .unwrap_or_else(|| "3:30".to_string());

    let duration_secs = parse_duration_secs(&duration_text);
    let thumbnail_url = renderer
        .pointer("/thumbnail/thumbnails")
        .and_then(|t| t.as_array())
        .and_then(|arr| arr.last())
        .and_then(|item| item.get("url"))
        .and_then(|u| u.as_str())
        .unwrap_or("")
        .to_string();

    Some(TrackItem {
        video_id,
        title,
        artist,
        album: String::new(),
        duration_text,
        duration_secs,
        thumbnail_url,
    })
}

fn parse_responsive_list_item(renderer: &Value) -> Option<TrackItem> {
    let video_id = renderer
        .pointer("/playlistItemData/videoId")
        .and_then(|v| v.as_str())
        .or_else(|| {
            renderer
                .pointer("/navigationEndpoint/watchEndpoint/videoId")
                .and_then(|v| v.as_str())
        })
        .or_else(|| {
            renderer
                .pointer("/overlay/musicItemThumbnailOverlayRenderer/content/musicPlayButtonRenderer/playNavigationEndpoint/watchEndpoint/videoId")
                .and_then(|v| v.as_str())
        })?
        .to_string();

    if video_id.is_empty() {
        return None;
    }

    let flex_columns = renderer.get("flexColumns")?.as_array()?;
    let title = flex_columns
        .first()
        .and_then(|col| {
            col.pointer("/musicResponsiveListItemFlexColumnRenderer/text/runs")
                .and_then(|r| r.as_array())
        })
        .map(|runs| join_runs(runs))
        .unwrap_or_else(|| "Unknown Title".to_string());

    let mut artist = String::new();
    let mut album = String::new();
    let mut duration_text = String::new();

    if let Some(second_col) = flex_columns.get(1) {
        if let Some(runs) = second_col
            .pointer("/musicResponsiveListItemFlexColumnRenderer/text/runs")
            .and_then(|r| r.as_array())
        {
            let parts: Vec<String> = runs
                .iter()
                .filter_map(|r| r.get("text").and_then(|t| t.as_str()))
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty() && s != "•" && s != " & " && s != ",")
                .collect();

            for part in parts {
                if is_duration_format(&part) {
                    duration_text = part;
                } else if artist.is_empty()
                    && !part.eq_ignore_ascii_case("Song")
                    && !part.eq_ignore_ascii_case("Video")
                    && !part.eq_ignore_ascii_case("Şarkı")
                {
                    artist = part;
                } else if album.is_empty()
                    && !part.eq_ignore_ascii_case("Song")
                    && !part.eq_ignore_ascii_case("Video")
                    && !part.eq_ignore_ascii_case("Şarkı")
                {
                    album = part;
                }
            }
        }
    }

    if duration_text.is_empty() {
        if let Some(fixed_cols) = renderer.get("fixedColumns").and_then(|c| c.as_array()) {
            if let Some(first_fixed) = fixed_cols.first() {
                if let Some(runs) = first_fixed
                    .pointer("/musicResponsiveListItemFixedColumnRenderer/text/runs")
                    .and_then(|r| r.as_array())
                {
                    let candidate = join_runs(runs);
                    if is_duration_format(&candidate) {
                        duration_text = candidate;
                    }
                }
            }
        }
    }

    if artist.is_empty() {
        artist = "YouTube Music".to_string();
    }
    if duration_text.is_empty() {
        duration_text = "3:30".to_string();
    }

    let duration_secs = parse_duration_secs(&duration_text);
    let thumbnail_url = extract_thumbnail(renderer);

    Some(TrackItem {
        video_id,
        title,
        artist,
        album,
        duration_text,
        duration_secs,
        thumbnail_url,
    })
}

fn parse_two_row_item(renderer: &Value) -> Option<TrackItem> {
    // Make sure we don't accidentally treat a playlist card as a single track
    if renderer
        .pointer("/navigationEndpoint/browseEndpoint/browseId")
        .is_some()
    {
        return None;
    }

    let video_id = renderer
        .pointer("/navigationEndpoint/watchEndpoint/videoId")
        .and_then(|v| v.as_str())
        .or_else(|| {
            renderer
                .pointer("/thumbnailOverlay/musicItemThumbnailOverlayRenderer/content/musicPlayButtonRenderer/playNavigationEndpoint/watchEndpoint/videoId")
                .and_then(|v| v.as_str())
        })?
        .to_string();

    if video_id.is_empty() {
        return None;
    }

    let title = renderer
        .pointer("/title/runs")
        .and_then(|r| r.as_array())
        .map(|runs| join_runs(runs))
        .unwrap_or_else(|| "Unknown Title".to_string());

    let subtitle = renderer
        .pointer("/subtitle/runs")
        .and_then(|r| r.as_array())
        .map(|runs| {
            runs.iter()
                .filter_map(|r| r.get("text").and_then(|t| t.as_str()))
                .filter(|s| {
                    let st = s.trim();
                    !st.is_empty()
                        && st != "•"
                        && !st.eq_ignore_ascii_case("Song")
                        && !st.eq_ignore_ascii_case("Video")
                        && !st.eq_ignore_ascii_case("Şarkı")
                })
                .collect::<Vec<_>>()
                .join(" ")
        })
        .unwrap_or_else(|| "YouTube Music".to_string());

    let thumbnail_url = extract_thumbnail(renderer);

    Some(TrackItem {
        video_id,
        title,
        artist: if subtitle.is_empty() {
            "YouTube Music".to_string()
        } else {
            subtitle
        },
        album: String::new(),
        duration_text: "3:30".to_string(),
        duration_secs: 210,
        thumbnail_url,
    })
}

fn join_runs(runs: &[Value]) -> String {
    runs.iter()
        .filter_map(|r| r.get("text").and_then(|t| t.as_str()))
        .collect::<Vec<_>>()
        .join("")
        .trim()
        .to_string()
}

fn extract_thumbnail(renderer: &Value) -> String {
    renderer
        .pointer("/thumbnail/musicThumbnailRenderer/thumbnail/thumbnails")
        .or_else(|| renderer.pointer("/thumbnailRenderer/musicThumbnailRenderer/thumbnail/thumbnails"))
        .and_then(|t| t.as_array())
        .and_then(|arr| arr.last())
        .and_then(|item| item.get("url"))
        .and_then(|u| u.as_str())
        .unwrap_or("")
        .to_string()
}

fn is_duration_format(s: &str) -> bool {
    let parts: Vec<&str> = s.trim().split(':').collect();
    if parts.len() < 2 || parts.len() > 3 {
        return false;
    }
    parts
        .iter()
        .all(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()))
}

pub fn parse_duration_secs(duration_text: &str) -> u64 {
    let parts: Vec<u64> = duration_text
        .trim()
        .split(':')
        .filter_map(|p| p.parse::<u64>().ok())
        .collect();

    match parts.as_slice() {
        [m, s] => m * 60 + s,
        [h, m, s] => h * 3600 + m * 60 + s,
        _ => 210,
    }
}
