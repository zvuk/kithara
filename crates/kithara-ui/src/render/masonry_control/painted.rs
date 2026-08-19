use std::rc::Rc;

use kithara_platform::time::Instant;
use num_traits::cast::AsPrimitive;

use super::{
    controls::{MasonryControl, Retained},
    custom::{HostAction, Repaint},
};
#[cfg(test)]
use crate::interact::Gestures;
use crate::{
    draw::{DrawList, DrawListBuilder, DrawPools, Rect, Transform},
    interact::{
        CursorShape, Hit, Hover, Input, Outcome, PointerOwnership, PointerPhase,
        recognizers::{Edge, Scalar, ScalarState, Span as SpanRecognizer, SpanState, click},
    },
    render::{
        ControlAction, ReadValue, ScalarRange, Skin, UiEvent, control_event,
        controls::{DataRefresh, Drag, Grip, IndexEvent, IndexPress, Indexing, Press, Span},
        document::Ctx,
        span_event,
    },
    shaping::TextContext,
};

/// One built-in control mounted as a Masonry leaf: a painter, the data it
/// draws, and the gesture it may answer.
pub(crate) struct Painted<Painter>
where
    Painter: Retained,
{
    data: Painter::Data,
    interaction: Option<Interaction<Painter::Data>>,
    index: IndexPress,
    painter: Painter,
    pools: Option<DrawPools>,
    press: Press,
    refresh: Option<DataRefresh<Painter::Data>>,
    repaint: bool,
    text: TextContext,
}

/// What a control does with the pointer, and where it publishes the answer.
struct Interaction<Data> {
    map_event: Rc<dyn Fn(UiEvent) -> HostAction>,
    path: String,
    recognize: Recognize<Data>,
}

enum Recognize<Data> {
    Press,
    Command(fn() -> UiEvent),
    Drag(Box<Dragged>),
    Index {
        count: usize,
        map: Option<IndexEvent<Data>>,
    },
    Span(Box<Spanned>),
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

/// An interval drag in flight. Kept for the same reason a scalar drag is: the
/// press picks a handle by measuring against the interval the recognizer was
/// built with, so a new interval re-makes it while the gesture's own state
/// survives the hand's answer arriving back from the application.
struct Spanned {
    recognizer: SpanRecognizer,
    spec: Span,
    state: SpanState,
}

impl Spanned {
    fn new(spec: Span) -> Self {
        Self {
            recognizer: spec.recognizer(),
            spec,
            state: SpanState::default(),
        }
    }

    fn at(&mut self, value: ScalarRange) {
        self.spec = self.spec.at(value);
        self.recognizer = self.spec.recognizer();
    }

    fn follow(&mut self, input: Input<'_>, hit: &Hit) -> Outcome<(Edge, f32)> {
        let had_pointer = self.state.captures_pointer();
        let outcome = self.recognizer.on_input(&mut self.state, input, hit);
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
    #[cfg(test)]
    pub(crate) fn new(painter: Painter, data: Painter::Data, skin: &Skin) -> Self {
        Self {
            data,
            interaction: None,
            index: IndexPress::default(),
            painter,
            pools: None,
            press: Press::default(),
            refresh: None,
            repaint: false,
            text: TextContext::from(skin.text_resources()),
        }
    }

    pub(crate) fn pooled(
        painter: Painter,
        data: Painter::Data,
        skin: &Skin,
        pools: &DrawPools,
    ) -> Self {
        Self {
            data,
            interaction: None,
            index: IndexPress::default(),
            painter,
            pools: Some(pools.clone()),
            press: Press::default(),
            refresh: None,
            repaint: false,
            text: TextContext::from(skin.text_resources()),
        }
    }

    pub(crate) fn interactive(
        mut self,
        grip: Grip,
        path: String,
        map_event: Rc<dyn Fn(UiEvent) -> HostAction>,
        index_event: Option<IndexEvent<Painter::Data>>,
    ) -> Self {
        let recognize = match grip {
            Grip::None => return self,
            Grip::Press => Recognize::Press,
            Grip::Command(event) => Recognize::Command(event),
            Grip::Drag(drag) => Recognize::Drag(Box::new(Dragged::new(drag))),
            Grip::Index { count } => Recognize::Index {
                count,
                map: index_event,
            },
            Grip::Span(span) => Recognize::Span(Box::new(Spanned::new(span))),
        };
        self.interaction = Some(Interaction {
            map_event,
            path,
            recognize,
        });
        self
    }

    pub(crate) fn refreshing(mut self, refresh: DataRefresh<Painter::Data>) -> Self {
        self.refresh = Some(refresh);
        self
    }

    #[cfg(test)]
    pub(crate) fn gestures(&self) -> Gestures {
        match self
            .interaction
            .as_ref()
            .map(|interaction| &interaction.recognize)
        {
            None => Gestures::NONE,
            Some(Recognize::Press | Recognize::Command(_) | Recognize::Index { .. }) => {
                Gestures::PRESS
            }
            Some(Recognize::Drag(drag)) => drag.spec.gestures(),
            Some(Recognize::Span(_)) => Gestures::DRAG,
        }
    }

    /// The gesture is measured against the part of the box the painter says the
    /// pointer works, which for most controls is all of it.
    fn gripped(&self, hit: &Hit) -> Hit {
        Hit::new(hit.at(), self.painter.grip_bounds(&self.data, hit.area()))
    }

    /// Takes the value the control now draws into whatever counts from it.
    fn moved_to(&mut self, value: &ReadValue<'_>) {
        let Some(interaction) = &mut self.interaction else {
            return;
        };
        match (&mut interaction.recognize, value) {
            (Recognize::Drag(drag), ReadValue::Scalar(value)) => {
                drag.at(AsPrimitive::<f32>::as_(value.clamp(0.0, 1.0)));
            }
            (Recognize::Span(span), ReadValue::Range(value)) => span.at(*value),
            _ => {}
        }
    }
}

impl<Painter> MasonryControl for Painted<Painter>
where
    Painter: Retained,
{
    fn draw_list(&mut self, bounds: Rect, transform: Transform) -> DrawList {
        self.repaint = false;
        let mut list = self
            .pools
            .as_ref()
            .map_or_else(DrawListBuilder::default, DrawPools::list);
        let indexed = matches!(
            self.interaction
                .as_ref()
                .map(|interaction| &interaction.recognize),
            Some(Recognize::Index { .. })
        );
        list.transformed(transform, |list| {
            if indexed {
                self.painter.draw_indexed(
                    list,
                    &mut self.text,
                    &self.data,
                    bounds,
                    self.index.visual(),
                );
            } else {
                self.painter.draw(
                    list,
                    &mut self.text,
                    &self.data,
                    bounds,
                    self.press.visual(),
                );
            }
        });
        list.finish()
    }

    fn measure(&mut self) -> crate::solve::Size {
        self.painter.measure(&mut self.text, &self.data)
    }

    fn input(&mut self, input: Input<'_>, hit: &Hit) -> Outcome<HostAction> {
        let indexed =
            self.interaction
                .as_ref()
                .and_then(|interaction| match &interaction.recognize {
                    Recognize::Index { count, .. } => {
                        Some((*count, self.painter.index_at(&self.data, hit, *count)))
                    }
                    _ => None,
                });
        if Painter::READS_POINTER && indexed.is_none() {
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
            Recognize::Index { map, .. } => {
                let index = indexed.and_then(|(_, index)| index);
                let (changed, outcome) = Indexing::new(&self.data, &interaction.path, *map)
                    .on_input(&mut self.index, input, index);
                self.repaint |= Painter::READS_POINTER && changed;
                return outcome.map(|event| (interaction.map_event)(event));
            }
            Recognize::Span(span) => {
                let outcome = span.follow(input, &gripped);
                if let Some((edge, value)) = outcome.value() {
                    // The control draws the end it just authored; the host's
                    // own answer, snapped and gapped, lands a frame later.
                    let next = span.spec.moved(edge, value);
                    span.at(next);
                    self.repaint |= Painter::set_read(&mut self.data, &ReadValue::Range(next));
                }
                return outcome.map(|(edge, value)| {
                    (interaction.map_event)(span_event(&interaction.path, edge, value))
                });
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

    fn refresh(&mut self, ctx: Ctx<'_, '_>) -> bool {
        let Some(refresh) = self.refresh.as_ref() else {
            return false;
        };
        self.repaint |= refresh(&mut self.data, ctx);
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
                    Recognize::Index { count, .. } => self
                        .painter
                        .index_at(&self.data, hit, *count)
                        .map_or(CursorShape::None, |_| CursorShape::Pointer),
                    Recognize::Span(span) => {
                        span.recognizer.cursor(&span.state, &self.gripped(hit))
                    }
                }
            })
    }

    fn hover(&mut self, hovered: bool) -> bool {
        if !Painter::READS_POINTER {
            return false;
        }
        self.repaint |= match self
            .interaction
            .as_ref()
            .map(|interaction| &interaction.recognize)
        {
            Some(Recognize::Index { .. }) => self.index.hover(hovered),
            _ => self.press.hover(hovered),
        };
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

#[cfg(test)]
mod indexed {
    use std::rc::Rc;

    use kithara_test_utils::kithara;

    use super::*;
    use crate::{
        atoms::{
            bar::preset::{Preset, PresetData},
            design::segmented::{Segmented, SegmentedData},
        },
        builtin,
        draw::Pt,
        interact::{PointerOwnership, PointerPhase, Propagation, mouse},
        mount,
        render::{Reads, controls::Draws, document::probe},
    };

    #[derive(Clone, Copy)]
    struct Points {
        first: Pt,
        gap: Pt,
        second: Pt,
        x_padding: Pt,
        y_padding: Pt,
    }

    fn bounds() -> Rect {
        Rect {
            h: 42.0,
            w: 126.0,
            x: 0.0,
            y: 0.0,
        }
    }

    fn preset_data() -> PresetData {
        struct NoReads;

        impl Reads for NoReads {
            fn get(&self, _endpoint: &str) -> Option<ReadValue<'_>> {
                None
            }
        }

        mount::Preset::snapshot(probe(&NoReads))
    }

    fn skin() -> Skin {
        let mut skin = builtin::skin().clone();
        skin.global_bar.selector_padding_y = 4.0;
        skin
    }

    fn points(skin: &Skin) -> Points {
        let metrics = skin.global_bar;
        let selector_x = metrics.selector_padding_x;
        let selector_y = metrics.selector_padding_y;
        let selector_width = bounds().w - selector_x * 2.0;
        let chip_width = (selector_width - metrics.chip_gap) / 2.0;
        let y = selector_y + (bounds().h - selector_y * 2.0) / 2.0;
        Points {
            first: Pt {
                x: selector_x + chip_width / 2.0,
                y,
            },
            gap: Pt {
                x: selector_x + chip_width + metrics.chip_gap / 2.0,
                y,
            },
            second: Pt {
                x: selector_x + chip_width + metrics.chip_gap + chip_width / 2.0,
                y,
            },
            x_padding: Pt {
                x: selector_x / 2.0,
                y,
            },
            y_padding: Pt {
                x: selector_x + chip_width / 2.0,
                y: selector_y / 2.0,
            },
        }
    }

    fn preset(skin: &Skin, map: Option<IndexEvent<PresetData>>) -> Painted<Preset> {
        Painted::new(Preset::new(skin), preset_data(), skin).interactive(
            Grip::Index { count: 2 },
            "bar/presets".to_owned(),
            Rc::new(HostAction::new),
            map,
        )
    }

    fn answer<Painter>(
        control: &mut Painted<Painter>,
        phase: PointerPhase,
        point: Option<Pt>,
    ) -> (Option<UiEvent>, Propagation, PointerOwnership)
    where
        Painter: Retained,
    {
        let outcome = control.input(
            Input::Pointer(mouse(phase, point)),
            &Hit::new(point, bounds()),
        );
        let propagation = outcome.propagation();
        let ownership = outcome.ownership();
        let event = outcome
            .value()
            .and_then(|action| action.downcast::<UiEvent>().ok());
        (event, propagation, ownership)
    }

    #[kithara::test]
    fn an_ordinary_retained_index_keeps_its_path_addressed_select_index() {
        let skin = builtin::skin();
        let mut control = Painted::new(
            Segmented::new(skin),
            SegmentedData {
                active: None,
                items: vec!["A".to_owned(), "B".to_owned()],
            },
            skin,
        )
        .interactive(
            Grip::Index { count: 2 },
            "gallery/segments".to_owned(),
            Rc::new(HostAction::new),
            None,
        );

        assert_eq!(
            answer(
                &mut control,
                PointerPhase::Down,
                Some(Pt { x: 94.0, y: 21.0 }),
            ),
            (
                Some(UiEvent::Control {
                    path: "gallery/segments".to_owned(),
                    action: ControlAction::SelectIndex(1),
                }),
                Propagation::Captured,
                PointerOwnership::Unchanged,
            )
        );
    }

    #[kithara::test]
    fn retained_preset_mapper_arms_on_down_and_publishes_only_on_same_chip_up() {
        let map = mount::Preset.index_event();
        let skin = skin();
        let points = points(&skin);

        for (point, expected) in [
            (points.first, builtin::MICRO_PRESET),
            (points.second, builtin::PLAYER_PRESET),
        ] {
            let mut control = preset(&skin, map);
            assert_eq!(
                answer(&mut control, PointerPhase::Down, Some(point)),
                (None, Propagation::Captured, PointerOwnership::Claim,)
            );
            assert_eq!(
                answer(&mut control, PointerPhase::Up, Some(point)),
                (
                    Some(UiEvent::SelectPreset(expected.to_owned())),
                    Propagation::Captured,
                    PointerOwnership::Release,
                )
            );
        }
    }

    #[kithara::test]
    fn retained_preset_mapper_cancels_on_another_chip_cancel_or_leave() {
        let map = mount::Preset.index_event();
        let skin = skin();
        let points = points(&skin);
        let mut control = preset(&skin, map);

        let _ = answer(&mut control, PointerPhase::Down, Some(points.first));
        let _ = answer(&mut control, PointerPhase::Move, Some(points.second));
        assert_eq!(control.index.visual().pressed_origin, Some(0));
        assert_eq!(control.index.visual().hovered, Some(1));
        assert_eq!(
            answer(&mut control, PointerPhase::Up, Some(points.second)),
            (None, Propagation::Captured, PointerOwnership::Release,)
        );

        for phase in [PointerPhase::Cancel, PointerPhase::Leave] {
            let _ = answer(&mut control, PointerPhase::Down, Some(points.first));
            assert_eq!(
                answer(&mut control, phase, None),
                (None, Propagation::Captured, PointerOwnership::Release,)
            );
            assert_eq!(control.index.visual().pressed_origin, None);
        }
    }

    /// The one-pixel seam the chips are painted apart used to be listed here.
    /// It is not padding: on a two-chip selector it is the exact middle of the
    /// control, which is where a hand aims and where the crate's own
    /// `Scenario::press` presses. This test named that a contract; it was the
    /// defect, written down.
    #[kithara::test]
    fn retained_preset_padding_and_the_outside_are_not_interactive() {
        let skin = skin();
        let mut control = preset(&skin, mount::Preset.index_event());
        let points = points(&skin);
        for point in [points.first, points.second] {
            assert_eq!(
                control.cursor(&Hit::new(Some(point), bounds())),
                CursorShape::Pointer
            );
        }

        for point in [
            points.x_padding,
            points.y_padding,
            Pt {
                x: -1.0,
                y: points.first.y,
            },
        ] {
            let hit = Hit::new(Some(point), bounds());
            assert_eq!(control.cursor(&hit), CursorShape::None);
            assert_eq!(
                answer(&mut control, PointerPhase::Move, Some(point)),
                (None, Propagation::Ignored, PointerOwnership::Unchanged,)
            );
            assert_eq!(control.index.visual().hovered, None);
            assert_eq!(
                answer(&mut control, PointerPhase::Down, Some(point)),
                (None, Propagation::Ignored, PointerOwnership::Unchanged,)
            );
            assert_eq!(control.index.visual().pressed_origin, None);
            assert_eq!(
                answer(&mut control, PointerPhase::Up, Some(point)),
                (None, Propagation::Ignored, PointerOwnership::Unchanged,)
            );

            let _ = answer(&mut control, PointerPhase::Down, Some(points.first));
            let _ = answer(&mut control, PointerPhase::Move, Some(point));
            assert_eq!(control.index.visual().hovered, None);
            assert_eq!(
                answer(&mut control, PointerPhase::Up, Some(point)),
                (None, Propagation::Captured, PointerOwnership::Release,)
            );
        }
    }

    /// The seam is a target on this host too. Which of the two chips a boundary
    /// belongs to is a convention the atom pins; what matters here is that the
    /// middle of the control offers a pointer at all.
    #[kithara::test]
    fn the_retained_seam_between_two_preset_chips_is_a_target() {
        let skin = skin();
        let control = preset(&skin, mount::Preset.index_event());
        let hit = Hit::new(Some(points(&skin).gap), bounds());

        assert_eq!(control.cursor(&hit), CursorShape::Pointer);
    }

    fn no_event(_data: &PresetData, _index: usize) -> Option<UiEvent> {
        None
    }

    fn bounded_segment_event(data: &SegmentedData, index: usize) -> Option<UiEvent> {
        data.items
            .get(index)
            .map(|_| UiEvent::SelectPreset(index.to_string()))
    }

    #[kithara::test]
    fn retained_invalid_and_out_of_range_mapper_results_publish_nothing() {
        let skin = skin();
        let points = points(&skin);
        let mut control = preset(&skin, Some(no_event));
        assert_eq!(
            answer(&mut control, PointerPhase::Down, Some(points.first)),
            (None, Propagation::Captured, PointerOwnership::Claim,)
        );
        assert_eq!(
            answer(&mut control, PointerPhase::Up, Some(points.first)),
            (None, Propagation::Captured, PointerOwnership::Release,)
        );

        let skin = builtin::skin();
        let mut out_of_range = Painted::new(
            Segmented::new(skin),
            SegmentedData {
                active: None,
                items: vec!["A".to_owned(), "B".to_owned()],
            },
            skin,
        )
        .interactive(
            Grip::Index { count: 3 },
            "gallery/segments".to_owned(),
            Rc::new(HostAction::new),
            Some(bounded_segment_event),
        );
        let third = Pt { x: 105.0, y: 21.0 };
        assert_eq!(
            answer(&mut out_of_range, PointerPhase::Down, Some(third)),
            (None, Propagation::Captured, PointerOwnership::Claim,)
        );
        assert_eq!(
            answer(&mut out_of_range, PointerPhase::Up, Some(third)),
            (None, Propagation::Captured, PointerOwnership::Release,)
        );
        assert_eq!(out_of_range.index.visual().pressed_origin, None);
    }
}
