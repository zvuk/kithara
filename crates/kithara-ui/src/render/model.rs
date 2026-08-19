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
    /// What the model calls this run of buckets.
    ///
    /// A different value means a different run. A viewer that keeps a copy —
    /// which every retained host does — takes that on trust instead of
    /// comparing a megabyte of buckets against its copy on every frame, so
    /// whoever writes the buckets must move this when it writes them.
    pub revision: u64,
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

/// Borrowed value in one renderer-facing table cell.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum TableValue<'a> {
    Empty,
    Number(u8),
    Text(&'a str),
}

/// A table cell addressed by the document column id.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TableCell<'a> {
    id: &'a str,
    value: TableValue<'a>,
}

impl<'a> TableCell<'a> {
    #[must_use]
    pub const fn empty(id: &'a str) -> Self {
        Self {
            id,
            value: TableValue::Empty,
        }
    }

    #[must_use]
    pub const fn number(id: &'a str, value: u8) -> Self {
        Self {
            id,
            value: TableValue::Number(value),
        }
    }

    #[must_use]
    pub const fn text(id: &'a str, value: &'a str) -> Self {
        Self {
            id,
            value: TableValue::Text(value),
        }
    }

    #[must_use]
    pub const fn id(&self) -> &'a str {
        self.id
    }

    #[must_use]
    pub const fn value(&self) -> TableValue<'a> {
        self.value
    }
}

/// One renderer-facing table row. Cells carry document column ids, so the
/// same model can serve any declared order or subset.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TableRow<'a> {
    cells: Vec<TableCell<'a>>,
    selected: bool,
}

impl<'a> TableRow<'a> {
    #[must_use]
    pub fn new(cells: Vec<TableCell<'a>>, selected: bool) -> Self {
        Self { cells, selected }
    }

    #[must_use]
    pub fn cells(&self) -> &[TableCell<'a>] {
        &self.cells
    }

    #[must_use]
    pub fn selected(&self) -> bool {
        self.selected
    }
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
    Table(&'a [TableRow<'a>]),
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
