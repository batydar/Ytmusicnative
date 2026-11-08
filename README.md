# YouTube Music Native Client (Windows)

A lightweight, native YouTube Music desktop client for Windows written in Rust with **Slint UI** and **headless MPV IPC**, without Electron or WebViews.

## Features
- **Zero WebViews:** Slint UI native Direct3D rendering with sub-80MB memory footprint.
- **Headless MPV Audio Engine:** Controlled via Windows Named Pipe JSON-IPC (`\\.\pipe\ytmusic_mpv`).
- **InnerTube API:** `WEB_REMIX` client emulation, browse feed (`FEmusic_home`), search, and `cookies.txt` support with `SAPISIDHASH`.
- **Cache-First Playback:** Downloads tracks via `yt-dlp` as Opus format and plays cached files with 0 network latency.
- **Real-Time DSP:** Superequalizer presets (Flat, Bass Boost, Treble Boost, V-Shape, Vocal) + Dynaudnorm loudness normalization.
- **Synchronized Lyrics:** Time-synced LRC from LRCLIB.
- **SponsorBlock:** Automatically skips non-music intervals (`music_offtopic`).
- **Desktop Integrations:** Windows SMTC media keys, System Tray, and Discord Rich Presence.

## Setup
Place the following binaries in the `bin/` directory:
- `mpv.exe`
- `yt-dlp.exe`
- `ffmpeg.exe` (optional, for metadata/opus extraction)

## Build & Run
```powershell
cargo run --release
```
