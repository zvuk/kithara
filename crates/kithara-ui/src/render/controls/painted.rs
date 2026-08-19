use std::cell::RefCell;

use iced::{
    Element, Event, Length, Rectangle, Renderer, Size as IcedSize, Theme, Vector,
    advanced::{
        Clipboard, Renderer as _, Shell, Widget as IcedWidget,
        graphics::geometry::Renderer as _,
        layout::{self, Layout},
        renderer,
        widget::{self, Tree},
    },
    mouse::{self, Cursor},
    widget::canvas::{self, Action},
};
use kithara_platform::time::Instant;

#[cfg(test)]
use crate::interact::Gestures;
use crate::{
    atoms::{
        button::VisualState,
        painter::{ControlPainter, IndexedVisual},
    },
    backends::replay_ordered_in,
    draw::{DrawList, DrawListBuilder, DrawPools, Rect, Transform, ink, union},
    interact::{
        CursorShape, Hit, Hover, Input, Outcome, iced as iced_interact,
        recognizers::{Crossing, Scalar, ScalarState, Span as SpanRecognizer, SpanState, click},
    },
    render::{
        Skin, UiEvent, activate, command,
        controls::{Drag, Grip, IndexEvent, IndexPress, Indexing, Press, Span, snapped},
        publish, scalar, span_event,
    },
    shaping::{TextContext, TextResources},
    solve,
};

/// One neutral painter drawn straight into an iced canvas.
///
/// Both hosts replay the same [`DrawList`], so there is one adapter rather than
/// one per control: what a control looks like is settled in its painter, and
/// neither host gets an opinion about it.
pub(crate) struct Paint<'skin, Painter>
where
    Painter: ControlPainter,
{
    data: Painter::Data,
    painter: Painter,
    pools: Option<DrawPools>,
    text_resources: &'skin TextResources,
    transform: Transform,
}

/// What one painted canvas keeps between frames: the shaping context, the
/// geometry it last tessellated, and the list that geometry was drawn from.
#[derive(Default)]
pub(crate) struct PaintState {
    drawn: RefCell<Option<DrawList>>,
    geometry: canvas::Cache,
    text: RefCell<Option<TextContext>>,
}

impl PaintState {
    /// Drops the kept geometry when the list behind it changed, and reports
    /// whether it had to. That answer is the whole of this cache, so it is
    /// returned rather than inferred from the pixels.
    ///
    /// The key is the drawn list itself and not a hash of what went into it.
    /// An input hash has to name everything a painter reads — the value, the
    /// box, and the shape the skin gave it — and anything it forgets freezes a
    /// control on the screen. Two equal lists draw the same picture by
    /// construction, and equality also settles the floats for free: `-0.0`
    /// compares equal to `0.0`, which is the normalisation the lsq wheel spells
    /// out with `OrderedFloat` for its own path caches.
    pub(crate) fn refresh(&self, list: &DrawList) -> bool {
        let mut drawn = self.drawn.borrow_mut();
        if drawn.as_ref() == Some(list) {
            return false;
        }
        self.geometry.clear();
        *drawn = Some(list.clone());
        true
    }

    /// The shaping context this widget keeps between frames, made on first ask.
    pub(crate) fn shaped<T>(
        &self,
        resources: &TextResources,
        with: impl FnOnce(&mut TextContext) -> T,
    ) -> T {
        let mut text = self.text.borrow_mut();
        with(text.get_or_insert_with(|| resources.into()))
    }

    /// Replays one list at `bounds`, tessellating it only when it differs from
    /// the list the kept geometry was built from.
    ///
    /// `region` is the part of the box the list may put ink in, which is the
    /// box itself unless something moved the drawing out of it.
    pub(crate) fn replay(
        &self,
        renderer: &mut Renderer,
        bounds: Rectangle,
        region: Rectangle,
        list: &DrawList,
        resources: &TextResources,
    ) {
        self.refresh(list);
        let inner = Rect {
            h: region.height,
            w: region.width,
            x: region.x,
            y: region.y,
        };
        renderer.with_translation(Vector::new(bounds.x, bounds.y), |renderer| {
            let geometry = self.geometry.draw_with_bounds(renderer, region, |frame| {
                replay_ordered_in(list, frame, resources, inner);
            });
            renderer.draw_geometry(geometry);
        });
    }
}

impl<'skin, Painter> Paint<'skin, Painter>
where
    Painter: ControlPainter + 'skin,
{
    #[cfg(test)]
    pub(crate) fn new(painter: Painter, data: Painter::Data, skin: &'skin Skin) -> Self {
        Self {
            data,
            painter,
            pools: None,
            text_resources: skin.text_resources(),
            transform: Transform::IDENTITY,
        }
    }

    pub(crate) fn pooled(
        painter: Painter,
        data: Painter::Data,
        skin: &'skin Skin,
        pools: &DrawPools,
    ) -> Self {
        Self {
            data,
            painter,
            pools: Some(pools.clone()),
            text_resources: skin.text_resources(),
            transform: Transform::IDENTITY,
        }
    }

    /// Offsets everything this control draws, without moving the box it draws
    /// into or the region that answers the pointer.
    #[must_use]
    pub(crate) const fn posed(mut self, transform: Transform) -> Self {
        self.transform = transform;
        self
    }

    pub(crate) fn view(self) -> Element<'skin, UiEvent> {
        Element::new(self)
    }

    /// The box the painter asks for, in the toolkit's own words.
    ///
    /// Shaping a word to measure it needs a context the mounted widget does not
    /// have yet, so this builds one. It is the same cost the button paid before
    /// it reached this adapter, and only the painters that measure their word
    /// pay it at all.
    fn length(&self) -> (Length, Length) {
        let size = self
            .painter
            .length(&mut self.text_resources.into(), &self.data);
        (iced_length(size.width), iced_length(size.height))
    }

    /// The box the toolkit settles on: what the painter asks for, resolved
    /// against the room it is offered and the size it measures for itself.
    fn node(&self, state: &PaintState, limits: &layout::Limits) -> layout::Node {
        let (width, height) = self.length();
        let mut text = state.text.borrow_mut();
        let text = text.get_or_insert_with(|| self.text_resources.into());
        let measured = self.painter.measure(text, &self.data);
        layout::Node::new(limits.resolve(
            width,
            height,
            IcedSize::new(measured.width, measured.height),
        ))
    }

    /// The part of the box the pointer works, as the painter carves it.
    fn grip_bounds(&self, bounds: Rect) -> Rect {
        self.painter.grip_bounds(&self.data, bounds)
    }

    fn index_at(&self, hit: &Hit, count: usize) -> Option<usize> {
        self.painter.index_at(&self.data, hit, count)
    }

    pub(crate) fn draw_list(
        &self,
        state: &PaintState,
        bounds: Rect,
        visual: VisualState,
    ) -> DrawList {
        let mut text = state.text.borrow_mut();
        let text = text.get_or_insert_with(|| self.text_resources.into());
        let mut builder = self
            .pools
            .as_ref()
            .map_or_else(DrawListBuilder::default, DrawPools::list);
        builder.transformed(self.transform, |builder| {
            self.painter.draw(builder, text, &self.data, bounds, visual);
        });
        builder.finish()
    }

    pub(crate) fn indexed_draw_list(
        &self,
        state: &PaintState,
        bounds: Rect,
        visual: IndexedVisual,
    ) -> DrawList {
        let mut text = state.text.borrow_mut();
        let text = text.get_or_insert_with(|| self.text_resources.into());
        let mut builder = self
            .pools
            .as_ref()
            .map_or_else(DrawListBuilder::default, DrawPools::list);
        builder.transformed(self.transform, |builder| {
            self.painter
                .draw_indexed(builder, text, &self.data, bounds, visual);
        });
        builder.finish()
    }

    /// Replays the painter into the renderer at the box the layout gave it.
    ///
    /// The kept geometry is dropped only when the drawn list changed, so a
    /// control that did not change is not tessellated again.
    #[cfg_attr(feature = "perf", hotpath::measure(label = "iced.control.paint"))]
    fn paint_into(
        &self,
        state: &PaintState,
        renderer: &mut Renderer,
        bounds: Rectangle,
        visual: VisualState,
    ) {
        if bounds.width < 1.0 || bounds.height < 1.0 {
            return;
        }
        let bounds = snapped(bounds);
        let list = self.draw_list(
            state,
            Rect {
                h: bounds.height,
                w: bounds.width,
                x: 0.0,
                y: 0.0,
            },
            visual,
        );
        self.replay_into(state, renderer, bounds, &list);
    }

    #[cfg_attr(feature = "perf", hotpath::measure(label = "iced.control.paint"))]
    fn paint_indexed_into(
        &self,
        state: &PaintState,
        renderer: &mut Renderer,
        bounds: Rectangle,
        visual: IndexedVisual,
    ) {
        if bounds.width < 1.0 || bounds.height < 1.0 {
            return;
        }
        let bounds = snapped(bounds);
        let list = self.indexed_draw_list(
            state,
            Rect {
                h: bounds.height,
                w: bounds.width,
                x: 0.0,
                y: 0.0,
            },
            visual,
        );
        self.replay_into(state, renderer, bounds, &list);
    }

    fn replay_into(
        &self,
        state: &PaintState,
        renderer: &mut Renderer,
        bounds: Rectangle,
        list: &DrawList,
    ) {
        let region = self.region(bounds.size(), list);
        state.replay(renderer, bounds, region, list, self.text_resources);
    }

    /// Where this control may put ink, in the coordinates its box was handed.
    ///
    /// A painter draws inside its box, so a control no object moved is clipped
    /// to that box exactly as it always was. A control an object moved, turned
    /// or scaled is clipped to the box the pose carried that rectangle to,
    /// which is what the retained host already lets it do: cropping it here
    /// would drop a travelling control from one host and keep it in the other.
    fn region(&self, size: IcedSize, list: &DrawList) -> Rectangle {
        let box_local = Rect {
            h: size.height,
            w: size.width,
            x: 0.0,
            y: 0.0,
        };
        if self.transform.is_identity() {
            return Rectangle::with_size(size);
        }
        let region = ink(list).map_or(box_local, |ink| union(box_local, ink));
        Rectangle {
            height: region.h,
            width: region.w,
            x: region.x,
            y: region.y,
        }
    }
}

/// What the pointer alone says about the picture, for a painter mounted without
/// a gesture: a control the engine drives still lights up under the hand, it
/// just does not answer the click itself.
fn hovered(reads_pointer: bool, bounds: Rectangle, cursor: Cursor) -> VisualState {
    if reads_pointer && iced_interact::hit(bounds, cursor).over() {
        VisualState::Hovered
    } else {
        VisualState::Idle
    }
}

/// The solver's length in the toolkit's words. The two vocabularies agree case
/// for case; only the solver's can be spoken by a painter.
fn iced_length(length: solve::Length) -> Length {
    match length {
        solve::Length::Fill => Length::Fill,
        solve::Length::FillPortion(factor) => Length::FillPortion(factor),
        solve::Length::Fixed(value) => Length::Fixed(value),
        solve::Length::Shrink => Length::Shrink,
    }
}

impl<Painter> IcedWidget<UiEvent, Theme, Renderer> for Paint<'_, Painter>
where
    Painter: ControlPainter,
{
    fn size(&self) -> IcedSize<Length> {
        let (width, height) = self.length();
        IcedSize::new(width, height)
    }

    fn tag(&self) -> widget::tree::Tag {
        widget::tree::Tag::of::<PaintState>()
    }

    fn state(&self) -> widget::tree::State {
        widget::tree::State::new(PaintState::default())
    }

    fn layout(
        &mut self,
        tree: &mut Tree,
        _renderer: &Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        self.node(tree.state.downcast_ref::<PaintState>(), limits)
    }

    fn draw(
        &self,
        tree: &Tree,
        renderer: &mut Renderer,
        _theme: &Theme,
        _style: &renderer::Style,
        layout: Layout<'_>,
        cursor: Cursor,
        _viewport: &Rectangle,
    ) {
        let bounds = layout.bounds();
        self.paint_into(
            tree.state.downcast_ref::<PaintState>(),
            renderer,
            bounds,
            hovered(Painter::READS_POINTER, bounds, cursor),
        );
    }
}

impl<'skin, Painter> From<Paint<'skin, Painter>> for Element<'skin, UiEvent>
where
    Painter: ControlPainter + 'skin,
{
    fn from(paint: Paint<'skin, Painter>) -> Self {
        Self::new(paint)
    }
}

/// A painted control that also answers a gesture.
///
/// Pressing and dragging differ only in what the pointer means, so one wrapper
/// carries both rather than two near-identical canvases.
pub(crate) struct Gesture<'skin, Painter>
where
    Painter: ControlPainter,
{
    paint: Paint<'skin, Painter>,
    path: String,
    recognize: Recognize<Painter::Data>,
}

/// What the pointer means to a control: a press it activates on, or a drag
/// along one axis that sets a scalar.
enum Recognize<Data> {
    Press,
    Command(fn() -> UiEvent),
    Drag(Box<Dragging>),
    Index {
        count: usize,
        map: Option<IndexEvent<Data>>,
    },
    /// An interval drag, which publishes under whichever of its two ends the
    /// press took rather than under the control's own path.
    Span(Box<SpanRecognizer>),
}

/// A mounted scalar drag: the recognizer, and the description it was built from
/// — which is what says how the published value is rounded.
struct Dragging {
    recognizer: Scalar,
    spec: Drag,
}

/// What a gesturing canvas keeps between frames: the gesture, and the shaping
/// context the painter draws through.
#[derive(Default)]
pub(crate) struct GestureState {
    crossing: Crossing,
    drag: ScalarState,
    paint: PaintState,
    press: Press,
    index: IndexPress,
    span: SpanState,
}

impl<'skin, Painter> Gesture<'skin, Painter>
where
    Painter: ControlPainter + 'skin,
{
    pub(crate) fn press(path: &str, paint: Paint<'skin, Painter>) -> Self {
        Self {
            paint,
            path: path.to_owned(),
            recognize: Recognize::Press,
        }
    }

    pub(crate) fn command(
        path: &str,
        paint: Paint<'skin, Painter>,
        event: fn() -> UiEvent,
    ) -> Self {
        Self {
            paint,
            path: path.to_owned(),
            recognize: Recognize::Command(event),
        }
    }

    pub(crate) fn index(
        path: &str,
        paint: Paint<'skin, Painter>,
        count: usize,
        map: Option<IndexEvent<Painter::Data>>,
    ) -> Self {
        Self {
            paint,
            path: path.to_owned(),
            recognize: Recognize::Index { count, map },
        }
    }

    pub(crate) fn drag(path: &str, paint: Paint<'skin, Painter>, drag: Drag) -> Self {
        Self {
            paint,
            path: path.to_owned(),
            recognize: Recognize::Drag(Box::new(Dragging {
                recognizer: drag.recognizer(),
                spec: drag,
            })),
        }
    }

    pub(crate) fn span(path: &str, paint: Paint<'skin, Painter>, span: Span) -> Self {
        Self {
            paint,
            path: path.to_owned(),
            recognize: Recognize::Span(Box::new(span.recognizer())),
        }
    }

    /// Mounts the gesture described by a control, returning the untouched
    /// painter when the control does not own input.
    pub(crate) fn with_grip(
        path: &str,
        paint: Paint<'skin, Painter>,
        grip: Grip,
        index_event: Option<IndexEvent<Painter::Data>>,
    ) -> Result<Self, Paint<'skin, Painter>> {
        match grip {
            Grip::None => Err(paint),
            Grip::Press => Ok(Self::press(path, paint)),
            Grip::Command(event) => Ok(Self::command(path, paint, event)),
            Grip::Drag(drag) => Ok(Self::drag(path, paint, drag)),
            Grip::Index { count } => Ok(Self::index(path, paint, count, index_event)),
            Grip::Span(span) => Ok(Self::span(path, paint, span)),
        }
    }

    #[cfg(test)]
    pub(crate) fn gestures(&self) -> Gestures {
        match &self.recognize {
            Recognize::Press | Recognize::Command(_) | Recognize::Index { .. } => Gestures::PRESS,
            Recognize::Drag(drag) => drag.spec.gestures(),
            Recognize::Span(_) => Gestures::DRAG,
        }
    }

    pub(crate) fn view(self) -> Element<'skin, UiEvent> {
        Element::new(self)
    }

    /// What one input means to this control, and whether the picture moved.
    ///
    /// Split out of the toolkit's `update` so the gesture can be exercised
    /// without a shell.
    pub(crate) fn on_input(
        &self,
        state: &mut GestureState,
        event: &Event,
        bounds: Rectangle,
        cursor: Cursor,
    ) -> Option<Action<UiEvent>> {
        let input = iced_interact::input(event)?;
        let hit = iced_interact::hit(bounds, cursor);
        let mut repaint = false;
        if Painter::READS_POINTER && !matches!(&self.recognize, Recognize::Index { .. }) {
            repaint = state.follow(input, &hit);
        }
        let action = match &self.recognize {
            Recognize::Press => activate(&self.path, click::on_input(input, &hit)),
            Recognize::Command(event) => command(*event, click::on_input(input, &hit)),
            Recognize::Drag(drag) => scalar(
                &self.path,
                drag.recognizer
                    .on_input(&mut state.drag, input, &self.gripped(hit), Instant::now())
                    .map(|value| drag.spec.published(input, value)),
            ),
            Recognize::Index { count, map } => {
                let (changed, outcome) = self.indexed_input(state, input, &hit, *count, *map);
                repaint = Painter::READS_POINTER && changed;
                publish(outcome)
            }
            Recognize::Span(span) => publish(
                span.on_input(&mut state.span, input, &self.gripped(hit))
                    .map(|(edge, value)| span_event(&self.path, edge, value)),
            ),
        };
        action.or_else(|| repaint.then(Action::request_redraw))
    }

    fn indexed_input(
        &self,
        state: &mut GestureState,
        input: Input<'_>,
        hit: &Hit,
        count: usize,
        map: Option<IndexEvent<Painter::Data>>,
    ) -> (bool, Outcome<UiEvent>) {
        let index = self.paint.index_at(hit, count);
        Indexing::new(&self.paint.data, &self.path, map).on_input(&mut state.index, input, index)
    }

    fn indexed_cursor(&self, hit: &Hit, count: usize) -> CursorShape {
        self.paint
            .index_at(hit, count)
            .map_or(CursorShape::None, |_| CursorShape::Pointer)
    }

    /// The gesture is measured against the part of the box the painter says the
    /// pointer works, which for most controls is all of it.
    fn gripped(&self, hit: Hit) -> Hit {
        Hit::new(hit.at(), self.paint.grip_bounds(hit.area()))
    }
}

impl<Painter> IcedWidget<UiEvent, Theme, Renderer> for Gesture<'_, Painter>
where
    Painter: ControlPainter,
{
    fn size(&self) -> IcedSize<Length> {
        IcedWidget::<UiEvent, Theme, Renderer>::size(&self.paint)
    }

    fn tag(&self) -> widget::tree::Tag {
        widget::tree::Tag::of::<GestureState>()
    }

    fn state(&self) -> widget::tree::State {
        widget::tree::State::new(GestureState::default())
    }

    fn layout(
        &mut self,
        tree: &mut Tree,
        _renderer: &Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        self.paint
            .node(&tree.state.downcast_ref::<GestureState>().paint, limits)
    }

    fn draw(
        &self,
        tree: &Tree,
        renderer: &mut Renderer,
        _theme: &Theme,
        _style: &renderer::Style,
        layout: Layout<'_>,
        _cursor: Cursor,
        _viewport: &Rectangle,
    ) {
        let state = tree.state.downcast_ref::<GestureState>();
        match &self.recognize {
            Recognize::Index { .. } => self.paint.paint_indexed_into(
                &state.paint,
                renderer,
                layout.bounds(),
                state.index.visual(),
            ),
            _ => self.paint.paint_into(
                &state.paint,
                renderer,
                layout.bounds(),
                state.press.visual(),
            ),
        }
    }

    fn mouse_interaction(
        &self,
        tree: &Tree,
        layout: Layout<'_>,
        cursor: Cursor,
        _viewport: &Rectangle,
        _renderer: &Renderer,
    ) -> mouse::Interaction {
        let state = tree.state.downcast_ref::<GestureState>();
        let hit = iced_interact::hit(layout.bounds(), cursor);
        match &self.recognize {
            Recognize::Press | Recognize::Command(_) => Hover::new(CursorShape::Pointer)
                .cursor(state.press.is_pressed(), &hit)
                .into(),
            Recognize::Drag(drag) => drag
                .recognizer
                .cursor(&state.drag, &self.gripped(hit))
                .into(),
            Recognize::Index { count, .. } => self.indexed_cursor(&hit, *count).into(),
            Recognize::Span(span) => span.cursor(&state.span, &self.gripped(hit)).into(),
        }
    }

    fn update(
        &mut self,
        tree: &mut Tree,
        event: &Event,
        layout: Layout<'_>,
        cursor: Cursor,
        _renderer: &Renderer,
        _clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, UiEvent>,
        _viewport: &Rectangle,
    ) {
        let state = tree.state.downcast_mut::<GestureState>();
        let Some(action) = self.on_input(state, event, layout.bounds(), cursor) else {
            return;
        };
        let (message, redraw, status) = action.into_inner();
        shell.request_redraw_at(redraw);
        if let Some(message) = message {
            shell.publish(message);
        }
        if status == iced::event::Status::Captured {
            shell.capture_event();
        }
    }
}

impl<'skin, Painter> From<Gesture<'skin, Painter>> for Element<'skin, UiEvent>
where
    Painter: ControlPainter + 'skin,
{
    fn from(gesture: Gesture<'skin, Painter>) -> Self {
        Self::new(gesture)
    }
}

impl GestureState {
    /// Follows the pointer over and onto the control, answering whether the
    /// painter now draws something else. Crossing and pressing are recognised
    /// rather than read off the raw event, so a leaf agrees with the engine
    /// about what those words mean.
    fn follow(&mut self, input: Input<'_>, hit: &Hit) -> bool {
        let crossed = self
            .crossing
            .on_input(input, hit)
            .value()
            .is_some_and(|over| self.press.hover(over));
        self.press.press(input, hit) || crossed
    }
}

#[cfg(all(test, feature = "masonry"))]
mod tests {
    use std::rc::Rc;

    use kithara_test_utils::kithara;

    use super::*;
    use crate::{
        atoms::{
            bar::preset::{Preset, PresetData},
            button::{Button, ButtonConfig, ButtonLabel},
            design::{
                cell::Cell,
                crossfader::Crossfader,
                fader::Fader,
                meter::Meter,
                status_dot::{StatusDot, StatusDotData},
                swatch::Swatch,
            },
            icon::glyph::{Glyph, GlyphData},
            knob::Knob,
            meter::StereoMeter,
            nav_item::NavItem,
            painter::{ButtonData, Captioned, CellData, Labelled, NavData},
            tab::TabLarge,
            toggle::Binary,
            vu::VerticalVu,
        },
        builtin,
        draw::Pt,
        interact::{PointerPhase, mouse as pointer_input},
        module::{ButtonStyle, FaderStyle, Tone},
        mount,
        render::{
            Icon, ReadValue, Reads, StereoLevels,
            controls::{Draws, Grip},
            document::probe,
            masonry::{HostAction, MasonryControl, Painted},
        },
        skin::ColorRole,
    };

    const LEVELS: StereoLevels = StereoLevels {
        l: 0.6,
        r: 0.4,
        volume: 0.8,
    };

    struct NoReads;

    impl Reads for NoReads {
        fn get(&self, _endpoint: &str) -> Option<ReadValue<'_>> {
            None
        }
    }

    fn preset_data(active: Option<usize>) -> PresetData {
        let mut data = mount::Preset::snapshot(probe(&NoReads));
        data.active = active;
        data
    }

    #[kithara::test]
    fn iced_and_masonry_record_the_same_preset_states() {
        let skin = builtin::skin();
        let bounds = Rect {
            h: skin.global_bar.height,
            w: skin.global_bar.selector_width,
            x: 0.0,
            y: 0.0,
        };
        let iced_bounds = Rectangle {
            height: bounds.h,
            width: bounds.w,
            x: bounds.x,
            y: bounds.y,
        };
        let hit = |x| Hit::new(Some(Pt { x, y: 20.0 }), bounds);
        let input = |phase, x| Input::Pointer(pointer_input(phase, Some(Pt { x, y: 20.0 })));
        let map = mount::Preset.index_event();
        for active in [None, Some(0), Some(1)] {
            let immediate = Gesture::index(
                "bar/presets",
                Paint::new(Preset::new(skin), preset_data(active), skin),
                2,
                map,
            );
            let iced_state = GestureState::default();
            let mut masonry = Painted::new(Preset::new(skin), preset_data(active), skin)
                .interactive(
                    Grip::Index { count: 2 },
                    "bar/presets".to_owned(),
                    Rc::new(HostAction::new),
                    map,
                );
            assert_eq!(
                immediate.paint.indexed_draw_list(
                    &iced_state.paint,
                    bounds,
                    iced_state.index.visual(),
                ),
                masonry.draw_list(bounds, Transform::IDENTITY)
            );
        }

        let mut masonry = Painted::new(Preset::new(skin), preset_data(Some(0)), skin).interactive(
            Grip::Index { count: 2 },
            "bar/presets".to_owned(),
            Rc::new(HostAction::new),
            map,
        );
        let immediate = Gesture::index(
            "bar/presets",
            Paint::new(Preset::new(skin), preset_data(Some(0)), skin),
            2,
            map,
        );
        let mut iced_state = GestureState::default();

        assert_eq!(
            immediate
                .paint
                .indexed_draw_list(&iced_state.paint, bounds, iced_state.index.visual(),),
            masonry.draw_list(bounds, Transform::IDENTITY)
        );

        let first = bounds.w / 4.0;
        let iced_first = iced::Point::new(first, 20.0);
        let _ = immediate.on_input(
            &mut iced_state,
            &Event::Mouse(mouse::Event::CursorMoved {
                position: iced_first,
            }),
            iced_bounds,
            Cursor::Available(iced_first),
        );
        let _ = masonry.input(input(PointerPhase::Move, first), &hit(first));
        assert_eq!(
            immediate
                .paint
                .indexed_draw_list(&iced_state.paint, bounds, iced_state.index.visual(),),
            masonry.draw_list(bounds, Transform::IDENTITY)
        );

        let second = bounds.w * 3.0 / 4.0;
        let iced_second = iced::Point::new(second, 20.0);
        let _ = immediate.on_input(
            &mut iced_state,
            &Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)),
            iced_bounds,
            Cursor::Available(iced_second),
        );
        let _ = masonry.input(input(PointerPhase::Down, second), &hit(second));
        assert_eq!(
            immediate
                .paint
                .indexed_draw_list(&iced_state.paint, bounds, iced_state.index.visual(),),
            masonry.draw_list(bounds, Transform::IDENTITY)
        );

        let _ = immediate.on_input(
            &mut iced_state,
            &Event::Mouse(mouse::Event::CursorMoved {
                position: iced_first,
            }),
            iced_bounds,
            Cursor::Available(iced_first),
        );
        let _ = masonry.input(input(PointerPhase::Move, first), &hit(first));
        assert_eq!(
            immediate
                .paint
                .indexed_draw_list(&iced_state.paint, bounds, iced_state.index.visual(),),
            masonry.draw_list(bounds, Transform::IDENTITY)
        );

        let _ = immediate.on_input(
            &mut iced_state,
            &Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)),
            iced_bounds,
            Cursor::Available(iced_first),
        );
        let _ = masonry.input(input(PointerPhase::Up, first), &hit(first));
        assert_eq!(
            immediate
                .paint
                .indexed_draw_list(&iced_state.paint, bounds, iced_state.index.visual(),),
            masonry.draw_list(bounds, Transform::IDENTITY)
        );
    }

    #[kithara::test]
    fn iced_and_masonry_record_the_same_vertical_vu() {
        let skin = builtin::skin();
        let bounds = Rect {
            h: 96.0,
            w: 38.0,
            x: 0.0,
            y: 0.0,
        };
        for ticks in [false, true] {
            let iced = Paint::new(VerticalVu::new(ticks, skin), LEVELS, skin).draw_list(
                &PaintState::default(),
                bounds,
                VisualState::Idle,
            );
            let mut masonry = Painted::new(VerticalVu::new(ticks, skin), LEVELS, skin);

            assert_eq!(
                iced,
                MasonryControl::draw_list(&mut masonry, bounds, Transform::IDENTITY)
            );
        }
    }

    /// The switch is the first control both hosts reach through the same
    /// generic adapter, so this is also the check that the adapter itself does
    /// not add or drop anything on the way.
    #[kithara::test]
    fn iced_and_masonry_record_the_same_switch() {
        let skin = builtin::skin();
        for (name, painter, bounds) in [
            (
                "toggle",
                Binary::toggle as fn(&Skin) -> Binary,
                Rect {
                    h: 14.0,
                    w: 26.0,
                    x: 0.0,
                    y: 0.0,
                },
            ),
            (
                "checkbox",
                Binary::checkbox as fn(&Skin) -> Binary,
                Rect {
                    h: 14.0,
                    w: 14.0,
                    x: 0.0,
                    y: 0.0,
                },
            ),
        ] {
            for active in [false, true] {
                let iced = Paint::new(painter(skin), active, skin).draw_list(
                    &PaintState::default(),
                    bounds,
                    VisualState::Idle,
                );
                let mut masonry = Painted::new(painter(skin), active, skin);

                assert_eq!(
                    iced,
                    MasonryControl::draw_list(&mut masonry, bounds, Transform::IDENTITY),
                    "the two hosts must record the same {name}"
                );
            }
        }
    }

    /// A meter with no endpoint behind it is an empty track under both hosts,
    /// not an empty box under one of them.
    #[kithara::test]
    fn iced_and_masonry_record_the_same_meter() {
        let skin = builtin::skin();
        let bounds = Rect {
            h: 10.0,
            w: 100.0,
            x: 0.0,
            y: 0.0,
        };
        for level in [0.0, 0.5, 1.0] {
            let iced = Paint::new(Meter::new(skin), level, skin).draw_list(
                &PaintState::default(),
                bounds,
                VisualState::Idle,
            );
            let mut masonry = Painted::new(Meter::new(skin), level, skin);

            assert_eq!(
                iced,
                MasonryControl::draw_list(&mut masonry, bounds, Transform::IDENTITY)
            );
        }
    }

    #[kithara::test]
    fn iced_and_masonry_record_the_same_cell() {
        let skin = builtin::skin();
        let bounds = Rect {
            h: 36.0,
            w: 40.0,
            x: 0.0,
            y: 0.0,
        };
        for label in [None, Some("A1".to_owned())] {
            for highlighted in [false, true] {
                let data = || CellData {
                    highlighted,
                    label: label.clone(),
                };
                let iced = Paint::new(Cell::new(skin), data(), skin).draw_list(
                    &PaintState::default(),
                    bounds,
                    VisualState::Idle,
                );
                let mut masonry = Painted::new(Cell::new(skin), data(), skin);

                assert_eq!(
                    iced,
                    MasonryControl::draw_list(&mut masonry, bounds, Transform::IDENTITY)
                );
            }
        }
    }

    #[kithara::test]
    fn iced_and_masonry_record_the_same_swatch() {
        let skin = builtin::skin();
        let bounds = Rect {
            h: 78.0,
            w: 120.0,
            x: 0.0,
            y: 0.0,
        };
        for role in [ColorRole::Accent, ColorRole::BgInset] {
            let iced = Paint::new(Swatch::new(role, skin), "ACCENT".to_owned(), skin).draw_list(
                &PaintState::default(),
                bounds,
                VisualState::Idle,
            );
            let mut masonry = Painted::new(Swatch::new(role, skin), "ACCENT".to_owned(), skin);

            assert_eq!(
                iced,
                MasonryControl::draw_list(&mut masonry, bounds, Transform::IDENTITY)
            );
        }
    }

    #[kithara::test]
    fn iced_and_masonry_record_the_same_status_dot() {
        let skin = builtin::skin();
        let bounds = Rect {
            h: 18.0,
            w: 64.0,
            x: 0.0,
            y: 0.0,
        };
        for tone in [Tone::Neutral, Tone::Accent, Tone::Success, Tone::Danger] {
            let data = StatusDotData {
                active: false,
                label: "LIVE".to_owned(),
            };
            let iced = Paint::new(
                StatusDot::with_active_tone(tone, None, None, skin),
                data,
                skin,
            )
            .draw_list(&PaintState::default(), bounds, VisualState::Idle);
            let mut masonry = Painted::new(
                StatusDot::with_active_tone(tone, None, None, skin),
                StatusDotData {
                    active: false,
                    label: "LIVE".to_owned(),
                },
                skin,
            );

            assert_eq!(
                iced,
                MasonryControl::draw_list(&mut masonry, bounds, Transform::IDENTITY)
            );
        }
    }

    /// Both kinds of icon, on both hosts. The authored one is here because it
    /// used to leave the draw list for an iced widget, which left the retained
    /// host with nothing to paint at all.
    #[kithara::test]
    fn iced_and_masonry_record_the_same_nav_item() {
        let skin = builtin::skin();
        let bounds = Rect {
            h: 30.0,
            w: 198.0,
            x: 0.0,
            y: 0.0,
        };
        for icon in [Icon::Play, Icon::Zvuk] {
            let mark = icon
                .mark()
                .unwrap_or_else(|| panic!("{icon:?} must have a mark"));
            for active in [false, true] {
                let data = || NavData {
                    active,
                    label: "BUTTONS".to_owned(),
                    mark,
                };
                let iced = Paint::new(NavItem::new(skin), data(), skin).draw_list(
                    &PaintState::default(),
                    bounds,
                    VisualState::Idle,
                );
                let mut masonry = Painted::new(NavItem::new(skin), data(), skin);

                assert_eq!(
                    iced,
                    MasonryControl::draw_list(&mut masonry, bounds, Transform::IDENTITY)
                );
                assert!(
                    iced.commands().len() >= 4,
                    "{icon:?} must draw its icon alongside the rest"
                );
            }
        }
    }

    /// Every style the document can ask for, in both states, through both
    /// hosts. The button is the one control whose picture also moves with the
    /// pointer, so each style is checked in every visual state as well.
    #[kithara::test]
    fn iced_and_masonry_record_the_same_button() {
        let skin = builtin::skin();
        let bounds = Rect {
            h: 30.0,
            w: 72.0,
            x: 0.0,
            y: 0.0,
        };
        for (style, icon) in [
            (ButtonStyle::Default, None),
            (ButtonStyle::Default, Some(Icon::Play)),
            (ButtonStyle::Transport, None),
            (ButtonStyle::TransportPrimary, None),
            (ButtonStyle::MicroPrimary, Some(Icon::Play)),
            (ButtonStyle::VisNav, None),
        ] {
            for active in [false, true] {
                let painter = || {
                    Button::new(
                        ButtonConfig::builder()
                            .maybe_mark(icon.and_then(Icon::mark))
                            .style(style)
                            .build(),
                        icon.and_then(Icon::mark),
                        skin,
                    )
                };
                let data = || ButtonData {
                    active,
                    label: ButtonLabel {
                        active: Some("PAUSE".to_owned()),
                        label: "PLAY".to_owned(),
                    },
                };
                let iced = Paint::new(painter(), data(), skin).draw_list(
                    &PaintState::default(),
                    bounds,
                    VisualState::Idle,
                );
                let mut masonry = Painted::new(painter(), data(), skin);

                assert_eq!(
                    iced,
                    MasonryControl::draw_list(&mut masonry, bounds, Transform::IDENTITY),
                    "the two hosts must record the same {style:?} button"
                );
            }
        }
    }

    #[kithara::test]
    fn iced_and_masonry_record_the_same_tab() {
        let skin = builtin::skin();
        let bounds = Rect {
            h: skin.tab_large.height,
            w: 94.0,
            x: 0.0,
            y: 0.0,
        };
        for active in [false, true] {
            let data = || Labelled {
                active,
                label: "DECK MICRO".to_owned(),
            };
            let iced = Paint::new(TabLarge::new(skin), data(), skin).draw_list(
                &PaintState::default(),
                bounds,
                VisualState::Idle,
            );
            let mut masonry = Painted::new(TabLarge::new(skin), data(), skin);

            assert_eq!(
                iced,
                MasonryControl::draw_list(&mut masonry, bounds, Transform::IDENTITY)
            );
        }
    }

    /// Both states, and both kinds of art. The authored one is here for the
    /// same reason it is on the rail item: it used to leave the draw list for
    /// an iced widget, which left the retained host with nothing to paint.
    #[kithara::test]
    fn iced_and_masonry_record_the_same_glyph() {
        let skin = builtin::skin();
        let bounds = Rect {
            h: 24.0,
            w: 24.0,
            x: 0.0,
            y: 0.0,
        };
        let painter = || {
            Glyph::new(
                skin.rgba(ColorRole::Text),
                skin.rgba(ColorRole::Accent),
                14.0,
            )
        };
        for icon in [Icon::Play, Icon::Zvuk] {
            let mark = icon
                .mark()
                .unwrap_or_else(|| panic!("{icon:?} must have a mark"));
            for active in [false, true] {
                let data = || GlyphData {
                    active,
                    active_mark: None,
                    mark,
                };
                let iced = Paint::new(painter(), data(), skin).draw_list(
                    &PaintState::default(),
                    bounds,
                    VisualState::Idle,
                );
                let mut masonry = Painted::new(painter(), data(), skin);

                assert_eq!(
                    iced,
                    MasonryControl::draw_list(&mut masonry, bounds, Transform::IDENTITY)
                );
            }
        }
    }

    /// Both looks, and the captioned one as well: the caption is what moves the
    /// rail sideways, so a host that drew one and not the other would put the
    /// travel somewhere else.
    #[kithara::test]
    fn iced_and_masonry_record_the_same_fader() {
        let skin = builtin::skin();
        let bounds = Rect {
            h: 34.0,
            w: 200.0,
            x: 0.0,
            y: 0.0,
        };
        for (style, label) in [
            (FaderStyle::Default, None),
            (FaderStyle::Default, Some("VOL".to_owned())),
            (FaderStyle::Volume, None),
        ] {
            let data = || Captioned {
                label: label.clone(),
                value: 0.5,
            };
            let iced = Paint::new(Fader::new(style, skin), data(), skin).draw_list(
                &PaintState::default(),
                bounds,
                VisualState::Idle,
            );
            let mut masonry = Painted::new(Fader::new(style, skin), data(), skin);

            assert_eq!(
                iced,
                MasonryControl::draw_list(&mut masonry, bounds, Transform::IDENTITY),
                "the two hosts must record the same {style:?} fader with caption {label:?}"
            );
        }
    }

    #[kithara::test]
    fn iced_and_masonry_record_the_same_crossfader() {
        let skin = builtin::skin();
        let bounds = Rect {
            h: 64.0,
            w: 220.0,
            x: 0.0,
            y: 0.0,
        };
        for ticks in [false, true] {
            let iced = Paint::new(Crossfader::new(ticks, skin), 0.8_f32, skin).draw_list(
                &PaintState::default(),
                bounds,
                VisualState::Idle,
            );
            let mut masonry = Painted::new(Crossfader::new(ticks, skin), 0.8_f32, skin);

            assert_eq!(
                iced,
                MasonryControl::draw_list(&mut masonry, bounds, Transform::IDENTITY)
            );
        }
    }

    /// With and without its caption: the caption is what moves the dial up
    /// inside the box, so a host that drew one and not the other would put the
    /// knob somewhere else.
    #[kithara::test]
    fn iced_and_masonry_record_the_same_knob() {
        let skin = builtin::skin();
        let bounds = Rect {
            h: 39.0,
            w: 28.0,
            x: 0.0,
            y: 0.0,
        };
        for label in [None, Some("GAIN".to_owned())] {
            let data = || Captioned {
                label: label.clone(),
                value: 0.25,
            };
            let iced = Paint::new(Knob::new(skin), data(), skin).draw_list(
                &PaintState::default(),
                bounds,
                VisualState::Idle,
            );
            let mut masonry = Painted::new(Knob::new(skin), data(), skin);

            assert_eq!(
                iced,
                MasonryControl::draw_list(&mut masonry, bounds, Transform::IDENTITY)
            );
        }
    }

    #[kithara::test]
    fn iced_and_masonry_record_the_same_stereo_meter() {
        let skin = builtin::skin();
        let bounds = Rect {
            h: 24.0,
            w: 120.0,
            x: 0.0,
            y: 0.0,
        };
        let iced = Paint::new(StereoMeter::new(skin), LEVELS, skin).draw_list(
            &PaintState::default(),
            bounds,
            VisualState::Idle,
        );
        let mut masonry = Painted::new(StereoMeter::new(skin), LEVELS, skin);

        assert_eq!(
            iced,
            MasonryControl::draw_list(&mut masonry, bounds, Transform::IDENTITY)
        );
    }
}

#[cfg(test)]
mod indexed {
    use iced::{Point, event::Status, mouse};
    use kithara_test_utils::kithara;

    use super::*;
    use crate::{
        atoms::{
            bar::preset::{Preset, PresetData},
            design::segmented::{Segmented, SegmentedData},
        },
        builtin,
        interact::{PointerOwnership, PointerPhase},
        mount,
        render::{ControlAction, ReadValue, Reads, controls::Draws, document::probe},
    };

    #[derive(Clone, Copy)]
    struct Points {
        first: Point,
        gap: Point,
        second: Point,
        x_padding: Point,
        y_padding: Point,
    }

    fn bounds() -> Rectangle {
        Rectangle {
            height: 42.0,
            width: 126.0,
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
        let selector_width = bounds().width - selector_x * 2.0;
        let chip_width = (selector_width - metrics.chip_gap) / 2.0;
        let y = selector_y + (bounds().height - selector_y * 2.0) / 2.0;
        Points {
            first: Point::new(selector_x + chip_width / 2.0, y),
            gap: Point::new(selector_x + chip_width + metrics.chip_gap / 2.0, y),
            second: Point::new(
                selector_x + chip_width + metrics.chip_gap + chip_width / 2.0,
                y,
            ),
            x_padding: Point::new(selector_x / 2.0, y),
            y_padding: Point::new(selector_x + chip_width / 2.0, selector_y / 2.0),
        }
    }

    fn preset(skin: &Skin, map: Option<IndexEvent<PresetData>>) -> Gesture<'_, Preset> {
        Gesture::index(
            "bar/presets",
            Paint::new(Preset::new(skin), preset_data(), skin),
            2,
            map,
        )
    }

    fn event<Painter>(
        gesture: &Gesture<'_, Painter>,
        state: &mut GestureState,
        event: &Event,
        point: Option<Point>,
    ) -> (Option<UiEvent>, Status)
    where
        Painter: ControlPainter,
    {
        gesture
            .on_input(
                state,
                event,
                bounds(),
                point.map_or(Cursor::Unavailable, Cursor::Available),
            )
            .map_or((None, Status::Ignored), |action| {
                let (event, _, status) = action.into_inner();
                (event, status)
            })
    }

    #[kithara::test]
    fn an_ordinary_indexed_control_keeps_its_path_addressed_select_index() {
        let skin = builtin::skin();
        let gesture = Gesture::index(
            "gallery/segments",
            Paint::new(
                Segmented::new(skin),
                SegmentedData {
                    active: None,
                    items: vec!["A".to_owned(), "B".to_owned()],
                },
                skin,
            ),
            2,
            None,
        );

        assert_eq!(
            event(
                &gesture,
                &mut GestureState::default(),
                &Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)),
                Some(Point::new(94.0, 21.0)),
            ),
            (
                Some(UiEvent::Control {
                    path: "gallery/segments".to_owned(),
                    action: ControlAction::SelectIndex(1),
                }),
                Status::Captured,
            )
        );
    }

    #[kithara::test]
    fn preset_mapper_arms_on_down_and_publishes_only_on_same_chip_up() {
        let control = mount::Preset;
        let map = control.index_event();
        let skin = skin();
        let points = points(&skin);

        for (point, expected) in [
            (points.first, builtin::MICRO_PRESET),
            (points.second, builtin::PLAYER_PRESET),
        ] {
            let gesture = preset(&skin, map);
            let mut state = GestureState::default();
            assert_eq!(
                event(
                    &gesture,
                    &mut state,
                    &Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)),
                    Some(point),
                ),
                (None, Status::Captured),
            );
            assert_eq!(
                event(
                    &gesture,
                    &mut state,
                    &Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)),
                    Some(point),
                ),
                (
                    Some(UiEvent::SelectPreset(expected.to_owned())),
                    Status::Captured,
                )
            );
        }
    }

    #[kithara::test]
    fn preset_mapper_cancels_on_another_chip_cancel_or_leave() {
        let map = mount::Preset.index_event();
        let skin = skin();
        let points = points(&skin);
        let gesture = preset(&skin, map);
        let mut state = GestureState::default();

        let _ = event(
            &gesture,
            &mut state,
            &Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)),
            Some(points.first),
        );
        let _ = event(
            &gesture,
            &mut state,
            &Event::Mouse(mouse::Event::CursorMoved {
                position: points.second,
            }),
            Some(points.second),
        );
        assert_eq!(state.index.visual().pressed_origin, Some(0));
        assert_eq!(state.index.visual().hovered, Some(1));
        assert_eq!(
            event(
                &gesture,
                &mut state,
                &Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)),
                Some(points.second),
            )
            .0,
            None
        );

        let _ = event(
            &gesture,
            &mut state,
            &Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)),
            Some(points.first),
        );
        let cancel = Input::Pointer(crate::interact::mouse(PointerPhase::Cancel, None));
        let hit = Hit::new(None, bounds().into());
        let (_, cancelled) = gesture.indexed_input(&mut state, cancel, &hit, 2, map);
        assert_eq!(
            cancelled.propagation(),
            crate::interact::Propagation::Captured
        );
        assert_eq!(cancelled.ownership(), PointerOwnership::Release);
        assert_eq!(cancelled.value(), None);
        assert_eq!(state.index.visual().pressed_origin, None);

        let _ = event(
            &gesture,
            &mut state,
            &Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)),
            Some(points.first),
        );
        assert_eq!(
            event(
                &gesture,
                &mut state,
                &Event::Mouse(mouse::Event::CursorLeft),
                None,
            ),
            (None, Status::Captured)
        );
        assert_eq!(state.index.visual().pressed_origin, None);
    }

    /// The one-pixel seam the chips are painted apart used to be listed here.
    /// It is not padding: on a two-chip selector it is the exact middle of the
    /// control, which is where a hand aims and where the crate's own
    /// `Scenario::press` presses. This test named that a contract; it was the
    /// defect, written down.
    #[kithara::test]
    fn preset_padding_and_the_outside_are_not_interactive() {
        let map = mount::Preset.index_event();
        let skin = skin();
        let gesture = preset(&skin, map);
        let points = points(&skin);
        for point in [points.first, points.second] {
            let hit = Hit::new(Some(point.into()), bounds().into());
            assert_eq!(gesture.indexed_cursor(&hit, 2), CursorShape::Pointer);
        }

        for point in [
            points.x_padding,
            points.y_padding,
            Point::new(-1.0, points.first.y),
        ] {
            let hit = Hit::new(Some(point.into()), bounds().into());
            assert_eq!(gesture.indexed_cursor(&hit, 2), CursorShape::None);
            let mut state = GestureState::default();
            assert_eq!(
                event(
                    &gesture,
                    &mut state,
                    &Event::Mouse(mouse::Event::CursorMoved { position: point }),
                    Some(point),
                ),
                (None, Status::Ignored),
            );
            assert_eq!(state.index.visual().hovered, None);
            assert_eq!(
                event(
                    &gesture,
                    &mut state,
                    &Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)),
                    Some(point),
                ),
                (None, Status::Ignored),
            );
            assert_eq!(state.index.visual().pressed_origin, None);
            assert_eq!(
                event(
                    &gesture,
                    &mut state,
                    &Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)),
                    Some(point),
                ),
                (None, Status::Ignored),
            );

            let _ = event(
                &gesture,
                &mut state,
                &Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)),
                Some(points.first),
            );
            let _ = event(
                &gesture,
                &mut state,
                &Event::Mouse(mouse::Event::CursorMoved { position: point }),
                Some(point),
            );
            assert_eq!(state.index.visual().hovered, None);
            assert_eq!(
                event(
                    &gesture,
                    &mut state,
                    &Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)),
                    Some(point),
                ),
                (None, Status::Captured),
            );
        }
    }

    /// The seam is a target. Which of the two chips a boundary belongs to is a
    /// convention the atom pins; what matters here is that the middle of the
    /// control offers a pointer at all.
    #[kithara::test]
    fn the_seam_between_two_preset_chips_is_a_target() {
        let skin = skin();
        let gesture = preset(&skin, mount::Preset.index_event());
        let hit = Hit::new(Some(points(&skin).gap.into()), bounds().into());

        assert_eq!(gesture.indexed_cursor(&hit, 2), CursorShape::Pointer);
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
    fn invalid_and_out_of_range_mapper_results_publish_nothing() {
        let skin = skin();
        let points = points(&skin);
        let gesture = preset(&skin, Some(no_event));
        let mut state = GestureState::default();
        assert_eq!(
            event(
                &gesture,
                &mut state,
                &Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)),
                Some(points.first),
            ),
            (None, Status::Captured)
        );
        assert_eq!(
            event(
                &gesture,
                &mut state,
                &Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)),
                Some(points.first),
            ),
            (None, Status::Captured)
        );
        assert_eq!(state.index.visual().pressed_origin, None);

        let skin = builtin::skin();
        let out_of_range = Gesture::index(
            "gallery/segments",
            Paint::new(
                Segmented::new(skin),
                SegmentedData {
                    active: None,
                    items: vec!["A".to_owned(), "B".to_owned()],
                },
                skin,
            ),
            3,
            Some(bounded_segment_event),
        );
        let mut state = GestureState::default();
        for event_kind in [
            mouse::Event::ButtonPressed(mouse::Button::Left),
            mouse::Event::ButtonReleased(mouse::Button::Left),
        ] {
            assert_eq!(
                event(
                    &out_of_range,
                    &mut state,
                    &Event::Mouse(event_kind),
                    Some(Point::new(105.0, 21.0)),
                ),
                (None, Status::Captured)
            );
        }
        assert_eq!(state.index.visual().pressed_origin, None);
    }
}

/// What a press on the shared adapter means, for every control that grips one.
#[cfg(test)]
mod pressed {
    use iced::{Point, event, mouse, window::RedrawRequest};
    use kithara_test_utils::kithara;

    use super::*;
    use crate::{
        atoms::{
            bar::settings::Settings,
            button::{Button, ButtonConfig, ButtonLabel},
            nav_item::NavItem,
            painter::{ButtonData, NavData},
        },
        builtin,
        module::ButtonStyle,
        render::{ControlAction, Icon},
    };

    /// One pin for every `Grip::Press` control rather than one per control:
    /// what a press publishes is the adapter's answer, not the painter's.
    #[kithara::test]
    fn a_press_inside_the_bounds_publishes_an_activation() {
        let skin = builtin::skin();
        let mark = Icon::Play
            .mark()
            .unwrap_or_else(|| panic!("the play icon must have a mark"));
        let paint = Paint::new(
            NavItem::new(skin),
            NavData {
                active: false,
                label: "BUTTONS".to_owned(),
                mark,
            },
            skin,
        );
        let gesture = Gesture::press("gallery/buttons/item", paint);
        let bounds = Rectangle {
            height: 30.0,
            width: 198.0,
            x: 0.0,
            y: 0.0,
        };
        let cursor = Cursor::Available(Point::new(99.0, 15.0));
        let press = Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left));
        let mut state = GestureState::default();

        let action = gesture
            .on_input(&mut state, &press, bounds, cursor)
            .unwrap_or_else(|| panic!("a press inside the bounds must publish"));

        assert_eq!(
            action.into_inner(),
            (
                Some(UiEvent::Control {
                    path: "gallery/buttons/item".to_owned(),
                    action: ControlAction::Activate,
                }),
                RedrawRequest::Wait,
                event::Status::Captured,
            )
        );
    }

    /// A command publishes the event its control named, not an activation of
    /// the path it happens to sit at. The path is still where the control
    /// lives, and a command that leaked it would set an endpoint nobody wrote.
    #[kithara::test]
    fn a_press_on_a_command_publishes_the_event_the_control_named() {
        let skin = builtin::skin();
        let mark = Icon::Gear
            .mark()
            .unwrap_or_else(|| panic!("the gear icon must have a mark"));
        let gesture = Gesture::command(
            "bar/settings",
            Paint::new(Settings::new(skin), mark, skin),
            || UiEvent::OpenSettings,
        );
        let bounds = Rectangle {
            height: 32.0,
            width: 32.0,
            x: 0.0,
            y: 0.0,
        };
        let cursor = Cursor::Available(Point::new(16.0, 16.0));
        let press = Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left));
        let mut state = GestureState::default();

        let action = gesture
            .on_input(&mut state, &press, bounds, cursor)
            .unwrap_or_else(|| panic!("a press inside the bounds must publish"));

        assert_eq!(
            action.into_inner(),
            (
                Some(UiEvent::OpenSettings),
                RedrawRequest::Wait,
                event::Status::Captured,
            )
        );
    }

    /// Only a painter that says it reads the pointer gets one. The button is
    /// the only control that does today, so it is where the tracking is pinned;
    /// letting go of it must repaint even though it publishes nothing.
    #[kithara::test]
    fn releasing_a_pressed_button_repaints_without_publishing() {
        let skin = builtin::skin();
        let gesture = Gesture::press("transport/play", button_paint(skin, false));
        let bounds = Rectangle {
            height: 28.0,
            width: 48.0,
            x: 0.0,
            y: 0.0,
        };
        let cursor = Cursor::Available(Point::new(24.0, 14.0));
        let mut state = GestureState::default();

        let pressed = Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left));
        let _ = gesture.on_input(&mut state, &pressed, bounds, cursor);
        assert!(state.press.is_pressed(), "a press inside the bounds holds");

        let released = Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left));
        let action = gesture
            .on_input(&mut state, &released, bounds, cursor)
            .unwrap_or_else(|| panic!("letting go of a pressed button must repaint it"));

        assert!(!state.press.is_pressed());
        assert_eq!(action.into_inner().0, None);
    }

    /// The pointer changes the picture, and only for the painters that asked
    /// for it. A control that does not read the pointer draws the same list
    /// under the hand as away from it.
    #[kithara::test]
    fn a_button_draws_a_different_list_while_it_is_pressed() {
        let skin = builtin::skin();
        let paint = button_paint(skin, false);
        let bounds = Rect {
            h: 28.0,
            w: 48.0,
            x: 0.0,
            y: 0.0,
        };
        let state = PaintState::default();

        assert_ne!(
            paint.draw_list(&state, bounds, VisualState::Idle),
            paint.draw_list(&state, bounds, VisualState::Pressed)
        );
    }

    #[kithara::test]
    fn a_painter_that_ignores_the_pointer_says_so() {
        assert!(Button::READS_POINTER);
        assert!(!NavItem::READS_POINTER);
    }

    fn button_paint(skin: &Skin, active: bool) -> Paint<'_, Button> {
        Paint::new(
            Button::new(
                ButtonConfig::builder()
                    .style(ButtonStyle::TransportPrimary)
                    .build(),
                None,
                skin,
            ),
            ButtonData {
                active,
                label: ButtonLabel {
                    active: Some("PAUSE".to_owned()),
                    label: "PLAY".to_owned(),
                },
            },
            skin,
        )
    }
}

/// What a drag on the shared adapter means, for every control that grips one.
#[cfg(test)]
mod dragged {
    use iced::{Point, mouse};
    use kithara_test_utils::kithara;

    use super::*;
    use crate::{
        atoms::{knob::Knob, painter::Captioned},
        builtin,
        interact::recognizers::Track,
        module::FaderStyle,
        mount,
        render::{
            ControlAction,
            controls::{Draws, Grip},
        },
    };

    const RANGE: f32 = 100.0;

    /// One pin for every `Grip::Drag` control rather than one per control: the
    /// track a control names is what its travel is measured against, and the
    /// press that arms a relative drag publishes nothing on its own.
    #[kithara::test]
    fn a_relative_drag_publishes_the_value_it_walked_to() {
        let skin = builtin::skin();
        let gesture = Gesture::drag(
            "mixer/gain",
            Paint::new(
                Knob::new(skin),
                Captioned {
                    label: None,
                    value: 0.5,
                },
                skin,
            ),
            Drag::builder()
                .cursor(CursorShape::ResizeV)
                .track(Track::RelativeVertical {
                    range: RANGE,
                    value: 0.5,
                })
                .build(),
        );
        let bounds = Rectangle {
            height: 40.0,
            width: 40.0,
            x: 0.0,
            y: 0.0,
        };
        let mut state = GestureState::default();

        let press = Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left));
        let armed = gesture.on_input(
            &mut state,
            &press,
            bounds,
            Cursor::Available(Point::new(20.0, 30.0)),
        );

        assert_eq!(
            armed.and_then(|action| action.into_inner().0),
            None,
            "a relative press arms the drag rather than seeking"
        );

        let moved = Event::Mouse(mouse::Event::CursorMoved {
            position: Point::new(20.0, 20.0),
        });
        let action = gesture
            .on_input(
                &mut state,
                &moved,
                bounds,
                Cursor::Available(Point::new(20.0, 20.0)),
            )
            .unwrap_or_else(|| panic!("a drag after a press must publish"));

        assert_eq!(
            action.into_inner().0,
            Some(UiEvent::Control {
                path: "mixer/gain".to_owned(),
                action: ControlAction::SetScalar(f64::from(0.5 + 10.0 / RANGE)),
            })
        );
    }

    /// A fader's pointer works the rail, not the whole box. The caption sits
    /// beside the rail and is not part of the travel, so a gesture measured
    /// against the control would put the zero of the value under the caption.
    #[kithara::test]
    fn a_fader_press_seeks_to_the_fraction_of_the_rail_it_landed_on() {
        let step = builtin::skin().fader.step;

        for fraction in [0.0_f32, 0.25, 0.5, 0.75] {
            let value = fader_press(fraction);

            assert!(
                (value - f64::from(fraction)).abs() <= step,
                "a press at {fraction} of the rail set {value}"
            );
        }
    }

    /// And what it publishes walks in the step the skin names. A rail is
    /// hundreds of pixels wide, so a fader that answered every one of them
    /// would never land on a round number.
    #[kithara::test]
    fn a_fader_publishes_a_multiple_of_the_step_its_skin_names() {
        let step = builtin::skin().fader.step;
        // A third of the way along, which no whole number of steps reaches.
        let steps = fader_press(1.0 / 3.0) / step;

        assert!(
            (steps - steps.round()).abs() < 1e-9,
            "a fader must publish a multiple of {step}, and {steps} steps is not one"
        );
    }

    /// What a captioned fader publishes for a press at `fraction` of its rail.
    fn fader_press(fraction: f32) -> f64 {
        let skin = builtin::skin();
        let control = mount::Fader::builder().style(FaderStyle::Default).build();
        let data = || Captioned {
            label: Some("VOL".to_owned()),
            value: 0.5,
        };
        let Grip::Drag(drag) = control.grip(skin, &data()) else {
            panic!("a fader must grip a drag");
        };
        let bounds = Rectangle {
            height: 34.0,
            width: 200.0,
            x: 0.0,
            y: 0.0,
        };
        let rail = control.painter(skin).rail(bounds.into(), true);
        let gesture = Gesture::drag(
            "mixer/vol",
            Paint::new(control.painter(skin), data(), skin),
            drag,
        );
        let press = Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left));
        let cursor = Cursor::Available(Point::new(
            rail.x + rail.w * fraction,
            rail.y + rail.h / 2.0,
        ));
        let action = gesture
            .on_input(&mut GestureState::default(), &press, bounds, cursor)
            .unwrap_or_else(|| panic!("a press at {fraction} of the rail must publish"));
        let Some(UiEvent::Control {
            action: ControlAction::SetScalar(value),
            ..
        }) = action.into_inner().0
        else {
            panic!("a press at {fraction} of the rail must set a scalar");
        };
        value
    }

    /// The other half of the vocabulary: an absolute track seeks, so the press
    /// goes straight to the fraction of the rail it landed on.
    ///
    /// The drag comes from the control rather than being written out here, so
    /// this is also the pin that a crossfader still declares one — the retained
    /// host leaves it to the engine plan, and would not notice if it stopped.
    #[kithara::test]
    fn an_absolute_drag_seeks_to_the_fraction_the_press_landed_on() {
        let skin = builtin::skin();
        let control = mount::Crossfader::builder().ticks(false).build();
        let value = 0.8_f32;
        let Grip::Drag(drag) = control.grip(skin, &value) else {
            panic!("a crossfader must grip a drag");
        };
        let gesture = Gesture::drag(
            "mixer/xfade",
            Paint::new(control.painter(skin), value, skin),
            drag,
        );
        let bounds = Rectangle {
            height: 40.0,
            width: 200.0,
            x: 0.0,
            y: 0.0,
        };
        let mut state = GestureState::default();

        let press = Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left));
        let action = gesture
            .on_input(
                &mut state,
                &press,
                bounds,
                Cursor::Available(Point::new(50.0, 20.0)),
            )
            .unwrap_or_else(|| panic!("an absolute press must seek"));

        assert_eq!(
            action.into_inner().0,
            Some(UiEvent::Control {
                path: "mixer/xfade".to_owned(),
                action: ControlAction::SetScalar(0.25),
            })
        );
    }
}

/// What a control asks its row for, which is the one length a document cannot
/// state.
#[cfg(test)]
mod lengths {
    use std::borrow::Cow;

    use iced::{
        Pixels,
        advanced::{graphics::text::font_system, layout::Limits, widget::Tree},
    };
    use iced_renderer::fallback::Renderer as FallbackRenderer;
    use iced_tiny_skia::Renderer as TinySkiaRenderer;
    use kithara_test_utils::kithara;

    use super::*;
    use crate::{
        atoms::{
            button::{Button, ButtonConfig, ButtonLabel},
            painter::{ButtonData, Labelled},
            tab::TabLarge,
        },
        builtin,
        module::ButtonStyle,
        render::fonts::{FONT_BYTES, SANS},
    };

    fn headless_renderer() -> Renderer {
        let mut fonts = font_system()
            .write()
            .unwrap_or_else(|error| panic!("iced font system must be available: {error}"));
        for bytes in FONT_BYTES {
            fonts.load_font(Cow::Borrowed(bytes));
        }
        drop(fonts);

        FallbackRenderer::Secondary(TinySkiaRenderer::new(SANS, Pixels(14.0)))
    }

    fn paint(style: ButtonStyle, skin: &Skin) -> Paint<'_, Button> {
        Paint::new(
            Button::new(ButtonConfig::builder().style(style).build(), None, skin),
            ButtonData {
                active: false,
                label: ButtonLabel {
                    active: None,
                    label: "PLAY".to_owned(),
                },
            },
            skin,
        )
    }

    /// The transport row is weighted: its primary button is wider than the rest
    /// by a factor the skin names. `Fill` would divide the row evenly and the
    /// row would be wrong, so the share has to survive the shared adapter.
    #[kithara::test]
    fn a_transport_button_keeps_its_share_of_the_row() {
        let skin = builtin::skin();

        assert_eq!(
            paint(ButtonStyle::Transport, skin).length().0,
            Length::FillPortion(skin.button.transport_fill)
        );
        assert_eq!(
            paint(ButtonStyle::TransportPrimary, skin).length().0,
            Length::FillPortion(skin.button.primary_fill)
        );
    }

    /// A button with no share is as wide as the skin or its own word says.
    #[kithara::test]
    fn a_fixed_button_takes_the_width_its_skin_names() {
        let skin = builtin::skin();

        assert_eq!(
            paint(ButtonStyle::MicroPrimary, skin).length().0,
            Length::Fixed(skin.button.micro_size)
        );
        assert_eq!(
            paint(ButtonStyle::VisNav, skin).length().0,
            Length::Fixed(skin.vis.nav_cell_size)
        );
    }

    #[kithara::test]
    fn a_default_button_is_as_wide_as_its_word() {
        let skin = builtin::skin();

        assert!(matches!(
            paint(ButtonStyle::Default, skin).length().0,
            Length::Fixed(width) if width > 0.0
        ));
    }

    /// A tab is the one control whose width is neither fixed nor a share: it is
    /// as wide as the word it shows. That number has to survive the adapter, or
    /// a strip of tabs stops being a row of headings.
    #[kithara::test]
    fn a_tab_is_as_wide_as_its_word() {
        let skin = builtin::skin();
        let renderer = headless_renderer();
        let mut element = Paint::new(
            TabLarge::new(skin),
            Labelled {
                active: true,
                label: "DECK MICRO".to_owned(),
            },
            skin,
        )
        .view();
        let mut tree = Tree::new(element.as_widget());

        let node = element.as_widget_mut().layout(
            &mut tree,
            &renderer,
            &Limits::new(IcedSize::ZERO, IcedSize::new(320.0, 80.0)),
        );

        assert!(
            (node.size().width - 94.0).abs() < 0.001,
            "iced gave this label and skin 94 px; a {} px tab would move its siblings",
            node.size().width
        );
        assert_eq!(node.size().height, skin.tab_large.height);
    }

    #[cfg(feature = "masonry")]
    mod masonry_host {
        use super::*;
        use crate::atoms::button::declared_width;

        /// The retained host settles a row's shares while it is still walking the
        /// document, which is before it holds a painter. It reads the same table
        /// rather than keeping a second copy, and this is the pin that the two
        /// answers cannot drift apart.
        #[kithara::test]
        fn what_a_parent_is_told_matches_what_the_painter_asks_for() {
            let skin = builtin::skin();
            for style in [
                ButtonStyle::MicroPrimary,
                ButtonStyle::Transport,
                ButtonStyle::TransportPrimary,
                ButtonStyle::VisNav,
            ] {
                assert_eq!(
                    iced_length(declared_width(style, skin)),
                    paint(style, skin).length().0,
                    "the two hosts must weigh a {style:?} button the same"
                );
            }
        }

        /// The one width a parent cannot be told, because the button only knows it
        /// once it holds the word it is going to draw.
        #[kithara::test]
        fn a_measured_width_is_not_something_a_parent_can_be_told() {
            assert_eq!(
                declared_width(ButtonStyle::Default, builtin::skin()),
                solve::Length::Shrink
            );
        }

        /// The retained host is told a tab's length before it holds a painter, so
        /// the two answers have to come from one place.
        #[kithara::test]
        fn both_hosts_ask_for_the_same_tab_box() {
            let skin = builtin::skin();
            let painter = TabLarge::new(skin);
            let data = Labelled {
                active: false,
                label: "DECK MICRO".to_owned(),
            };

            assert_eq!(
                painter.length(&mut skin.text_resources().into(), &data),
                TabLarge::declared_length(skin.tab_large.height)
            );
        }
    }

    /// Answering the length is not enough: it has to reach the mounted canvas,
    /// or the row divides evenly and the share is lost on the way.
    #[kithara::test]
    fn a_painted_canvas_is_sized_from_its_painter() {
        let skin = builtin::skin();
        let element = paint(ButtonStyle::TransportPrimary, skin).view();

        assert_eq!(
            element.as_widget().size(),
            iced::Size::new(Length::FillPortion(skin.button.primary_fill), Length::Fill)
        );
    }

    /// And the same for the canvas that also answers a press: it is a second
    /// call site, so it is a second way to lose the share.
    #[kithara::test]
    fn a_gesturing_canvas_is_sized_from_its_painter() {
        let skin = builtin::skin();
        let element =
            Gesture::press("transport/play", paint(ButtonStyle::TransportPrimary, skin)).view();

        assert_eq!(
            element.as_widget().size(),
            iced::Size::new(Length::FillPortion(skin.button.primary_fill), Length::Fill)
        );
    }
}

/// What the cache is allowed to keep, and what it must drop.
#[cfg(test)]
mod cached {
    use kithara_test_utils::kithara;

    use super::*;
    use crate::{
        atoms::{design::meter::Meter, toggle::Binary},
        builtin,
    };

    const BOX: Rect = Rect {
        h: 40.0,
        w: 120.0,
        x: 0.0,
        y: 0.0,
    };

    /// One frame of the immediate-mode host: the element is built afresh, and
    /// the canvas state is the one thing that survived the last frame.
    fn frame<Painter>(
        state: &PaintState,
        painter: Painter,
        data: Painter::Data,
        bounds: Rect,
    ) -> bool
    where
        Painter: ControlPainter,
    {
        let paint = Paint::new(painter, data, builtin::skin());
        state.refresh(&paint.draw_list(state, bounds, VisualState::Idle))
    }

    /// The host rebuilds the whole element tree every frame. A control whose
    /// value did not move must not be tessellated again — and must be the
    /// moment it does.
    #[kithara::test]
    fn an_unchanged_control_keeps_the_geometry_it_drew() {
        let skin = builtin::skin();
        let state = PaintState::default();

        assert!(
            frame(&state, Meter::new(skin), 0.5, BOX),
            "the first frame draws"
        );
        assert!(
            !frame(&state, Meter::new(skin), 0.5, BOX),
            "an unchanged control must keep what it drew"
        );
        assert!(
            frame(&state, Meter::new(skin), 0.75, BOX),
            "a control whose value moved must draw again"
        );
    }

    /// The same value in a different box is a different picture.
    #[kithara::test]
    fn a_control_that_was_resized_draws_again() {
        let skin = builtin::skin();
        let state = PaintState::default();
        let wider = Rect {
            w: BOX.w + 1.0,
            ..BOX
        };

        assert!(frame(&state, Meter::new(skin), 0.5, BOX));
        assert!(frame(&state, Meter::new(skin), 0.5, wider));
    }

    /// The canvas state belongs to a place in the tree, not to a control, so a
    /// document that puts a different control there must get a different
    /// picture. Keying on what a painter *reads* would miss this: a switch and
    /// a checkbox are one painter type and read the same `false`.
    #[kithara::test]
    fn a_control_replaced_by_another_does_not_keep_its_picture() {
        let skin = builtin::skin();
        let state = PaintState::default();

        assert!(frame(&state, Binary::toggle(skin), false, BOX));
        assert!(
            frame(&state, Binary::checkbox(skin), false, BOX),
            "a checkbox must not be left showing a switch"
        );
    }
}
