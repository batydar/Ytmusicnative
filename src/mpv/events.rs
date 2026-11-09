use serde_json::Value;

#[derive(Debug, Clone)]
pub enum MpvEvent {
    TimePos(f64),
    Duration(f64),
    Paused(bool),
    MediaTitle(String),
    FileLoaded,
    EndFile(String),
    Shutdown,
    Raw(Value),
}

pub fn parse_mpv_event_line(line: &str) -> Option<MpvEvent> {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return None;
    }

    let val: Value = serde_json::from_str(trimmed).ok()?;

    if let Some(event_name) = val.get("event").and_then(|v| v.as_str()) {
        match event_name {
            "property-change" => {
                let prop_name = val.get("name").and_then(|v| v.as_str()).unwrap_or("");
                let data = val.get("data");
                match prop_name {
                    "time-pos" => {
                        if let Some(pos) = data.and_then(|d| d.as_f64()) {
                            return Some(MpvEvent::TimePos(pos));
                        }
                    }
                    "duration" => {
                        if let Some(dur) = data.and_then(|d| d.as_f64()) {
                            return Some(MpvEvent::Duration(dur));
                        }
                    }
                    "pause" => {
                        if let Some(paused) = data.and_then(|d| d.as_bool()) {
                            return Some(MpvEvent::Paused(paused));
                        }
                    }
                    "media-title" => {
                        if let Some(title) = data.and_then(|d| d.as_str()) {
                            return Some(MpvEvent::MediaTitle(title.to_string()));
                        }
                    }
                    "eof-reached" => {
                        if data.and_then(|d| d.as_bool()) == Some(true) {
                            return Some(MpvEvent::EndFile("eof".to_string()));
                        }
                    }
                    _ => {}
                }
            }
            "file-loaded" => return Some(MpvEvent::FileLoaded),
            "end-file" => {
                let reason = val
                    .get("reason")
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown")
                    .to_string();
                return Some(MpvEvent::EndFile(reason));
            }
            "shutdown" => return Some(MpvEvent::Shutdown),
            _ => {}
        }
    }

    Some(MpvEvent::Raw(val))
}
