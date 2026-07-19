/// One normalized waveform column.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct WaveBucket {
    pub low: f32,
    pub mid: f32,
    pub high: f32,
}

/// Borrowed waveform data exposed to renderers.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WaveformView<'a> {
    pub buckets: &'a [WaveBucket],
    pub beats: &'a [f32],
    pub downbeats: &'a [f32],
    pub bpm: Option<f32>,
}

/// Borrowed track-list row exposed to renderers.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TrackRow<'a> {
    pub title: &'a str,
    pub artist: Option<&'a str>,
    pub time: Option<&'a str>,
}

/// Value resolved from a renderer-facing read endpoint.
#[derive(Clone, Copy, Debug, PartialEq)]
#[non_exhaustive]
pub enum ReadValue<'a> {
    Text(&'a str),
    Bool(bool),
    Scalar(f64),
    Waveform(WaveformView<'a>),
    TrackList(&'a [TrackRow<'a>]),
}

/// Renderer-facing endpoint reader.
pub trait Reads {
    fn get(&self, endpoint: &str) -> Option<ReadValue<'_>>;
}
