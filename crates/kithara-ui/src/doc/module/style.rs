use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[non_exhaustive]
pub enum IconName {
    Activity,
    Bell,
    ChevronDown,
    ChevronRight,
    ChevronUp,
    Circle,
    Clock,
    Crown,
    Disc,
    Faders,
    FastForward,
    FolderPlus,
    Gear,
    Headphones,
    Lock,
    LockOpen,
    Maximize,
    Menu,
    Monitor,
    Orbit,
    Play,
    PlayReverse,
    Playlist,
    Plus,
    Radio,
    RefreshCw,
    Rewind,
    Save,
    SlidersHorizontal,
    SpeakerHigh,
    Waveform,
    X,
    ZoomIn,
    ZoomOut,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[non_exhaustive]
pub enum TextAlign {
    #[default]
    Start,
    Center,
    End,
}

/// The geometry a popover surface opens from.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[non_exhaustive]
pub enum PopoverAt {
    #[default]
    Anchor,
    Pointer,
}

/// Which edge of the popover surface lines up with that geometry.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[non_exhaustive]
pub enum PopoverAlign {
    #[default]
    Start,
    End,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[non_exhaustive]
pub enum GlyphStyle {
    #[default]
    Default,
    Vis,
    Menu,
    MenuBurger,
    MenuSmall,
    MenuCell,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[non_exhaustive]
pub enum DeckSummaryStyle {
    #[default]
    Default,
    Micro,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[non_exhaustive]
pub enum WindowControlsStyle {
    #[default]
    Standard,
    Compact,
    CloseWide,
    CloseMicro,
    CloseFramed,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[non_exhaustive]
pub enum TextStyle {
    #[default]
    Body,
    Brand,
    BrandSmall,
    DeckLetter,
    TrackTitle,
    Telemetry,
    MicroLabel,
    Section,
    Mono,
    PivotArrow,
    PivotDuration,
    PivotFooter,
    PivotLabel,
    PivotRatio,
    PivotSmall,
    PivotTrackArtist,
    PivotTrackTitle,
    PivotTitle,
    PivotValue,
    Caption,
    VisFooter,
    VisMeta,
    VisTitle,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[non_exhaustive]
pub enum ButtonStyle {
    #[default]
    Default,
    Transport,
    TransportPrimary,
    MicroPrimary,
    VisNav,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[non_exhaustive]
pub enum ScalarFormat {
    #[default]
    Default,
    Percent,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[non_exhaustive]
pub enum FaderStyle {
    #[default]
    Default,
    Volume,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[non_exhaustive]
pub enum ChipStyle {
    #[default]
    Deck,
    PivotFamily,
    PivotMultiplier,
    Routing,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[non_exhaustive]
pub enum WaveStyle {
    #[default]
    Default,
    Hero,
    Micro,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct TableColumn {
    id: String,
    label: String,
    style: TableColumnStyle,
    width: f32,
    #[serde(default)]
    flexible: bool,
}

impl TableColumn {
    pub fn new<I, L>(id: I, label: L, style: TableColumnStyle, width: f32, flexible: bool) -> Self
    where
        I: Into<String>,
        L: Into<String>,
    {
        Self {
            id: id.into(),
            label: label.into(),
            style,
            width,
            flexible,
        }
    }

    #[must_use]
    pub fn flexible(&self) -> bool {
        self.flexible
    }

    #[must_use]
    pub fn id(&self) -> &str {
        &self.id
    }

    #[must_use]
    pub fn label(&self) -> &str {
        &self.label
    }

    #[must_use]
    pub fn style(&self) -> TableColumnStyle {
        self.style
    }

    #[must_use]
    pub fn width(&self) -> f32 {
        self.width
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[non_exhaustive]
pub enum TableColumnStyle {
    Index,
    Badge,
    Primary,
    #[default]
    Secondary,
    Metric,
    Mono,
    Time,
    Meter,
    Transition,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[non_exhaustive]
pub enum Tone {
    #[default]
    Neutral,
    Accent,
    Success,
    Danger,
}
