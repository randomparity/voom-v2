#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EffectiveTiming {
    pub duration_ms: u64,
    pub progress_interval_ms: u64,
}

impl EffectiveTiming {
    #[must_use]
    pub fn for_test(duration_ms: u64, progress_interval_ms: u64) -> Self {
        Self {
            duration_ms,
            progress_interval_ms,
        }
    }
}

#[must_use]
pub fn branch_codec(seed: u64, branch_id: &str) -> &'static str {
    let suffix = branch_id
        .rsplit_once('-')
        .and_then(|(_, suffix)| suffix.parse::<u64>().ok())
        .unwrap_or(0);
    if (seed + suffix).is_multiple_of(2) {
        "h265"
    } else {
        "h264"
    }
}
