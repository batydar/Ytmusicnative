use regex::Regex;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LyricLine {
    pub timestamp_secs: f64,
    pub time_label: String,
    pub text: String,
}

#[derive(Debug, Deserialize)]
struct LrcLibResponse {
    #[serde(rename = "syncedLyrics")]
    synced_lyrics: Option<String>,
    #[serde(rename = "plainLyrics")]
    plain_lyrics: Option<String>,
}

#[derive(Clone)]
pub struct LyricsService {
    http: reqwest::Client,
}

impl LyricsService {
    pub fn new() -> Self {
        let http = reqwest::Client::builder()
            .user_agent("YTMusicNative/0.1.0 (https://github.com/yt-music-native)")
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());
        Self { http }
    }

    /// Fetch time-synced lyrics from LRCLIB:
    /// `GET https://lrclib.net/api/get?track_name={title}&artist_name={artist}&duration={duration}`
    pub async fn fetch_lyrics(
        &self,
        title: &str,
        artist: &str,
        duration_secs: u64,
    ) -> Result<Vec<LyricLine>, String> {
        let duration_param = duration_secs.to_string();
        let response = self
            .http
            .get("https://lrclib.net/api/get")
            .query(&[
                ("track_name", title),
                ("artist_name", artist),
                ("duration", duration_param.as_str()),
            ])
            .send()
            .await
            .map_err(|e| format!("LRCLIB request failed: {}", e))?;

        if response.status().is_success() {
            let data: LrcLibResponse = response
                .json()
                .await
                .map_err(|e| format!("Failed to decode LRCLIB JSON: {}", e))?;

            if let Some(synced) = data.synced_lyrics {
                let parsed = parse_synced_lrc(&synced);
                if !parsed.is_empty() {
                    return Ok(parsed);
                }
            }
            if let Some(plain) = data.plain_lyrics {
                let parsed = parse_plain_lyrics(&plain);
                if !parsed.is_empty() {
                    return Ok(parsed);
                }
            }
        }

        // Fallback search endpoint if exact duration did not match
        let search_resp = self
            .http
            .get("https://lrclib.net/api/search")
            .query(&[("track_name", title), ("artist_name", artist)])
            .send()
            .await
            .map_err(|e| format!("LRCLIB search fallback failed: {}", e))?;

        if search_resp.status().is_success() {
            let items: Vec<LrcLibResponse> = search_resp
                .json()
                .await
                .map_err(|e| format!("Failed to decode LRCLIB search JSON: {}", e))?;

            for item in items {
                if let Some(synced) = item.synced_lyrics {
                    let parsed = parse_synced_lrc(&synced);
                    if !parsed.is_empty() {
                        return Ok(parsed);
                    }
                }
            }
        }

        Err("No synchronized lyrics found for this track.".to_string())
    }
}

/// Parse `[mm:ss.xx]` timestamps into sorted `LyricLine` entries.
pub fn parse_synced_lrc(lrc_text: &str) -> Vec<LyricLine> {
    let re = Regex::new(r"^\[(\d{1,2}):(\d{2})(?:\.(\d{1,3}))?\](.*)$").expect("valid lrc regex");
    let mut lines = Vec::new();

    for raw in lrc_text.lines() {
        let trimmed = raw.trim();
        if let Some(caps) = re.captures(trimmed) {
            let mins: f64 = caps
                .get(1)
                .and_then(|m| m.as_str().parse().ok())
                .unwrap_or(0.0);
            let secs: f64 = caps
                .get(2)
                .and_then(|m| m.as_str().parse().ok())
                .unwrap_or(0.0);
            let frac: f64 = caps
                .get(3)
                .map(|m| {
                    let s = m.as_str();
                    let val: f64 = s.parse().unwrap_or(0.0);
                    match s.len() {
                        1 => val / 10.0,
                        2 => val / 100.0,
                        _ => val / 1000.0,
                    }
                })
                .unwrap_or(0.0);

            let timestamp_secs = mins * 60.0 + secs + frac;
            let text = caps
                .get(4)
                .map(|m| m.as_str().trim().to_string())
                .unwrap_or_default();

            let display_text = if text.is_empty() {
                "♪".to_string()
            } else {
                text
            };

            let time_label = format!("{:02}:{:02}", mins as u64, secs as u64);

            lines.push(LyricLine {
                timestamp_secs,
                time_label,
                text: display_text,
            });
        }
    }

    lines.sort_by(|a, b| {
        a.timestamp_secs
            .partial_cmp(&b.timestamp_secs)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    lines
}

fn parse_plain_lyrics(plain: &str) -> Vec<LyricLine> {
    plain
        .lines()
        .enumerate()
        .filter_map(|(idx, line)| {
            let t = line.trim();
            if t.is_empty() {
                None
            } else {
                Some(LyricLine {
                    timestamp_secs: (idx as f64) * 4.0,
                    time_label: "--:--".to_string(),
                    text: t.to_string(),
                })
            }
        })
        .collect()
}

/// Find the active lyric index corresponding to MPV's `time-pos` property.
pub fn find_active_lyric_index(lines: &[LyricLine], time_pos: f64) -> Option<usize> {
    if lines.is_empty() {
        return None;
    }

    let mut active = None;
    for (idx, line) in lines.iter().enumerate() {
        if time_pos + 0.10 >= line.timestamp_secs {
            active = Some(idx);
        } else {
            break;
        }
    }
    active
}
