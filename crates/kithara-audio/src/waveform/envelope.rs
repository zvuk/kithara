use std::{ops::Deref, sync::Arc};

/// Peak-normalised per-bucket magnitudes in `[0, 1]` (loudest bucket
/// `1.0`, all-zero when silent). Obtainable only via
/// [`PeakAccumulator::finalize`](super::peaks::PeakAccumulator::finalize),
/// so the invariant holds by construction.
#[derive(Debug, Clone)]
pub struct Envelope(Arc<[f32]>);

impl From<Vec<f32>> for Envelope {
    fn from(peaks: Vec<f32>) -> Self {
        Self(Arc::from(peaks))
    }
}

impl Deref for Envelope {
    type Target = [f32];

    fn deref(&self) -> &[f32] {
        &self.0
    }
}
