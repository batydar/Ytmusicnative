#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

pub mod audio;
pub mod config;
pub mod integrations;
pub mod mpv;
pub mod services;

slint::include_modules!();

use audio::{apply_and_persist_audio_filters, build_filter_chain, EqPreset};
use config::{cookies_file_path, AppSettings};
use integrations::{DiscordRpcClient, SmtcAction, SmtcController, SystemTray, TrayAction};
use mpv::{MpvDaemon, MpvEvent, MpvIpcClient, PlaybackSource};
use services::{
    find_active_lyric_index, spawn_download_task, DownloadEvent, InnerTubeClient, LyricLine,
    LyricsService, PlaylistItem, SponsorBlockService, SponsorSegment, TrackItem,
};
use slint::{ComponentHandle, ModelRc, SharedString, VecModel};
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;

struct AppRuntimeState {
    settings: AppSettings,
    home_tracks: Vec<TrackItem>,
    search_tracks: Vec<TrackItem>,
    playlists: Vec<PlaylistItem>,
    active_playlist_tracks: Vec<TrackItem>,
    autoplaylist_tracks: Vec<TrackItem>,
    current_track: Option<TrackItem>,
    current_lyrics: Vec<LyricLine>,
    last_lyric_index: Option<usize>,
    sponsor_segments: Vec<SponsorSegment>,
    last_sponsor_skip_end: f64,
    active_downloads: HashMap<String, f32>,
    is_playing: bool,
    window_visible: bool,
}

fn format_mm_ss(seconds: f64) -> String {
    if !seconds.is_finite() || seconds < 0.0 {
        return "00:00".to_string();
    }
    let total = seconds.round() as u64;
    let mins = total / 60;
    let secs = total % 60;
    format!("{:02}:{:02}", mins, secs)
}

fn to_ui_tracks(
    items: &[TrackItem],
    settings: &AppSettings,
    active_downloads: &HashMap<String, f32>,
) -> ModelRc<UiTrack> {
    let vec: Vec<UiTrack> = items
        .iter()
        .map(|t| {
            let is_cached = settings.find_cached_opus(&t.video_id).is_some();
            let dl_pct = active_downloads.get(&t.video_id).copied();
            UiTrack {
                video_id: SharedString::from(t.video_id.as_str()),
                title: SharedString::from(t.title.as_str()),
                artist: SharedString::from(t.artist.as_str()),
                duration: SharedString::from(t.duration_text.as_str()),
                duration_secs: t.duration_secs as i32,
                is_cached,
                is_downloading: dl_pct.is_some(),
                download_progress: dl_pct.unwrap_or(0.0),
            }
        })
        .collect();
    ModelRc::from(Rc::new(VecModel::from(vec)))
}

fn to_ui_playlists(items: &[PlaylistItem]) -> ModelRc<UiPlaylist> {
    let vec: Vec<UiPlaylist> = items
        .iter()
        .map(|p| UiPlaylist {
            browse_id: SharedString::from(p.browse_id.as_str()),
            title: SharedString::from(p.title.as_str()),
            subtitle: SharedString::from(p.subtitle.as_str()),
            track_count: SharedString::from(p.track_count.as_str()),
        })
        .collect();
    ModelRc::from(Rc::new(VecModel::from(vec)))
}

fn to_ui_downloaded_tracks(settings: &AppSettings) -> ModelRc<UiTrack> {
    let cached = settings.list_downloaded_tracks();
    let vec: Vec<UiTrack> = cached
        .into_iter()
        .map(|c| UiTrack {
            video_id: SharedString::from(c.video_id.as_str()),
            title: SharedString::from(c.title.as_str()),
            artist: SharedString::from(c.artist.as_str()),
            duration: SharedString::from("OPUS"),
            duration_secs: 0,
            is_cached: true,
            is_downloading: false,
            download_progress: 100.0,
        })
        .collect();
    ModelRc::from(Rc::new(VecModel::from(vec)))
}

fn to_ui_lyrics(lines: &[LyricLine], active_idx: Option<usize>) -> ModelRc<UiLyricLine> {
    let vec: Vec<UiLyricLine> = lines
        .iter()
        .enumerate()
        .map(|(i, l)| UiLyricLine {
            time_label: SharedString::from(l.time_label.as_str()),
            text: SharedString::from(l.text.as_str()),
            active: active_idx == Some(i),
        })
        .collect();
    ModelRc::from(Rc::new(VecModel::from(vec)))
}

fn refresh_all_track_models(ui: &MainWindow, state: &AppRuntimeState) {
    ui.set_home_tracks(to_ui_tracks(
        &state.home_tracks,
        &state.settings,
        &state.active_downloads,
    ));
    ui.set_search_results(to_ui_tracks(
        &state.search_tracks,
        &state.settings,
        &state.active_downloads,
    ));
    ui.set_active_playlist_tracks(to_ui_tracks(
        &state.active_playlist_tracks,
        &state.settings,
        &state.active_downloads,
    ));
    ui.set_autoplaylist_tracks(to_ui_tracks(
        &state.autoplaylist_tracks,
        &state.settings,
        &state.active_downloads,
    ));
    ui.set_downloaded_tracks(to_ui_downloaded_tracks(&state.settings));

    if let Some(cur) = &state.current_track {
        let is_cached = state.settings.find_cached_opus(&cur.video_id).is_some();
        ui.set_can_download_current(!is_cached);
    }
}

fn find_track_by_id(state: &AppRuntimeState, video_id: &str) -> TrackItem {
    if let Some(t) = state.autoplaylist_tracks.iter().find(|t| t.video_id == video_id) {
        return t.clone();
    }
    if let Some(t) = state.home_tracks.iter().find(|t| t.video_id == video_id) {
        return t.clone();
    }
    if let Some(t) = state.search_tracks.iter().find(|t| t.video_id == video_id) {
        return t.clone();
    }
    if let Some(t) = state.active_playlist_tracks.iter().find(|t| t.video_id == video_id) {
        return t.clone();
    }
    for cached in state.settings.list_downloaded_tracks() {
        if cached.video_id == video_id {
            return TrackItem {
                video_id: cached.video_id,
                title: cached.title,
                artist: cached.artist,
                album: String::new(),
                duration_text: "OPUS".to_string(),
                duration_secs: 210,
                thumbnail_url: String::new(),
            };
        }
    }
    TrackItem {
        video_id: video_id.to_string(),
        title: format!("Track [{}]", video_id),
        artist: "YouTube Music".to_string(),
        album: String::new(),
        duration_text: "03:30".to_string(),
        duration_secs: 210,
        thumbnail_url: String::new(),
    }
}

fn select_adjacent_track(state: &AppRuntimeState, forward: bool) -> Option<String> {
    let cur_id = state.current_track.as_ref()?.video_id.as_str();
    let list: &[TrackItem] = if !state.autoplaylist_tracks.is_empty()
        && state.autoplaylist_tracks.iter().any(|t| t.video_id == cur_id)
    {
        &state.autoplaylist_tracks
    } else if !state.active_playlist_tracks.is_empty()
        && state.active_playlist_tracks.iter().any(|t| t.video_id == cur_id)
    {
        &state.active_playlist_tracks
    } else if !state.home_tracks.is_empty()
        && state.home_tracks.iter().any(|t| t.video_id == cur_id)
    {
        &state.home_tracks
    } else if !state.search_tracks.is_empty()
        && state.search_tracks.iter().any(|t| t.video_id == cur_id)
    {
        &state.search_tracks
    } else if !state.home_tracks.is_empty() {
        &state.home_tracks
    } else {
        &state.search_tracks
    };

    if list.is_empty() {
        return None;
    }

    let pos = list.iter().position(|t| t.video_id == cur_id).unwrap_or(0);
    let next_idx = if forward {
        (pos + 1) % list.len()
    } else if pos == 0 {
        list.len() - 1
    } else {
        pos - 1
    };

    Some(list[next_idx].video_id.clone())
}

#[allow(clippy::too_many_arguments)]
fn start_playback_for_id(
    video_id: &str,
    ui: &MainWindow,
    state_arc: &Arc<Mutex<AppRuntimeState>>,
    mpv: &MpvIpcClient,
    smtc: &Option<Arc<SmtcController>>,
    discord: &DiscordRpcClient,
    lyrics_service: &LyricsService,
    sponsor_service: &SponsorBlockService,
) {
    let track = {
        let mut st = state_arc.lock().expect("state lock");
        let track = find_track_by_id(&st, video_id);
        let source_res = mpv.play_track(&track.video_id, &st.settings);

        let eq = EqPreset::from_name(&st.settings.eq_preset);
        let chain = build_filter_chain(eq, st.settings.loudness_normalization);
        let _ = mpv.set_audio_filters(&chain);
        let _ = mpv.set_volume(st.settings.volume);

        let badge = match source_res {
            Ok(PlaybackSource::LocalCache(_)) => "DISK OPUS",
            Ok(PlaybackSource::RemoteStream(_)) => "YT STREAM",
            Err(_) => "MPV IPC",
        };

        let is_cached = st.settings.find_cached_opus(&track.video_id).is_some();

        ui.set_current_video_id(SharedString::from(track.video_id.as_str()));
        ui.set_current_title(SharedString::from(track.title.as_str()));
        ui.set_current_artist(SharedString::from(track.artist.as_str()));
        ui.set_playback_source_badge(SharedString::from(badge));
        ui.set_is_playing(true);
        ui.set_time_pos(0.0);
        ui.set_time_pos_text(SharedString::from("00:00"));
        ui.set_duration(track.duration_secs as f32);
        ui.set_duration_text(SharedString::from(
            format_mm_ss(track.duration_secs as f64).as_str(),
        ));
        ui.set_can_download_current(!is_cached);
        ui.set_lyric_lines(ModelRc::from(Rc::new(VecModel::<UiLyricLine>::default())));
        ui.set_lyrics_status(SharedString::from("Fetching synchronized lyrics from LRCLIB..."));

        st.current_track = Some(track.clone());
        st.current_lyrics.clear();
        st.last_lyric_index = None;
        st.sponsor_segments.clear();
        st.last_sponsor_skip_end = 0.0;
        st.is_playing = true;

        track
    };

    if let Some(smtc_ctrl) = smtc {
        smtc_ctrl.update_metadata(&track.title, &track.artist, &track.thumbnail_url);
        smtc_ctrl.set_playback_status(true);
    }

    discord.update_presence(
        &track.video_id,
        &track.title,
        &track.artist,
        0.0,
        track.duration_secs as f64,
        true,
    );

    // Fetch synchronized lyrics from LRCLIB asynchronously
    {
        let lyrics_svc = lyrics_service.clone();
        let ui_weak = ui.as_weak();
        let state_clone = Arc::clone(state_arc);
        let title_clone = track.title.clone();
        let artist_clone = track.artist.clone();
        let dur_secs = track.duration_secs.max(1);
        let vid_clone = track.video_id.clone();

        tokio::spawn(async move {
            let res = lyrics_svc
                .fetch_lyrics(&title_clone, &artist_clone, dur_secs)
                .await;

            let _ = slint::invoke_from_event_loop(move || {
                let Some(ui) = ui_weak.upgrade() else {
                    return;
                };
                let mut st = state_clone.lock().expect("state lock");
                if st.current_track.as_ref().map(|t| t.video_id.as_str())
                    != Some(vid_clone.as_str())
                {
                    return;
                }

                match res {
                    Ok(lines) => {
                        ui.set_lyric_lines(to_ui_lyrics(&lines, None));
                        ui.set_lyrics_status(SharedString::from(""));
                        st.current_lyrics = lines;
                        st.last_lyric_index = None;
                    }
                    Err(msg) => {
                        ui.set_lyric_lines(ModelRc::from(Rc::new(
                            VecModel::<UiLyricLine>::default(),
                        )));
                        ui.set_lyrics_status(SharedString::from(msg.as_str()));
                    }
                }
            });
        });
    }

    // Fetch SponsorBlock skip segments asynchronously
    {
        let sb_svc = sponsor_service.clone();
        let state_clone = Arc::clone(state_arc);
        let vid_clone = track.video_id.clone();

        tokio::spawn(async move {
            if let Ok(segments) = sb_svc.fetch_skip_segments(&vid_clone).await {
                let mut st = state_clone.lock().expect("state lock");
                if st.current_track.as_ref().map(|t| t.video_id.as_str())
                    == Some(vid_clone.as_str())
                {
                    st.sponsor_segments = segments;
                }
            }
        });
    }
}

fn log_step(msg: &str) {
    use std::io::Write;
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(crate::config::app_data_dir().join("startup.log"))
    {
        let _ = writeln!(f, "{}", msg);
        let _ = f.flush();
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let _ = std::fs::create_dir_all(crate::config::app_data_dir());
    let _ = std::fs::write(crate::config::app_data_dir().join("startup.log"), "=== STARTUP ===\n");
    log_step("1. Building Tokio runtime");

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    let _rt_guard = rt.enter();

    log_step("2. Loading AppSettings");
    let settings = AppSettings::load();

    log_step("3. Creating Slint MainWindow first (initializes Win32/OLE event loop)");
    let ui = MainWindow::new()?;

    log_step("4. Starting headless MPV daemon");
    let mut mpv_daemon = MpvDaemon::new();
    let _ = mpv_daemon.start();

    log_step("5. Connecting MpvIpcClient");
    let (mpv_ipc, mut mpv_event_rx) = MpvIpcClient::connect_and_spawn();

    // Apply initial EQ preset, Loudness Normalization, and Volume
    let initial_preset = EqPreset::from_name(&settings.eq_preset);
    let initial_chain = build_filter_chain(initial_preset, settings.loudness_normalization);
    let _ = mpv_ipc.set_audio_filters(&initial_chain);
    let _ = mpv_ipc.set_volume(settings.volume);

    log_step("6. Initializing SMTC");
    let (smtc_tx, mut smtc_rx) = mpsc::unbounded_channel::<SmtcAction>();
    let smtc_controller = SmtcController::new(smtc_tx).ok().map(Arc::new);

    log_step("7. Initializing SystemTray");
    let (tray_tx, mut tray_rx) = mpsc::unbounded_channel::<TrayAction>();
    let _system_tray = SystemTray::new(tray_tx).ok();

    log_step("8. Initializing Discord RPC & Services");
    let discord_rpc = DiscordRpcClient::new();

    let innertube = InnerTubeClient::new();
    let lyrics_service = LyricsService::new();
    let sponsor_service = SponsorBlockService::new();
    let (download_tx, mut download_rx) = mpsc::unbounded_channel::<DownloadEvent>();

    log_step("9. Syncing initial UI properties");

    ui.set_download_folder(SharedString::from(
        settings.download_dir.to_string_lossy().as_ref(),
    ));
    ui.set_cookies_path(SharedString::from(
        cookies_file_path().to_string_lossy().as_ref(),
    ));
    ui.set_is_logged_in(InnerTubeClient::is_logged_in());
    ui.set_active_eq_preset(SharedString::from(initial_preset.as_str()));
    ui.set_normalization_enabled(settings.loudness_normalization);
    ui.set_sponsorblock_enabled(settings.sponsorblock_enabled);
    ui.set_filter_chain_preview(SharedString::from(initial_chain.as_str()));
    ui.set_volume(settings.volume as f32);
    ui.set_downloaded_tracks(to_ui_downloaded_tracks(&settings));

    let state = Arc::new(Mutex::new(AppRuntimeState {
        settings,
        home_tracks: Vec::new(),
        search_tracks: Vec::new(),
        playlists: Vec::new(),
        active_playlist_tracks: Vec::new(),
        autoplaylist_tracks: Vec::new(),
        current_track: None,
        current_lyrics: Vec::new(),
        last_lyric_index: None,
        sponsor_segments: Vec::new(),
        last_sponsor_skip_end: 0.0,
        active_downloads: HashMap::new(),
        is_playing: false,
        window_visible: true,
    }));

    // Trigger initial FEmusic_home recommendations load
    {
        let ui_weak = ui.as_weak();
        let state_clone = Arc::clone(&state);
        let yt_client = innertube.clone();
        ui.set_home_loading(true);
        ui.set_home_status(SharedString::from(
            "Loading your music recommendations...",
        ));

        tokio::spawn(async move {
            let result = yt_client.fetch_home_recommendations().await;
            let _ = slint::invoke_from_event_loop(move || {
                let Some(ui) = ui_weak.upgrade() else {
                    return;
                };
                ui.set_home_loading(false);
                match result {
                    Ok(tracks) => {
                        let count = tracks.len();
                        let mut st = state_clone.lock().expect("state lock");
                        st.home_tracks = tracks;
                        refresh_all_track_models(&ui, &st);
                        ui.set_home_status(SharedString::from(
                            format!("Loaded {} tracks", count).as_str(),
                        ));
                    }
                    Err(err) => {
                        ui.set_home_status(SharedString::from(err.as_str()));
                    }
                }
            });
        });
    }

    // Trigger initial Playlists load
    {
        let ui_weak = ui.as_weak();
        let state_clone = Arc::clone(&state);
        let yt_client = innertube.clone();
        ui.set_playlist_loading(true);

        tokio::spawn(async move {
            let result = yt_client.fetch_playlists().await;
            let _ = slint::invoke_from_event_loop(move || {
                let Some(ui) = ui_weak.upgrade() else {
                    return;
                };
                ui.set_playlist_loading(false);
                if let Ok(playlists) = result {
                    let mut st = state_clone.lock().expect("state lock");
                    st.playlists = playlists;
                    ui.set_playlists(to_ui_playlists(&st.playlists));
                }
            });
        });
    }

    // Wire UI Callbacks:
    // A. Play Track -> Starts playback AND opens an Auto-Playlist with the song + its similar tracks (`RDAMVM<id>`)
    {
        let ui_weak = ui.as_weak();
        let state_clone = Arc::clone(&state);
        let mpv_clone = mpv_ipc.clone();
        let smtc_clone = smtc_controller.clone();
        let discord_clone = discord_rpc.clone();
        let lyrics_clone = lyrics_service.clone();
        let sb_clone = sponsor_service.clone();
        let yt_client = innertube.clone();

        ui.on_play_track(move |video_id| {
            let id = video_id.to_string();
            if id.trim().is_empty() {
                return;
            }

            if let Some(ui) = ui_weak.upgrade() {
                start_playback_for_id(
                    id.as_str(),
                    &ui,
                    &state_clone,
                    &mpv_clone,
                    &smtc_clone,
                    &discord_clone,
                    &lyrics_clone,
                    &sb_clone,
                );

                let seed_track = {
                    let mut st = state_clone.lock().expect("state lock");
                    let t = find_track_by_id(&st, &id);
                    st.autoplaylist_tracks = vec![t.clone()];
                    refresh_all_track_models(&ui, &st);
                    t
                };

                ui.set_active_tab(0);
                ui.set_autoplaylist_title(SharedString::from(
                    format!("{} • Benzer Şarkılar", seed_track.title).as_str(),
                ));
                ui.set_autoplaylist_loading(true);

                let ui_weak_inner = ui_weak.clone();
                let state_inner = Arc::clone(&state_clone);
                let yt_inner = yt_client.clone();
                let seed_clone = seed_track.clone();
                let id_clone = id.clone();

                tokio::spawn(async move {
                    let res = yt_inner.fetch_radio_queue(&id_clone).await;
                    let _ = slint::invoke_from_event_loop(move || {
                        let Some(ui) = ui_weak_inner.upgrade() else {
                            return;
                        };
                        ui.set_autoplaylist_loading(false);
                        if let Ok(mut radio_tracks) = res {
                            let mut st = state_inner.lock().expect("state lock");
                            if !radio_tracks.iter().any(|t| t.video_id == seed_clone.video_id) {
                                radio_tracks.insert(0, seed_clone);
                            } else if let Some(pos) = radio_tracks
                                .iter()
                                .position(|t| t.video_id == seed_clone.video_id)
                            {
                                if pos > 0 {
                                    let item = radio_tracks.remove(pos);
                                    radio_tracks.insert(0, item);
                                }
                            }
                            st.autoplaylist_tracks = radio_tracks;
                            refresh_all_track_models(&ui, &st);
                        }
                    });
                });
            }
        });
    }

    // A2. Play Track Inside an Already-Open Playlist (Auto-Playlist or Saved Playlist)
    {
        let ui_weak = ui.as_weak();
        let state_clone = Arc::clone(&state);
        let mpv_clone = mpv_ipc.clone();
        let smtc_clone = smtc_controller.clone();
        let discord_clone = discord_rpc.clone();
        let lyrics_clone = lyrics_service.clone();
        let sb_clone = sponsor_service.clone();

        ui.on_play_in_playlist(move |video_id| {
            if let Some(ui) = ui_weak.upgrade() {
                start_playback_for_id(
                    video_id.as_str(),
                    &ui,
                    &state_clone,
                    &mpv_clone,
                    &smtc_clone,
                    &discord_clone,
                    &lyrics_clone,
                    &sb_clone,
                );
            }
        });
    }

    // A3. Close Auto-Playlist View and Return to Clean Home Screen
    {
        let ui_weak = ui.as_weak();
        ui.on_close_autoplaylist(move || {
            if let Some(ui) = ui_weak.upgrade() {
                ui.set_autoplaylist_title(SharedString::from(""));
            }
        });
    }

    // B. Download Track via yt-dlp.exe
    {
        let ui_weak = ui.as_weak();
        let state_clone = Arc::clone(&state);
        let dl_tx = download_tx.clone();

        ui.on_download_track(move |video_id| {
            let id_str = video_id.to_string();
            if id_str.trim().is_empty() {
                return;
            }
            let (title, dl_dir, already_active) = {
                let mut st = state_clone.lock().expect("state lock");
                if st.active_downloads.contains_key(&id_str)
                    || st.settings.find_cached_opus(&id_str).is_some()
                {
                    (String::new(), st.settings.download_dir.clone(), true)
                } else {
                    let t = find_track_by_id(&st, &id_str);
                    st.active_downloads.insert(id_str.clone(), 0.0);
                    if let Some(ui) = ui_weak.upgrade() {
                        ui.set_active_download_title(SharedString::from(t.title.as_str()));
                        ui.set_active_download_progress(0.0);
                        ui.set_active_download_status(SharedString::from("İndirme başlatılıyor..."));
                        refresh_all_track_models(&ui, &st);
                    }
                    (t.title, st.settings.download_dir.clone(), false)
                }
            };
            if !already_active {
                spawn_download_task(id_str, title, &dl_dir, dl_tx.clone());
            }
        });
    }

    // C. Refresh Home Recommendations
    {
        let ui_weak = ui.as_weak();
        let state_clone = Arc::clone(&state);
        let yt_client = innertube.clone();

        ui.on_refresh_home(move || {
            if let Some(ui) = ui_weak.upgrade() {
                ui.set_home_loading(true);
                ui.set_home_status(SharedString::from("Refreshing music feed..."));
            }
            let ui_weak_inner = ui_weak.clone();
            let state_inner = Arc::clone(&state_clone);
            let yt_inner = yt_client.clone();

            tokio::spawn(async move {
                let res = yt_inner.fetch_home_recommendations().await;
                let _ = slint::invoke_from_event_loop(move || {
                    let Some(ui) = ui_weak_inner.upgrade() else {
                        return;
                    };
                    ui.set_home_loading(false);
                    match res {
                        Ok(tracks) => {
                            let count = tracks.len();
                            let mut st = state_inner.lock().expect("state lock");
                            st.home_tracks = tracks;
                            refresh_all_track_models(&ui, &st);
                            ui.set_home_status(SharedString::from(
                                format!("Loaded {} recommended tracks", count).as_str(),
                            ));
                        }
                        Err(e) => {
                            ui.set_home_status(SharedString::from(e.as_str()));
                        }
                    }
                });
            });
        });
    }

    // D. Search YouTube Music
    {
        let ui_weak = ui.as_weak();
        let state_clone = Arc::clone(&state);
        let yt_client = innertube.clone();

        ui.on_search_submitted(move |query| {
            let q = query.to_string();
            if q.trim().is_empty() {
                return;
            }
            if let Some(ui) = ui_weak.upgrade() {
                ui.set_search_loading(true);
            }
            let ui_weak_inner = ui_weak.clone();
            let state_inner = Arc::clone(&state_clone);
            let yt_inner = yt_client.clone();

            tokio::spawn(async move {
                let res = yt_inner.search(&q).await;
                let _ = slint::invoke_from_event_loop(move || {
                    let Some(ui) = ui_weak_inner.upgrade() else {
                        return;
                    };
                    ui.set_search_loading(false);
                    if let Ok(tracks) = res {
                        let mut st = state_inner.lock().expect("state lock");
                        st.search_tracks = tracks;
                        refresh_all_track_models(&ui, &st);
                    }
                });
            });
        });
    }

    // E. Playlists: Select Playlist and Fetch Tracks
    {
        let ui_weak = ui.as_weak();
        let state_clone = Arc::clone(&state);
        let yt_client = innertube.clone();

        ui.on_select_playlist(move |browse_id, title| {
            let b_id = browse_id.to_string();
            let p_title = title.to_string();

            if let Some(ui) = ui_weak.upgrade() {
                ui.set_active_playlist_browse_id(SharedString::from(b_id.as_str()));
                ui.set_active_playlist_title(SharedString::from(p_title.as_str()));

                if b_id.is_empty() {
                    return;
                }

                ui.set_playlist_tracks_loading(true);
            }

            let ui_weak_inner = ui_weak.clone();
            let state_inner = Arc::clone(&state_clone);
            let yt_inner = yt_client.clone();

            tokio::spawn(async move {
                let res = yt_inner.fetch_playlist_tracks(&b_id).await;
                let _ = slint::invoke_from_event_loop(move || {
                    let Some(ui) = ui_weak_inner.upgrade() else {
                        return;
                    };
                    ui.set_playlist_tracks_loading(false);
                    if let Ok(tracks) = res {
                        let mut st = state_inner.lock().expect("state lock");
                        st.active_playlist_tracks = tracks;
                        refresh_all_track_models(&ui, &st);
                    }
                });
            });
        });
    }

    // E2. Playlists: Refresh Playlists List
    {
        let ui_weak = ui.as_weak();
        let state_clone = Arc::clone(&state);
        let yt_client = innertube.clone();

        ui.on_refresh_playlists(move || {
            if let Some(ui) = ui_weak.upgrade() {
                ui.set_playlist_loading(true);
            }
            let ui_weak_inner = ui_weak.clone();
            let state_inner = Arc::clone(&state_clone);
            let yt_inner = yt_client.clone();

            tokio::spawn(async move {
                let res = yt_inner.fetch_playlists().await;
                let _ = slint::invoke_from_event_loop(move || {
                    let Some(ui) = ui_weak_inner.upgrade() else {
                        return;
                    };
                    ui.set_playlist_loading(false);
                    if let Ok(playlists) = res {
                        let mut st = state_inner.lock().expect("state lock");
                        st.playlists = playlists;
                        ui.set_playlists(to_ui_playlists(&st.playlists));
                    }
                });
            });
        });
    }

    // F. Direct In-App Google / QR Login & Automatic Cookie Sync Callbacks
    {
        let ui_weak = ui.as_weak();
        let state_clone = Arc::clone(&state);
        let yt_client = innertube.clone();

        ui.on_start_browser_login(move || {
            if let Some(ui) = ui_weak.upgrade() {
                ui.set_login_busy(true);
                ui.set_login_status_text(SharedString::from(
                    "Giriş penceresi açıldı — Giriş yapıp pencereyi kapatın veya 'Girişi Tamamla'ya basın.",
                ));
            }

            let ui_weak_inner = ui_weak.clone();
            let state_inner = Arc::clone(&state_clone);
            let yt_inner = yt_client.clone();

            tokio::spawn(async move {
                let login_res = tokio::task::spawn_blocking(|| {
                    InnerTubeClient::launch_interactive_login_window_blocking()
                })
                .await;

                let is_logged = matches!(login_res, Ok(Ok(true))) || InnerTubeClient::is_logged_in();

                let h_res = yt_inner.fetch_home_recommendations().await;
                let p_res = yt_inner.fetch_playlists().await;

                let _ = slint::invoke_from_event_loop(move || {
                    let Some(ui) = ui_weak_inner.upgrade() else {
                        return;
                    };
                    ui.set_login_busy(false);
                    ui.set_is_logged_in(is_logged);

                    if is_logged {
                        ui.set_login_status_text(SharedString::from(
                            "✓ Giriş Başarılı! Kişisel önerileriniz ve çalma listeleriniz yüklendi.",
                        ));
                        ui.set_show_login(false);
                    } else {
                        ui.set_login_status_text(SharedString::from(
                            "Oturum algılanamadı. Lütfen açılan pencerede giriş yaptığınızdan emin olun.",
                        ));
                    }

                    let mut st = state_inner.lock().expect("state lock");
                    if let Ok(tracks) = h_res {
                        st.home_tracks = tracks;
                    }
                    if let Ok(playlists) = p_res {
                        st.playlists = playlists;
                        ui.set_playlists(to_ui_playlists(&st.playlists));
                    }
                    refresh_all_track_models(&ui, &st);
                });
            });
        });
    }

    {
        let ui_weak = ui.as_weak();
        let state_clone = Arc::clone(&state);
        let yt_client = innertube.clone();

        ui.on_complete_browser_login(move || {
            if let Some(ui) = ui_weak.upgrade() {
                ui.set_login_busy(true);
                ui.set_login_status_text(SharedString::from(
                    "Oturum bilgileri aktarılıyor ve çalma listeleriniz çekiliyor...",
                ));
                ui.set_home_loading(true);
                ui.set_playlist_loading(true);
            }

            let ui_weak_inner = ui_weak.clone();
            let state_inner = Arc::clone(&state_clone);
            let yt_inner = yt_client.clone();

            tokio::spawn(async move {
                let _ = tokio::task::spawn_blocking(|| {
                    InnerTubeClient::extract_cookies_from_auth_profile()
                })
                .await;

                let is_logged = InnerTubeClient::is_logged_in();
                let h_res = yt_inner.fetch_home_recommendations().await;
                let p_res = yt_inner.fetch_playlists().await;

                let _ = slint::invoke_from_event_loop(move || {
                    let Some(ui) = ui_weak_inner.upgrade() else {
                        return;
                    };
                    ui.set_login_busy(false);
                    ui.set_is_logged_in(is_logged);
                    ui.set_home_loading(false);
                    ui.set_playlist_loading(false);

                    if is_logged {
                        ui.set_login_status_text(SharedString::from(
                            "✓ Google Hesabınız Bağlandı! Tüm çalma listeleriniz eşitlendi.",
                        ));
                        ui.set_show_login(false);
                    } else {
                        ui.set_login_status_text(SharedString::from(
                            "Giriş bulunamadı. Önce '1. Google / QR ile Giriş Penceresini Aç' butonuna tıklayın.",
                        ));
                    }

                    let mut st = state_inner.lock().expect("state lock");
                    if let Ok(tracks) = h_res {
                        st.home_tracks = tracks;
                    }
                    if let Ok(playlists) = p_res {
                        st.playlists = playlists;
                        ui.set_playlists(to_ui_playlists(&st.playlists));
                    }
                    refresh_all_track_models(&ui, &st);
                });
            });
        });
    }

    {
        let ui_weak = ui.as_weak();
        let state_clone = Arc::clone(&state);
        let yt_client = innertube.clone();

        ui.on_logout_account(move || {
            if let Some(ui) = ui_weak.upgrade() {
                ui.set_login_busy(true);
                ui.set_login_status_text(SharedString::from("Çıkış yapılıyor..."));
            }

            let ui_weak_inner = ui_weak.clone();
            let state_inner = Arc::clone(&state_clone);
            let yt_inner = yt_client.clone();

            tokio::spawn(async move {
                let _ = tokio::task::spawn_blocking(InnerTubeClient::logout).await;
                let h_res = yt_inner.fetch_home_recommendations().await;
                let p_res = yt_inner.fetch_playlists().await;

                let _ = slint::invoke_from_event_loop(move || {
                    let Some(ui) = ui_weak_inner.upgrade() else {
                        return;
                    };
                    ui.set_login_busy(false);
                    ui.set_is_logged_in(false);
                    ui.set_login_status_text(SharedString::from(
                        "Hesaptan çıkış yapıldı. Yeni hesapla giriş yapabilirsiniz.",
                    ));

                    let mut st = state_inner.lock().expect("state lock");
                    if let Ok(tracks) = h_res {
                        st.home_tracks = tracks;
                    }
                    if let Ok(playlists) = p_res {
                        st.playlists = playlists;
                        ui.set_playlists(to_ui_playlists(&st.playlists));
                    }
                    refresh_all_track_models(&ui, &st);
                });
            });
        });
    }

    // G. Change Download Folder
    {
        let ui_weak = ui.as_weak();
        let state_clone = Arc::clone(&state);

        ui.on_change_download_folder(move || {
            let ui_weak_inner = ui_weak.clone();
            let state_inner = Arc::clone(&state_clone);

            let current_settings = {
                let st = state_clone.lock().expect("state lock");
                st.settings.clone()
            };

            tokio::spawn(async move {
                let mut temp_settings = current_settings;
                if let Some(chosen_dir) = temp_settings.pick_and_save_download_dir().await {
                    let _ = slint::invoke_from_event_loop(move || {
                        let Some(ui) = ui_weak_inner.upgrade() else {
                            return;
                        };
                        let mut st = state_inner.lock().expect("state lock");
                        st.settings.download_dir = chosen_dir.clone();
                        let _ = st.settings.save();
                        ui.set_download_folder(SharedString::from(
                            chosen_dir.to_string_lossy().as_ref(),
                        ));
                        refresh_all_track_models(&ui, &st);
                    });
                }
            });
        });
    }

    // H. Rescan Downloads Folder
    {
        let ui_weak = ui.as_weak();
        let state_clone = Arc::clone(&state);

        ui.on_refresh_downloads(move || {
            if let Some(ui) = ui_weak.upgrade() {
                let st = state_clone.lock().expect("state lock");
                refresh_all_track_models(&ui, &st);
            }
        });
    }

    // I. Transport Controls
    {
        let mpv_clone = mpv_ipc.clone();
        ui.on_play_pause_clicked(move || {
            let _ = mpv_clone.toggle_pause();
        });
    }

    {
        let ui_weak = ui.as_weak();
        let state_clone = Arc::clone(&state);
        let mpv_clone = mpv_ipc.clone();
        let smtc_clone = smtc_controller.clone();
        let discord_clone = discord_rpc.clone();
        let lyrics_clone = lyrics_service.clone();
        let sb_clone = sponsor_service.clone();

        ui.on_next_clicked(move || {
            let next_id = {
                let st = state_clone.lock().expect("state lock");
                select_adjacent_track(&st, true)
            };
            if let (Some(id), Some(ui)) = (next_id, ui_weak.upgrade()) {
                start_playback_for_id(
                    &id,
                    &ui,
                    &state_clone,
                    &mpv_clone,
                    &smtc_clone,
                    &discord_clone,
                    &lyrics_clone,
                    &sb_clone,
                );
            }
        });
    }

    {
        let ui_weak = ui.as_weak();
        let state_clone = Arc::clone(&state);
        let mpv_clone = mpv_ipc.clone();
        let smtc_clone = smtc_controller.clone();
        let discord_clone = discord_rpc.clone();
        let lyrics_clone = lyrics_service.clone();
        let sb_clone = sponsor_service.clone();

        ui.on_prev_clicked(move || {
            let prev_id = {
                let st = state_clone.lock().expect("state lock");
                select_adjacent_track(&st, false)
            };
            if let (Some(id), Some(ui)) = (prev_id, ui_weak.upgrade()) {
                start_playback_for_id(
                    &id,
                    &ui,
                    &state_clone,
                    &mpv_clone,
                    &smtc_clone,
                    &discord_clone,
                    &lyrics_clone,
                    &sb_clone,
                );
            }
        });
    }

    {
        let ui_weak = ui.as_weak();
        let state_clone = Arc::clone(&state);
        let mpv_clone = mpv_ipc.clone();
        let discord_clone = discord_rpc.clone();

        ui.on_seek_to(move |pos| {
            let _ = mpv_clone.seek_absolute(pos as f64);
            if let Some(ui) = ui_weak.upgrade() {
                let st = state_clone.lock().expect("state lock");
                if let Some(cur) = &st.current_track {
                    discord_clone.update_presence(
                        &cur.video_id,
                        &cur.title,
                        &cur.artist,
                        pos as f64,
                        ui.get_duration() as f64,
                        st.is_playing,
                    );
                }
            }
        });
    }

    {
        let ui_weak = ui.as_weak();
        let state_clone = Arc::clone(&state);
        let mpv_clone = mpv_ipc.clone();

        ui.on_volume_changed(move |vol| {
            let v = (vol as f64).clamp(0.0, 100.0);
            let _ = mpv_clone.set_volume(v);
            if let Some(ui) = ui_weak.upgrade() {
                ui.set_volume(v as f32);
            }
            let mut st = state_clone.lock().expect("state lock");
            st.settings.volume = v;
            let _ = st.settings.save();
        });
    }

    // J. Equalizer & Loudness Normalization Callbacks
    {
        let ui_weak = ui.as_weak();
        let state_clone = Arc::clone(&state);
        let mpv_clone = mpv_ipc.clone();

        ui.on_select_eq_preset(move |preset_name| {
            let preset = EqPreset::from_name(preset_name.as_str());
            let mut st = state_clone.lock().expect("state lock");
            let norm = st.settings.loudness_normalization;
            if let Ok(chain) =
                apply_and_persist_audio_filters(&mpv_clone, &mut st.settings, preset, norm)
            {
                if let Some(ui) = ui_weak.upgrade() {
                    ui.set_active_eq_preset(SharedString::from(preset.as_str()));
                    ui.set_filter_chain_preview(SharedString::from(chain.as_str()));
                }
            }
        });
    }

    {
        let ui_weak = ui.as_weak();
        let state_clone = Arc::clone(&state);
        let mpv_clone = mpv_ipc.clone();

        ui.on_toggle_normalization(move |enabled| {
            let mut st = state_clone.lock().expect("state lock");
            let preset = EqPreset::from_name(&st.settings.eq_preset);
            if let Ok(chain) =
                apply_and_persist_audio_filters(&mpv_clone, &mut st.settings, preset, enabled)
            {
                if let Some(ui) = ui_weak.upgrade() {
                    ui.set_normalization_enabled(enabled);
                    ui.set_filter_chain_preview(SharedString::from(chain.as_str()));
                }
            }
        });
    }

    {
        let ui_weak = ui.as_weak();
        let state_clone = Arc::clone(&state);

        ui.on_toggle_sponsorblock(move |enabled| {
            let mut st = state_clone.lock().expect("state lock");
            st.settings.sponsorblock_enabled = enabled;
            let _ = st.settings.save();
            if let Some(ui) = ui_weak.upgrade() {
                ui.set_sponsorblock_enabled(enabled);
            }
        });
    }

    // 5. Spawn Background Event Listeners (MPV IPC, Downloader, SMTC, System Tray)
    {
        let ui_weak = ui.as_weak();
        let state_clone = Arc::clone(&state);
        let mpv_clone = mpv_ipc.clone();
        let smtc_clone = smtc_controller.clone();
        let discord_clone = discord_rpc.clone();
        let lyrics_clone = lyrics_service.clone();
        let sb_clone = sponsor_service.clone();

        tokio::spawn(async move {
            while let Some(evt) = mpv_event_rx.recv().await {
                let ui_weak_inner = ui_weak.clone();
                let state_inner = Arc::clone(&state_clone);
                let mpv_inner = mpv_clone.clone();
                let smtc_inner = smtc_clone.clone();
                let discord_inner = discord_clone.clone();
                let lyrics_inner = lyrics_clone.clone();
                let sb_inner = sb_clone.clone();

                let _ = slint::invoke_from_event_loop(move || {
                    let Some(ui) = ui_weak_inner.upgrade() else {
                        return;
                    };
                    let mut st = state_inner.lock().expect("state lock");

                    match evt {
                        MpvEvent::TimePos(pos) => {
                            ui.set_time_pos(pos as f32);
                            ui.set_time_pos_text(SharedString::from(format_mm_ss(pos).as_str()));

                            if !st.current_lyrics.is_empty() {
                                let active_idx = find_active_lyric_index(&st.current_lyrics, pos);
                                if active_idx != st.last_lyric_index {
                                    st.last_lyric_index = active_idx;
                                    ui.set_active_lyric_index(
                                        active_idx.map(|i| i as i32).unwrap_or(-1),
                                    );
                                    ui.set_lyric_lines(to_ui_lyrics(
                                        &st.current_lyrics,
                                        active_idx,
                                    ));
                                }
                            }

                            if st.settings.sponsorblock_enabled && !st.sponsor_segments.is_empty() {
                                if let Some(skip_to) =
                                    SponsorBlockService::check_skip(&st.sponsor_segments, pos)
                                {
                                    if (skip_to - st.last_sponsor_skip_end).abs() > 1.0 {
                                        st.last_sponsor_skip_end = skip_to;
                                        let _ = mpv_inner.seek_absolute(skip_to);
                                    }
                                }
                            }
                        }
                        MpvEvent::Duration(dur) => {
                            if dur > 0.0 {
                                ui.set_duration(dur as f32);
                                ui.set_duration_text(
                                    SharedString::from(format_mm_ss(dur).as_str()),
                                );
                                if let Some(cur) = &st.current_track {
                                    discord_inner.update_presence(
                                        &cur.video_id,
                                        &cur.title,
                                        &cur.artist,
                                        ui.get_time_pos() as f64,
                                        dur,
                                        st.is_playing,
                                    );
                                }
                            }
                        }
                        MpvEvent::Paused(paused) => {
                            let playing = !paused;
                            st.is_playing = playing;
                            ui.set_is_playing(playing);
                            if let Some(smtc_ctrl) = &smtc_inner {
                                smtc_ctrl.set_playback_status(playing);
                            }
                            if let Some(cur) = &st.current_track {
                                discord_inner.update_presence(
                                    &cur.video_id,
                                    &cur.title,
                                    &cur.artist,
                                    ui.get_time_pos() as f64,
                                    ui.get_duration() as f64,
                                    playing,
                                );
                            }
                        }
                        MpvEvent::EndFile(reason) => {
                            // Seamless Autoplay: when a song finishes naturally, immediately advance to the next track in the playlist!
                            if reason == "eof" {
                                let next_id = select_adjacent_track(&st, true);
                                drop(st);
                                if let Some(id) = next_id {
                                    start_playback_for_id(
                                        &id,
                                        &ui,
                                        &state_inner,
                                        &mpv_inner,
                                        &smtc_inner,
                                        &discord_inner,
                                        &lyrics_inner,
                                        &sb_inner,
                                    );
                                }
                            }
                        }
                        _ => {}
                    }
                });
            }
        });
    }

    // Downloader Event Listener
    {
        let ui_weak = ui.as_weak();
        let state_clone = Arc::clone(&state);

        tokio::spawn(async move {
            while let Some(dl_evt) = download_rx.recv().await {
                let ui_weak_inner = ui_weak.clone();
                let state_inner = Arc::clone(&state_clone);

                let _ = slint::invoke_from_event_loop(move || {
                    let Some(ui) = ui_weak_inner.upgrade() else {
                        return;
                    };
                    let mut st = state_inner.lock().expect("state lock");

                    match dl_evt {
                        DownloadEvent::Started { video_id, title } => {
                            st.active_downloads.insert(video_id, 0.0);
                            ui.set_active_download_title(SharedString::from(title.as_str()));
                            ui.set_active_download_progress(0.0);
                            ui.set_active_download_status(SharedString::from("Starting yt-dlp..."));
                            refresh_all_track_models(&ui, &st);
                        }
                        DownloadEvent::Progress {
                            video_id,
                            title,
                            percent,
                            status_text,
                        } => {
                            st.active_downloads.insert(video_id, percent);
                            ui.set_active_download_title(SharedString::from(title.as_str()));
                            ui.set_active_download_progress(percent);
                            ui.set_active_download_status(SharedString::from(status_text.as_str()));
                            refresh_all_track_models(&ui, &st);
                        }
                        DownloadEvent::Completed { video_id, title } => {
                            st.active_downloads.remove(&video_id);
                            ui.set_active_download_title(SharedString::from(title.as_str()));
                            ui.set_active_download_progress(100.0);
                            ui.set_active_download_status(SharedString::from(
                                "Saved Opus to cache ✓",
                            ));
                            refresh_all_track_models(&ui, &st);
                        }
                        DownloadEvent::Failed {
                            video_id,
                            title,
                            error,
                        } => {
                            st.active_downloads.remove(&video_id);
                            ui.set_active_download_title(SharedString::from(title.as_str()));
                            ui.set_active_download_progress(0.0);
                            ui.set_active_download_status(SharedString::from(error.as_str()));
                            refresh_all_track_models(&ui, &st);
                        }
                    }
                });
            }
        });
    }

    // Windows SMTC Hardware Media Keys Listener
    {
        let ui_weak = ui.as_weak();
        let state_clone = Arc::clone(&state);
        let mpv_clone = mpv_ipc.clone();
        let smtc_clone = smtc_controller.clone();
        let discord_clone = discord_rpc.clone();
        let lyrics_clone = lyrics_service.clone();
        let sb_clone = sponsor_service.clone();

        tokio::spawn(async move {
            while let Some(action) = smtc_rx.recv().await {
                let ui_weak_inner = ui_weak.clone();
                let state_inner = Arc::clone(&state_clone);
                let mpv_inner = mpv_clone.clone();
                let smtc_inner = smtc_clone.clone();
                let discord_inner = discord_clone.clone();
                let lyrics_inner = lyrics_clone.clone();
                let sb_inner = sb_clone.clone();

                let _ = slint::invoke_from_event_loop(move || {
                    let Some(ui) = ui_weak_inner.upgrade() else {
                        return;
                    };
                    match action {
                        SmtcAction::Play => {
                            let _ = mpv_inner.set_pause(false);
                        }
                        SmtcAction::Pause => {
                            let _ = mpv_inner.set_pause(true);
                        }
                        SmtcAction::Stop => {
                            let _ = mpv_inner.stop();
                        }
                        SmtcAction::Next => {
                            let next_id = {
                                let st = state_inner.lock().expect("state lock");
                                select_adjacent_track(&st, true)
                            };
                            if let Some(id) = next_id {
                                start_playback_for_id(
                                    &id,
                                    &ui,
                                    &state_inner,
                                    &mpv_inner,
                                    &smtc_inner,
                                    &discord_inner,
                                    &lyrics_inner,
                                    &sb_inner,
                                );
                            }
                        }
                        SmtcAction::Previous => {
                            let prev_id = {
                                let st = state_inner.lock().expect("state lock");
                                select_adjacent_track(&st, false)
                            };
                            if let Some(id) = prev_id {
                                start_playback_for_id(
                                    &id,
                                    &ui,
                                    &state_inner,
                                    &mpv_inner,
                                    &smtc_inner,
                                    &discord_inner,
                                    &lyrics_inner,
                                    &sb_inner,
                                );
                            }
                        }
                    }
                });
            }
        });
    }

    // System Tray Menu Action Listener
    {
        let ui_weak = ui.as_weak();
        let state_clone = Arc::clone(&state);
        let mpv_clone = mpv_ipc.clone();
        let smtc_clone = smtc_controller.clone();
        let discord_clone = discord_rpc.clone();
        let lyrics_clone = lyrics_service.clone();
        let sb_clone = sponsor_service.clone();

        tokio::spawn(async move {
            while let Some(action) = tray_rx.recv().await {
                let ui_weak_inner = ui_weak.clone();
                let state_inner = Arc::clone(&state_clone);
                let mpv_inner = mpv_clone.clone();
                let smtc_inner = smtc_clone.clone();
                let discord_inner = discord_clone.clone();
                let lyrics_inner = lyrics_clone.clone();
                let sb_inner = sb_clone.clone();

                let _ = slint::invoke_from_event_loop(move || {
                    let Some(ui) = ui_weak_inner.upgrade() else {
                        return;
                    };
                    match action {
                        TrayAction::ShowHide => {
                            let mut st = state_inner.lock().expect("state lock");
                            if st.window_visible {
                                let _ = ui.hide();
                                st.window_visible = false;
                            } else {
                                let _ = ui.show();
                                st.window_visible = true;
                            }
                        }
                        TrayAction::PlayPause => {
                            let _ = mpv_inner.toggle_pause();
                        }
                        TrayAction::Next => {
                            let next_id = {
                                let st = state_inner.lock().expect("state lock");
                                select_adjacent_track(&st, true)
                            };
                            if let Some(id) = next_id {
                                start_playback_for_id(
                                    &id,
                                    &ui,
                                    &state_inner,
                                    &mpv_inner,
                                    &smtc_inner,
                                    &discord_inner,
                                    &lyrics_inner,
                                    &sb_inner,
                                );
                            }
                        }
                        TrayAction::Quit => {
                            let _ = mpv_inner.quit();
                            discord_inner.clear();
                            let _ = slint::quit_event_loop();
                        }
                    }
                });
            }
        });
    }

    // Run native Slint GUI loop
    log_step("10. Entering ui.run()");
    ui.run()?;

    // Clean shutdown of headless MPV daemon & Discord RPC
    let _ = mpv_ipc.quit();
    discord_rpc.clear();
    mpv_daemon.stop();

    Ok(())
}
