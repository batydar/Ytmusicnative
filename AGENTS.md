# YouTube Music Native Client - System Architecture & Specification

You are a Principal Rust and Systems Engineer. Your objective is to build a high-performance, modular, native YouTube Music desktop player for Windows 10/11.

The application must run natively without Electron, Chromium, or persistent WebViews, maintaining a total runtime memory footprint under 60–80 MB of RAM while streaming and processing audio filters.

---

## 1. Technical Stack & Core Constraints

1. **Language & Runtime:** Rust (latest stable) with `tokio` multi-threaded runtime.
2. **GUI Framework:** `Slint` (v1.5+), rendering natively via Direct3D/Software backend without web wrappers.
3. **Audio Subsystem (Strict Rule):**
   - DO NOT bind `libmpv` via C/FFI to avoid MSVC/MinGW toolchain linker errors on Windows.
   - Spawn an external headless `bin/mpv.exe` child process hidden from the taskbar (`CREATE_NO_WINDOW = 0x08000000`).
   - Control playback and filters strictly over a Windows Named Pipe (`\\.\pipe\ytmusic_mpv`) using MPV's JSON-IPC protocol.
   - Launch MPV with initial flags:
     `--idle=yes --no-video --demuxer-max-bytes=25M --demuxer-max-back-bytes=5M --input-ipc-server=\\.\pipe\ytmusic_mpv`
4. **Data Layer (InnerTube API):**
   - Use `reqwest` (with `rustls-tls` to avoid OpenSSL dependencies) and `serde_json`.
   - Client emulation: `WEB_REMIX` (`clientName: "WEB_REMIX"`, `clientVersion: "1.20240101.01.00"`).
   - Recommendations: `POST https://music.youtube.com/youtubei/v1/browse` (`{"browseId": "FEmusic_home"}`).
   - Search: `POST https://music.youtube.com/youtubei/v1/search`.
   - Auth: Load session cookies from `%APPDATA%/YTMusicNative/cookies.txt` if present. Generate `SAPISIDHASH` via SHA-1: `sha1("{timestamp} {sapisid} https://music.youtube.com")`.

---

## 2. Dynamic Download Engine & Cache-First Playback

### A. Custom Directory Selection (`src/config.rs`)
- Default download path: `%USERPROFILE%/Music/YTMusicDownloads`.
- Persist user settings in `%APPDATA%/YTMusicNative/settings.json`.
- UI Action: Provide a "Change Download Folder" button in the UI that triggers `rfd::AsyncFileDialog::new().pick_folder()` on a non-blocking task. Save the chosen directory immediately to `settings.json`.

### B. Download Formatting & Execution (`src/services/downloader.rs`)
- Invoke `bin/yt-dlp.exe` asynchronously with `CREATE_NO_WINDOW` (`0x08000000`).
- Output file naming template:
  `%(artist,uploader)s - %(title)s [%(id)s].%(ext)s`
- Full command arguments:
  `-f "ba" -x --audio-format opus --embed-thumbnail --embed-metadata -o "<CUSTOM_DOWNLOAD_DIR>/%(artist,uploader)s - %(title)s [%(id)s].%(ext)s" https://music.youtube.com/watch?v=<VIDEO_ID>`
- Stream stdout progress percentages back to the UI via a Tokio channel.

### C. Cache-First Playback Logic
- Whenever a track is requested to play:
  1. Scan `<CUSTOM_DOWNLOAD_DIR>` for any `.opus` file whose filename contains `[<video_id>]`.
  2. **If found locally:** Send `{"command": ["loadfile", "<LOCAL_ABSOLUTE_PATH>", "replace"]}` to MPV.
  3. **If not found:** Send `{"command": ["loadfile", "https://music.youtube.com/watch?v=<video_id>", "replace"]}` to MPV.

---

## 3. Audio Processing & Service Modules

### A. Equalizer & Loudness Normalization (`src/audio/`)
- Apply filters dynamically in real time via MPV IPC:
  `{"command": ["set_property", "af", "<FILTER_CHAIN>"]}`
- Built-in EQ Presets:
  - `Flat`: `""`
  - `Bass Boost`: `lavfi=[superequalizer=1b=6:2b=5:3b=3:4b=1:5b=0:6b=0:7b=0:8b=0:9b=0:10b=0:11b=0:12b=0:13b=0:14b=0:15b=0:16b=0:17b=0]`
  - `Treble Boost`: `lavfi=[superequalizer=1b=0:2b=0:3b=0:4b=0:5b=0:6b=0:7b=0:8b=0:9b=0:10b=0:11b=1:12b=2:13b=4:14b=5:15b=6:16b=6:17b=6]`
  - `V-Shape`: `lavfi=[superequalizer=1b=5:2b=4:3b=2:4b=0:5b=-2:6b=-3:7b=-3:8b=-2:9b=0:10b=1:11b=3:12b=4:13b=5:14b=5:15b=5:16b=4:17b=4]`
  - `Vocal`: `lavfi=[superequalizer=1b=-4:2b=-3:3b=-2:4b=0:5b=2:6b=4:7b=5:8b=5:9b=4:10b=3:11b=1:12b=0:13b=0:14b=-1:15b=-2:16b=-2:17b=-3]`
- Loudness Normalization toggle:
  `lavfi=[dynaudnorm=f=150:g=15:m=10.0]`
- If both EQ and Normalization are active, join them with a comma `,` inside the `af` property. Save active state in `settings.json`.

### B. Synchronized Lyrics (`src/services/lyrics.rs`)
- Fetch time-synced lyrics from LRCLIB:
  `GET https://lrclib.net/api/get?track_name={title}&artist_name={artist}&duration={duration}`
- Parse `[mm:ss.xx]` timestamps and synchronize the active line highlight in the Slint UI with MPV's `time-pos` property.

### C. SponsorBlock (`src/services/sponsorblock.rs`)
- Fetch non-music intervals:
  `GET https://sponsor.ajay.app/api/skipSegments?videoID={id}&categories=["music_offtopic"]`
- Automatically issue an MPV `seek` command when `time-pos` enters a `[start, end]` segment.

### D. Desktop Integrations (`src/integrations/`)
- **Windows SMTC (`smtc.rs`):** Bind to Windows `SystemMediaTransportControls` via `windows-rs` to show metadata on the Windows volume flyout and handle hardware media keys (Play, Pause, Next, Previous).
- **System Tray (`tray.rs`):** Implement tray icon via `tray-icon` crate with Show/Hide, Play/Pause, Next, and Quit actions.
- **Discord RPC (`discord.rs`):** Broadcast current song, artist, and elapsed time via `discord-rich-presence`.

---

## 4. Target Directory Structure

```text
yt-music-native/
├── AGENTS.md
├── Cargo.toml
├── build.rs
├── bin/
│   ├── mpv.exe
│   └── yt-dlp.exe
├── ui/
│   ├── app.slint
│   └── components/
│       ├── sidebar.slint
│       ├── home_view.slint
│       ├── search_view.slint
│       ├── downloads_view.slint
│       ├── lyrics_panel.slint
│       ├── eq_popover.slint
│       └── player_bar.slint
└── src/
    ├── main.rs
    ├── config.rs
    ├── mpv/
    │   ├── mod.rs
    │   ├── daemon.rs
    │   ├── ipc.rs
    │   └── events.rs
    ├── audio/
    │   ├── mod.rs
    │   ├── equalizer.rs
    │   └── normalization.rs
    ├── services/
    │   ├── mod.rs
    │   ├── innertube.rs
    │   ├── downloader.rs
    │   ├── lyrics.rs
    │   └── sponsorblock.rs
    └── integrations/
        ├── mod.rs
        ├── smtc.rs
        ├── tray.rs
        └── discord.rs
```

---

## 5. Required Dependencies (`Cargo.toml`)

```toml
[package]
name = "yt-music-native"
version = "0.1.0"
edition = "2021"

[dependencies]
slint = "1.5"
tokio = { version = "1.36", features = ["full"] }
reqwest = { version = "0.11", default-features = false, features = ["json", "rustls-tls"] }
serde = { version = "1.0", features = ["derive"] }
serde_json = "1.0"
interprocess = "1.2"
sha1 = "0.10"
directories = "5.0"
tray-icon = "0.14"
discord-rich-presence = "0.1"
rfd = "0.14"
regex = "1.10"
walkdir = "2.4"
windows = { version = "0.52", features = [
    "Media", 
    "Media_Playback", 
    "Storage_Streams",
    "Foundation"
]}

[build-dependencies]
slint-build = "1.5"
```

---

## 6. Acceptance Criteria
1. Project compiles cleanly with `cargo build --release` on Windows without manual C/C++ linker setups.
2. External `mpv.exe` and `yt-dlp.exe` processes run with `CREATE_NO_WINDOW` (zero flashing console windows).
3. Total working set RAM remains under 80 MB during active streaming with EQ and Normalization active.
4. Selecting a custom download folder via the native Windows dialog updates `settings.json` and immediately applies to new downloads and local cache lookups.
5. Tracks already downloaded as `<Artist> - <Title> [<id>].opus` in the custom folder play directly from disk with zero network buffering.
6. Switching Equalizer presets or toggling Loudness Normalization updates MPV audio filters seamlessly without restarting the track.