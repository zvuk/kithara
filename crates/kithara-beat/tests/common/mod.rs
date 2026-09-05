//! Shared helpers for the parity tests: fixtures, golden loading and MIR
//! F-measure.

use std::path::{Path, PathBuf};

use num_traits::cast::AsPrimitive;
use serde::Deserialize;

/// The standard MIR tolerance a beat is matched within.
pub(crate) const WINDOW: f64 = 0.070;

pub(crate) fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

pub(crate) fn load_pcm_fixture(name: &str) -> Vec<f32> {
    let path = fixture(name);
    let bytes = std::fs::read(&path)
        .unwrap_or_else(|e| panic!("failed to read PCM fixture {}: {e}", path.display()));
    assert_eq!(
        bytes.len() % 4,
        0,
        "raw f32le fixture length must be a multiple of 4"
    );
    bytes
        .chunks_exact(4)
        .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
        .collect()
}

pub(crate) fn report(kind: &str, s: &Score) {
    eprintln!(
        "{kind}: F={:.4} matched {}/{} (ref {}) max_diff={:.1}ms mean_diff={:.1}ms",
        s.f_measure,
        s.matched,
        s.n_est,
        s.n_ref,
        s.max_matched_diff * 1000.0,
        s.mean_matched_diff * 1000.0,
    );
}

/// A golden output for parity tests.
#[derive(Debug, Deserialize)]
pub(crate) struct Golden {
    pub beats: Vec<f32>,
    pub downbeats: Vec<f32>,
}

/// Load a committed golden JSON fixture, panicking with a clear message on failure.
pub(crate) fn load_golden(path: &Path) -> Golden {
    let text = std::fs::read_to_string(path)
        .unwrap_or_else(|e| panic!("failed to read golden {}: {e}", path.display()));
    serde_json::from_str(&text)
        .unwrap_or_else(|e| panic!("failed to parse golden {}: {e}", path.display()))
}

/// Result of an F-measure comparison, including timing diagnostics.
pub(crate) struct Score {
    pub f_measure: f64,
    pub max_matched_diff: f64,
    pub mean_matched_diff: f64,
    pub matched: usize,
    pub n_est: usize,
    pub n_ref: usize,
}

/// Standard MIR beat F-measure with one-to-one matching inside `window` seconds.
///
/// Greedy two-pointer over sorted sequences: for near-identical inputs this equals
/// the optimal bipartite matching; if it ever under-matches it lowers F (fails loud).
pub(crate) fn f_measure(reference: &[f32], estimated: &[f32], window: f64) -> Score {
    let mut r: Vec<f64> = reference.iter().map(|&x| f64::from(x)).collect();
    let mut e: Vec<f64> = estimated.iter().map(|&x| f64::from(x)).collect();
    r.sort_by(f64::total_cmp);
    e.sort_by(f64::total_cmp);

    let (mut i, mut j, mut matched) = (0usize, 0usize, 0usize);
    let mut max_diff = 0.0f64;
    let mut sum_diff = 0.0f64;
    while i < r.len() && j < e.len() {
        let d = (r[i] - e[j]).abs();
        if d <= window {
            matched += 1;
            max_diff = max_diff.max(d);
            sum_diff += d;
            i += 1;
            j += 1;
        } else if r[i] < e[j] {
            i += 1;
        } else {
            j += 1;
        }
    }

    let matched_f: f64 = matched.as_();
    let e_len: f64 = e.len().as_();
    let r_len: f64 = r.len().as_();
    let precision = if e.is_empty() { 0.0 } else { matched_f / e_len };
    let recall = if r.is_empty() { 0.0 } else { matched_f / r_len };
    let f = if precision + recall > 0.0 {
        2.0 * precision * recall / (precision + recall)
    } else {
        0.0
    };
    Score {
        f_measure: f,
        matched,
        n_ref: r.len(),
        n_est: e.len(),
        max_matched_diff: if matched > 0 { max_diff } else { 0.0 },
        mean_matched_diff: if matched > 0 {
            sum_diff / matched_f
        } else {
            0.0
        },
    }
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::*;

    #[kithara::test(native, flash(false))]
    fn identical_sequences_score_one() {
        let seq = vec![0.3, 0.54, 0.84, 1.12];
        let s = f_measure(&seq, &seq, 0.070);
        assert_eq!(s.f_measure, 1.0);
        assert_eq!(s.matched, 4);
        assert_eq!(s.max_matched_diff, 0.0);
    }

    #[kithara::test(native, flash(false))]
    fn within_window_still_matches() {
        let reference = vec![1.0, 2.0, 3.0];
        let estimated = vec![1.05, 1.96, 3.02];
        let s = f_measure(&reference, &estimated, 0.070);
        assert_eq!(s.f_measure, 1.0);
        assert!(s.max_matched_diff <= 0.05 + 1e-9);
    }

    #[kithara::test(native, flash(false))]
    fn beat_outside_window_lowers_f() {
        let reference = vec![1.0, 2.0, 3.0];
        let estimated = vec![1.0, 2.0, 3.2];
        let s = f_measure(&reference, &estimated, 0.070);
        assert_eq!(s.matched, 2);
        assert!(s.f_measure < 1.0);
    }

    #[kithara::test(native, flash(false))]
    fn extra_estimate_lowers_precision() {
        let reference = vec![1.0, 2.0];
        let estimated = vec![1.0, 2.0, 5.0];
        let s = f_measure(&reference, &estimated, 0.070);
        assert_eq!(s.matched, 2);
        assert!(s.f_measure < 1.0);
    }

    #[kithara::test(native, flash(false))]
    fn empty_sequences() {
        let s = f_measure(&[], &[], 0.070);
        assert_eq!(s.f_measure, 0.0);
        assert_eq!(s.matched, 0);
    }
}
