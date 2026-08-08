use std::rc::Rc;

use kithara_platform::time::Instant;
use num_traits::cast::AsPrimitive;

use super::custom::{HostAction, Repaint};
use crate::{
    atoms::{
        bar::{
            brand::Brand, context::Context, divider::Divider, settings::Settings, spacer::Spacer,
        },
        button::Button,
        chip::Chip,
        deck::{clock::Clock, summary::Summary, tempo::Tempo},
        design::{
            cell::Cell, crossfader::Crossfader, fader::Fader, meter::Meter, segmented::Segmented,
            select::Select, status_dot::StatusDot, swatch::Swatch,
        },
        icon::glyph::Glyph,
        knob::Knob,
        label::telemetry::Telemetry,
        meter::StereoMeter,
        nav_item::NavItem,
        painter::{ControlPainter, Labelled},
        readout::Readout,
        tab::TabLarge,
        toggle::Binary,
        vu::VerticalVu,
        wave::{face::Wave, snapshot::WaveformData},
    },
    draw::{DrawList, DrawListBuilder, Rect},
    interact::{
        CursorShape, Hit, Hover, Input, Outcome, PointerOwnership, PointerPhase,
        recognizers::{Scalar, ScalarState, click},
    },
    render::{
        ControlAction, ReadValue, Skin, StereoLevels, UiEvent, control_event,
        controls::{Drag, Grip, Press},
    },
    text::TextContext,
};

/// One built-in control mounted as a Masonry leaf.
///
/// Built-ins take this route rather than the public custom-component contract
/// because they need direct cursor and capture ownership, which that contract
/// does not expose.
pub(crate) trait MasonryControl {
    fn draw_list(&mut self, bounds: Rect) -> DrawList;

    /// How big the control is on the axes it settles for itself. A zero on an
    /// axis leaves it to the row, which is what a leaf that cannot measure has
    /// always answered on both.
    fn measure(&mut self) -> crate::solve::Size {
        crate::solve::Size::ZERO
    }

    fn input(&mut self, input: Input<'_>, hit: &Hit) -> Outcome<HostAction>;

    fn accepts_input(&self) -> bool;

    /// Takes what the control's endpoint now reads, and reports whether the
    /// control draws differently for it. This is the one way a mounted control
    /// learns a new value; no control is special-cased anywhere above it.
    fn set_read(&mut self, _value: &ReadValue<'_>) -> bool {
        false
    }

    fn cursor(&self, _hit: &Hit) -> CursorShape {
        CursorShape::None
    }

    /// Takes the hover edge the host observed and reports whether the control
    /// now draws differently.
    fn hover(&mut self, _hovered: bool) -> bool {
        false
    }

    fn repaint(&self) -> Repaint {
        Repaint::None
    }
}

#[cfg(test)]
mod flags {
    use kithara_test_utils::kithara;

    use super::{MasonryControl, Painted};
    use crate::{
        atoms::{
            button::{Button, ButtonConfig, ButtonLabel},
            chip::Chip,
            nav_item::NavItem,
            painter::{ButtonData, Labelled, NavData},
            tab::TabLarge,
        },
        builtin,
        draw::Rect,
        module::{ButtonStyle, ChipStyle},
        render::{Mark, ReadValue, Skin},
    };

    /// Every control whose picture is decided by a flag. A control that reads a
    /// flag and cannot be told the flag changed can only catch up by being
    /// rebuilt, and a rebuild throws away the gesture the hand is in the middle
    /// of, the pointer capture feeding it, and the run of clicks that makes a
    /// double click. So each one here must answer `set_read` and draw
    /// differently for it.
    #[kithara::test]
    fn a_flag_bound_control_redraws_when_its_endpoint_flips() {
        let skin = builtin::skin();
        let bounds = Rect {
            h: 30.0,
            w: 120.0,
            x: 0.0,
            y: 0.0,
        };
        let mark = Mark::Glyph(char::from(lucide_icons::Icon::Disc));
        let labelled = || Labelled {
            active: false,
            label: "MIXER".to_owned(),
        };
        let mounted: [(&str, Box<dyn MasonryControl>); 4] = [
            (
                "Chip",
                Box::new(Painted::new(
                    Chip::new(ChipStyle::Deck, skin),
                    labelled(),
                    skin,
                )),
            ),
            (
                "NavItem",
                Box::new(Painted::new(
                    NavItem::new(skin),
                    NavData {
                        active: false,
                        label: "MIXER".to_owned(),
                        mark,
                    },
                    skin,
                )),
            ),
            (
                "TabLarge",
                Box::new(Painted::new(TabLarge::new(skin), labelled(), skin)),
            ),
            ("Button", Box::new(button(skin))),
        ];

        for (name, mut control) in mounted {
            let idle = control.draw_list(bounds);
            assert!(
                control.set_read(&ReadValue::Bool(true)),
                "{name} must report that flipping its endpoint changed its picture"
            );
            assert_ne!(
                idle,
                control.draw_list(bounds),
                "{name} drew the same picture after its endpoint flipped, so only a rebuild \
                 could ever show the new state"
            );
        }
    }

    fn button(skin: &Skin) -> Painted<Button> {
        Painted::new(
            Button::new(
                ButtonConfig::builder()
                    .style(ButtonStyle::TransportPrimary)
                    .build(),
                None,
                skin,
            ),
            ButtonData {
                active: false,
                label: ButtonLabel {
                    active: Some("PAUSE".to_owned()),
                    label: "PLAY".to_owned(),
                },
            },
            skin,
        )
    }
}

/// What a wave does when its track's shape finally arrives.
#[cfg(test)]
mod analysed {
    use kithara_test_utils::kithara;

    use super::{MasonryControl, Painted};
    use crate::{
        atoms::wave::face::{Drawn, Wave},
        builtin,
        draw::Rect,
        module::WaveStyle,
        render::{ReadValue, Reads, WaveBucket, WaveformView},
    };

    struct NoReads;

    impl Reads for NoReads {
        fn get(&self, _endpoint: &str) -> Option<ReadValue<'_>> {
            None
        }
    }

    /// A deck mounts its wave before the track has been analysed, and the shape
    /// arrives later. A retained wave that could not be told would show an
    /// empty frame until something unrelated rebuilt it.
    #[kithara::test]
    fn a_wave_draws_the_shape_that_arrives_after_it_mounted() {
        let skin = builtin::skin();
        let bounds = Rect {
            h: 120.0,
            w: 640.0,
            x: 0.0,
            y: 0.0,
        };
        let mut wave = Painted::new(
            Wave::new(WaveStyle::Default, skin),
            Drawn::read(WaveStyle::Default, 1.0, None, None, &NoReads, ""),
            skin,
        );
        let empty = wave.draw_list(bounds);
        let buckets = [
            WaveBucket {
                low: 0.25,
                mid: 0.5,
                high: 0.75,
            },
            WaveBucket {
                low: 0.75,
                mid: 0.5,
                high: 0.25,
            },
        ];

        assert!(
            wave.set_read(&ReadValue::Waveform(WaveformView {
                buckets: &buckets,
                beats: &[],
                downbeats: &[],
                bpm: None,
                r#loop: None,
                cues: &[],
            })),
            "a wave handed a shape must report that its picture changed"
        );
        assert_ne!(
            empty,
            wave.draw_list(bounds),
            "the wave drew the same empty frame after its shape arrived"
        );
    }
}

/// What a drag on a retained control counts its travel from.
#[cfg(test)]
mod dragged {
    use std::rc::Rc;

    use kithara_test_utils::kithara;

    use super::{HostAction, Knob, MasonryControl, Painted};
    use crate::{
        atoms::painter::Captioned,
        builtin,
        draw::{Pt, Rect},
        interact::{Hit, Input, PointerPhase, mouse},
        mount,
        render::{ControlAction, ReadValue, UiEvent, controls::Draws},
    };

    /// How far up the hand walks the knob, as a fraction of its travel.
    const STEP: f32 = 0.05;

    fn area() -> Rect {
        Rect {
            h: 40.0,
            w: 40.0,
            x: 0.0,
            y: 0.0,
        }
    }

    /// A relative track counts travel from the value its recognizer was built
    /// with. A retained control is built once and told its new values, so a
    /// recognizer left holding the mounted value walks the knob back to it the
    /// next time a hand touches it.
    #[kithara::test]
    fn a_knob_drags_on_from_the_value_its_endpoint_last_reported() {
        let mut knob = mounted(0.5);

        assert!(knob.set_read(&ReadValue::Scalar(0.9)));
        knob.input(down(20.0), &at(20.0));

        assert!(
            (dragged(&mut knob, 20.0) - 0.95).abs() < 0.001,
            "a knob told its endpoint reads 0.9 must drag on from there"
        );
    }

    /// And the same the second time a hand touches it: the knob draws what its
    /// own gesture authored, so that is where the next one starts — the
    /// application's answer is a frame behind and may never come at all.
    ///
    /// The second press lands away from the first so the two are not read as a
    /// double click, which would reset the knob instead of dragging it.
    #[kithara::test]
    fn a_knob_drags_on_from_the_value_its_own_last_gesture_set() {
        let mut knob = mounted(0.5);

        knob.input(down(20.0), &at(20.0));
        let first = dragged(&mut knob, 20.0);
        knob.input(up(20.0), &at(20.0));
        knob.input(down(32.0), &at(32.0));

        assert!(
            (dragged(&mut knob, 32.0) - (first + f64::from(STEP))).abs() < 0.001,
            "the second drag must start where the first one left the knob"
        );
    }

    fn mounted(value: f32) -> Painted<Knob> {
        let skin = builtin::skin();
        let control = mount::Knob::builder().build();
        let data = Captioned { label: None, value };
        let grip = control.grip(skin, &data);
        Painted::new(control.painter(skin), data, skin).interactive(
            grip,
            "mixer/gain".to_owned(),
            Rc::new(HostAction::new),
        )
    }

    /// What the knob publishes when the hand walks it one step up from `from`.
    fn dragged(knob: &mut Painted<Knob>, from: f32) -> f64 {
        let to = from - builtin::skin().knob.drag_range * STEP;
        knob.input(moved(to), &at(to))
            .value()
            .and_then(|action| action.downcast::<UiEvent>().ok())
            .and_then(|event| match event {
                UiEvent::Control {
                    action: ControlAction::SetScalar(value),
                    ..
                } => Some(value),
                _ => None,
            })
            .unwrap_or_else(|| panic!("a move after a press must publish a scalar"))
    }

    fn at(y: f32) -> Hit {
        Hit::new(Some(Pt { x: 20.0, y }), area())
    }

    fn down(y: f32) -> Input<'static> {
        Input::Pointer(mouse(PointerPhase::Down, Some(Pt { x: 20.0, y })))
    }

    fn moved(y: f32) -> Input<'static> {
        Input::Pointer(mouse(PointerPhase::Move, Some(Pt { x: 20.0, y })))
    }

    fn up(y: f32) -> Input<'static> {
        Input::Pointer(mouse(PointerPhase::Up, Some(Pt { x: 20.0, y })))
    }
}

/// The half of the painter contract only a host that keeps its widgets needs.
///
/// A host that rebuilds its tree on every message learns a new value by being
/// rebuilt. A retained host cannot: rebuilding would throw away the gesture the
/// hand is in the middle of, the pointer capture feeding it, and the run of
/// clicks a double click is made of. So it tells the mounted control instead,
/// through here.
pub(crate) trait Retained: ControlPainter {
    /// Puts what the endpoint now reads into the data this painter draws, and
    /// says whether that changed the picture.
    fn set_read(_data: &mut Self::Data, _value: &ReadValue<'_>) -> bool {
        false
    }
}

/// Takes a flag off an endpoint.
fn set_bool(data: &mut bool, value: &ReadValue<'_>) -> bool {
    let ReadValue::Bool(active) = value else {
        return false;
    };
    std::mem::replace(data, *active) != *active
}

/// Takes a fraction off an endpoint, clamped to the unit range every painter
/// draws in.
fn set_scalar(data: &mut f32, value: &ReadValue<'_>) -> bool {
    let ReadValue::Scalar(value) = value else {
        return false;
    };
    let value = AsPrimitive::<f32>::as_(value.clamp(0.0, 1.0));
    std::mem::replace(data, value) != value
}

/// A meter shows the levels it reads; the scalar it publishes while dragged is
/// its volume, which is the one part of those levels a hand can set.
fn set_levels(data: &mut StereoLevels, value: &ReadValue<'_>) -> bool {
    let levels = match value {
        ReadValue::Stereo(levels) => *levels,
        ReadValue::Scalar(volume) => StereoLevels {
            volume: AsPrimitive::<f32>::as_(volume.clamp(0.0, 1.0)),
            ..*data
        },
        _ => return false,
    };
    std::mem::replace(data, levels) != levels
}

/// A word and a state: the flag decides the picture, and a text endpoint may
/// supply the word.
fn set_labelled(data: &mut Labelled, value: &ReadValue<'_>) -> bool {
    match value {
        ReadValue::Bool(active) => set_bool(&mut data.active, value) || *active != data.active,
        ReadValue::Text(label) => {
            *label != data.label && {
                data.label = (*label).to_owned();
                true
            }
        }
        _ => false,
    }
}

impl Retained for Chip {
    fn set_read(data: &mut Self::Data, value: &ReadValue<'_>) -> bool {
        set_labelled(data, value)
    }
}

impl Retained for Glyph {
    fn set_read(data: &mut Self::Data, value: &ReadValue<'_>) -> bool {
        set_bool(&mut data.active, value)
    }
}

impl Retained for NavItem {
    fn set_read(data: &mut Self::Data, value: &ReadValue<'_>) -> bool {
        set_bool(&mut data.active, value)
    }
}

impl Retained for TabLarge {
    fn set_read(data: &mut Self::Data, value: &ReadValue<'_>) -> bool {
        set_labelled(data, value)
    }
}

impl Retained for Knob {
    fn set_read(data: &mut Self::Data, value: &ReadValue<'_>) -> bool {
        set_scalar(&mut data.value, value)
    }
}

impl Retained for Fader {
    fn set_read(data: &mut Self::Data, value: &ReadValue<'_>) -> bool {
        set_scalar(&mut data.value, value)
    }
}

impl Retained for Crossfader {
    fn set_read(data: &mut Self::Data, value: &ReadValue<'_>) -> bool {
        set_scalar(data, value)
    }
}

impl Retained for Meter {
    fn set_read(data: &mut Self::Data, value: &ReadValue<'_>) -> bool {
        set_scalar(data, value)
    }
}

/// The bar's furniture shows what the skin said; no endpoint moves any of it.
impl Retained for Brand {}

impl Retained for Divider {}

impl Retained for Spacer {}

/// The gear the settings button shows comes from the built-in art, not from an
/// endpoint.
impl Retained for Settings {}

/// A wave follows the shape its own endpoint reports; the playhead beside it
/// is a sibling's reading and lands on rebuild.
impl Retained for Wave {
    fn set_read(data: &mut Self::Data, value: &ReadValue<'_>) -> bool {
        let ReadValue::Waveform(view) = value else {
            return false;
        };
        let next = WaveformData {
            buckets: view.buckets.to_vec().into_boxed_slice(),
            beats: view.beats.to_vec().into_boxed_slice(),
            downbeats: view.downbeats.to_vec().into_boxed_slice(),
            loop_region: view.r#loop,
            cues: view.cues.to_vec().into_boxed_slice(),
        };
        data.waveform.replace(next).as_ref() != data.waveform.as_ref()
    }
}

/// The strip follows the path its own endpoint reports; the scope beside it is
/// the document's list and lands on rebuild.
impl Retained for Context {
    fn set_read(data: &mut Self::Data, value: &ReadValue<'_>) -> bool {
        let ReadValue::Text(breadcrumb) = value else {
            return false;
        };
        std::mem::replace(&mut data.breadcrumb, (*breadcrumb).to_owned()) != data.breadcrumb
    }
}

/// A status dot and a cell show what the document said; no endpoint moves
/// either of them.
impl Retained for StatusDot {}

impl Retained for Cell {}

/// A readout shows what its endpoint last reported; the value is a word by the
/// time it reaches the painter, so a new one is a new word.
impl Retained for Readout {
    fn set_read(data: &mut Self::Data, value: &ReadValue<'_>) -> bool {
        let next = match value {
            ReadValue::Text(value) => (*value).to_owned(),
            ReadValue::Scalar(value) => format!("{value:.2}"),
            _ => return false,
        };
        std::mem::replace(&mut data.value, next) != data.value
    }
}

/// A reading is the number its endpoint reports; the painter is what turns it
/// into digits, so a new number is all this has to take.
impl Retained for Telemetry {
    fn set_read(data: &mut Self::Data, value: &ReadValue<'_>) -> bool {
        let ReadValue::Scalar(value) = value else {
            return false;
        };
        std::mem::replace(data, *value) != *value
    }
}

/// A clock moves with the track it reads, and the position is what its own
/// endpoint reports; the duration comes from a sibling and lands on rebuild.
impl Retained for Clock {
    fn set_read(data: &mut Self::Data, value: &ReadValue<'_>) -> bool {
        let ReadValue::Scalar(position) = value else {
            return false;
        };
        std::mem::replace(&mut data.position, *position) != *position
    }
}

/// The picked cell moves with the reading, rounded to a whole cell.
impl Retained for Segmented {
    fn set_read(data: &mut Self::Data, value: &ReadValue<'_>) -> bool {
        let ReadValue::Scalar(value) = value else {
            return false;
        };
        let picked = num_traits::ToPrimitive::to_usize(&value.round())
            .filter(|index| *index < data.items.len());
        std::mem::replace(&mut data.active, picked) != picked
    }
}

/// A summary shows the track its own endpoint names; where it came from is a
/// sibling's reading and lands on rebuild.
impl Retained for Summary {
    fn set_read(data: &mut Self::Data, value: &ReadValue<'_>) -> bool {
        let ReadValue::Text(title) = value else {
            return false;
        };
        !title.is_empty() && std::mem::replace(&mut data.title, (*title).to_owned()) != data.title
    }
}

/// A tempo readout is rebuilt when the track it reads changes, which is the
/// only thing that turns one reading into the other.
impl Retained for Tempo {}

impl Retained for Select {}

impl Retained for Swatch {}

impl Retained for Binary {
    fn set_read(data: &mut Self::Data, value: &ReadValue<'_>) -> bool {
        set_bool(data, value)
    }
}

impl Retained for VerticalVu {
    fn set_read(data: &mut Self::Data, value: &ReadValue<'_>) -> bool {
        set_levels(data, value)
    }
}

impl Retained for StereoMeter {
    fn set_read(data: &mut Self::Data, value: &ReadValue<'_>) -> bool {
        set_levels(data, value)
    }
}

impl Retained for Button {
    fn set_read(data: &mut Self::Data, value: &ReadValue<'_>) -> bool {
        set_bool(&mut data.active, value)
    }
}

/// One built-in control mounted as a Masonry leaf: a painter, the data it
/// draws, and the gesture it may answer.
pub(crate) struct Painted<Painter>
where
    Painter: Retained,
{
    data: Painter::Data,
    interaction: Option<Interaction>,
    painter: Painter,
    press: Press,
    repaint: bool,
    text: TextContext,
}

/// What a control does with the pointer, and where it publishes the answer.
struct Interaction {
    map_event: Rc<dyn Fn(UiEvent) -> HostAction>,
    path: String,
    recognize: Recognize,
}

enum Recognize {
    Press,
    Command(fn() -> UiEvent),
    Drag(Box<Dragged>),
}

/// A scalar drag in flight: what it was described as, the recognizer made from
/// that description, and the gesture's own state.
///
/// The recognizer is re-made whenever the value moves, because a relative track
/// counts travel from the value it was built with. The state is kept across
/// that, so a hand already dragging is not interrupted by its own answer coming
/// back from the application.
struct Dragged {
    recognizer: Scalar,
    spec: Drag,
    state: ScalarState,
}

impl Dragged {
    fn new(spec: Drag) -> Self {
        Self {
            recognizer: spec.recognizer(),
            spec,
            state: ScalarState::default(),
        }
    }

    fn at(&mut self, value: f32) {
        self.spec = self.spec.at(value);
        self.recognizer = self.spec.recognizer();
    }

    /// One input through the recognizer, carrying the pointer ownership the
    /// host needs to route the rest of the gesture to this leaf.
    fn follow(&mut self, input: Input<'_>, hit: &Hit) -> Outcome {
        let had_pointer = self.state.captures_pointer();
        let outcome = self
            .recognizer
            .on_input(&mut self.state, input, hit, Instant::now());
        if matches!(
            input,
            Input::Pointer(pointer)
                if matches!(pointer.phase, PointerPhase::Cancel | PointerPhase::DoubleClick)
        ) {
            self.state.cancel_pointer();
        }
        let ownership = match (had_pointer, self.state.captures_pointer()) {
            (false, true) => PointerOwnership::Claim,
            (true, false) => PointerOwnership::Release,
            _ => PointerOwnership::Unchanged,
        };
        outcome.with_ownership(ownership)
    }
}

impl<Painter> Painted<Painter>
where
    Painter: Retained,
{
    pub(crate) fn new(painter: Painter, data: Painter::Data, skin: &Skin) -> Self {
        Self {
            data,
            interaction: None,
            painter,
            press: Press::default(),
            repaint: false,
            text: TextContext::from(skin.text_resources()),
        }
    }

    pub(crate) fn interactive(
        mut self,
        grip: Grip,
        path: String,
        map_event: Rc<dyn Fn(UiEvent) -> HostAction>,
    ) -> Self {
        let recognize = match grip {
            Grip::None => return self,
            Grip::Press => Recognize::Press,
            Grip::Command(event) => Recognize::Command(event),
            Grip::Drag(drag) => Recognize::Drag(Box::new(Dragged::new(drag))),
            // Picking a cell is a gesture only the immediate host recognises;
            // here the engine plan drives it, which is what this host has
            // always done. The two are reconciled by the gesture census.
            Grip::Index { .. } => return self,
        };
        self.interaction = Some(Interaction {
            map_event,
            path,
            recognize,
        });
        self
    }

    /// The gesture is measured against the part of the box the painter says the
    /// pointer works, which for most controls is all of it.
    fn gripped(&self, hit: &Hit) -> Hit {
        Hit::new(hit.at(), self.painter.grip_bounds(&self.data, hit.area()))
    }

    /// Takes the value the control now draws into whatever counts from it.
    fn moved_to(&mut self, value: &ReadValue<'_>) {
        let (Some(interaction), ReadValue::Scalar(value)) = (&mut self.interaction, value) else {
            return;
        };
        if let Recognize::Drag(drag) = &mut interaction.recognize {
            drag.at(AsPrimitive::<f32>::as_(value.clamp(0.0, 1.0)));
        }
    }
}

impl<Painter> MasonryControl for Painted<Painter>
where
    Painter: Retained,
{
    fn draw_list(&mut self, bounds: Rect) -> DrawList {
        self.repaint = false;
        let mut list = DrawListBuilder::default();
        self.painter.draw(
            &mut list,
            &mut self.text,
            &self.data,
            bounds,
            self.press.visual(),
        );
        list.finish()
    }

    fn measure(&mut self) -> crate::solve::Size {
        self.painter.measure(&mut self.text, &self.data)
    }

    fn input(&mut self, input: Input<'_>, hit: &Hit) -> Outcome<HostAction> {
        if Painter::READS_POINTER {
            self.repaint |= self.press.press(input, hit);
        }
        let gripped = self.gripped(hit);
        let Some(interaction) = &mut self.interaction else {
            return Outcome::IGNORED;
        };
        let (outcome, spec) = match &mut interaction.recognize {
            Recognize::Press => {
                return click::on_input(input, hit).map(|()| {
                    (interaction.map_event)(control_event(
                        &interaction.path,
                        ControlAction::Activate,
                    ))
                });
            }
            Recognize::Command(event) => {
                let event = *event;
                return click::on_input(input, hit).map(|()| (interaction.map_event)(event()));
            }
            Recognize::Drag(drag) => (drag.follow(input, &gripped), drag.spec),
        };
        // The control draws the value it just authored: the application is told
        // the same number, but its answer only comes back a frame later.
        if let Some(value) = outcome.value() {
            self.repaint |= Painter::set_read(&mut self.data, &ReadValue::Scalar(f64::from(value)));
            self.moved_to(&ReadValue::Scalar(f64::from(value)));
        }
        let Some(interaction) = &self.interaction else {
            return Outcome::IGNORED;
        };
        outcome.map(|value| {
            (interaction.map_event)(control_event(
                &interaction.path,
                ControlAction::SetScalar(spec.published(input, value)),
            ))
        })
    }

    fn accepts_input(&self) -> bool {
        self.interaction.is_some()
    }

    fn set_read(&mut self, value: &ReadValue<'_>) -> bool {
        self.repaint |= Painter::set_read(&mut self.data, value);
        self.moved_to(value);
        self.repaint
    }

    fn cursor(&self, hit: &Hit) -> CursorShape {
        self.interaction
            .as_ref()
            .map_or(CursorShape::None, |interaction| {
                match &interaction.recognize {
                    Recognize::Press | Recognize::Command(_) => {
                        Hover::new(CursorShape::Pointer).cursor(self.press.is_pressed(), hit)
                    }
                    Recognize::Drag(drag) => {
                        drag.recognizer.cursor(&drag.state, &self.gripped(hit))
                    }
                }
            })
    }

    fn hover(&mut self, hovered: bool) -> bool {
        if !Painter::READS_POINTER {
            return false;
        }
        self.repaint |= self.press.hover(hovered);
        self.repaint
    }

    fn repaint(&self) -> Repaint {
        if self.repaint {
            Repaint::NextFrame
        } else {
            Repaint::None
        }
    }
}
