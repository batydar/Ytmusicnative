use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum EqPreset {
    Flat,
    BassBoost,
    TrebleBoost,
    VShape,
    Vocal,
}

impl EqPreset {
    pub const ALL: [EqPreset; 5] = [
        EqPreset::Flat,
        EqPreset::BassBoost,
        EqPreset::TrebleBoost,
        EqPreset::VShape,
        EqPreset::Vocal,
    ];

    pub fn as_str(&self) -> &'static str {
        match self {
            EqPreset::Flat => "Flat",
            EqPreset::BassBoost => "Bass Boost",
            EqPreset::TrebleBoost => "Treble Boost",
            EqPreset::VShape => "V-Shape",
            EqPreset::Vocal => "Vocal",
        }
    }

    pub fn from_name(name: &str) -> Self {
        match name.trim() {
            "Bass Boost" | "BassBoost" => EqPreset::BassBoost,
            "Treble Boost" | "TrebleBoost" => EqPreset::TrebleBoost,
            "V-Shape" | "VShape" => EqPreset::VShape,
            "Vocal" => EqPreset::Vocal,
            _ => EqPreset::Flat,
        }
    }

    pub fn filter_string(&self) -> &'static str {
        // Note: FFmpeg's `superequalizer` band gains (1b..18b) are linear multipliers in range [0.0, 20.0],
        // where 1.0 is flat (0 dB), <1.0 cuts, and >1.0 boosts. Negative values cause lavfi initialization to fail.
        match self {
            EqPreset::Flat => "",
            EqPreset::BassBoost => {
                "lavfi=[superequalizer=1b=2.4:2b=2.1:3b=1.6:4b=1.2:5b=1:6b=1:7b=1:8b=1:9b=1:10b=1:11b=1:12b=1:13b=1:14b=1:15b=1:16b=1:17b=1]"
            }
            EqPreset::TrebleBoost => {
                "lavfi=[superequalizer=1b=1:2b=1:3b=1:4b=1:5b=1:6b=1:7b=1:8b=1:9b=1:10b=1:11b=1.2:12b=1.4:13b=1.8:14b=2.1:15b=2.4:16b=2.4:17b=2.4]"
            }
            EqPreset::VShape => {
                "lavfi=[superequalizer=1b=2.1:2b=1.8:3b=1.4:4b=1:5b=0.8:6b=0.7:7b=0.7:8b=0.8:9b=1:10b=1.2:11b=1.5:12b=1.8:13b=2.1:14b=2.1:15b=2.1:16b=1.8:17b=1.8]"
            }
            EqPreset::Vocal => {
                "lavfi=[superequalizer=1b=0.6:2b=0.7:3b=0.8:4b=1:5b=1.4:6b=1.8:7b=2.1:8b=2.1:9b=1.8:10b=1.5:11b=1.2:12b=1:13b=1:14b=0.9:15b=0.8:16b=0.8:17b=0.7]"
            }
        }
    }
}
