use serde::{Deserialize, Serialize};

/// What one `apply` call did. Comparable across every optimizer so the golden
/// set can rank them.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OptMetrics {
    pub bytes_in: usize,
    pub bytes_out: usize,
    pub est_tokens_in: usize,
    pub est_tokens_out: usize,
    /// Wall-clock milliseconds spent in `apply`.
    pub ms: u64,
    /// True when `output` carries exactly the information of `input`.
    pub lossless: bool,
    /// Key of the full original in the [`crate::OriginalStore`].
    pub original_ref: String,
}

impl OptMetrics {
    /// Fraction of bytes removed, `0.0` for empty input.
    pub fn reduction(&self) -> f64 {
        if self.bytes_in == 0 {
            return 0.0;
        }
        1.0 - (self.bytes_out as f64 / self.bytes_in as f64)
    }
}

/// Rough token estimate (about four bytes per token). An estimate, not a count:
/// real accounting comes from `ta cost report`.
pub fn estimate_tokens(text: &str) -> usize {
    text.len().div_ceil(4)
}
