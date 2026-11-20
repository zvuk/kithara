use std::{borrow::Cow, collections::BTreeMap};

use crate::{draw::Pt, module::IconName};

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
    /// Track fractions the analysis has not covered, as `[start, end]` pairs.
    pub unready: &'a [[f32; 2]],
    pub beats: &'a [f32],
    pub cues: &'a [f32],
    pub downbeats: &'a [f32],
    pub bpm: Option<f32>,
    pub r#loop: Option<[f32; 2]>,
    /// What the model calls this run of buckets.
    ///
    /// A different value means a different run. A viewer that keeps a copy —
    /// which every retained host does — takes that on trust instead of
    /// comparing a megabyte of buckets against its copy on every frame, so
    /// whoever writes the buckets must move this when it writes them.
    pub revision: u64,
}

/// One destination tempo drawn by a portal map.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct PortalTarget {
    pub is_selected: bool,
    pub bpm: f32,
}

/// Borrowed tempo-ratio map exposed to renderers.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PortalMapView<'a> {
    pub targets: &'a [PortalTarget],
    pub master: f32,
    pub max: f32,
    pub min: f32,
}

/// Normalized lower and upper values exposed to a range control.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ScalarRange {
    pub max: f32,
    pub min: f32,
}

/// Borrowed browser-tree row exposed to renderers.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TreeRow<'a> {
    pub label: &'a str,
    pub icon: IconName,
    pub count: Option<u32>,
    pub expanded: Option<bool>,
    /// Whether the row has a page of its own. A row with children and no page
    /// opens and closes on a press anywhere on it; one with a page is selected
    /// from its label and opened from its chevron. A row without children
    /// selects either way.
    pub page: bool,
    pub muted: bool,
    pub selected: bool,
    pub depth: u8,
}

/// One letter of a badge cell, marked while what it names is active.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Badge<'a> {
    pub label: &'a str,
    pub active: bool,
}

/// Borrowed value in one renderer-facing table cell.
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum TableValue<'a> {
    Empty,
    /// A pressed icon publishes `action` through its document column's write.
    Icon {
        icon: IconName,
        active: bool,
        action: Option<Cow<'a, str>>,
    },
    Number(u8),
    Text(Cow<'a, str>),
    Badges(&'a [Badge<'a>]),
}

/// A table cell addressed by the document column id.
#[derive(Clone, Debug, Eq, PartialEq)]
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
    pub const fn id(&self) -> &'a str {
        self.id
    }

    #[must_use]
    pub const fn number(id: &'a str, value: u8) -> Self {
        Self {
            id,
            value: TableValue::Number(value),
        }
    }

    #[must_use]
    pub const fn badges(id: &'a str, value: &'a [Badge<'a>]) -> Self {
        Self {
            id,
            value: TableValue::Badges(value),
        }
    }

    #[must_use]
    pub fn text<T: Into<Cow<'a, str>>>(id: &'a str, value: T) -> Self {
        Self {
            id,
            value: TableValue::Text(value.into()),
        }
    }

    #[must_use]
    pub const fn value(&self) -> &TableValue<'a> {
        &self.value
    }

    #[must_use]
    pub const fn icon(id: &'a str, icon: IconName, active: bool) -> Self {
        Self {
            id,
            value: TableValue::Icon {
                icon,
                active,
                action: None,
            },
        }
    }

    /// Text an icon cell publishes through the document column's write when
    /// pressed. Only icon cells take a press.
    #[must_use]
    pub fn with_action<T: Into<Cow<'a, str>>>(mut self, text: T) -> Self {
        if let TableValue::Icon { action, .. } = &mut self.value {
            *action = Some(text.into());
        }
        self
    }

    #[must_use]
    pub fn action(&self) -> Option<&str> {
        match &self.value {
            TableValue::Icon { action, .. } => action.as_deref(),
            _ => None,
        }
    }
}

/// One renderer-facing table row. Cells carry document column ids, so the
/// same model can serve any declared order or subset.
#[derive(Clone, Debug, Eq, PartialEq, fieldwork::Fieldwork)]
#[fieldwork(opt_in, with)]
pub struct TableRow<'a> {
    cells: Vec<TableCell<'a>>,
    #[field(
        with(
            option_set_some,
            doc = "What the row carries out of its table, which a drop zone writes."
        ),
        vis = "pub"
    )]
    drag: Option<Cow<'a, BTreeMap<String, String>>>,
    selected: bool,
    #[field(
        with(doc = "Use the table's muted text colour without changing interaction data."),
        vis = "pub"
    )]
    muted: bool,
}

impl<'a> TableRow<'a> {
    #[must_use]
    pub fn with_cell(mut self, cell: TableCell<'a>) -> Self {
        self.cells.push(cell);
        self
    }

    #[must_use]
    pub fn new(cells: Vec<TableCell<'a>>, selected: bool) -> Self {
        Self {
            cells,
            drag: None,
            selected,
            muted: false,
        }
    }

    #[must_use]
    pub fn muted(&self) -> bool {
        self.muted
    }

    #[must_use]
    pub fn drag(&self) -> Option<&BTreeMap<String, String>> {
        self.drag.as_deref()
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
    Color(crate::skin::ColorRole),
    Text(&'a str),
    Image(&'a crate::draw::Image),
    Bool(bool),
    Scalar(f64),
    Point(Pt),
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
