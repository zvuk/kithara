use serde::{Deserialize, Serialize};

/// The face an icon is drawn with.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, kithara_derive::Variants)]
#[non_exhaustive]
pub enum IconName {
    Activity,
    Bell,
    Charts,
    ChevronDown,
    ChevronLeft,
    ChevronRight,
    ChevronUp,
    ChevronsLeft,
    ChevronsRight,
    Circle,
    Clock,
    Collection,
    Crown,
    Disc,
    Faders,
    FastForward,
    Folder,
    FolderPlus,
    Gear,
    Headphones,
    Heart,
    HeartFilled,
    Home,
    Instrument,
    Kithara,
    Lock,
    LockOpen,
    Maximize,
    Menu,
    Monitor,
    Music2,
    MusicNote,
    Orbit,
    Pause,
    Play,
    PlayReverse,
    Playlist,
    PlaylistAdd,
    Plus,
    Radio,
    RefreshCw,
    Repeat,
    RepeatOnce,
    Rewind,
    Save,
    Search,
    Shuffle,
    SkipBack,
    SkipForward,
    SlidersHorizontal,
    SpeakerHigh,
    SpeakerLow,
    SpeakerX,
    Usb,
    Waveform,
    X,
    ZoomIn,
    ZoomOut,
    Zvuk,
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

/// What shuts a popover that stands open on a view flag.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[non_exhaustive]
pub enum PopoverDismiss {
    /// A tap outside it or Escape.
    #[default]
    OnTapOutside,
    /// The same, and any press inside it that reaches the application.
    OnAnyAction,
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

/// The typographic roles a document may name, written once and expanded
/// wherever the set has to appear again: the word a document writes, the entry
/// a skin gives it, and the lookup that joins the two.
macro_rules! text_roles {
    ($expand:ident) => {
        $expand! {
            #[default]
            body => Body,
            brand => Brand,
            brand_small => BrandSmall,
            caption => Caption,
            cell => Cell,
            deck_letter => DeckLetter,
            micro_label => MicroLabel,
            module_title => ModuleTitle,
            mono => Mono,
            note => Note,
            pivot_arrow => PivotArrow,
            pivot_duration => PivotDuration,
            pivot_footer => PivotFooter,
            pivot_label => PivotLabel,
            pivot_ratio => PivotRatio,
            pivot_small => PivotSmall,
            pivot_track_artist => PivotTrackArtist,
            pivot_track_title => PivotTrackTitle,
            pivot_title => PivotTitle,
            pivot_value => PivotValue,
            section => Section,
            telemetry => Telemetry,
            track_title => TrackTitle,
            vis_footer => VisFooter,
            vis_meta => VisMeta,
            vis_title => VisTitle,
            window_title => WindowTitle,
        }
    };
}

pub(crate) use text_roles;

macro_rules! define_text_styles {
    ($($(#[$attr:meta])* $field:ident => $role:ident),* $(,)?) => {
        #[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
        #[non_exhaustive]
        pub enum TextStyle {
            $($(#[$attr])* $role,)*
        }
    };
}

text_roles!(define_text_styles);

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

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize, fieldwork::Fieldwork)]
#[fieldwork(opt_in, with)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct TableColumn {
    id: String,
    label: String,
    style: TableColumnStyle,
    #[serde(default)]
    flexible: bool,
    width: f32,
    #[serde(default)]
    #[field(with, option_set_some, vis = "pub(crate)")]
    write: Option<super::BindingRef>,
}

impl TableColumn {
    /// The slot, under the table's path, that a press on this column's
    /// cells writes through.
    pub(crate) fn action_slot(&self) -> String {
        format!("action/{}", self.id)
    }

    pub fn new<I, L>(id: I, label: L, style: TableColumnStyle, width: f32, flexible: bool) -> Self
    where
        I: Into<String>,
        L: Into<String>,
    {
        Self {
            style,
            width,
            flexible,
            id: id.into(),
            label: label.into(),
            write: None,
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

    #[must_use]
    pub(crate) fn write(&self) -> Option<&super::BindingRef> {
        self.write.as_ref()
    }
}

/// The space a table keeps beside its columns and whether it draws its
/// row-count footer.
#[derive(Clone, Copy, Debug, PartialEq)]
#[non_exhaustive]
pub struct TableFrame {
    pub(crate) padding_left: f32,
    pub(crate) padding_right: f32,
    pub(crate) footer: bool,
}

impl TableFrame {
    #[must_use]
    /// Creates table framing with nonnegative side padding.
    pub fn new(padding_left: f32, padding_right: f32, footer: bool) -> Self {
        Self {
            footer,
            padding_left: padding_left.max(0.0),
            padding_right: padding_right.max(0.0),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[non_exhaustive]
pub enum TableColumnStyle {
    Icon,
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
