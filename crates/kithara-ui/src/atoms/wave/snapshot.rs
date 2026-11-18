use crate::render::{WaveBucket, WaveformView};

#[derive(Clone, PartialEq)]
pub(crate) struct WaveformData {
    pub(crate) beats: Box<[f32]>,
    pub(crate) buckets: Box<[WaveBucket]>,
    pub(crate) cues: Box<[f32]>,
    pub(crate) downbeats: Box<[f32]>,
    /// Track fractions the analysis has not covered.
    pub(crate) unready: Box<[[f32; 2]]>,
    pub(crate) loop_region: Option<[f32; 2]>,
    pub(crate) revision: u64,
}

impl From<WaveformView<'_>> for WaveformData {
    fn from(view: WaveformView<'_>) -> Self {
        Self {
            buckets: view.buckets.to_vec().into_boxed_slice(),
            revision: view.revision,
            beats: view.beats.to_vec().into_boxed_slice(),
            downbeats: view.downbeats.to_vec().into_boxed_slice(),
            unready: view.unready.to_vec().into_boxed_slice(),
            loop_region: view.r#loop,
            cues: view.cues.to_vec().into_boxed_slice(),
        }
    }
}

#[derive(Clone, PartialEq)]
pub(crate) struct OverlayData {
    pub(crate) art: Option<crate::draw::Image>,
    pub(crate) artist: String,
    pub(crate) badge: String,
    pub(crate) bpm: String,
    pub(crate) key: String,
    pub(crate) key_color: Option<crate::skin::ColorRole>,
    pub(crate) remain: String,
    pub(crate) title: String,
}
