use crate::config::AppSettings;
use crate::mpv::daemon::MPV_PIPE_NAME;
use crate::mpv::events::{parse_mpv_event_line, MpvEvent};
use serde_json::{json, Value};
use std::path::PathBuf;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::windows::named_pipe::ClientOptions;
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};

#[derive(Debug, Clone)]
pub enum PlaybackSource {
    LocalCache(PathBuf),
    RemoteStream(String),
}

#[derive(Clone)]
pub struct MpvIpcClient {
    cmd_tx: UnboundedSender<String>,
}

impl MpvIpcClient {
    /// Spawn the Named Pipe IPC loop connected to `\\.\pipe\ytmusic_mpv`
    /// and return `(MpvIpcClient, UnboundedReceiver<MpvEvent>)`.
    pub fn connect_and_spawn() -> (Self, UnboundedReceiver<MpvEvent>) {
        let (cmd_tx, mut cmd_rx) = mpsc::unbounded_channel::<String>();
        let (event_tx, event_rx) = mpsc::unbounded_channel::<MpvEvent>();

        tokio::spawn(async move {
            loop {
                let pipe_result = ClientOptions::new().open(MPV_PIPE_NAME);
                match pipe_result {
                    Ok(pipe) => {
                        let (reader, mut writer) = tokio::io::split(pipe);
                        let mut lines = BufReader::new(reader).lines();

                        // Subscribe to real-time MPV properties immediately on connect
                        let initial_subscriptions = [
                            json!({"command": ["observe_property", 1, "time-pos"]}),
                            json!({"command": ["observe_property", 2, "duration"]}),
                            json!({"command": ["observe_property", 3, "pause"]}),
                            json!({"command": ["observe_property", 4, "media-title"]}),
                            json!({"command": ["observe_property", 5, "eof-reached"]}),
                        ];

                        for sub in &initial_subscriptions {
                            let payload = format!("{}\n", sub);
                            let _ = writer.write_all(payload.as_bytes()).await;
                        }
                        let _ = writer.flush().await;

                        loop {
                            tokio::select! {
                                maybe_cmd = cmd_rx.recv() => {
                                    match maybe_cmd {
                                        Some(mut cmd_str) => {
                                            if !cmd_str.ends_with('\n') {
                                                cmd_str.push('\n');
                                            }
                                            if writer.write_all(cmd_str.as_bytes()).await.is_err() {
                                                break;
                                            }
                                            let _ = writer.flush().await;
                                        }
                                        None => return,
                                    }
                                }
                                line_res = lines.next_line() => {
                                    match line_res {
                                        Ok(Some(line)) => {
                                            if let Some(evt) = parse_mpv_event_line(&line) {
                                                let _ = event_tx.send(evt);
                                            }
                                        }
                                        Ok(None) | Err(_) => {
                                            break;
                                        }
                                    }
                                }
                            }
                        }
                    }
                    Err(_) => {
                        tokio::time::sleep(Duration::from_millis(150)).await;
                    }
                }
            }
        });

        (Self { cmd_tx }, event_rx)
    }

    pub fn send_raw_json(&self, command: Value) -> Result<(), String> {
        let serialized = serde_json::to_string(&command)
            .map_err(|e| format!("Failed to serialize MPV IPC JSON: {}", e))?;
        self.cmd_tx
            .send(serialized)
            .map_err(|e| format!("MPV IPC channel closed: {}", e))
    }

    /// Cache-First Playback Logic:
    /// 1. Scan `<CUSTOM_DOWNLOAD_DIR>` for any `.opus` file whose filename contains `[<video_id>]`.
    /// 2. If found locally: Send `{"command": ["loadfile", "<LOCAL_ABSOLUTE_PATH>", "replace"]}` to MPV.
    /// 3. If not found: Send `{"command": ["loadfile", "https://music.youtube.com/watch?v=<video_id>", "replace"]}` to MPV.
    pub fn play_track(
        &self,
        video_id: &str,
        settings: &AppSettings,
    ) -> Result<PlaybackSource, String> {
        if let Some(local_path) = settings.find_cached_opus(video_id) {
            let local_str = local_path.to_string_lossy().to_string();
            let payload = json!({
                "command": ["loadfile", local_str, "replace"]
            });
            self.send_raw_json(payload)?;
            self.set_pause(false)?;
            Ok(PlaybackSource::LocalCache(local_path))
        } else {
            let url = format!("https://music.youtube.com/watch?v={}", video_id);
            let payload = json!({
                "command": ["loadfile", url, "replace"]
            });
            self.send_raw_json(payload)?;
            self.set_pause(false)?;
            Ok(PlaybackSource::RemoteStream(url))
        }
    }

    pub fn load_file(&self, target: &str) -> Result<(), String> {
        let payload = json!({
            "command": ["loadfile", target, "replace"]
        });
        self.send_raw_json(payload)?;
        self.set_pause(false)
    }

    pub fn set_pause(&self, paused: bool) -> Result<(), String> {
        let payload = json!({
            "command": ["set_property", "pause", paused]
        });
        self.send_raw_json(payload)
    }

    pub fn toggle_pause(&self) -> Result<(), String> {
        let payload = json!({
            "command": ["cycle", "pause"]
        });
        self.send_raw_json(payload)
    }

    pub fn stop(&self) -> Result<(), String> {
        let payload = json!({
            "command": ["stop"]
        });
        self.send_raw_json(payload)
    }

    pub fn seek_absolute(&self, position_secs: f64) -> Result<(), String> {
        let payload = json!({
            "command": ["seek", position_secs, "absolute"]
        });
        self.send_raw_json(payload)
    }

    pub fn set_volume(&self, volume: f64) -> Result<(), String> {
        let clamped = volume.clamp(0.0, 100.0);
        let payload = json!({
            "command": ["set_property", "volume", clamped]
        });
        self.send_raw_json(payload)
    }

    pub fn set_audio_filters(&self, filter_chain: &str) -> Result<(), String> {
        let payload = json!({
            "command": ["set_property", "af", filter_chain]
        });
        self.send_raw_json(payload)
    }

    pub fn quit(&self) -> Result<(), String> {
        let payload = json!({
            "command": ["quit"]
        });
        self.send_raw_json(payload)
    }
}
