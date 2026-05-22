use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
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

#[must_use]
pub fn seeded_timing(
    seed: u64,
    node_id: &str,
    branch_id: &str,
    base_duration_ms: u64,
    jitter_ms: u64,
) -> EffectiveTiming {
    let hash = stable_hash(seed, node_id, branch_id);
    let duration_ms = base_duration_ms + hash % (jitter_ms + 1);
    let progress_jitter = if jitter_ms == 0 {
        0
    } else {
        (hash / 97) % (jitter_ms + 1)
    };
    let progress_interval_ms = (1 + progress_jitter).min(duration_ms.max(1));
    EffectiveTiming {
        duration_ms,
        progress_interval_ms,
    }
}

fn stable_hash(seed: u64, node_id: &str, branch_id: &str) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64 ^ seed;
    for byte in node_id.bytes().chain([0]).chain(branch_id.bytes()) {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

#[cfg(test)]
#[path = "timing_test.rs"]
mod tests;
