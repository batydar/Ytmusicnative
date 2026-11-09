use crate::config::{resolve_binary_path, strip_unc_prefix};
use crate::mpv::CREATE_NO_WINDOW;
use regex::Regex;
use std::fs;
use std::path::Path;
use std::process::Stdio;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;
use tokio::sync::mpsc::UnboundedSender;

#[derive(Debug, Clone)]
pub enum DownloadEvent {
    Started {
        video_id: String,
        title: String,
    },
    Progress {
        video_id: String,
        title: String,
        percent: f32,
        status_text: String,
    },
    Completed {
        video_id: String,
        title: String,
    },
    Failed {
        video_id: String,
        title: String,
        error: String,
    },
}

/// Invoke `bin/yt-dlp.exe` asynchronously with `CREATE_NO_WINDOW` (`0x08000000`).
/// Template: `<CUSTOM_DOWNLOAD_DIR>/%(artist,uploader)s - %(title)s [%(id)s].%(ext)s`
/// Arguments:
/// `-f "ba" -x --audio-format opus --embed-thumbnail --embed-metadata -o "<CUSTOM_DOWNLOAD_DIR>/%(artist,uploader)s - %(title)s [%(id)s].%(ext)s" https://music.youtube.com/watch?v=<VIDEO_ID>`
pub fn spawn_download_task(
    video_id: String,
    title: String,
    download_dir: &Path,
    tx: UnboundedSender<DownloadEvent>,
) {
    let _ = fs::create_dir_all(download_dir);
    let abs_download_dir = fs::canonicalize(download_dir)
        .map(strip_unc_prefix)
        .unwrap_or_else(|_| download_dir.to_path_buf());

    let dir_str = abs_download_dir
        .to_string_lossy()
        .trim_end_matches(['\\', '/'])
        .replace('\\', "/");
    let output_template = format!("{}/%(artist,uploader)s - %(title)s [%(id)s].%(ext)s", dir_str);
    let url = format!("https://music.youtube.com/watch?v={}", video_id);
    let ytdlp_bin = resolve_binary_path("yt-dlp.exe");

    tokio::spawn(async move {
        let _ = tx.send(DownloadEvent::Started {
            video_id: video_id.clone(),
            title: title.clone(),
        });

        let mut cmd = Command::new(&ytdlp_bin);
        cmd.arg("-f")
            .arg("ba")
            .arg("-x")
            .arg("--audio-format")
            .arg("opus")
            .arg("--embed-thumbnail")
            .arg("--embed-metadata")
            .arg("--windows-filenames")
            .arg("--no-mtime")
            .arg("--no-part")
            .arg("--encoding")
            .arg("utf-8")
            .arg("--newline")
            .arg("--no-warnings")
            .env("PYTHONIOENCODING", "utf-8")
            .env("PYTHONUTF8", "1");

        if let Some(bin_dir) = ytdlp_bin.parent() {
            cmd.arg("--ffmpeg-location").arg(bin_dir);
        }

        cmd.arg("-o")
            .arg(&output_template)
            .arg(&url)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        #[cfg(target_os = "windows")]
        {
            cmd.creation_flags(CREATE_NO_WINDOW);
        }

        let mut child = match cmd.spawn() {
            Ok(c) => c,
            Err(e) => {
                let _ = tx.send(DownloadEvent::Failed {
                    video_id,
                    title,
                    error: format!("Failed to spawn '{}': {}", ytdlp_bin.display(), e),
                });
                return;
            }
        };

        // Drain stderr using raw bytes (`read_until`) so non-UTF8 cp1254 bytes never break the pipe!
        let stderr_handle = child.stderr.take().map(|stderr| {
            tokio::spawn(async move {
                let mut reader = BufReader::new(stderr);
                let mut buf = Vec::with_capacity(512);
                let mut last_err = String::new();
                while let Ok(n) = reader.read_until(b'\n', &mut buf).await {
                    if n == 0 {
                        break;
                    }
                    let line = String::from_utf8_lossy(&buf);
                    let trimmed = line.trim();
                    if !trimmed.is_empty() {
                        last_err = trimmed.to_string();
                    }
                    buf.clear();
                }
                last_err
            })
        });

        let percent_re = Regex::new(r"(\d+(?:\.\d+)?)%").expect("valid regex");

        // Read stdout using raw bytes (`read_until`) so non-UTF8 bytes never drop `reader` early
        // (dropping `stdout` early closes the pipe and causes Python to fail with OSError: [Errno 22] Invalid argument)
        if let Some(stdout) = child.stdout.take() {
            let mut reader = BufReader::new(stdout);
            let mut buf = Vec::with_capacity(512);
            while let Ok(n) = reader.read_until(b'\n', &mut buf).await {
                if n == 0 {
                    break;
                }
                let line = String::from_utf8_lossy(&buf);
                if let Some(caps) = percent_re.captures(&line) {
                    if let Some(m) = caps.get(1) {
                        if let Ok(pct) = m.as_str().parse::<f32>() {
                            let clamped = pct.clamp(0.0, 100.0);
                            let _ = tx.send(DownloadEvent::Progress {
                                video_id: video_id.clone(),
                                title: title.clone(),
                                percent: clamped,
                                status_text: format!("Downloading... {:.1}%", clamped),
                            });
                        }
                    }
                } else if line.contains("[ExtractAudio]") || line.contains("[Metadata]") {
                    let _ = tx.send(DownloadEvent::Progress {
                        video_id: video_id.clone(),
                        title: title.clone(),
                        percent: 99.0,
                        status_text: "Processing Opus audio & metadata...".to_string(),
                    });
                }
                buf.clear();
            }
        }

        let status_res = child.wait().await;
        let last_stderr = if let Some(h) = stderr_handle {
            h.await.unwrap_or_default()
        } else {
            String::new()
        };

        // Check if the .opus file with [<video_id>] now exists on disk
        let needle = format!("[{}]", video_id);
        let file_created = fs::read_dir(&abs_download_dir)
            .ok()
            .map(|entries| {
                entries.filter_map(Result::ok).any(|e| {
                    let p = e.path();
                    let is_opus = p
                        .extension()
                        .and_then(|x| x.to_str())
                        .map(|x| x.eq_ignore_ascii_case("opus"))
                        .unwrap_or(false);
                    let matches_id = p
                        .file_name()
                        .and_then(|n| n.to_str())
                        .map(|n| n.contains(&needle))
                        .unwrap_or(false);
                    is_opus && matches_id
                })
            })
            .unwrap_or(false);

        match status_res {
            Ok(status) if status.success() || file_created => {
                let _ = tx.send(DownloadEvent::Completed { video_id, title });
            }
            Ok(status) => {
                let err_msg = if !last_stderr.is_empty() {
                    last_stderr
                } else {
                    format!("yt-dlp exited with status: {}", status)
                };
                let _ = tx.send(DownloadEvent::Failed {
                    video_id,
                    title,
                    error: err_msg,
                });
            }
            Err(e) => {
                let _ = tx.send(DownloadEvent::Failed {
                    video_id,
                    title,
                    error: format!("Error waiting for yt-dlp: {}", e),
                });
            }
        }
    });
}
