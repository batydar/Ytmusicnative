use serde_json::{json, Value};
use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::sync::mpsc::{self, UnboundedSender};

/// Verified YouTube Music Discord Application ID ("YouTube Music")
pub const DISCORD_APP_ID: &str = "463151177836658699";

#[derive(Debug, Clone)]
enum DiscordCommand {
    UpdateTrack {
        video_id: String,
        title: String,
        artist: String,
        elapsed_secs: f64,
        duration_secs: f64,
        is_playing: bool,
    },
    Clear,
}

#[derive(Clone)]
pub struct DiscordRpcClient {
    tx: UnboundedSender<DiscordCommand>,
}

struct DiscordIpcPipe {
    pipe: File,
}

impl DiscordIpcPipe {
    fn connect() -> Option<Self> {
        for i in 0..10 {
            let pipe_name = format!(r"\\.\pipe\discord-ipc-{}", i);
            if let Ok(pipe) = OpenOptions::new().read(true).write(true).open(&pipe_name) {
                let mut conn = Self { pipe };
                let handshake = json!({
                    "v": 1,
                    "client_id": DISCORD_APP_ID
                });
                if conn.send_packet(0, &handshake).is_ok() {
                    if let Ok((_, resp)) = conn.read_packet() {
                        if resp.get("evt").and_then(|v| v.as_str()) == Some("READY") {
                            return Some(conn);
                        }
                    }
                }
            }
        }
        None
    }

    fn send_packet(&mut self, opcode: u32, payload: &Value) -> Result<(), String> {
        let data = serde_json::to_vec(payload).map_err(|e| e.to_string())?;
        let mut header = [0u8; 8];
        header[0..4].copy_from_slice(&opcode.to_le_bytes());
        header[4..8].copy_from_slice(&(data.len() as u32).to_le_bytes());
        self.pipe
            .write_all(&header)
            .map_err(|e| e.to_string())?;
        self.pipe.write_all(&data).map_err(|e| e.to_string())?;
        self.pipe.flush().map_err(|e| e.to_string())
    }

    fn read_packet(&mut self) -> Result<(u32, Value), String> {
        let mut header = [0u8; 8];
        self.pipe
            .read_exact(&mut header)
            .map_err(|e| e.to_string())?;
        let opcode = u32::from_le_bytes([header[0], header[1], header[2], header[3]]);
        let len = u32::from_le_bytes([header[4], header[5], header[6], header[7]]) as usize;
        if len > 1024 * 1024 {
            return Err("Discord IPC packet too large".to_string());
        }
        let mut buf = vec![0u8; len];
        self.pipe.read_exact(&mut buf).map_err(|e| e.to_string())?;
        let val: Value = serde_json::from_slice(&buf).map_err(|e| e.to_string())?;
        Ok((opcode, val))
    }

    fn set_activity(&mut self, activity: Option<Value>, nonce: u64) -> Result<(), String> {
        let pid = std::process::id();
        let payload = json!({
            "cmd": "SET_ACTIVITY",
            "nonce": nonce.to_string(),
            "args": {
                "pid": pid,
                "activity": activity
            }
        });
        self.send_packet(1, &payload)?;
        let _ = self.read_packet()?;
        Ok(())
    }
}

impl DiscordRpcClient {
    pub fn new() -> Self {
        let (tx, mut rx) = mpsc::unbounded_channel::<DiscordCommand>();

        std::thread::spawn(move || {
            let mut conn: Option<DiscordIpcPipe> = None;
            let mut nonce: u64 = 1;

            while let Some(cmd) = rx.blocking_recv() {
                // Coalesce queued updates so we always send the freshest state
                let mut latest_cmd = cmd;
                while let Ok(newer) = rx.try_recv() {
                    latest_cmd = newer;
                }

                if conn.is_none() {
                    conn = DiscordIpcPipe::connect();
                }

                let Some(pipe) = conn.as_mut() else {
                    continue;
                };

                nonce = nonce.wrapping_add(1);

                match latest_cmd {
                    DiscordCommand::UpdateTrack {
                        video_id,
                        title,
                        artist,
                        elapsed_secs,
                        duration_secs,
                        is_playing,
                    } => {
                        let now_secs = SystemTime::now()
                            .duration_since(UNIX_EPOCH)
                            .map(|d| d.as_secs() as i64)
                            .unwrap_or(0);
                        let start_ts = (now_secs - elapsed_secs.max(0.0) as i64).max(0);

                        let state_text = if is_playing {
                            artist.clone()
                        } else {
                            format!("⏸ Duraklatıldı • {}", artist)
                        };

                        let clean_title = if title.chars().count() < 2 {
                            format!("{} ", title)
                        } else {
                            title.clone()
                        };

                        let clean_state = if state_text.chars().count() < 2 {
                            format!("{} ", state_text)
                        } else {
                            state_text
                        };

                        let mut activity_obj = json!({
                            "type": 2, // 2 = Listening to YouTube Music
                            "details": clean_title,
                            "state": clean_state,
                        });

                        if is_playing {
                            if duration_secs > 1.0 {
                                let end_ts = start_ts + (duration_secs as i64);
                                activity_obj["timestamps"] = json!({
                                    "start": start_ts,
                                    "end": end_ts
                                });
                            } else {
                                activity_obj["timestamps"] = json!({
                                    "start": start_ts
                                });
                            }
                        }

                        if !video_id.trim().is_empty() {
                            let thumb_url =
                                format!("https://i.ytimg.com/vi/{}/hqdefault.jpg", video_id.trim());
                            let watch_url =
                                format!("https://music.youtube.com/watch?v={}", video_id.trim());

                            activity_obj["assets"] = json!({
                                "large_image": thumb_url,
                                "large_text": format!("{} — {}", title, artist)
                            });

                            activity_obj["buttons"] = json!([
                                {
                                    "label": "YouTube Music'te Dinle",
                                    "url": watch_url
                                }
                            ]);
                        }

                        if pipe.set_activity(Some(activity_obj), nonce).is_err() {
                            conn = DiscordIpcPipe::connect();
                        }
                    }
                    DiscordCommand::Clear => {
                        let _ = pipe.set_activity(None, nonce);
                        conn = None;
                    }
                }
            }
        });

        Self { tx }
    }

    pub fn update_presence(
        &self,
        video_id: &str,
        title: &str,
        artist: &str,
        elapsed_secs: f64,
        duration_secs: f64,
        is_playing: bool,
    ) {
        let _ = self.tx.send(DiscordCommand::UpdateTrack {
            video_id: video_id.to_string(),
            title: title.to_string(),
            artist: artist.to_string(),
            elapsed_secs,
            duration_secs,
            is_playing,
        });
    }

    pub fn clear(&self) {
        let _ = self.tx.send(DiscordCommand::Clear);
    }
}
