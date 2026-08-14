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

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[non_exhaustive]
pub enum TrackColumn {
    Index,
    Deck,
    Title,
    Artist,
    Bpm,
    Key,
    Time,
    Energy,
    Transition,
}

impl TrackColumn {
    #[must_use]
    pub const fn endpoint_name(self) -> &'static str {
        match self {
            Self::Index => "index",
            Self::Deck => "deck",
            Self::Title => "title",
            Self::Artist => "artist",
            Self::Bpm => "bpm",
            Self::Key => "key",
            Self::Time => "time",
            Self::Energy => "energy",
            Self::Transition => "transition",
        }
    }
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
