use crate::config::resolve_binary_path;
use std::io::Write;
use std::process::{Child, Command, Stdio};
use std::time::Duration;

#[cfg(target_os = "windows")]
use std::os::windows::process::CommandExt;

pub const CREATE_NO_WINDOW: u32 = 0x08000000;
pub const MPV_PIPE_NAME: &str = r"\\.\pipe\ytmusic_mpv";

pub struct MpvDaemon {
    child: Option<Child>,
}

impl MpvDaemon {
    pub fn new() -> Self {
        Self { child: None }
    }

    /// If a stale `mpv.exe` from a previous session is holding `\\.\pipe\ytmusic_mpv`,
    /// send `quit` over the pipe so our fresh daemon can bind cleanly.
    fn terminate_stale_pipe_owner() {
        if let Ok(mut pipe) = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(MPV_PIPE_NAME)
        {
            let _ = pipe.write_all(b"{\"command\":[\"quit\"]}\n");
            let _ = pipe.flush();
            std::thread::sleep(Duration::from_millis(180));
        }
    }

    /// Spawn headless `bin/mpv.exe` child process hidden from taskbar (`CREATE_NO_WINDOW = 0x08000000`).
    pub fn start(&mut self) -> Result<(), String> {
        if let Some(child) = &mut self.child {
            if let Ok(None) = child.try_wait() {
                return Ok(());
            }
        }

        Self::terminate_stale_pipe_owner();

        let mpv_bin = resolve_binary_path("mpv.exe");

        let mut cmd = Command::new(&mpv_bin);
        cmd.arg("--idle=yes")
            .arg("--no-video")
            .arg("--force-window=no")
            .arg("--vo=null")
            .arg("--demuxer-max-bytes=25M")
            .arg("--demuxer-max-back-bytes=5M")
            .arg(format!("--input-ipc-server={}", MPV_PIPE_NAME))
            .arg("--ytdl=yes")
            .arg("--ytdl-format=ba[acodec=opus]/ba/best")
            .arg("--script-opts=ytdl_hook-ytdl_path=yt-dlp.exe")
            .arg("--ytdl-raw-options=no-playlist=")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());

        if let Some(bin_dir) = mpv_bin.parent() {
            cmd.current_dir(bin_dir);
            if let Ok(current_path) = std::env::var("PATH") {
                cmd.env("PATH", format!("{};{}", bin_dir.display(), current_path));
            }
        }

        #[cfg(target_os = "windows")]
        {
            cmd.creation_flags(CREATE_NO_WINDOW);
        }

        let child = cmd.spawn().map_err(|e| {
            format!(
                "Failed to spawn headless mpv daemon at '{}': {}",
                mpv_bin.display(),
                e
            )
        })?;

        self.child = Some(child);
        Ok(())
    }

    pub fn is_running(&mut self) -> bool {
        if let Some(child) = &mut self.child {
            matches!(child.try_wait(), Ok(None))
        } else {
            false
        }
    }

    pub fn stop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

impl Drop for MpvDaemon {
    fn drop(&mut self) {
        self.stop();
    }
}
