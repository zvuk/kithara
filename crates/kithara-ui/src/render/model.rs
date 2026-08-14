/// Stereo levels and volume exposed to renderers.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct StereoLevels {
    pub l: f32,
    pub r: f32,
    pub volume: f32,
}

/// One normalized waveform column.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct WaveBucket {
    pub high: f32,
    pub low: f32,
    pub mid: f32,
}

/// Borrowed waveform data exposed to renderers.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WaveformView<'a> {
    pub buckets: &'a [WaveBucket],
    pub beats: &'a [f32],
    pub cues: &'a [f32],
    pub downbeats: &'a [f32],
    pub bpm: Option<f32>,
    pub r#loop: Option<[f32; 2]>,
}

/// One destination tempo drawn by a portal map.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct PortalTarget {
    pub bpm: f32,
    pub is_selected: bool,
}

/// Borrowed tempo-ratio map exposed to renderers.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PortalMapView<'a> {
    pub master: f32,
    pub min: f32,
    pub max: f32,
    pub targets: &'a [PortalTarget],
}

/// Normalized lower and upper values exposed to a range control.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ScalarRange {
    pub min: f32,
    pub max: f32,
}

/// Icon associated with a renderer-facing tree row.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum TreeIcon {
    Collection,
    Playlist,
    Folder,
    Plus,
    Zvuk,
    Search,
    Charts,
    Monitor,
    Home,
    Usb,
    Instrument,
    Waveform,
    Clock,
}

/// Borrowed browser-tree row exposed to renderers.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TreeRow<'a> {
    pub label: &'a str,
    pub count: Option<u32>,
    pub expanded: Option<bool>,
    pub icon: TreeIcon,
    pub muted: bool,
    pub selected: bool,
    pub depth: u8,
}

/// Borrowed track-list row exposed to renderers.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TrackRow<'a> {
    pub title: &'a str,
    pub artist: Option<&'a str>,
    pub bpm: Option<&'a str>,
    pub deck: Option<&'a str>,
    pub energy: Option<u8>,
    pub key: Option<&'a str>,
    pub search: Option<&'a str>,
    pub time: Option<&'a str>,
    pub transition: Option<&'a str>,
    pub selected: bool,
}

/// Value resolved from a renderer-facing read endpoint.
#[derive(Clone, Copy, Debug, PartialEq)]
#[non_exhaustive]
pub enum ReadValue<'a> {
    Text(&'a str),
    Bool(bool),
    Scalar(f64),
    Stereo(StereoLevels),
    Waveform(WaveformView<'a>),
    PortalMap(PortalMapView<'a>),
    Range(ScalarRange),
    TrackList(&'a [TrackRow<'a>]),
    Tree(&'a [TreeRow<'a>]),
}

/// Renderer-facing endpoint reader. Endpoints are canonical scoped keys:
/// `<id>` for unscoped bindings, `<id>@<k>=<v>[,...]` for scoped ones.
pub trait Reads {
    fn get(&self, endpoint: &str) -> Option<ReadValue<'_>>;
}

/// Derived widget endpoint qualified by the control's scope suffix.
pub(crate) fn derived(base: &str, scope: &str) -> String {
    let mut endpoint = String::with_capacity(base.len() + scope.len());
    endpoint.push_str(base);
    endpoint.push_str(scope);
    endpoint
}
