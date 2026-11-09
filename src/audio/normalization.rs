pub const LOUDNESS_NORMALIZATION_FILTER: &str = "lavfi=[dynaudnorm=f=150:g=15:m=10.0]";

#[derive(Debug, Clone, Copy, Default)]
pub struct LoudnessNormalization {
    pub enabled: bool,
}

impl LoudnessNormalization {
    pub fn new(enabled: bool) -> Self {
        Self { enabled }
    }

    pub fn filter_string(&self) -> &'static str {
        if self.enabled {
            LOUDNESS_NORMALIZATION_FILTER
        } else {
            ""
        }
    }
}
