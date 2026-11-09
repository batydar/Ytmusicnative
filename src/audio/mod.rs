pub mod equalizer;
pub mod normalization;

pub use equalizer::EqPreset;
pub use normalization::{LoudnessNormalization, LOUDNESS_NORMALIZATION_FILTER};

use crate::config::AppSettings;
use crate::mpv::MpvIpcClient;

/// Construct the combined MPV `af` filter chain string.
/// If both EQ and Normalization are active, joins them with a comma `,`.
pub fn build_filter_chain(preset: EqPreset, normalization_enabled: bool) -> String {
    let eq_str = preset.filter_string();
    let norm_str = if normalization_enabled {
        LOUDNESS_NORMALIZATION_FILTER
    } else {
        ""
    };

    match (!eq_str.is_empty(), !norm_str.is_empty()) {
        (true, true) => format!("{},{}", eq_str, norm_str),
        (true, false) => eq_str.to_string(),
        (false, true) => norm_str.to_string(),
        (false, false) => String::new(),
    }
}

/// Apply current EQ preset and Loudness Normalization dynamically to MPV
/// via `{"command": ["set_property", "af", "<FILTER_CHAIN>"]}` and persist in `settings.json`.
pub fn apply_and_persist_audio_filters(
    mpv: &MpvIpcClient,
    settings: &mut AppSettings,
    preset: EqPreset,
    normalization_enabled: bool,
) -> Result<String, String> {
    settings.eq_preset = preset.as_str().to_string();
    settings.loudness_normalization = normalization_enabled;
    let _ = settings.save();

    let chain = build_filter_chain(preset, normalization_enabled);
    mpv.set_audio_filters(&chain)?;
    Ok(chain)
}
