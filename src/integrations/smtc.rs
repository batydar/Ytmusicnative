use tokio::sync::mpsc::UnboundedSender;
use windows::core::HSTRING;
use windows::Foundation::{TypedEventHandler, Uri};
use windows::Media::Playback::MediaPlayer;
use windows::Media::{
    MediaPlaybackStatus, MediaPlaybackType, SystemMediaTransportControls,
    SystemMediaTransportControlsButton, SystemMediaTransportControlsButtonPressedEventArgs,
};
use windows::Storage::Streams::RandomAccessStreamReference;

#[derive(Debug, Clone, Copy)]
pub enum SmtcAction {
    Play,
    Pause,
    Next,
    Previous,
    Stop,
}

pub struct SmtcController {
    _player: MediaPlayer,
    smtc: SystemMediaTransportControls,
}

impl SmtcController {
    /// Bind to Windows `SystemMediaTransportControls` via `windows-rs`
    /// to show metadata on the Windows volume flyout and handle hardware media keys.
    pub fn new(action_tx: UnboundedSender<SmtcAction>) -> Result<Self, String> {
        let player =
            MediaPlayer::new().map_err(|e| format!("Failed to create WinRT MediaPlayer: {}", e))?;

        if let Ok(cmd_manager) = player.CommandManager() {
            let _ = cmd_manager.SetIsEnabled(false);
        }

        let smtc = player
            .SystemMediaTransportControls()
            .map_err(|e| format!("Failed to acquire SMTC handle: {}", e))?;

        let _ = smtc.SetIsEnabled(true);
        let _ = smtc.SetIsPlayEnabled(true);
        let _ = smtc.SetIsPauseEnabled(true);
        let _ = smtc.SetIsNextEnabled(true);
        let _ = smtc.SetIsPreviousEnabled(true);
        let _ = smtc.SetIsStopEnabled(true);
        let _ = smtc.SetPlaybackStatus(MediaPlaybackStatus::Stopped);

        let handler = TypedEventHandler::<
            SystemMediaTransportControls,
            SystemMediaTransportControlsButtonPressedEventArgs,
        >::new(move |_, args| {
            if let Some(args) = args {
                if let Ok(button) = args.Button() {
                    let action = match button {
                        SystemMediaTransportControlsButton::Play => Some(SmtcAction::Play),
                        SystemMediaTransportControlsButton::Pause => Some(SmtcAction::Pause),
                        SystemMediaTransportControlsButton::Next => Some(SmtcAction::Next),
                        SystemMediaTransportControlsButton::Previous => Some(SmtcAction::Previous),
                        SystemMediaTransportControlsButton::Stop => Some(SmtcAction::Stop),
                        _ => None,
                    };
                    if let Some(act) = action {
                        let _ = action_tx.send(act);
                    }
                }
            }
            Ok(())
        });

        smtc.ButtonPressed(&handler)
            .map_err(|e| format!("Failed to register SMTC ButtonPressed handler: {}", e))?;

        Ok(Self {
            _player: player,
            smtc,
        })
    }

    pub fn update_metadata(&self, title: &str, artist: &str, thumbnail_url: &str) {
        if let Ok(updater) = self.smtc.DisplayUpdater() {
            let _ = updater.SetType(MediaPlaybackType::Music);
            if let Ok(music_props) = updater.MusicProperties() {
                let _ = music_props.SetTitle(&HSTRING::from(title));
                let _ = music_props.SetArtist(&HSTRING::from(artist));
            }
            if !thumbnail_url.trim().is_empty() {
                if let Ok(uri) = Uri::CreateUri(&HSTRING::from(thumbnail_url)) {
                    if let Ok(stream_ref) = RandomAccessStreamReference::CreateFromUri(&uri) {
                        let _ = updater.SetThumbnail(&stream_ref);
                    }
                }
            }
            let _ = updater.Update();
        }
    }

    pub fn set_playback_status(&self, is_playing: bool) {
        let status = if is_playing {
            MediaPlaybackStatus::Playing
        } else {
            MediaPlaybackStatus::Paused
        };
        let _ = self.smtc.SetPlaybackStatus(status);
    }
}
