use crate::render::WaveBucket;

#[derive(PartialEq)]
pub(crate) struct WaveformData {
    pub(crate) buckets: Box<[WaveBucket]>,
    pub(crate) beats: Box<[f32]>,
    pub(crate) downbeats: Box<[f32]>,
    pub(crate) loop_region: Option<[f32; 2]>,
    pub(crate) cues: Box<[f32]>,
}

pub(crate) struct OverlayData {
    pub(crate) title: String,
    pub(crate) artist: String,
    pub(crate) bpm: String,
    pub(crate) key: String,
    pub(crate) remain: String,
    pub(crate) badge: String,
}
