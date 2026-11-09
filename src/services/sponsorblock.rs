use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SponsorSegment {
    pub segment: [f64; 2],
    #[serde(default)]
    pub category: String,
}

impl SponsorSegment {
    pub fn start(&self) -> f64 {
        self.segment[0]
    }

    pub fn end(&self) -> f64 {
        self.segment[1]
    }
}

#[derive(Clone)]
pub struct SponsorBlockService {
    http: reqwest::Client,
}

impl SponsorBlockService {
    pub fn new() -> Self {
        let http = reqwest::Client::builder()
            .user_agent("YTMusicNative/0.1.0")
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());
        Self { http }
    }

    /// Fetch non-music intervals from SponsorBlock:
    /// `GET https://sponsor.ajay.app/api/skipSegments?videoID={id}&categories=["music_offtopic"]`
    pub async fn fetch_skip_segments(&self, video_id: &str) -> Result<Vec<SponsorSegment>, String> {
        if video_id.trim().is_empty() {
            return Ok(Vec::new());
        }

        let url = format!(
            "https://sponsor.ajay.app/api/skipSegments?videoID={}&categories=[\"music_offtopic\"]",
            video_id.trim()
        );

        let response = self
            .http
            .get(&url)
            .send()
            .await
            .map_err(|e| format!("SponsorBlock request failed: {}", e))?;

        if !response.status().is_success() {
            // 404 is normal when a video has no skip segments
            return Ok(Vec::new());
        }

        let segments: Vec<SponsorSegment> = response
            .json()
            .await
            .map_err(|e| format!("Failed to parse SponsorBlock segments: {}", e))?;

        Ok(segments)
    }

    /// Check if `time_pos` has entered any `[start, end]` segment.
    /// Returns `Some(end)` so MPV can issue a `seek` command immediately.
    pub fn check_skip(segments: &[SponsorSegment], time_pos: f64) -> Option<f64> {
        for seg in segments {
            let start = seg.start();
            let end = seg.end();
            if end > start && time_pos >= start && time_pos < (end - 0.35) {
                return Some(end);
            }
        }
        None
    }
}
