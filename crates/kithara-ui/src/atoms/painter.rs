use crate::{
    atoms::{
        bar::{
            brand::Brand,
            context::{Context, Viewed},
            divider::Divider,
            settings::Settings,
            spacer::Spacer,
        },
        button::{Button, ButtonLabel, VisualState},
        chip::Chip,
        deck::{
            clock::{Clock, Elapsed},
            summary::{Loaded, Summary},
            tempo::{Reading as Beat, Tempo},
        },
        design::{
            cell::Cell,
            crossfader::Crossfader,
            fader::Fader,
            meter::Meter,
            segmented::{Segmented, SegmentedData},
            select::Select,
            status_dot::StatusDot,
            swatch::Swatch,
        },
        icon::glyph::{Glyph, GlyphData},
        knob::Knob,
        label::telemetry::Telemetry,
        meter::StereoMeter,
        nav_item::NavItem,
        readout::{Readout, ReadoutData},
        tab::TabLarge,
        toggle::Binary,
        vu::VerticalVu,
        wave::face::{Drawn, Wave},
    },
    draw::{DrawListBuilder, Rect},
    render::{Mark, StereoLevels},
    solve::{Length, Size},
    text::TextContext,
};

/// A neutral painter, drawn the same way by every host.
///
/// The skin is resolved when the painter is built; everything that changes
/// while it is mounted arrives as `Data`. A host that keeps its widgets also
/// needs to be told when that data changes — see the `Retained` half of the
/// contract, which only such a host implements.
pub(crate) trait ControlPainter {
    /// What the host hands the painter each frame: a word for most, the pair a
    /// button swaps between while active, a value for a meter.
    type Data;

    /// Whether the pointer resting on or pressing the control changes what it
    /// draws, which decides if a host tracks those edges and repaints on them.
    const READS_POINTER: bool = false;

    fn draw(
        &self,
        list: &mut DrawListBuilder,
        text: &mut TextContext,
        data: &Self::Data,
        bounds: Rect,
        state: VisualState,
    );

    /// The box it asks for when the skin, rather than the row it sits in,
    /// settles an axis.
    ///
    /// A share of the row is the one length a document cannot state — `Dim` has
    /// no portion, and the portions in this repository all come from the skin —
    /// so it is said once here rather than once per host.
    fn length(&self, _text: &mut TextContext, _data: &Self::Data) -> Size<Length> {
        Size::new(Length::Fill, Length::Fill)
    }

    /// How big it actually is, on the axes it settles for itself.
    ///
    /// A zero on an axis means the painter has no opinion there and the row
    /// decides — which is what both hosts already do with a leaf that does not
    /// measure. Only the painters whose [`Self::length`] can answer `Shrink` or
    /// a measured `Fixed` need this; the rest fill what they are given.
    fn measure(&self, _text: &mut TextContext, _data: &Self::Data) -> Size {
        Size::ZERO
    }

    /// The part of its box the pointer works, when that is not all of it.
    ///
    /// A fader is a rail with a caption beside it, and the hand drives the rail
    /// alone — measuring the gesture against the whole control would put the
    /// zero of the value under the caption. Only the painter knows where it put
    /// the rail, so it is asked rather than told.
    fn grip_bounds(&self, _data: &Self::Data, bounds: Rect) -> Rect {
        bounds
    }
}

/// What a control that shows one word and a state is handed each frame.
pub(crate) struct Labelled {
    pub(crate) active: bool,
    pub(crate) label: String,
}

impl ControlPainter for Chip {
    type Data = Labelled;

    fn draw(
        &self,
        list: &mut DrawListBuilder,
        text: &mut TextContext,
        data: &Self::Data,
        bounds: Rect,
        _state: VisualState,
    ) {
        self.paint(list, text, &data.label, data.active, bounds);
    }
}

/// What a nav item is handed each frame: its word, its state, and the mark it
/// shows beside them.
///
/// The mark travels with the word rather than with the skin because reading an
/// authored icon can fail, and a control whose art cannot be read draws nothing
/// at all rather than a row with a hole in it.
pub(crate) struct NavData {
    pub(crate) active: bool,
    pub(crate) label: String,
    pub(crate) mark: Mark,
}

impl ControlPainter for NavItem {
    type Data = NavData;

    fn draw(
        &self,
        list: &mut DrawListBuilder,
        text: &mut TextContext,
        data: &Self::Data,
        bounds: Rect,
        _state: VisualState,
    ) {
        self.paint(list, text, data, bounds);
    }
}

impl ControlPainter for Glyph {
    type Data = GlyphData;

    fn draw(
        &self,
        list: &mut DrawListBuilder,
        text: &mut TextContext,
        data: &Self::Data,
        bounds: Rect,
        _state: VisualState,
    ) {
        self.paint(list, text, data, bounds);
    }
}

impl ControlPainter for TabLarge {
    type Data = Labelled;

    fn draw(
        &self,
        list: &mut DrawListBuilder,
        text: &mut TextContext,
        data: &Self::Data,
        bounds: Rect,
        _state: VisualState,
    ) {
        self.paint(list, text, &data.label, data.active, bounds);
    }

    /// A tab is as wide as its own word: a strip of tabs is a row of headings,
    /// not a set of equal columns, so a tab that filled its share would move
    /// its neighbours whenever a word changed.
    fn length(&self, _text: &mut TextContext, _data: &Self::Data) -> Size<Length> {
        Self::declared_length(self.height())
    }

    fn measure(&self, text: &mut TextContext, data: &Self::Data) -> Size {
        let (width, height) = self.intrinsic_size(text, &data.label);
        Size::new(width, height)
    }
}

/// What a control that sets one fraction and captions it is handed each frame.
pub(crate) struct Captioned {
    pub(crate) label: Option<String>,
    pub(crate) value: f32,
}

impl ControlPainter for Knob {
    type Data = Captioned;

    fn draw(
        &self,
        list: &mut DrawListBuilder,
        text: &mut TextContext,
        data: &Self::Data,
        bounds: Rect,
        _state: VisualState,
    ) {
        self.paint(list, text, data.value, data.label.as_deref(), bounds);
    }
}

impl ControlPainter for Fader {
    type Data = Captioned;

    fn draw(
        &self,
        list: &mut DrawListBuilder,
        text: &mut TextContext,
        data: &Self::Data,
        bounds: Rect,
        _state: VisualState,
    ) {
        self.paint(list, text, data.value, data.label.as_deref(), bounds);
    }

    fn grip_bounds(&self, data: &Self::Data, bounds: Rect) -> Rect {
        self.rail(bounds, data.label.is_some())
    }
}

impl ControlPainter for Crossfader {
    type Data = f32;

    fn draw(
        &self,
        list: &mut DrawListBuilder,
        text: &mut TextContext,
        data: &Self::Data,
        bounds: Rect,
        _state: VisualState,
    ) {
        self.paint(list, text, *data, bounds);
    }
}

impl ControlPainter for VerticalVu {
    type Data = StereoLevels;

    fn draw(
        &self,
        list: &mut DrawListBuilder,
        _text: &mut TextContext,
        data: &Self::Data,
        bounds: Rect,
        _state: VisualState,
    ) {
        self.paint(list, *data, bounds);
    }
}

impl ControlPainter for StereoMeter {
    type Data = StereoLevels;

    fn draw(
        &self,
        list: &mut DrawListBuilder,
        _text: &mut TextContext,
        data: &Self::Data,
        bounds: Rect,
        _state: VisualState,
    ) {
        self.paint(list, *data, bounds);
    }
}

/// What a button is handed each frame: the word for each of its states, and
/// which state it is in.
pub(crate) struct ButtonData {
    pub(crate) active: bool,
    pub(crate) label: ButtonLabel<String>,
}

impl ControlPainter for Button {
    type Data = ButtonData;

    const READS_POINTER: bool = true;

    fn draw(
        &self,
        list: &mut DrawListBuilder,
        text: &mut TextContext,
        data: &Self::Data,
        bounds: Rect,
        state: VisualState,
    ) {
        self.paint(list, text, &data.label, data.active, bounds, state);
    }

    fn length(&self, text: &mut TextContext, data: &Self::Data) -> Size<Length> {
        self.declared(text, &data.label, data.active)
    }

    /// Only the width: every button fills the height of the row it sits in.
    fn measure(&self, text: &mut TextContext, data: &Self::Data) -> Size {
        Size::new(self.intrinsic_width(text, &data.label, data.active), 0.0)
    }
}

impl ControlPainter for Binary {
    type Data = bool;

    fn draw(
        &self,
        list: &mut DrawListBuilder,
        _text: &mut TextContext,
        data: &Self::Data,
        bounds: Rect,
        _state: VisualState,
    ) {
        self.paint(list, *data, bounds);
    }
}

impl ControlPainter for Meter {
    type Data = f32;

    fn draw(
        &self,
        list: &mut DrawListBuilder,
        _text: &mut TextContext,
        data: &Self::Data,
        bounds: Rect,
        _state: VisualState,
    ) {
        self.paint(list, *data, bounds);
    }
}

impl ControlPainter for StatusDot {
    type Data = String;

    fn draw(
        &self,
        list: &mut DrawListBuilder,
        text: &mut TextContext,
        data: &Self::Data,
        bounds: Rect,
        _state: VisualState,
    ) {
        self.paint(list, text, data, bounds);
    }
}

/// What a cell is handed each frame: its caption, and whether it is the one
/// picked out.
pub(crate) struct CellData {
    pub(crate) highlighted: bool,
    pub(crate) label: Option<String>,
}

impl ControlPainter for Cell {
    type Data = CellData;

    fn draw(
        &self,
        list: &mut DrawListBuilder,
        text: &mut TextContext,
        data: &Self::Data,
        bounds: Rect,
        _state: VisualState,
    ) {
        self.paint(list, text, data.label.as_deref(), data.highlighted, bounds);
    }
}

impl ControlPainter for Brand {
    type Data = ();

    fn draw(
        &self,
        list: &mut DrawListBuilder,
        text: &mut TextContext,
        _data: &Self::Data,
        bounds: Rect,
        _state: VisualState,
    ) {
        self.paint(list, text, bounds);
    }
}

impl ControlPainter for Divider {
    type Data = ();

    fn draw(
        &self,
        list: &mut DrawListBuilder,
        _text: &mut TextContext,
        _data: &Self::Data,
        bounds: Rect,
        _state: VisualState,
    ) {
        self.paint(list, bounds);
    }
}

impl ControlPainter for Spacer {
    type Data = ();

    fn draw(
        &self,
        list: &mut DrawListBuilder,
        _text: &mut TextContext,
        _data: &Self::Data,
        bounds: Rect,
        _state: VisualState,
    ) {
        self.paint(list, bounds);
    }
}

/// The naming panel steps aside while the pointer is on the waveform, so the
/// hand can see the shape it is about to scrub.
impl ControlPainter for Wave {
    type Data = Drawn;

    const READS_POINTER: bool = true;

    fn draw(
        &self,
        list: &mut DrawListBuilder,
        text: &mut TextContext,
        data: &Self::Data,
        bounds: Rect,
        state: VisualState,
    ) {
        self.paint(list, text, data, bounds, matches!(state, VisualState::Idle));
    }
}

impl ControlPainter for Context {
    type Data = Viewed;

    fn draw(
        &self,
        list: &mut DrawListBuilder,
        text: &mut TextContext,
        data: &Self::Data,
        bounds: Rect,
        _state: VisualState,
    ) {
        self.paint(list, text, data, bounds);
    }
}

impl ControlPainter for Settings {
    type Data = Mark;

    const READS_POINTER: bool = true;

    fn draw(
        &self,
        list: &mut DrawListBuilder,
        text: &mut TextContext,
        data: &Self::Data,
        bounds: Rect,
        state: VisualState,
    ) {
        self.paint(list, text, *data, bounds, state);
    }
}

impl ControlPainter for Readout {
    type Data = ReadoutData;

    fn draw(
        &self,
        list: &mut DrawListBuilder,
        text: &mut TextContext,
        data: &Self::Data,
        bounds: Rect,
        _state: VisualState,
    ) {
        self.paint(list, text, data, bounds);
    }
}

impl ControlPainter for Clock {
    type Data = Elapsed;

    fn draw(
        &self,
        list: &mut DrawListBuilder,
        text: &mut TextContext,
        data: &Self::Data,
        bounds: Rect,
        _state: VisualState,
    ) {
        self.paint(list, text, data, bounds);
    }
}

impl ControlPainter for Summary {
    type Data = Loaded;

    fn draw(
        &self,
        list: &mut DrawListBuilder,
        text: &mut TextContext,
        data: &Self::Data,
        bounds: Rect,
        _state: VisualState,
    ) {
        self.paint(list, text, data, bounds);
    }

    /// A deck's headline is wider than the controls beside it by a factor the
    /// skin names, so `Fill` would divide the row evenly and the deck would be
    /// wrong.
    fn length(&self, _text: &mut TextContext, _data: &Self::Data) -> Size<Length> {
        Self::declared_length(self.metrics())
    }
}

impl ControlPainter for Tempo {
    type Data = Beat;

    fn draw(
        &self,
        list: &mut DrawListBuilder,
        text: &mut TextContext,
        data: &Self::Data,
        bounds: Rect,
        _state: VisualState,
    ) {
        self.paint(list, text, data, bounds);
    }
}

impl ControlPainter for Telemetry {
    type Data = f64;

    fn draw(
        &self,
        list: &mut DrawListBuilder,
        text: &mut TextContext,
        data: &Self::Data,
        bounds: Rect,
        _state: VisualState,
    ) {
        self.paint(list, text, &self.format(*data), bounds);
    }

    fn length(&self, _text: &mut TextContext, _data: &Self::Data) -> Size<Length> {
        self.declared()
    }

    /// Only the width: a reading fills the height of the row it sits in.
    fn measure(&self, text: &mut TextContext, data: &Self::Data) -> Size {
        Size::new(self.intrinsic_width(text, &self.format(*data)), 0.0)
    }
}

impl ControlPainter for Segmented {
    type Data = SegmentedData;

    fn draw(
        &self,
        list: &mut DrawListBuilder,
        text: &mut TextContext,
        data: &Self::Data,
        bounds: Rect,
        _state: VisualState,
    ) {
        self.paint(list, text, data, bounds);
    }
}

impl ControlPainter for Select {
    type Data = String;

    fn draw(
        &self,
        list: &mut DrawListBuilder,
        text: &mut TextContext,
        data: &Self::Data,
        bounds: Rect,
        _state: VisualState,
    ) {
        self.paint(list, text, data, bounds);
    }
}

impl ControlPainter for Swatch {
    type Data = String;

    fn draw(
        &self,
        list: &mut DrawListBuilder,
        text: &mut TextContext,
        data: &Self::Data,
        bounds: Rect,
        _state: VisualState,
    ) {
        self.paint(list, text, data, bounds);
    }
}
