use iced::{
    Element, Event, Length, Rectangle, Renderer, Size, Theme, Vector,
    advanced::{
        Clipboard, Shell, Widget as IcedWidget,
        layout::{self, Layout},
        mouse, overlay, renderer,
        widget::{self, Operation, Tree},
    },
    window,
};
use kithara_platform::time::Instant;

use super::geometry::HostedLayout;
use crate::{
    engine::{Descriptor, Engine, Target},
    expand::ExpandedNode,
    interact::{Input, PointerPhase, ScrollAxis, iced as iced_interact},
    module::ChromeStyle,
    render::{
        Skin, UiEvent, controls::sync_tree_scroll, document::Ctx, engine as engine_event,
        hosted_picker_overlay, sync_picker, sync_table_scroll, sync_text_input, toggle_module,
    },
};

#[derive(Clone, Copy)]
pub(super) struct ModuleHost<'a> {
    pub(super) instance: &'a str,
    pub(super) module: &'a str,
    pub(super) chrome: ChromeStyle,
    pub(super) collapsed: bool,
    pub(super) drop: bool,
}

pub(super) fn module_host<'a>(
    child: Element<'a, UiEvent>,
    spec: ModuleHost<'a>,
) -> Element<'a, UiEvent> {
    Element::new(Host {
        child,
        layout: HostedLayout::module(spec),
    })
}

pub(super) fn host<'a>(
    child: Element<'a, UiEvent>,
    root: &ExpandedNode,
    ctx: Ctx<'_, '_>,
    skin: &Skin,
) -> Element<'a, UiEvent> {
    Element::new(Host {
        child,
        layout: HostedLayout::new(root, ctx, skin),
    })
}

struct Host<'a> {
    child: Element<'a, UiEvent>,
    layout: HostedLayout,
}

struct State {
    engine: Engine,
    last_hovered_control: Option<String>,
    last_mouse_interaction: Option<mouse::Interaction>,
}

impl State {
    fn new(layout: &HostedLayout) -> Self {
        let mut engine = Engine::default();
        engine.reconcile(layout.descriptors());
        Self {
            engine,
            last_hovered_control: None,
            last_mouse_interaction: None,
        }
    }
}

impl IcedWidget<UiEvent, Theme, Renderer> for Host<'_> {
    fn tag(&self) -> widget::tree::Tag {
        widget::tree::Tag::of::<State>()
    }

    fn state(&self) -> widget::tree::State {
        widget::tree::State::new(State::new(&self.layout))
    }

    fn children(&self) -> Vec<Tree> {
        vec![Tree::new(&self.child)]
    }

    fn diff(&self, tree: &mut Tree) {
        tree.state
            .downcast_mut::<State>()
            .engine
            .reconcile(self.layout.descriptors());
        tree.diff_children(std::slice::from_ref(&self.child));
    }

    delegate::delegate! {
        to self.child.as_widget() {
            fn size(&self) -> Size<Length>;
            fn size_hint(&self) -> Size<Length>;
        }
    }

    fn layout(
        &mut self,
        tree: &mut Tree,
        renderer: &Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        let node = self
            .child
            .as_widget_mut()
            .layout(&mut tree.children[0], renderer, limits);
        let child_layout = Layout::new(&node);
        let state = tree.state.downcast_mut::<State>();
        let targets = self.layout.targets_with_engine(
            child_layout,
            mouse::Cursor::Unavailable,
            Some(&state.engine),
        );
        state
            .engine
            .reconcile(active_descriptors(&self.layout, &targets));
        for target in &targets {
            state
                .engine
                .set_scroll_viewport(target.path, target.hit.area());
        }
        sync_scrolls(
            &mut self.child,
            &mut tree.children[0],
            child_layout,
            renderer,
            &state.engine,
            &targets,
        );
        sync_pickers(
            &mut self.child,
            &mut tree.children[0],
            child_layout,
            renderer,
            &self.layout,
            &state.engine,
        );
        sync_text_inputs(
            &mut self.child,
            &mut tree.children[0],
            child_layout,
            renderer,
            &state.engine,
        );
        node
    }

    fn update(
        &mut self,
        tree: &mut Tree,
        event: &Event,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        renderer: &Renderer,
        clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, UiEvent>,
        viewport: &Rectangle,
    ) {
        let state = tree.state.downcast_mut::<State>();
        let input = iced_interact::input(event);
        let focus_before = state.engine.focused_path().map(ToOwned::to_owned);
        let item_was_pressed = state.engine.has_pressed_item();
        let picker_before = self.layout.picker_snapshots(&state.engine);
        let text_before = state.engine.text_input_snapshots();
        let (captured, scroll_captured, control_pointer_captured) = if let Some(input) = input {
            let targets = self
                .layout
                .targets_with_engine(layout, cursor, Some(&state.engine));
            if let Some(emission) = state.engine.handle(input, &targets, Instant::now()) {
                let captured = emission.outcome.is_captured();
                let scroll_captured = captured
                    && matches!(input, Input::Wheel(_))
                    && state.engine.scroll_offset(&emission.path).is_some();
                let control_pointer_captured =
                    control_pointer_answered(&state.engine, input, &emission.path, captured);
                let action = if let Some(module) = self.layout.header_module(&emission.path) {
                    toggle_module(module, emission.outcome.map(|_| ()))
                } else {
                    engine_event(&emission.path, emission.child, emission.outcome)
                };
                if let Some(action) = action {
                    let (message, redraw_request, _) = action.into_inner();
                    shell.request_redraw_at(redraw_request);
                    if let Some(message) = message {
                        shell.publish(message);
                    }
                }
                (captured, scroll_captured, control_pointer_captured)
            } else {
                (false, false, false)
            }
        } else {
            (false, false, false)
        };
        if state.engine.focused_path().is_some()
            && focus_before.as_deref() != state.engine.focused_path()
        {
            let mut unfocus = widget::operation::focusable::unfocus();
            self.child.as_widget_mut().operate(
                &mut tree.children[0],
                layout,
                renderer,
                &mut unfocus,
            );
        }
        if !captured {
            self.child.as_widget_mut().update(
                &mut tree.children[0],
                event,
                layout,
                cursor,
                renderer,
                clipboard,
                shell,
                viewport,
            );
        }
        let item_projection_changed = item_was_pressed || state.engine.has_pressed_item();
        let picker_after = self.layout.picker_snapshots(&state.engine);
        let picker_projection_changed = picker_before != picker_after;
        let text_after = state.engine.text_input_snapshots();
        let text_projection_changed = text_before != text_after;
        let picker_overlay_needs_rebuild = captured
            && matches!(
                input,
                Some(Input::KeyPressed { .. } | Input::KeyReleased { .. })
            )
            && picker_after.iter().any(|(_, snapshot)| snapshot.open);
        if scroll_captured
            || item_projection_changed
            || picker_projection_changed
            || picker_overlay_needs_rebuild
            || text_projection_changed
        {
            let targets = self.layout.targets_with_engine(
                layout,
                mouse::Cursor::Unavailable,
                Some(&state.engine),
            );
            sync_scrolls(
                &mut self.child,
                &mut tree.children[0],
                layout,
                renderer,
                &state.engine,
                &targets,
            );
            if picker_projection_changed {
                sync_pickers(
                    &mut self.child,
                    &mut tree.children[0],
                    layout,
                    renderer,
                    &self.layout,
                    &state.engine,
                );
            }
            if text_projection_changed {
                sync_text_inputs(
                    &mut self.child,
                    &mut tree.children[0],
                    layout,
                    renderer,
                    &state.engine,
                );
            }
            if picker_projection_changed || picker_overlay_needs_rebuild {
                shell.invalidate_layout();
            }
            shell.request_redraw();
        }
        if captures_event(
            input,
            captured,
            scroll_captured,
            control_pointer_captured,
            state.engine.captures_pointer(),
        ) {
            shell.capture_event();
        }

        if matches!(event, Event::Window(window::Event::RedrawRequested(_))) {
            let targets = self
                .layout
                .targets_with_engine(layout, cursor, Some(&state.engine));
            let request = iced_interact::input_method(state.engine.input_method(&targets));
            shell.request_input_method(&request);
        }

        if shell.redraw_request() != window::RedrawRequest::NextFrame {
            let targets = self
                .layout
                .targets_with_engine(layout, cursor, Some(&state.engine));
            let interaction = state.engine.cursor(&targets).into();
            let hovered = hovered_control(&targets);
            if matches!(event, Event::Window(window::Event::RedrawRequested(_))) {
                if state.last_hovered_control.as_deref() != hovered {
                    state.last_hovered_control = hovered.map(ToOwned::to_owned);
                }
                state.last_mouse_interaction = Some(interaction);
            } else if state
                .last_mouse_interaction
                .is_some_and(|last| last != interaction)
                || state.last_hovered_control.as_deref() != hovered
            {
                shell.request_redraw();
            }
        }
    }

    fn mouse_interaction(
        &self,
        tree: &Tree,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
        renderer: &Renderer,
    ) -> mouse::Interaction {
        let interaction = interaction(
            &tree.state.downcast_ref::<State>().engine,
            &self.layout,
            layout,
            cursor,
        );
        if interaction == mouse::Interaction::None {
            self.child.as_widget().mouse_interaction(
                &tree.children[0],
                layout,
                cursor,
                viewport,
                renderer,
            )
        } else {
            interaction
        }
    }

    fn draw(
        &self,
        tree: &Tree,
        renderer: &mut Renderer,
        theme: &Theme,
        style: &renderer::Style,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
    ) {
        self.child.as_widget().draw(
            &tree.children[0],
            renderer,
            theme,
            style,
            layout,
            cursor,
            viewport,
        );
    }

    fn operate(
        &mut self,
        tree: &mut Tree,
        layout: Layout<'_>,
        renderer: &Renderer,
        operation: &mut dyn Operation,
    ) {
        self.child
            .as_widget_mut()
            .operate(&mut tree.children[0], layout, renderer, operation);
    }

    fn overlay<'a>(
        &'a mut self,
        tree: &'a mut Tree,
        layout: Layout<'a>,
        renderer: &Renderer,
        viewport: &Rectangle,
        translation: Vector,
    ) -> Option<overlay::Element<'a, UiEvent, Theme, Renderer>> {
        let layout_tree = &self.layout;
        let state = tree.state.downcast_mut::<State>();
        let open = layout_tree
            .picker_snapshots(&state.engine)
            .iter()
            .any(|(_, snapshot)| snapshot.open);
        let child = self.child.as_widget_mut().overlay(
            &mut tree.children[0],
            layout,
            renderer,
            viewport,
            translation,
        )?;
        if !open {
            return Some(child);
        }
        Some(hosted_picker_overlay(child, move |event, cursor, shell| {
            route_open_picker(layout_tree, &mut state.engine, layout, event, cursor, shell)
        }))
    }
}

fn route_open_picker(
    layout_tree: &HostedLayout,
    engine: &mut Engine,
    layout: Layout<'_>,
    event: &Event,
    cursor: mouse::Cursor,
    shell: &mut Shell<'_, UiEvent>,
) -> bool {
    let Some(input @ Input::Pointer(pointer)) = iced_interact::input(event) else {
        return false;
    };
    if !matches!(pointer.phase, PointerPhase::Down | PointerPhase::Move) {
        return false;
    }
    let before = layout_tree.picker_snapshots(engine);
    if !before.iter().any(|(_, snapshot)| snapshot.open) {
        return false;
    }
    let targets = layout_tree.targets_with_engine(layout, cursor, Some(engine));
    let Some(emission) = engine.handle(input, &targets, Instant::now()) else {
        return false;
    };
    let captured = emission.outcome.is_captured();
    if let Some(action) = engine_event(&emission.path, emission.child, emission.outcome) {
        let (message, redraw, _) = action.into_inner();
        shell.request_redraw_at(redraw);
        if let Some(message) = message {
            shell.publish(message);
        }
    }
    if before != layout_tree.picker_snapshots(engine) {
        shell.request_redraw();
        shell.invalidate_layout();
    }
    if captured {
        shell.capture_event();
    }
    captured
}

fn sync_scrolls(
    child: &mut Element<'_, UiEvent>,
    tree: &mut Tree,
    layout: Layout<'_>,
    renderer: &Renderer,
    engine: &Engine,
    targets: &[Target<'_>],
) {
    for target in targets {
        let Some(offset) = engine.scroll_offset(target.path) else {
            continue;
        };
        let mut sync = sync_tree_scroll(target.path, offset);
        child
            .as_widget_mut()
            .operate(tree, layout, renderer, &mut sync);
        let horizontal_path = format!("{}/scroll-x", target.path);
        let horizontal = engine.scroll_offset(&horizontal_path).unwrap_or(0.0);
        let pressed = engine.pressed_item_index(target.path);
        let mut sync = sync_table_scroll(target.path, horizontal, pressed, offset);
        child
            .as_widget_mut()
            .operate(tree, layout, renderer, &mut sync);
    }
}

fn sync_pickers(
    child: &mut Element<'_, UiEvent>,
    tree: &mut Tree,
    layout: Layout<'_>,
    renderer: &Renderer,
    layout_tree: &HostedLayout,
    engine: &Engine,
) {
    for (path, _, _) in layout_tree.pickers() {
        let Some(snapshot) = engine.picker_snapshot(path) else {
            continue;
        };
        let mut sync = sync_picker(path, snapshot);
        child
            .as_widget_mut()
            .operate(tree, layout, renderer, &mut sync);
    }
}

fn sync_text_inputs(
    child: &mut Element<'_, UiEvent>,
    tree: &mut Tree,
    layout: Layout<'_>,
    renderer: &Renderer,
    engine: &Engine,
) {
    for (path, snapshot) in engine.text_input_snapshots() {
        let mut sync = sync_text_input(&path, snapshot);
        child
            .as_widget_mut()
            .operate(tree, layout, renderer, &mut sync);
    }
}

fn interaction(
    engine: &Engine,
    layout_tree: &HostedLayout,
    layout: Layout<'_>,
    cursor: mouse::Cursor,
) -> mouse::Interaction {
    engine
        .cursor(&layout_tree.targets_with_engine(layout, cursor, Some(engine)))
        .into()
}

fn active_descriptors(layout: &HostedLayout, targets: &[Target<'_>]) -> Vec<Descriptor> {
    layout
        .descriptors()
        .into_iter()
        .filter(|descriptor| match descriptor {
            Descriptor::Scroll { path, config } if config.axis() == ScrollAxis::Horizontal => {
                targets.iter().any(|target| target.path == path)
            }
            _ => true,
        })
        .collect()
}

fn hovered_control<'a>(targets: &[Target<'a>]) -> Option<&'a str> {
    targets
        .iter()
        .rev()
        .find(|target| target.hit.over())
        .map(|target| target.path)
}

fn captures_event(
    input: Option<Input<'_>>,
    answered: bool,
    scroll_answered: bool,
    control_pointer_answered: bool,
    pointer_captured: bool,
) -> bool {
    let routed_answered = answered
        && matches!(
            input,
            Some(Input::InputMethod(_) | Input::KeyPressed { .. } | Input::KeyReleased { .. })
        );
    let pointer_retained = pointer_captured && matches!(input, Some(Input::Pointer(_)));
    scroll_answered || routed_answered || control_pointer_answered || pointer_retained
}

fn control_pointer_answered(engine: &Engine, input: Input<'_>, path: &str, answered: bool) -> bool {
    answered
        && matches!(
            input,
            Input::Pointer(pointer)
                if matches!(pointer.phase, PointerPhase::Down | PointerPhase::Move | PointerPhase::Up)
        )
        && (engine.picker_snapshot(path).is_some() || engine.text_input_snapshot(path).is_some())
}

#[cfg(test)]
mod tests {
    use std::{borrow::Cow, cell::Cell, rc::Rc, sync::LazyLock};

    use iced::{
        Pixels, Point, Rectangle, Size,
        advanced::{
            InputMethod as IcedInputMethod, clipboard,
            graphics::text::font_system,
            input_method,
            layout::{Layout, Limits},
            overlay,
            widget::{Tree, tree::Tag},
        },
        keyboard::{
            self, Location, Modifiers as IcedModifiers,
            key::{Code, Named, Physical},
        },
        mouse::{self, Button, Cursor},
        time::Instant as IcedInstant,
        widget::{Space, Stack, container, mouse_area},
        window,
    };
    use iced_renderer::fallback::Renderer as FallbackRenderer;
    use iced_tiny_skia::Renderer as TinySkiaRenderer;
    use kithara_test_utils::kithara;
    use num_traits::cast::AsPrimitive;

    use super::*;
    use crate::{
        builtin,
        compile::{CompiledNode, CompiledUi, compile},
        draw::{DrawList, Pt, Rect},
        engine::{PickerSnapshot, ScrollConfig},
        expand::ControlSpec,
        ids::EndpointId,
        interact::{CursorShape, Key, Modifiers, mouse as mouse_input},
        module::WaveStyle,
        registry::{EndpointCategory, EndpointDesc, EndpointRegistry, ValueKind},
        render::{
            ControlAction, DragPhase, DropZone, HostLayer, InputOwner, LayerHit, ModuleChrome,
            ReadValue, Reads, StereoLevels, TableCell, TableRow, TreeIcon, TreeRow, WaveBucket,
            WaveformView, WheelSurface, Widget, WindowCommand, WindowLayerProgram,
            document::{Clock, Ctx},
            fonts::{FONT_BYTES, SANS},
            tree::control::HostedControl,
            window_layer,
        },
        shaping::TextResources,
        solve::{Length as SolveLength, Size as SolveSize},
        source::{MemResolver, UiConfig},
    };

    fn redraw_event() -> Event {
        Event::Window(window::Event::RedrawRequested(IcedInstant::now()))
    }

    fn pointer_input(phase: PointerPhase, at: Option<Pt>) -> Input<'static> {
        Input::Pointer(mouse_input(phase, at))
    }

    struct OverlapWindowProgram {
        area: Rc<Cell<Rect>>,
    }

    impl WindowLayerProgram for OverlapWindowProgram {
        type State = ();

        fn size(&self) -> SolveSize<SolveLength> {
            SolveSize::new(SolveLength::Fill, SolveLength::Fill)
        }

        fn layer(
            &self,
            _state: &(),
            bounds: Rect,
            _pointer: Option<Pt>,
        ) -> HostLayer<WindowCommand> {
            HostLayer::new(
                bounds,
                DrawList::default(),
                vec![LayerHit::new(
                    self.area.get(),
                    CursorShape::None,
                    WindowCommand::Drag,
                )],
            )
        }

        fn resources(&self) -> Option<&TextResources> {
            None
        }
    }

    fn dispatch_press(
        element: &mut Element<'_, UiEvent>,
        tree: &mut Tree,
        node: &layout::Node,
        renderer: &Renderer,
        viewport: Size,
        point: Point,
    ) -> (Vec<UiEvent>, bool) {
        let event = Event::Mouse(mouse::Event::ButtonPressed(Button::Left));
        let cursor = Cursor::Available(point);
        let bounds = Rectangle::with_size(viewport);
        let mut clipboard = clipboard::Null;
        let mut messages = Vec::new();
        let mut shell = Shell::new(&mut messages);
        let mut base_cursor = cursor;
        {
            let root_layout = Layout::new(node);
            if let Some(overlay) =
                element
                    .as_widget_mut()
                    .overlay(tree, root_layout, renderer, &bounds, Vector::ZERO)
            {
                let mut nested = overlay::Nested::new(overlay);
                let overlay_node = nested.layout(renderer, viewport);
                nested.update(
                    &event,
                    Layout::new(&overlay_node),
                    cursor,
                    renderer,
                    &mut clipboard,
                    &mut shell,
                );
                if !shell.is_event_captured()
                    && nested.mouse_interaction(Layout::new(&overlay_node), cursor, renderer)
                        != mouse::Interaction::None
                {
                    base_cursor = Cursor::Unavailable;
                }
            }
        }
        if !shell.is_event_captured() {
            element.as_widget_mut().update(
                tree,
                &event,
                Layout::new(node),
                base_cursor,
                renderer,
                &mut clipboard,
                &mut shell,
                &bounds,
            );
        }
        let captured = shell.is_event_captured();
        drop(shell);
        (messages, captured)
    }

    struct Fixtures {
        table_rows: Vec<TableRow<'static>>,
        tree_rows: [TreeRow<'static>; 8],
        wave_buckets: [WaveBucket; 2],
    }

    static FIXTURES: LazyLock<Fixtures> = LazyLock::new(|| Fixtures {
        table_rows: vec![
            TableRow::new(
                vec![
                    TableCell::text("title", "Row"),
                    TableCell::text("artist", "Detail"),
                    TableCell::text("time", "04:12"),
                    TableCell::text("deck", "A"),
                    TableCell::text("bpm", "124.0"),
                    TableCell::text("key", "8A"),
                    TableCell::number("energy", 7),
                    TableCell::text("transition", "blend"),
                ],
                false,
            );
            9
        ],
        tree_rows: [TreeRow {
            depth: 1,
            label: "Row",
            icon: TreeIcon::Folder,
            count: None,
            expanded: None,
            selected: false,
            muted: false,
        }; 8],
        wave_buckets: [
            WaveBucket {
                low: 0.2,
                mid: 0.4,
                high: 0.6,
            },
            WaveBucket {
                low: 0.3,
                mid: 0.5,
                high: 0.7,
            },
        ],
    });

    struct Registry {
        boolean: EndpointDesc,
        scalar: EndpointDesc,
        scoped_scalar: EndpointDesc,
        stereo: EndpointDesc,
        text: EndpointDesc,
        table: EndpointDesc,
        tree: EndpointDesc,
        trigger: EndpointDesc,
        waveform: EndpointDesc,
    }

    impl Default for Registry {
        fn default() -> Self {
            Self {
                boolean: EndpointDesc::new(ValueKind::Bool),
                scalar: EndpointDesc::new(ValueKind::Scalar),
                scoped_scalar: EndpointDesc::new(ValueKind::Scalar).with_scope("deck"),
                stereo: EndpointDesc::new(ValueKind::Stereo),
                text: EndpointDesc::new(ValueKind::Text),
                table: EndpointDesc::new(ValueKind::Table),
                tree: EndpointDesc::new(ValueKind::Tree),
                trigger: EndpointDesc::new(ValueKind::Trigger),
                waveform: EndpointDesc::new(ValueKind::Waveform).with_scope("deck"),
            }
        }
    }

    impl EndpointRegistry for Registry {
        fn endpoint(&self, category: EndpointCategory, id: &EndpointId) -> Option<&EndpointDesc> {
            match (category, id.0.as_str()) {
                (EndpointCategory::Parameter, "gain")
                | (EndpointCategory::Model | EndpointCategory::Parameter, "mock.cells.segmented")
                | (EndpointCategory::Model, "mock.volume") => Some(&self.scalar),
                (EndpointCategory::Telemetry, "levels")
                | (EndpointCategory::Model, "mock.levels") => Some(&self.stereo),
                (
                    EndpointCategory::Model,
                    "mock.toggle.on" | "mock.toggle.off" | "mock.checkbox.on" | "mock.checkbox.off"
                    | "mock.button.play" | "mock.button.cue" | "mock.button.sync"
                    | "mock.chip.active" | "mock.chip.inactive",
                ) => Some(&self.boolean),
                (
                    EndpointCategory::Model,
                    "gallery.label.meters"
                    | "gallery.label.toggles"
                    | "gallery.label.chips"
                    | "gallery.label.transport"
                    | "gallery.label.regular"
                    | "gallery.label.text"
                    | "gallery.label.faders"
                    | "gallery.label.scalar"
                    | "mock.track.title"
                    | "mock.track.artist",
                ) => Some(&self.text),
                (EndpointCategory::Model, endpoint)
                    if endpoint.starts_with("gallery.tab.")
                        || endpoint.starts_with("gallery.module.") =>
                {
                    Some(&self.boolean)
                }
                (EndpointCategory::Model, "mock.wave") => Some(&self.waveform),
                (EndpointCategory::Model, "gallery.table.preset" | "library.scope") => {
                    Some(&self.scalar)
                }
                (EndpointCategory::Model, "library.breadcrumb" | "library.query") => {
                    Some(&self.text)
                }
                (EndpointCategory::Model, "library.visible_tracks") => Some(&self.table),
                (EndpointCategory::Model, "library.tree") => Some(&self.tree),
                (EndpointCategory::Model, endpoint)
                    if endpoint.starts_with("gallery.table.columns.") =>
                {
                    if endpoint.starts_with("gallery.table.columns.width.") {
                        Some(&self.scalar)
                    } else {
                        Some(&self.boolean)
                    }
                }
                (EndpointCategory::Command, "mock.seek")
                | (EndpointCategory::Telemetry, "deck.playback.position_normalized") => {
                    Some(&self.scoped_scalar)
                }
                (EndpointCategory::Command, "eq-menu-toggle") => Some(&self.trigger),
                _ => None,
            }
        }
    }

    /// What the host hands the document for one frame, built from a fixture
    /// reader so a test drives the clock rather than waiting for one.
    fn ctx<'a>(ui: &'a CompiledUi, reads: &'a dyn Reads) -> Ctx<'a, 'a> {
        Ctx::new(ui, reads, builtin::skin_doc(), Clock::default())
    }

    struct FixtureReads {
        gain: f64,
        progress: f64,
        query: String,
    }

    impl Default for FixtureReads {
        fn default() -> Self {
            Self {
                gain: 0.5,
                progress: 0.75,
                query: String::new(),
            }
        }
    }

    impl Reads for FixtureReads {
        fn get(&self, endpoint: &str) -> Option<ReadValue<'_>> {
            match endpoint {
                "gain" | "mock.volume" => Some(ReadValue::Scalar(self.gain)),
                "levels" | "mock.levels" => Some(ReadValue::Stereo(StereoLevels {
                    l: 0.4,
                    r: 0.6,
                    volume: 0.5,
                })),
                "mock.toggle.on" | "mock.checkbox.on" | "mock.button.play" | "mock.button.sync" => {
                    Some(ReadValue::Bool(true))
                }
                "mock.toggle.off" | "mock.checkbox.off" | "mock.button.cue" => {
                    Some(ReadValue::Bool(false))
                }
                "mock.chip.active" => Some(ReadValue::Bool(true)),
                "mock.chip.inactive" => Some(ReadValue::Bool(false)),
                "mock.cells.segmented" => Some(ReadValue::Scalar(2.0)),
                "gallery.table.preset" | "library.scope" => Some(ReadValue::Scalar(0.0)),
                "library.breadcrumb" => Some(ReadValue::Text("All Tracks")),
                "library.query" => Some(ReadValue::Text(&self.query)),
                "library.visible_tracks" => Some(ReadValue::Table(&FIXTURES.table_rows)),
                "library.tree" => Some(ReadValue::Tree(&FIXTURES.tree_rows)),
                "gallery.label.meters" => Some(ReadValue::Text("VU / STEREO / VERTICAL")),
                "gallery.label.toggles" => Some(ReadValue::Text("TOGGLES / CHECKBOXES")),
                "gallery.label.chips" => Some(ReadValue::Text("CHIP")),
                "gallery.label.transport" => Some(ReadValue::Text("TRANSPORT")),
                "gallery.label.regular" => Some(ReadValue::Text("REGULAR")),
                "gallery.label.text" => Some(ReadValue::Text("TEXT STYLES")),
                "gallery.label.faders" => Some(ReadValue::Text("HORIZONTAL FADERS")),
                "gallery.label.scalar" => Some(ReadValue::Text("SCALAR TELEMETRY")),
                "mock.track.title" => Some(ReadValue::Text("Track")),
                "mock.track.artist" => Some(ReadValue::Text("Artist")),
                endpoint if endpoint.starts_with("gallery.tab.") => {
                    Some(ReadValue::Bool(endpoint == "gallery.tab.atoms"))
                }
                endpoint if endpoint.starts_with("gallery.module.") => {
                    Some(ReadValue::Bool(endpoint == "gallery.module.deck"))
                }
                endpoint if endpoint.starts_with("gallery.table.columns.width.") => None,
                endpoint if endpoint.starts_with("gallery.table.columns.") => {
                    Some(ReadValue::Bool(true))
                }
                "mock.wave@deck=a" => Some(ReadValue::Waveform(WaveformView {
                    buckets: &FIXTURES.wave_buckets,
                    revision: 0,
                    beats: &[],
                    downbeats: &[],
                    bpm: None,
                    r#loop: None,
                    cues: &[],
                })),
                "deck.playback.position_normalized@deck=a" => {
                    Some(ReadValue::Scalar(self.progress))
                }
                _ => None,
            }
        }
    }

    fn compiled_fixture() -> CompiledUi {
        let mut resolver = MemResolver::default();
        resolver.insert(
            "layout.klayout.ron",
            include_str!("../../../tests/fixtures/retained_host/layout.klayout.ron"),
        );
        resolver.insert(
            "mixer.kmodule.ron",
            include_str!("../../../tests/fixtures/retained_host/mixer.kmodule.ron"),
        );
        resolver.insert(
            "app-strip.kmodule.ron",
            include_str!("../../../tests/fixtures/retained_host/app-strip.kmodule.ron"),
        );
        compile(
            "layout.klayout.ron",
            &resolver,
            &Registry::default(),
            builtin::skin_doc(),
            builtin::text_doc(),
            &UiConfig::default(),
        )
        .unwrap_or_else(|error| panic!("retained host fixture must compile: {error}"))
    }

    fn compiled_tree_surface() -> CompiledUi {
        let mut resolver = MemResolver::default();
        resolver.insert(
            "tree.klayout.ron",
            r#"(schema: "kithara.layout", version: 1, id: "tree-surface-host",
                root: Module(instance: "tree", source: "tree.kmodule.ron"))"#,
        );
        resolver.insert(
            "tree.kmodule.ron",
            r#"(schema: "kithara.module", version: 1, id: "gallery-tree-tab",
                root: Column(
                    id: "surface",
                    write: Parameter(id: "gain"),
                    children: [
                        Tree(id: "browser", read: Model(id: "library.tree")),
                    ],
                ))"#,
        );
        compile(
            "tree.klayout.ron",
            &resolver,
            &Registry::default(),
            builtin::skin_doc(),
            builtin::text_doc(),
            &UiConfig::default(),
        )
        .unwrap_or_else(|error| panic!("tree surface fixture must compile: {error}"))
    }

    fn compiled_gallery_library() -> CompiledUi {
        let mut resolver = MemResolver::default();
        resolver.insert(
            "library.klayout.ron",
            r#"(schema: "kithara.layout", version: 1, id: "gallery-library-host",
                root: Module(instance: "library2", source: "library2.kmodule.ron"))"#,
        );
        resolver.insert(
            "library2.kmodule.ron",
            include_str!("../../../examples/gallery/assets/modules/tabs/library2.kmodule.ron"),
        );
        compile(
            "library.klayout.ron",
            &resolver,
            &Registry::default(),
            builtin::skin_doc(),
            builtin::text_doc(),
            &UiConfig::default(),
        )
        .unwrap_or_else(|error| panic!("gallery library fixture must compile: {error}"))
    }

    fn compiled_gallery_primitive(page: &str, source: &str) -> CompiledUi {
        let mut resolver = MemResolver::default();
        resolver.insert(
            "gallery.klayout.ron",
            r#"(schema: "kithara.layout", version: 1, id: "gallery-primitive-host",
                root: Module(instance: "atoms", source: "modules/tabs/atoms.kmodule.ron"))"#,
        );
        let tab = format!(
            r#"(schema: "kithara.module", version: 1, id: "gallery-atoms-tab",
                root: Column(children: [
                    Text(id: "intro", label: "ATOMS"),
                    Include(id: "{page}", source: "../primitives/{page}.kmodule.ron"),
                ]))"#
        );
        resolver.insert("modules/tabs/atoms.kmodule.ron", &tab);
        resolver.insert(&format!("modules/primitives/{page}.kmodule.ron"), source);
        compile(
            "gallery.klayout.ron",
            &resolver,
            &Registry::default(),
            builtin::skin_doc(),
            builtin::text_doc(),
            &UiConfig::default(),
        )
        .unwrap_or_else(|error| panic!("gallery {page} fixture must compile: {error}"))
    }

    fn compiled_gallery_meters() -> CompiledUi {
        compiled_gallery_primitive(
            "meters",
            include_str!("../../../examples/gallery/assets/modules/primitives/meters.kmodule.ron"),
        )
    }

    fn compiled_gallery_toggles() -> CompiledUi {
        compiled_gallery_primitive(
            "toggles",
            include_str!("../../../examples/gallery/assets/modules/primitives/toggles.kmodule.ron"),
        )
    }

    fn compiled_gallery_chips() -> CompiledUi {
        compiled_gallery_primitive(
            "chips",
            include_str!("../../../examples/gallery/assets/modules/primitives/chips.kmodule.ron"),
        )
    }

    fn compiled_gallery_buttons() -> CompiledUi {
        let mut resolver = MemResolver::default();
        resolver.insert(
            "gallery.klayout.ron",
            r#"(schema: "kithara.layout", version: 1, id: "gallery-buttons-host",
                root: Module(instance: "buttons", source: "buttons.kmodule.ron"))"#,
        );
        resolver.insert(
            "buttons.kmodule.ron",
            include_str!("../../../examples/gallery/assets/modules/tabs/buttons.kmodule.ron"),
        );
        compile(
            "gallery.klayout.ron",
            &resolver,
            &Registry::default(),
            builtin::skin_doc(),
            builtin::text_doc(),
            &UiConfig::default(),
        )
        .unwrap_or_else(|error| panic!("gallery buttons fixture must compile: {error}"))
    }

    fn compiled_gallery_cells() -> CompiledUi {
        let mut resolver = MemResolver::default();
        resolver.insert(
            "gallery.klayout.ron",
            r#"(schema: "kithara.layout", version: 1, id: "gallery-cells-host",
                root: Module(instance: "cells", source: "cells.kmodule.ron"))"#,
        );
        resolver.insert(
            "cells.kmodule.ron",
            include_str!("../../../examples/gallery/assets/modules/tabs/cells.kmodule.ron"),
        );
        compile(
            "gallery.klayout.ron",
            &resolver,
            &Registry::default(),
            builtin::skin_doc(),
            builtin::text_doc(),
            &UiConfig::default(),
        )
        .unwrap_or_else(|error| panic!("gallery cells fixture must compile: {error}"))
    }

    fn compiled_gallery_table() -> CompiledUi {
        let mut resolver = MemResolver::default();
        resolver.insert(
            "gallery.klayout.ron",
            r#"(schema: "kithara.layout", version: 1, id: "gallery-track-list-host",
                root: Module(instance: "table", source: "table.kmodule.ron"))"#,
        );
        resolver.insert(
            "table.kmodule.ron",
            include_str!("../../../examples/gallery/assets/modules/tabs/table.kmodule.ron"),
        );
        compile(
            "gallery.klayout.ron",
            &resolver,
            &Registry::default(),
            builtin::skin_doc(),
            builtin::text_doc(),
            &UiConfig::default(),
        )
        .unwrap_or_else(|error| panic!("gallery track-list fixture must compile: {error}"))
    }

    fn compiled_gallery_faders() -> CompiledUi {
        let mut resolver = MemResolver::default();
        resolver.insert(
            "gallery.klayout.ron",
            r#"(schema: "kithara.layout", version: 1, id: "gallery-faders-host",
                root: Module(instance: "faders", source: "faders.kmodule.ron"))"#,
        );
        resolver.insert(
            "faders.kmodule.ron",
            include_str!("../../../examples/gallery/assets/modules/tabs/faders.kmodule.ron"),
        );
        compile(
            "gallery.klayout.ron",
            &resolver,
            &Registry::default(),
            builtin::skin_doc(),
            builtin::text_doc(),
            &UiConfig::default(),
        )
        .unwrap_or_else(|error| panic!("gallery faders fixture must compile: {error}"))
    }

    fn compiled_gallery_tabs() -> CompiledUi {
        let mut resolver = MemResolver::default();
        resolver.insert(
            "gallery.klayout.ron",
            r#"(schema: "kithara.layout", version: 1, id: "gallery-tabs-host",
                root: Module(instance: "modules-tabs", source: "module-tabs.kmodule.ron"))"#,
        );
        resolver.insert(
            "module-tabs.kmodule.ron",
            include_str!("../../../examples/gallery/assets/modules/module-tabs.kmodule.ron"),
        );
        compile(
            "gallery.klayout.ron",
            &resolver,
            &Registry::default(),
            builtin::skin_doc(),
            builtin::text_doc(),
            &UiConfig::default(),
        )
        .unwrap_or_else(|error| panic!("gallery module tabs fixture must compile: {error}"))
    }

    fn compiled_gallery_nav() -> CompiledUi {
        let mut resolver = MemResolver::default();
        resolver.insert(
            "gallery.klayout.ron",
            r#"(schema: "kithara.layout", version: 1, id: "gallery-nav-host",
                root: Module(instance: "gallery", source: "modules/nav.kmodule.ron"))"#,
        );
        resolver.insert(
            "modules/nav.kmodule.ron",
            include_str!("../../../examples/gallery/assets/modules/nav.kmodule.ron"),
        );
        resolver.insert(
            "modules/nav/item.kmodule.ron",
            include_str!("../../../examples/gallery/assets/modules/nav/item.kmodule.ron"),
        );
        compile(
            "gallery.klayout.ron",
            &resolver,
            &Registry::default(),
            builtin::skin_doc(),
            builtin::text_doc(),
            &UiConfig::default(),
        )
        .unwrap_or_else(|error| panic!("gallery nav fixture must compile: {error}"))
    }

    /// A nav item outside any hosted subtree. Every one the repository ships is
    /// inside one, so this is the only way the item's own grip is what answers
    /// a press.
    fn compiled_leaf_nav_item() -> CompiledUi {
        let mut resolver = MemResolver::default();
        resolver.insert(
            "layout.klayout.ron",
            r#"(schema: "kithara.layout", version: 1, id: "nav-leaf-host",
                root: Module(instance: "nav", source: "nav-leaf.kmodule.ron"))"#,
        );
        resolver.insert(
            "nav-leaf.kmodule.ron",
            r#"(schema: "kithara.module", version: 1, id: "nav-leaf",
                root: NavItem(
                    id: "item",
                    label: "BUTTONS",
                    icon: "Play",
                    read: Model(id: "mock.toggle.off"),
                ))"#,
        );
        compile(
            "layout.klayout.ron",
            &resolver,
            &Registry::default(),
            builtin::skin_doc(),
            builtin::text_doc(),
            &UiConfig::default(),
        )
        .unwrap_or_else(|error| panic!("the leaf nav fixture must compile: {error}"))
    }

    /// Four rows in a box one row tall, and nothing under that box. Three of
    /// the rows are past the fold: the window shows blank where their layout
    /// still puts them, which is what makes a press there answerable by exactly
    /// one thing — the cut, or the row the box hid.
    fn compiled_clipped_scroll() -> CompiledUi {
        let mut resolver = MemResolver::default();
        resolver.insert(
            "layout.klayout.ron",
            r#"(schema: "kithara.layout", version: 1, id: "clipped-scroll-host",
                root: Module(instance: "nav", source: "clipped-scroll.kmodule.ron"))"#,
        );
        resolver.insert(
            "clipped-scroll.kmodule.ron",
            r#"(schema: "kithara.module", version: 1, id: "clipped-scroll",
                root: Column(children: [
                    Scroll(
                        id: "rows",
                        size: (w: Fill, h: Fixed(30.0)),
                        child: Column(children: [
                            NavItem(id: "first", label: "FIRST", icon: "Play",
                                read: Model(id: "mock.toggle.off")),
                            NavItem(id: "second", label: "SECOND", icon: "Play",
                                read: Model(id: "mock.toggle.off")),
                            NavItem(id: "third", label: "THIRD", icon: "Play",
                                read: Model(id: "mock.toggle.off")),
                            NavItem(id: "fourth", label: "FOURTH", icon: "Play",
                                read: Model(id: "mock.toggle.off")),
                        ]),
                    ),
                ]))"#,
        );
        compile(
            "layout.klayout.ron",
            &resolver,
            &Registry::default(),
            builtin::skin_doc(),
            builtin::text_doc(),
            &UiConfig::default(),
        )
        .unwrap_or_else(|error| panic!("the clipped scroll fixture must compile: {error}"))
    }

    /// A meter taller than the box it scrolls in, and a knob below that box.
    ///
    /// The meter seeks to wherever the pointer is rather than to how far it has
    /// travelled, so it is the control a cut can silence mid-drag: it reads the
    /// position out of the hit it is handed, and a cut hit carries none. Where
    /// the pointer ends up there is the knob, which must stay silent while the
    /// meter owns the gesture.
    fn compiled_clipped_drag() -> CompiledUi {
        let mut resolver = MemResolver::default();
        resolver.insert(
            "layout.klayout.ron",
            r#"(schema: "kithara.layout", version: 1, id: "clipped-drag-host",
                root: Module(instance: "nav", source: "clipped-drag.kmodule.ron"))"#,
        );
        resolver.insert(
            "clipped-drag.kmodule.ron",
            r#"(schema: "kithara.module", version: 1, id: "clipped-drag",
                root: Column(gap: 0.0, children: [
                    Scroll(
                        id: "rows",
                        size: (w: Fill, h: Fixed(120.0)),
                        child: Column(gap: 0.0, children: [
                            VuVertical(
                                id: "held",
                                size: (w: Fill, h: Fixed(240.0)),
                                read: Telemetry(id: "levels"),
                                write: Parameter(id: "gain"),
                            ),
                        ]),
                    ),
                    Knob(id: "other", read: Parameter(id: "gain"),
                        write: Parameter(id: "gain")),
                ]))"#,
        );
        compile(
            "layout.klayout.ron",
            &resolver,
            &Registry::default(),
            builtin::skin_doc(),
            builtin::text_doc(),
            &UiConfig::default(),
        )
        .unwrap_or_else(|error| panic!("the clipped drag fixture must compile: {error}"))
    }

    fn compiled_overview_row() -> CompiledUi {
        let mut resolver = MemResolver::default();
        resolver.insert(
            "layout.klayout.ron",
            r#"(schema: "kithara.layout", version: 1, id: "overview-host",
                root: Module(instance: "overview", source: "app-overview.kmodule.ron"))"#,
        );
        resolver.insert(
            "app-overview.kmodule.ron",
            r#"(schema: "kithara.module", version: 1, id: "app-overview",
                root: Row(children: [
                    Include(
                        id: "a",
                        source: "app-overview-row.kmodule.ron",
                        with: { "deck": "a" },
                    ),
                ]))"#,
        );
        resolver.insert(
            "app-overview-row.kmodule.ron",
            r#"(schema: "kithara.module", version: 1, id: "app-overview-row",
                parameters: ["deck"],
                root: Row(gap: 0.0, size: (w: Fill, h: Fixed(40.0)), children: [
                    Text(id: "letter", label: "A"),
                    Wave(
                        id: "wave",
                        read: Model(id: "mock.wave", with: { "deck": "$deck" }),
                        write: Command(id: "mock.seek", with: { "deck": "$deck" }),
                    ),
                    Text(id: "remain", label: "00:00"),
                ]))"#,
        );
        compile(
            "layout.klayout.ron",
            &resolver,
            &Registry::default(),
            builtin::skin_doc(),
            builtin::text_doc(),
            &UiConfig::default(),
        )
        .unwrap_or_else(|error| panic!("overview row fixture must compile: {error}"))
    }

    fn headless_renderer() -> Renderer {
        let mut fonts = font_system()
            .write()
            .unwrap_or_else(|error| panic!("iced font system lock must be available: {error}"));
        for bytes in FONT_BYTES {
            fonts.load_font(Cow::Borrowed(bytes));
        }
        drop(fonts);

        FallbackRenderer::Secondary(TinySkiaRenderer::new(SANS, Pixels(14.0)))
    }

    fn key_event(key: Named, code: Code) -> Event {
        Event::Keyboard(keyboard::Event::KeyPressed {
            key: keyboard::Key::Named(key),
            modified_key: keyboard::Key::Named(key),
            physical_key: Physical::Code(code),
            location: Location::Standard,
            modifiers: IcedModifiers::empty(),
            text: None,
            repeat: false,
        })
    }

    fn key_release_event(key: Named, code: Code) -> Event {
        Event::Keyboard(keyboard::Event::KeyReleased {
            key: keyboard::Key::Named(key),
            modified_key: keyboard::Key::Named(key),
            physical_key: Physical::Code(code),
            location: Location::Standard,
            modifiers: IcedModifiers::empty(),
        })
    }

    fn character_event(character: &str, code: Code) -> Event {
        Event::Keyboard(keyboard::Event::KeyPressed {
            key: keyboard::Key::Character(character.into()),
            modified_key: keyboard::Key::Character(character.into()),
            physical_key: Physical::Code(code),
            location: Location::Standard,
            modifiers: IcedModifiers::empty(),
            text: Some(character.into()),
            repeat: false,
        })
    }

    fn host_count(tree: &Tree) -> usize {
        usize::from(tree.tag == Tag::of::<State>())
            + tree.children.iter().map(host_count).sum::<usize>()
    }

    fn claimed_components(node: &ExpandedNode, components: &mut Vec<&'static str>) {
        match node {
            ExpandedNode::Row { children, .. }
            | ExpandedNode::Column { children, .. }
            | ExpandedNode::Slot { children, .. }
            | ExpandedNode::Stage { children, .. } => {
                for child in children {
                    claimed_components(child, components);
                }
            }
            ExpandedNode::Object { child, .. }
            | ExpandedNode::Optional { child, .. }
            | ExpandedNode::Scroll { child, .. } => {
                claimed_components(child, components);
            }
            ExpandedNode::Control { spec, .. } => match spec {
                ControlSpec::Button { .. }
                | ControlSpec::NavItem { .. }
                | ControlSpec::TabLarge { .. }
                | ControlSpec::Toggle
                | ControlSpec::Checkbox
                | ControlSpec::Chip { .. } => {
                    components.push("activation");
                }
                ControlSpec::Segmented { .. } => {
                    components.push("segmented");
                }
                ControlSpec::ContextBar { .. } => {
                    components.push("picker");
                }
                ControlSpec::Table { .. } => {
                    components.push("track-list");
                }
                ControlSpec::Fader { .. } => {
                    components.push("fader");
                }
                ControlSpec::Knob { .. } => {
                    components.push("knob");
                }
                ControlSpec::VuStereo => {
                    components.push("stereo-meter");
                }
                ControlSpec::VuVertical { .. } => {
                    components.push("vertical-vu");
                }
                ControlSpec::Crossfader { .. } => {
                    components.push("crossfader");
                }
                ControlSpec::Wave { style, .. } => {
                    components.push(if *style == WaveStyle::Hero {
                        "hero-wave"
                    } else {
                        "wave"
                    });
                }
                ControlSpec::Tree { .. } => {
                    components.push("text-input");
                    components.push("scroll");
                }
                _ => {}
            },
            ExpandedNode::Popover { anchor, .. } => claimed_components(anchor, components),
            ExpandedNode::Pressable { child, .. } => claimed_components(child, components),
        }
    }

    fn descriptor_path(descriptor: &Descriptor) -> &str {
        match descriptor {
            Descriptor::Activation { path }
            | Descriptor::Crossing { path }
            | Descriptor::Segmented { path, .. }
            | Descriptor::Picker { path, .. }
            | Descriptor::TextInput { path, .. }
            | Descriptor::Scroll { path, .. }
            | Descriptor::ColumnDivider { path, .. }
            | Descriptor::Fader { path, .. }
            | Descriptor::Crossfader { path }
            | Descriptor::Knob { path, .. }
            | Descriptor::StereoMeter { path }
            | Descriptor::VerticalVu { path }
            | Descriptor::Wave { path }
            | Descriptor::HeroWave { path, .. } => path,
            Descriptor::Item { target, .. } => target,
        }
    }

    fn chrome_child<'a>(
        content: Element<'a, UiEvent>,
        module: &'a str,
        style: ChromeStyle,
        drop: bool,
        collapsed: bool,
    ) -> Element<'a, UiEvent> {
        ModuleChrome::builder()
            .content(content)
            .module(module)
            .assign(Vec::new())
            .style(style)
            .input_owner(InputOwner::Engine)
            .maybe_drop(drop.then(|| DropZone::new(false)))
            .collapsed(collapsed)
            .skin(builtin::skin())
            .build()
            .view()
    }

    #[kithara::test]
    fn module_drop_crossing_observes_boundaries_and_forwards_to_the_child() {
        let instance = "deck-a";
        let module = "app-deck";
        let spec = ModuleHost {
            instance,
            module,
            chrome: ChromeStyle::Plain,
            collapsed: false,
            drop: true,
        };
        let content = mouse_area(Space::new().width(Length::Fill).height(Length::Fill))
            .on_move(|_| UiEvent::OpenSettings)
            .on_exit(UiEvent::OpenSettings)
            .into();
        let child = chrome_child(content, module, ChromeStyle::Plain, true, false);
        let mut element = module_host(child, spec);
        let renderer = headless_renderer();
        let viewport = Size::new(100.0, 40.0);
        let mut tree = Tree::new(element.as_widget());
        let node = element.as_widget_mut().layout(
            &mut tree,
            &renderer,
            &Limits::new(Size::ZERO, viewport),
        );
        let hosted = HostedLayout::module(spec);
        let targets = hosted.targets(Layout::new(&node), Cursor::Unavailable);
        assert_eq!(
            targets.iter().map(|target| target.path).collect::<Vec<_>>(),
            ["deck-a/drop"],
            "the whole drop zone is the outer host's only target"
        );
        assert_eq!(targets[0].hit.area(), Rectangle::with_size(viewport).into());

        let mut clipboard = clipboard::Null;
        let mut messages = Vec::new();
        let inside = Point::new(50.0, 20.0);
        let mut shell = Shell::new(&mut messages);
        element.as_widget_mut().update(
            &mut tree,
            &Event::Mouse(mouse::Event::CursorMoved { position: inside }),
            Layout::new(&node),
            Cursor::Available(inside),
            &renderer,
            &mut clipboard,
            &mut shell,
            &Rectangle::with_size(viewport),
        );
        assert!(!shell.is_event_captured());
        drop(shell);
        assert_eq!(
            messages,
            [
                UiEvent::Control {
                    path: "deck-a/drop".to_owned(),
                    action: ControlAction::Drag(DragPhase::Over(true)),
                },
                UiEvent::OpenSettings,
            ],
            "entry publishes once and the observed move still reaches the child"
        );

        let still_inside = Point::new(60.0, 20.0);
        let mut shell = Shell::new(&mut messages);
        element.as_widget_mut().update(
            &mut tree,
            &Event::Mouse(mouse::Event::CursorMoved {
                position: still_inside,
            }),
            Layout::new(&node),
            Cursor::Available(still_inside),
            &renderer,
            &mut clipboard,
            &mut shell,
            &Rectangle::with_size(viewport),
        );
        assert!(!shell.is_event_captured());
        drop(shell);
        assert_eq!(
            messages,
            [
                UiEvent::Control {
                    path: "deck-a/drop".to_owned(),
                    action: ControlAction::Drag(DragPhase::Over(true)),
                },
                UiEvent::OpenSettings,
                UiEvent::OpenSettings,
            ],
            "an inside move produces no second crossing and still reaches the child"
        );

        let mut shell = Shell::new(&mut messages);
        element.as_widget_mut().update(
            &mut tree,
            &Event::Mouse(mouse::Event::CursorLeft),
            Layout::new(&node),
            Cursor::Unavailable,
            &renderer,
            &mut clipboard,
            &mut shell,
            &Rectangle::with_size(viewport),
        );
        assert!(!shell.is_event_captured());
        drop(shell);
        assert_eq!(
            messages,
            [
                UiEvent::Control {
                    path: "deck-a/drop".to_owned(),
                    action: ControlAction::Drag(DragPhase::Over(true)),
                },
                UiEvent::OpenSettings,
                UiEvent::OpenSettings,
                UiEvent::Control {
                    path: "deck-a/drop".to_owned(),
                    action: ControlAction::Drag(DragPhase::Over(false)),
                },
                UiEvent::OpenSettings,
            ],
            "exit publishes once and the observed leave still reaches the child"
        );
    }

    #[kithara::test]
    fn full_module_header_activation_toggles_the_module_directly() {
        let module = "app-deck";
        let spec = ModuleHost {
            instance: "deck-a",
            module,
            chrome: ChromeStyle::Full,
            collapsed: false,
            drop: true,
        };
        let content = Space::new().width(Length::Fill).height(Length::Fill).into();
        let child = chrome_child(content, module, ChromeStyle::Full, true, false);
        let mut element = module_host(child, spec);
        let renderer = headless_renderer();
        let viewport = Size::new(200.0, 120.0);
        let mut tree = Tree::new(element.as_widget());
        let node = element.as_widget_mut().layout(
            &mut tree,
            &renderer,
            &Limits::new(Size::ZERO, viewport),
        );
        let hosted = HostedLayout::module(spec);
        let descriptors = hosted.descriptors();
        assert_eq!(
            descriptors.iter().map(descriptor_path).collect::<Vec<_>>(),
            ["deck-a/drop", "deck-a/header"]
        );
        assert!(matches!(
            descriptors.as_slice(),
            [Descriptor::Crossing { .. }, Descriptor::Activation { .. }]
        ));
        let targets = hosted.targets(Layout::new(&node), Cursor::Unavailable);
        assert_eq!(
            targets.iter().map(|target| target.path).collect::<Vec<_>>(),
            ["deck-a/drop", "deck-a/header"]
        );
        let header = targets[1].hit.area();
        let cursor = Cursor::Available(Point::new(
            header.x + header.w / 2.0,
            header.y + header.h / 2.0,
        ));
        let mut clipboard = clipboard::Null;
        let mut messages = Vec::new();
        let mut shell = Shell::new(&mut messages);
        element.as_widget_mut().update(
            &mut tree,
            &Event::Mouse(mouse::Event::ButtonPressed(Button::Left)),
            Layout::new(&node),
            cursor,
            &renderer,
            &mut clipboard,
            &mut shell,
            &Rectangle::with_size(viewport),
        );
        assert!(
            !shell.is_event_captured(),
            "the stateless activation does not retain the engine capture slot"
        );
        drop(shell);

        assert_eq!(messages, [UiEvent::ToggleModule(module.to_owned())]);
    }

    #[kithara::test]
    fn decoded_input_unanswered_by_the_engine_reaches_the_child() {
        let child = mouse_area(Space::new().width(Length::Fill).height(Length::Fill))
            .on_press(UiEvent::OpenSettings)
            .into();
        let mut element = Element::new(Host {
            child,
            layout: HostedLayout::Control(None),
        });
        let renderer = headless_renderer();
        let viewport = Size::new(100.0, 40.0);
        let mut tree = Tree::new(element.as_widget());
        let node = element.as_widget_mut().layout(
            &mut tree,
            &renderer,
            &Limits::new(Size::ZERO, viewport),
        );
        let mut clipboard = clipboard::Null;
        let mut messages = Vec::new();
        let mut shell = Shell::new(&mut messages);

        element.as_widget_mut().update(
            &mut tree,
            &Event::Mouse(mouse::Event::ButtonPressed(Button::Left)),
            Layout::new(&node),
            Cursor::Available(Point::new(50.0, 20.0)),
            &renderer,
            &mut clipboard,
            &mut shell,
            &Rectangle::with_size(viewport),
        );
        drop(shell);

        assert_eq!(messages, [UiEvent::OpenSettings]);
    }

    #[kithara::test]
    fn only_keys_answered_by_focus_are_reported_captured_to_the_host() {
        for key in [Key::Delete, Key::Backspace] {
            let input = Some(Input::KeyPressed {
                key,
                modifiers: Modifiers::default(),
                text: None,
            });
            assert!(
                captures_event(input, true, false, false, false),
                "a focused component's answer must suppress the app shortcut"
            );
            assert!(
                !captures_event(input, false, false, false, true),
                "an unrelated pointer capture must not swallow a key focus declined"
            );
        }
    }

    #[kithara::test]
    fn answered_control_pointer_input_is_reported_captured_to_the_host() {
        assert!(captures_event(
            Some(pointer_input(PointerPhase::Down, None)),
            true,
            false,
            true,
            false,
        ));
        assert!(captures_event(
            Some(pointer_input(
                PointerPhase::Move,
                Some(Pt { x: 1.0, y: 1.0 }),
            )),
            true,
            false,
            true,
            false,
        ));
        assert!(captures_event(
            Some(pointer_input(PointerPhase::Up, None)),
            true,
            false,
            true,
            false,
        ));
    }

    #[kithara::test]
    fn unanswered_wheel_reaches_the_still_iced_tempo_surface_once() {
        let child = WheelSurface::builder().path("deck-a/tempo").build().view();
        let mut element = Element::new(Host {
            child,
            layout: HostedLayout::Control(None),
        });
        let renderer = headless_renderer();
        let viewport = Size::new(100.0, 40.0);
        let mut tree = Tree::new(element.as_widget());
        let node = element.as_widget_mut().layout(
            &mut tree,
            &renderer,
            &Limits::new(Size::ZERO, viewport),
        );
        let mut clipboard = clipboard::Null;
        let mut messages = Vec::new();
        let mut shell = Shell::new(&mut messages);

        element.as_widget_mut().update(
            &mut tree,
            &Event::Mouse(mouse::Event::WheelScrolled {
                delta: mouse::ScrollDelta::Lines { x: 0.0, y: -1.0 },
            }),
            Layout::new(&node),
            Cursor::Available(Point::new(50.0, 20.0)),
            &renderer,
            &mut clipboard,
            &mut shell,
            &Rectangle::with_size(viewport),
        );
        assert!(
            shell.is_event_captured(),
            "the still-iced tempo surface owns its wheel detent"
        );
        drop(shell);

        assert_eq!(
            messages,
            [UiEvent::Control {
                path: "deck-a/tempo".to_owned(),
                action: ControlAction::StepScalar(1.0),
            }],
            "the unanswered detent must reach the child exactly once"
        );
    }

    #[kithara::test]
    fn tree_boundary_passes_downward_wheel_to_the_iced_surface_but_keeps_upward_wheel() {
        let ui = compiled_tree_surface();
        let reads = FixtureReads::default();
        let CompiledNode::Module {
            instance,
            module,
            root,
            ..
        } = &ui.root
        else {
            panic!("tree surface fixture root must be a module");
        };
        assert_eq!(ui.resolve(*module), "gallery-tree-tab");

        let child = super::super::node::render_engine_node(
            root,
            &[],
            *instance,
            ctx(&ui, &reads),
            builtin::skin(),
        );
        let mut element = host(child, root, ctx(&ui, &reads), builtin::skin());
        let renderer = headless_renderer();
        let viewport = Size::new(232.0, 120.0);
        let mut tree = Tree::new(element.as_widget());
        let node = element.as_widget_mut().layout(
            &mut tree,
            &renderer,
            &Limits::new(Size::ZERO, viewport),
        );
        let hosted = HostedLayout::new(root, ctx(&ui, &reads), builtin::skin());
        let descriptors = hosted.descriptors();
        assert!(matches!(
            descriptors.as_slice(),
            [
                Descriptor::TextInput {
                    path: search_path,
                    query,
                    ..
                },
                Descriptor::Scroll {
                    path,
                    config,
                },
            ] if search_path == "tree/browser/search"
                && query.is_empty()
                && path == "tree/browser"
                && *config == ScrollConfig::items(
                    ScrollAxis::Vertical,
                    192.0,
                    8,
                    24.0,
                    24.0,
                    6.0,
                )
        ));
        let targets = hosted.targets(Layout::new(&node), Cursor::Unavailable);
        let [search, target] = targets.as_slice() else {
            panic!("the tree document must expose its search input and scroll viewport");
        };
        assert_eq!(search.path, "tree/browser/search");
        assert_eq!(target.path, "tree/browser");
        let area = target.hit.area();
        let cursor = Cursor::Available(Point::new(area.x + area.w / 2.0, area.y + area.h / 2.0));
        let mut clipboard = clipboard::Null;
        let mut messages = Vec::new();

        let mut shell = Shell::new(&mut messages);
        element.as_widget_mut().update(
            &mut tree,
            &Event::Mouse(mouse::Event::WheelScrolled {
                delta: mouse::ScrollDelta::Pixels {
                    x: 0.0,
                    y: -1_000.0,
                },
            }),
            Layout::new(&node),
            cursor,
            &renderer,
            &mut clipboard,
            &mut shell,
            &Rectangle::with_size(viewport),
        );
        assert!(shell.is_event_captured());
        drop(shell);
        let bottom = tree
            .state
            .downcast_ref::<State>()
            .engine
            .scroll_offset("tree/browser")
            .unwrap_or_else(|| panic!("the retained tree must own an offset"));
        assert!(bottom > 0.0);
        assert!(
            messages.is_empty(),
            "engine-owned scrolling emits no UiEvent"
        );

        let mut shell = Shell::new(&mut messages);
        element.as_widget_mut().update(
            &mut tree,
            &Event::Mouse(mouse::Event::WheelScrolled {
                delta: mouse::ScrollDelta::Lines { x: 0.0, y: -1.0 },
            }),
            Layout::new(&node),
            cursor,
            &renderer,
            &mut clipboard,
            &mut shell,
            &Rectangle::with_size(viewport),
        );
        assert!(shell.is_event_captured());
        drop(shell);
        assert_eq!(
            messages,
            [UiEvent::Control {
                path: "tree/surface".to_owned(),
                action: ControlAction::StepScalar(1.0),
            }],
            "a downward wheel at the tree boundary must continue to the iced ancestor"
        );

        let mut shell = Shell::new(&mut messages);
        element.as_widget_mut().update(
            &mut tree,
            &Event::Mouse(mouse::Event::WheelScrolled {
                delta: mouse::ScrollDelta::Lines { x: 0.0, y: 1.0 },
            }),
            Layout::new(&node),
            cursor,
            &renderer,
            &mut clipboard,
            &mut shell,
            &Rectangle::with_size(viewport),
        );
        assert!(shell.is_event_captured());
        drop(shell);
        assert_eq!(
            messages.len(),
            1,
            "the movable tree must keep the upward wheel"
        );
        assert!(
            tree.state
                .downcast_ref::<State>()
                .engine
                .scroll_offset("tree/browser")
                .is_some_and(|offset| offset < bottom)
        );

        let offset = tree
            .state
            .downcast_ref::<State>()
            .engine
            .scroll_offset("tree/browser")
            .unwrap_or_else(|| panic!("the retained offset must survive the upward wheel"));
        let expected = ((offset + 1.0) / builtin::skin().tree.row_height)
            .floor()
            .as_();
        let row_cursor = Cursor::Available(Point::new(area.x + 20.0, area.y + 1.0));
        let mut shell = Shell::new(&mut messages);
        element.as_widget_mut().update(
            &mut tree,
            &Event::Mouse(mouse::Event::ButtonPressed(Button::Left)),
            Layout::new(&node),
            row_cursor,
            &renderer,
            &mut clipboard,
            &mut shell,
            &Rectangle::with_size(viewport),
        );
        drop(shell);
        assert_eq!(
            messages.last(),
            Some(&UiEvent::Control {
                path: "tree/browser".to_owned(),
                action: ControlAction::SelectIndex(expected),
            }),
            "row activation must keep the existing SelectIndex emission"
        );
    }

    #[kithara::test]
    fn compiled_tree_surface_installs_the_retained_host() {
        let ui = compiled_tree_surface();
        let reads = FixtureReads::default();
        let element =
            super::super::node::render_compiled(&ui.root, ctx(&ui, &reads), builtin::skin());
        let tree = Tree::new(element.as_widget());

        fn retained_hosts(tree: &Tree) -> usize {
            usize::from(tree.tag == Tag::of::<State>())
                + tree.children.iter().map(retained_hosts).sum::<usize>()
        }

        fn retained_state(tree: &Tree) -> Option<&State> {
            if tree.tag == Tag::of::<State>() {
                return Some(tree.state.downcast_ref::<State>());
            }
            tree.children.iter().find_map(retained_state)
        }

        assert_eq!(retained_hosts(&tree), 1);
        assert!(
            retained_state(&tree)
                .and_then(|state| state.engine.scroll_offset("tree/browser"))
                .is_some()
        );
    }

    #[kithara::test]
    fn engine_cursor_wins_and_none_falls_back_to_the_child() {
        let renderer = headless_renderer();
        let viewport = Size::new(100.0, 40.0);
        let cursor = Cursor::Available(Point::new(50.0, 20.0));
        let bounds = Rectangle::with_size(viewport);

        for (layout, expected) in [
            (
                HostedLayout::Control(Some(HostedControl::mounted(
                    crate::render::HostedControlPlan::Activation {
                        path: "hosted/button".to_owned(),
                    },
                    builtin::skin(),
                ))),
                mouse::Interaction::Pointer,
            ),
            (
                HostedLayout::Control(None),
                mouse::Interaction::ResizingHorizontally,
            ),
        ] {
            let child = container(
                mouse_area(Space::new().width(Length::Fill).height(Length::Fill))
                    .interaction(mouse::Interaction::ResizingHorizontally),
            )
            .width(Length::Fill)
            .height(Length::Fill)
            .into();
            let mut element = Element::new(Host { child, layout });
            let mut tree = Tree::new(element.as_widget());
            let node = element.as_widget_mut().layout(
                &mut tree,
                &renderer,
                &Limits::new(Size::ZERO, viewport),
            );

            assert_eq!(
                element.as_widget().mouse_interaction(
                    &tree,
                    Layout::new(&node),
                    cursor,
                    &bounds,
                    &renderer,
                ),
                expected,
            );
        }
    }

    #[kithara::test]
    fn the_meter_publishes_the_seeked_value_under_its_own_path() {
        let path = "mixer/deck-a/volume";
        let bounds = Rectangle::new(Point::new(0.0, 10.0), Size::new(12.0, 40.0));
        let cursor = Cursor::Available(Point::new(6.0, 30.0));
        let press = Event::Mouse(mouse::Event::ButtonPressed(Button::Left));
        let mut engine = Engine::default();
        engine.reconcile([Descriptor::vertical_vu(path.to_owned())]);

        let input = iced_interact::input(&press)
            .unwrap_or_else(|| panic!("a left press must become portable input"));
        let target = Target::new(path, iced_interact::hit(bounds, cursor));
        let emission = engine
            .handle(input, &[target], Instant::now())
            .unwrap_or_else(|| panic!("a press on the meter must publish"));
        let action = engine_event(&emission.path, emission.child, emission.outcome)
            .unwrap_or_else(|| panic!("the published value must cross the iced boundary"));

        assert_eq!(
            action.into_inner().0,
            Some(UiEvent::Control {
                path: path.to_owned(),
                action: ControlAction::SetScalar(0.5),
            })
        );
    }

    #[kithara::test]
    fn gallery_faders_host_their_exact_input_surfaces() {
        let ui = compiled_gallery_faders();
        let reads = FixtureReads::default();
        let CompiledNode::Module {
            instance,
            module,
            root,
            ..
        } = &ui.root
        else {
            panic!("gallery faders fixture root must be a module");
        };

        assert_eq!(ui.resolve(*module), "gallery-faders-tab");
        let mut components = Vec::new();
        claimed_components(root, &mut components);
        assert_eq!(components, ["fader", "fader", "vertical-vu"]);

        let full = super::super::node::render_compiled(&ui.root, ctx(&ui, &reads), builtin::skin());
        let full_tree = Tree::new(full.as_widget());
        assert_eq!(host_count(&full_tree), 1, "the faders page owns one engine");

        let renderer = headless_renderer();
        let viewport = Size::new(320.0, 500.0);
        let child = super::super::node::render_engine_node(
            root,
            &[],
            *instance,
            ctx(&ui, &reads),
            builtin::skin(),
        );
        let mut element = host(child, root, ctx(&ui, &reads), builtin::skin());
        let mut tree = Tree::new(element.as_widget());
        let node = element.as_widget_mut().layout(
            &mut tree,
            &renderer,
            &Limits::new(Size::ZERO, viewport),
        );
        let hosted = HostedLayout::new(root, ctx(&ui, &reads), builtin::skin());
        let descriptors = hosted.descriptors();
        assert_eq!(
            descriptors.iter().map(descriptor_path).collect::<Vec<_>>(),
            ["faders/default", "faders/volume", "faders/vertical"]
        );
        let [
            Descriptor::Fader {
                drag_step: default_step,
                ..
            },
            Descriptor::Fader {
                drag_step: volume_step,
                ..
            },
            Descriptor::VerticalVu { .. },
        ] = descriptors.as_slice()
        else {
            panic!("the hosted fader descriptors must keep one shared kind");
        };
        assert_eq!(*default_step, Some(builtin::skin().fader.step));
        assert_eq!(*volume_step, None);

        let targets = hosted.targets(Layout::new(&node), Cursor::Unavailable);
        assert_eq!(
            targets.iter().map(|target| target.path).collect::<Vec<_>>(),
            ["faders/default", "faders/volume", "faders/vertical"]
        );
        let area = |path: &str| {
            targets
                .iter()
                .find(|target| target.path == path)
                .unwrap_or_else(|| panic!("the hosted `{path}` target must exist"))
                .hit
                .area()
        };
        let default = area("faders/default");
        let volume = area("faders/volume");
        let vertical = area("faders/vertical");
        assert_eq!(default.h, 16.0);
        assert_eq!(volume.h, 14.0);
        assert_eq!((vertical.w, vertical.h), (18.0, 120.0));

        let speaker = Cursor::Available(Point::new(
            volume.x - builtin::skin().fader.content_gap - builtin::skin().fader.icon_width / 2.0,
            volume.y + volume.h / 2.0,
        ));
        let mut clipboard = clipboard::Null;
        let mut messages = Vec::new();
        let mut shell = Shell::new(&mut messages);
        element.as_widget_mut().update(
            &mut tree,
            &Event::Mouse(mouse::Event::ButtonPressed(Button::Left)),
            Layout::new(&node),
            speaker,
            &renderer,
            &mut clipboard,
            &mut shell,
            &Rectangle::with_size(viewport),
        );
        assert!(!shell.is_event_captured());
        drop(shell);
        assert!(
            messages.is_empty(),
            "the Volume speaker is outside the fader input surface"
        );

        let cursor = Cursor::Available(Point::new(
            default.x + default.w / 2.0,
            default.y + default.h / 2.0,
        ));
        let mut shell = Shell::new(&mut messages);
        element.as_widget_mut().update(
            &mut tree,
            &Event::Mouse(mouse::Event::ButtonPressed(Button::Left)),
            Layout::new(&node),
            cursor,
            &renderer,
            &mut clipboard,
            &mut shell,
            &Rectangle::with_size(viewport),
        );
        assert!(shell.is_event_captured());
        drop(shell);

        assert_eq!(
            messages,
            [UiEvent::Control {
                path: "faders/default".to_owned(),
                action: ControlAction::SetScalar(0.5),
            }]
        );
    }

    #[kithara::test]
    fn gallery_meters_is_explicitly_hosted_and_routes_the_stereo_gesture() {
        let ui = compiled_gallery_meters();
        let reads = FixtureReads::default();
        let CompiledNode::Module { instance, root, .. } = &ui.root else {
            panic!("gallery fixture root must be a module");
        };
        let ExpandedNode::Column { children, .. } = root.as_ref() else {
            panic!("gallery atoms root must be a column");
        };
        let meters = &children[1];

        assert!(ui.includes_module(*instance, &[1], "gallery-meters"));
        let mut components = Vec::new();
        claimed_components(meters, &mut components);
        assert_eq!(components, ["stereo-meter", "vertical-vu", "vertical-vu"]);

        let full = super::super::node::render_compiled(&ui.root, ctx(&ui, &reads), builtin::skin());
        let full_tree = Tree::new(full.as_widget());
        assert_eq!(
            host_count(&full_tree),
            1,
            "only the meter include owns an engine"
        );

        let renderer = headless_renderer();
        let viewport = Size::new(160.0, 180.0);
        let child = super::super::node::render_engine_node(
            meters,
            &[1],
            *instance,
            ctx(&ui, &reads),
            builtin::skin(),
        );
        let mut element = host(child, meters, ctx(&ui, &reads), builtin::skin());
        let mut tree = Tree::new(element.as_widget());
        let node = element.as_widget_mut().layout(
            &mut tree,
            &renderer,
            &Limits::new(Size::ZERO, viewport),
        );
        let hosted = HostedLayout::new(meters, ctx(&ui, &reads), builtin::skin());
        let targets = hosted.targets(Layout::new(&node), Cursor::Unavailable);
        assert_eq!(
            targets.iter().map(|target| target.path).collect::<Vec<_>>(),
            [
                "atoms/meters/stereo",
                "atoms/meters/vertical-120",
                "atoms/meters/vertical-64",
            ]
        );
        let stereo = targets
            .iter()
            .find(|target| target.path == "atoms/meters/stereo")
            .unwrap_or_else(|| panic!("the hosted stereo meter target must exist"));
        let area = stereo.hit.area();
        assert_eq!((area.w, area.h), (64.0, 22.0));
        let expected_path = stereo.path.to_owned();
        let cursor = Cursor::Available(Point::new(area.x + area.w * 0.25, area.y + area.h / 2.0));
        let mut clipboard = clipboard::Null;
        let mut messages = Vec::new();
        let mut shell = Shell::new(&mut messages);
        element.as_widget_mut().update(
            &mut tree,
            &Event::Mouse(mouse::Event::ButtonPressed(Button::Left)),
            Layout::new(&node),
            cursor,
            &renderer,
            &mut clipboard,
            &mut shell,
            &Rectangle::with_size(viewport),
        );
        assert!(shell.is_event_captured());
        drop(shell);

        assert_eq!(messages.len(), 1);
        let UiEvent::Control { path, action } = &messages[0] else {
            panic!("the hosted stereo meter must publish a control event");
        };
        assert_eq!(path, &expected_path);
        assert_eq!(action, &ControlAction::SetScalar(0.25));
    }

    #[kithara::test]
    fn gallery_toggles_route_activation_without_retaining_capture() {
        let ui = compiled_gallery_toggles();
        let reads = FixtureReads::default();
        let CompiledNode::Module { instance, root, .. } = &ui.root else {
            panic!("gallery fixture root must be a module");
        };
        let ExpandedNode::Column { children, .. } = root.as_ref() else {
            panic!("gallery atoms root must be a column");
        };
        let toggles = &children[1];

        assert!(ui.includes_module(*instance, &[1], "gallery-toggles"));
        let mut components = Vec::new();
        claimed_components(toggles, &mut components);
        assert_eq!(components, ["activation"; 4]);

        let full = super::super::node::render_compiled(&ui.root, ctx(&ui, &reads), builtin::skin());
        let full_tree = Tree::new(full.as_widget());
        assert_eq!(
            host_count(&full_tree),
            1,
            "only the toggles include owns an engine"
        );

        let renderer = headless_renderer();
        let viewport = Size::new(200.0, 100.0);
        let child = super::super::node::render_engine_node(
            toggles,
            &[1],
            *instance,
            ctx(&ui, &reads),
            builtin::skin(),
        );
        let mut element = host(child, toggles, ctx(&ui, &reads), builtin::skin());
        let mut tree = Tree::new(element.as_widget());
        let node = element.as_widget_mut().layout(
            &mut tree,
            &renderer,
            &Limits::new(Size::ZERO, viewport),
        );
        let hosted = HostedLayout::new(toggles, ctx(&ui, &reads), builtin::skin());
        let targets = hosted.targets(Layout::new(&node), Cursor::Unavailable);
        let mut clipboard = clipboard::Null;
        let mut messages = Vec::new();

        for expected_path in ["atoms/toggles/toggle-on", "atoms/toggles/checkbox-on"] {
            let target = targets
                .iter()
                .find(|target| target.path == expected_path)
                .unwrap_or_else(|| panic!("the hosted `{expected_path}` target must exist"));
            let area = target.hit.area();
            let cursor =
                Cursor::Available(Point::new(area.x + area.w / 2.0, area.y + area.h / 2.0));
            let mut shell = Shell::new(&mut messages);
            element.as_widget_mut().update(
                &mut tree,
                &Event::Mouse(mouse::Event::ButtonPressed(Button::Left)),
                Layout::new(&node),
                cursor,
                &renderer,
                &mut clipboard,
                &mut shell,
                &Rectangle::with_size(viewport),
            );
            assert!(
                !shell.is_event_captured(),
                "activation publishes without retaining the engine capture slot"
            );
        }

        assert_eq!(
            messages,
            [
                UiEvent::Control {
                    path: "atoms/toggles/toggle-on".to_owned(),
                    action: ControlAction::Activate,
                },
                UiEvent::Control {
                    path: "atoms/toggles/checkbox-on".to_owned(),
                    action: ControlAction::Activate,
                },
            ]
        );
    }

    #[kithara::test]
    fn gallery_chips_host_their_exact_activation_inventory() {
        let ui = compiled_gallery_chips();
        let reads = FixtureReads::default();
        let CompiledNode::Module { instance, root, .. } = &ui.root else {
            panic!("gallery fixture root must be a module");
        };
        let ExpandedNode::Column { children, .. } = root.as_ref() else {
            panic!("gallery atoms root must be a column");
        };
        let chips = &children[1];

        assert!(ui.includes_module(*instance, &[1], "gallery-chips"));
        let mut components = Vec::new();
        claimed_components(chips, &mut components);
        assert_eq!(components, ["activation"; 2]);

        let full = super::super::node::render_compiled(&ui.root, ctx(&ui, &reads), builtin::skin());
        let full_tree = Tree::new(full.as_widget());
        assert_eq!(
            host_count(&full_tree),
            1,
            "only the chips include owns an engine"
        );

        let renderer = headless_renderer();
        let viewport = Size::new(100.0, 80.0);
        let child = super::super::node::render_engine_node(
            chips,
            &[1],
            *instance,
            ctx(&ui, &reads),
            builtin::skin(),
        );
        let mut element = host(child, chips, ctx(&ui, &reads), builtin::skin());
        let mut tree = Tree::new(element.as_widget());
        let node = element.as_widget_mut().layout(
            &mut tree,
            &renderer,
            &Limits::new(Size::ZERO, viewport),
        );
        let hosted = HostedLayout::new(chips, ctx(&ui, &reads), builtin::skin());
        let descriptors = hosted.descriptors();
        assert_eq!(
            descriptors.iter().map(descriptor_path).collect::<Vec<_>>(),
            ["atoms/chips/active", "atoms/chips/inactive"]
        );
        assert!(
            descriptors
                .iter()
                .all(|descriptor| matches!(descriptor, Descriptor::Activation { .. }))
        );

        let targets = hosted.targets(Layout::new(&node), Cursor::Unavailable);
        assert_eq!(
            targets.iter().map(|target| target.path).collect::<Vec<_>>(),
            ["atoms/chips/active", "atoms/chips/inactive"]
        );
        let mut clipboard = clipboard::Null;
        let mut messages = Vec::new();
        for expected_path in ["atoms/chips/active", "atoms/chips/inactive"] {
            let target = targets
                .iter()
                .find(|target| target.path == expected_path)
                .unwrap_or_else(|| panic!("the hosted `{expected_path}` target must exist"));
            let area = target.hit.area();
            let cursor =
                Cursor::Available(Point::new(area.x + area.w / 2.0, area.y + area.h / 2.0));
            let mut shell = Shell::new(&mut messages);
            element.as_widget_mut().update(
                &mut tree,
                &Event::Mouse(mouse::Event::ButtonPressed(Button::Left)),
                Layout::new(&node),
                cursor,
                &renderer,
                &mut clipboard,
                &mut shell,
                &Rectangle::with_size(viewport),
            );
            assert!(
                !shell.is_event_captured(),
                "activation publishes without retaining the engine capture slot"
            );
        }

        assert_eq!(
            messages,
            [
                UiEvent::Control {
                    path: "atoms/chips/active".to_owned(),
                    action: ControlAction::Activate,
                },
                UiEvent::Control {
                    path: "atoms/chips/inactive".to_owned(),
                    action: ControlAction::Activate,
                },
            ]
        );
    }

    #[kithara::test]
    fn gallery_buttons_share_the_host_activation_component() {
        let ui = compiled_gallery_buttons();
        let reads = FixtureReads::default();
        let CompiledNode::Module {
            instance,
            module,
            root,
            ..
        } = &ui.root
        else {
            panic!("gallery buttons fixture root must be a module");
        };

        assert_eq!(ui.resolve(*module), "gallery-buttons-tab");
        let mut components = Vec::new();
        claimed_components(root, &mut components);
        assert_eq!(components, ["activation"; 6]);

        let full = super::super::node::render_compiled(&ui.root, ctx(&ui, &reads), builtin::skin());
        let full_tree = Tree::new(full.as_widget());
        assert_eq!(
            host_count(&full_tree),
            1,
            "the buttons page owns one engine"
        );

        let renderer = headless_renderer();
        let viewport = Size::new(320.0, 160.0);
        let child = super::super::node::render_engine_node(
            root,
            &[],
            *instance,
            ctx(&ui, &reads),
            builtin::skin(),
        );
        let mut element = host(child, root, ctx(&ui, &reads), builtin::skin());
        let mut tree = Tree::new(element.as_widget());
        let node = element.as_widget_mut().layout(
            &mut tree,
            &renderer,
            &Limits::new(Size::ZERO, viewport),
        );
        let hosted = HostedLayout::new(root, ctx(&ui, &reads), builtin::skin());
        let descriptors = hosted.descriptors();
        assert_eq!(
            descriptors.iter().map(descriptor_path).collect::<Vec<_>>(),
            [
                "buttons/play",
                "buttons/cue",
                "buttons/sync",
                "buttons/default",
                "buttons/primary",
                "buttons/micro",
            ]
        );
        assert!(
            descriptors
                .iter()
                .all(|descriptor| matches!(descriptor, Descriptor::Activation { .. }))
        );

        let targets = hosted.targets(Layout::new(&node), Cursor::Unavailable);
        let mut clipboard = clipboard::Null;
        let mut messages = Vec::new();
        let center = |path: &str| {
            let target = targets
                .iter()
                .find(|target| target.path == path)
                .unwrap_or_else(|| panic!("the hosted `{path}` target must exist"));
            let area = target.hit.area();
            Point::new(area.x + area.w / 2.0, area.y + area.h / 2.0)
        };
        let cue = center("buttons/cue");
        let state = tree.state.downcast_mut::<State>();
        state.last_hovered_control = Some("buttons/play".to_owned());
        state.last_mouse_interaction = Some(mouse::Interaction::Pointer);
        let mut shell = Shell::new(&mut messages);
        element.as_widget_mut().update(
            &mut tree,
            &Event::Mouse(mouse::Event::CursorMoved { position: cue }),
            Layout::new(&node),
            Cursor::Available(cue),
            &renderer,
            &mut clipboard,
            &mut shell,
            &Rectangle::with_size(viewport),
        );
        assert_eq!(
            shell.redraw_request(),
            window::RedrawRequest::NextFrame,
            "moving between adjacent activation controls must repaint both hover states"
        );
        drop(shell);

        for expected_path in ["buttons/play", "buttons/default"] {
            let target = targets
                .iter()
                .find(|target| target.path == expected_path)
                .unwrap_or_else(|| panic!("the hosted `{expected_path}` target must exist"));
            let area = target.hit.area();
            let cursor =
                Cursor::Available(Point::new(area.x + area.w / 2.0, area.y + area.h / 2.0));
            let mut shell = Shell::new(&mut messages);
            element.as_widget_mut().update(
                &mut tree,
                &Event::Mouse(mouse::Event::ButtonPressed(Button::Left)),
                Layout::new(&node),
                cursor,
                &renderer,
                &mut clipboard,
                &mut shell,
                &Rectangle::with_size(viewport),
            );
            assert!(
                !shell.is_event_captured(),
                "the hosted `{expected_path}` press in {area:?} publishes without retaining the \
                 engine capture slot"
            );
        }

        assert_eq!(
            messages,
            [
                UiEvent::Control {
                    path: "buttons/play".to_owned(),
                    action: ControlAction::Activate,
                },
                UiEvent::Control {
                    path: "buttons/default".to_owned(),
                    action: ControlAction::Activate,
                },
            ],
            "a button with no read endpoint must still have the same activation contract"
        );
    }

    #[kithara::test]
    fn gallery_cells_hosts_its_exact_engine_control_inventory() {
        let ui = compiled_gallery_cells();
        let reads = FixtureReads::default();
        let CompiledNode::Module {
            instance,
            module,
            root,
            ..
        } = &ui.root
        else {
            panic!("gallery cells fixture root must be a module");
        };

        assert_eq!(ui.resolve(*module), "gallery-cells-tab");
        let mut components = Vec::new();
        claimed_components(root, &mut components);
        assert_eq!(
            components,
            [
                "activation",
                "activation",
                "activation",
                "activation",
                "activation",
                "activation",
                "segmented",
                "activation",
                "activation",
                "activation",
                "activation",
            ]
        );

        let full = super::super::node::render_compiled(&ui.root, ctx(&ui, &reads), builtin::skin());
        let full_tree = Tree::new(full.as_widget());
        assert_eq!(host_count(&full_tree), 1, "the cells page owns one engine");

        let renderer = headless_renderer();
        let viewport = Size::new(1_000.0, 400.0);
        let child = super::super::node::render_engine_node(
            root,
            &[],
            *instance,
            ctx(&ui, &reads),
            builtin::skin(),
        );
        let mut element = host(child, root, ctx(&ui, &reads), builtin::skin());
        let mut tree = Tree::new(element.as_widget());
        let node = element.as_widget_mut().layout(
            &mut tree,
            &renderer,
            &Limits::new(Size::ZERO, viewport),
        );
        let hosted = HostedLayout::new(root, ctx(&ui, &reads), builtin::skin());
        let descriptors = hosted.descriptors();
        assert_eq!(
            descriptors.iter().map(descriptor_path).collect::<Vec<_>>(),
            [
                "cells/cue",
                "cells/play",
                "cells/deck-b",
                "cells/deck-a",
                "cells/fx-1",
                "cells/fx-2",
                "cells/beat",
                "cells/toggle-off",
                "cells/toggle-on",
                "cells/checkbox-off",
                "cells/checkbox-on",
            ]
        );
        assert!(matches!(
            descriptors.as_slice(),
            [
                Descriptor::Activation { .. },
                Descriptor::Activation { .. },
                Descriptor::Activation { .. },
                Descriptor::Activation { .. },
                Descriptor::Activation { .. },
                Descriptor::Activation { .. },
                Descriptor::Segmented { item_count: 4, .. },
                Descriptor::Activation { .. },
                Descriptor::Activation { .. },
                Descriptor::Activation { .. },
                Descriptor::Activation { .. },
            ]
        ));

        let targets = hosted.targets(Layout::new(&node), Cursor::Unavailable);
        let target = targets
            .iter()
            .find(|target| target.path == "cells/beat")
            .unwrap_or_else(|| panic!("the hosted segmented target must exist"));
        let area = target.hit.area();
        assert_eq!((area.w, area.h), (220.0, 26.0));
        let cursor = Cursor::Available(Point::new(area.x + area.w * 0.625, area.y + area.h / 2.0));
        let mut clipboard = clipboard::Null;
        let mut messages = Vec::new();
        let mut shell = Shell::new(&mut messages);
        element.as_widget_mut().update(
            &mut tree,
            &Event::Mouse(mouse::Event::ButtonPressed(Button::Left)),
            Layout::new(&node),
            cursor,
            &renderer,
            &mut clipboard,
            &mut shell,
            &Rectangle::with_size(viewport),
        );
        assert!(!shell.is_event_captured());
        drop(shell);

        assert_eq!(
            messages,
            [UiEvent::Control {
                path: "cells/beat".to_owned(),
                action: ControlAction::SelectIndex(2),
            }]
        );
    }

    #[kithara::test]
    fn gallery_table_hosts_its_exact_conditional_inventory() {
        let ui = compiled_gallery_table();
        let reads = FixtureReads::default();
        let CompiledNode::Module {
            instance,
            module,
            root,
            ..
        } = &ui.root
        else {
            panic!("gallery track-list fixture root must be a module");
        };

        assert_eq!(ui.resolve(*module), "gallery-table-tab");
        let mut components = Vec::new();
        claimed_components(root, &mut components);
        assert_eq!(
            components,
            [
                "segmented",
                "track-list",
                "activation",
                "activation",
                "activation",
                "activation",
                "activation",
                "activation",
                "activation",
                "activation",
                "activation",
                "activation",
            ]
        );

        let full = super::super::node::render_compiled(&ui.root, ctx(&ui, &reads), builtin::skin());
        let full_tree = Tree::new(full.as_widget());
        assert_eq!(
            host_count(&full_tree),
            1,
            "the track-list page owns one engine"
        );

        let renderer = headless_renderer();
        let narrow = Size::new(1_000.0, 640.0);
        let child = super::super::node::render_engine_node(
            root,
            &[],
            *instance,
            ctx(&ui, &reads),
            builtin::skin(),
        );
        let mut element = host(child, root, ctx(&ui, &reads), builtin::skin());
        let mut tree = Tree::new(element.as_widget());
        let node =
            element
                .as_widget_mut()
                .layout(&mut tree, &renderer, &Limits::new(Size::ZERO, narrow));
        let hosted = HostedLayout::new(root, ctx(&ui, &reads), builtin::skin());
        let targets = hosted.targets(Layout::new(&node), Cursor::Unavailable);
        assert_eq!(
            targets
                .iter()
                .filter(|target| target.path.starts_with("table/table"))
                .map(|target| target.path)
                .collect::<Vec<_>>(),
            [
                "table/table/scroll-x",
                "table/table",
                "table/table/rows",
                "table/table/width/index",
                "table/table/width/deck",
                "table/table/width/artist",
                "table/table/width/bpm",
                "table/table/width/key",
                "table/table/width/time",
            ]
        );
        let descriptors = active_descriptors(&hosted, &targets);
        assert_eq!(
            descriptors.iter().map(descriptor_path).collect::<Vec<_>>(),
            [
                "table/column-preset",
                "table/table/scroll-x",
                "table/table",
                "table/table/rows",
                "table/table/width/index",
                "table/table/width/deck",
                "table/table/width/artist",
                "table/table/width/bpm",
                "table/table/width/key",
                "table/table/width/time",
                "table/table/width/energy",
                "table/column-index",
                "table/column-deck",
                "table/column-title",
                "table/column-artist",
                "table/column-bpm",
                "table/column-key",
                "table/column-time",
                "table/column-energy",
                "table/column-transition",
                "table/reset-columns",
            ]
        );
        assert!(matches!(
            descriptors.as_slice(),
            [
                Descriptor::Segmented { item_count: 3, .. },
                Descriptor::Scroll { config: horizontal, .. },
                Descriptor::Scroll { config: vertical, .. },
                Descriptor::Item { .. },
                Descriptor::ColumnDivider { .. },
                Descriptor::ColumnDivider { .. },
                Descriptor::ColumnDivider { .. },
                Descriptor::ColumnDivider { .. },
                Descriptor::ColumnDivider { .. },
                Descriptor::ColumnDivider { .. },
                Descriptor::ColumnDivider { .. },
                Descriptor::Activation { .. },
                Descriptor::Activation { .. },
                Descriptor::Activation { .. },
                Descriptor::Activation { .. },
                Descriptor::Activation { .. },
                Descriptor::Activation { .. },
                Descriptor::Activation { .. },
                Descriptor::Activation { .. },
                Descriptor::Activation { .. },
                Descriptor::Activation { .. },
            ] if horizontal.axis() == ScrollAxis::Horizontal
                && vertical.axis() == ScrollAxis::Vertical
        ));
        let divider_targets: Vec<_> = targets
            .iter()
            .filter(|target| target.path.contains("/width/"))
            .collect();
        assert_eq!(divider_targets.len(), 6);
        assert!(
            divider_targets
                .iter()
                .all(|target| { target.hit.area().w == builtin::skin().table.divider_hit_width })
        );
        let viewport = targets
            .iter()
            .find(|target| target.path == "table/table/scroll-x")
            .map_or_else(
                || panic!("the narrow table must expose its horizontal viewport"),
                |target| target.hit.area(),
            );
        assert!(divider_targets.iter().all(|target| {
            let hit = target.hit.area();
            hit.x >= viewport.x && hit.x + hit.w <= viewport.x + viewport.w
        }));

        let wide = Size::new(1_200.0, 640.0);
        let mut child = super::super::node::render_engine_node(
            root,
            &[],
            *instance,
            ctx(&ui, &reads),
            builtin::skin(),
        );
        let mut child_tree = Tree::new(child.as_widget());
        let wide_node = child.as_widget_mut().layout(
            &mut child_tree,
            &renderer,
            &Limits::new(Size::ZERO, wide),
        );
        let wide_targets = hosted.targets(Layout::new(&wide_node), Cursor::Unavailable);
        assert_eq!(
            wide_targets
                .iter()
                .filter(|target| target.path.starts_with("table/table"))
                .map(|target| target.path)
                .collect::<Vec<_>>(),
            [
                "table/table",
                "table/table/rows",
                "table/table/width/index",
                "table/table/width/deck",
                "table/table/width/artist",
                "table/table/width/bpm",
                "table/table/width/key",
                "table/table/width/time",
                "table/table/width/energy",
            ]
        );
        assert!(
            wide_targets
                .iter()
                .all(|target| target.path != "table/table/scroll-x")
        );
        assert!(
            active_descriptors(&hosted, &wide_targets)
                .iter()
                .all(|descriptor| descriptor_path(descriptor) != "table/table/scroll-x")
        );
    }

    #[kithara::test]
    fn gallery_library_hosts_the_picker_and_its_exact_descriptor_inventory() {
        let ui = compiled_gallery_library();
        let reads = FixtureReads::default();
        let CompiledNode::Module {
            instance,
            module,
            root,
            ..
        } = &ui.root
        else {
            panic!("gallery library fixture root must be a module");
        };

        assert_eq!(ui.resolve(*module), "gallery-library2-tab");
        let mut components = Vec::new();
        claimed_components(root, &mut components);
        assert_eq!(components, ["text-input", "scroll", "picker", "track-list"]);

        let full = super::super::node::render_compiled(&ui.root, ctx(&ui, &reads), builtin::skin());
        let full_tree = Tree::new(full.as_widget());
        assert_eq!(
            host_count(&full_tree),
            1,
            "the library page owns one engine"
        );

        let renderer = headless_renderer();
        let viewport = Size::new(900.0, 600.0);
        let child = super::super::node::render_engine_node(
            root,
            &[],
            *instance,
            ctx(&ui, &reads),
            builtin::skin(),
        );
        let mut element = host(child, root, ctx(&ui, &reads), builtin::skin());
        let mut tree = Tree::new(element.as_widget());
        let node = element.as_widget_mut().layout(
            &mut tree,
            &renderer,
            &Limits::new(Size::ZERO, viewport),
        );
        let hosted = HostedLayout::new(root, ctx(&ui, &reads), builtin::skin());
        let descriptors = hosted.descriptors();
        assert_eq!(
            descriptors.iter().map(descriptor_path).collect::<Vec<_>>(),
            [
                "library2/browser/search",
                "library2/browser",
                "library2/context",
                "library2/table/scroll-x",
                "library2/table",
                "library2/table/rows",
                "library2/table/width/index",
                "library2/table/width/artist",
                "library2/table/width/bpm",
                "library2/table/width/key",
            ]
        );
        assert!(matches!(
            &descriptors[2],
            Descriptor::Picker {
                path,
                item_count: 2,
                selected: Some(0),
            } if path == "library2/context"
        ));

        let targets = hosted.targets(Layout::new(&node), Cursor::Unavailable);
        assert_eq!(
            targets.iter().map(|target| target.path).collect::<Vec<_>>(),
            [
                "library2/browser/search",
                "library2/browser",
                "library2/context",
                "library2/table",
                "library2/table/rows",
                "library2/table/width/index",
                "library2/table/width/artist",
                "library2/table/width/bpm",
                "library2/table/width/key",
            ]
        );
        let picker = targets
            .iter()
            .find(|target| target.path == "library2/context")
            .unwrap_or_else(|| panic!("the ContextBar picker target must exist"));
        assert_eq!(picker.hit.area().h, builtin::skin().tree.scope_item_height);
        assert_eq!(
            active_descriptors(&hosted, &targets).len(),
            9,
            "the non-overflowing table omits only its horizontal scroll descriptor"
        );

        let area = picker.hit.area();
        let picker_cursor =
            Cursor::Available(Point::new(area.x + area.w / 2.0, area.y + area.h / 2.0));
        let viewport_bounds = Rectangle::with_size(viewport);
        let mut clipboard = clipboard::Null;
        let mut messages = Vec::new();
        let search = targets[0].hit.area();
        let browser = targets[1].hit.area();
        let search_cursor = Cursor::Available(Point::new(
            search.x + search.w / 2.0,
            search.y + search.h / 2.0,
        ));
        let mut shell = Shell::new(&mut messages);
        element.as_widget_mut().update(
            &mut tree,
            &Event::Mouse(mouse::Event::ButtonPressed(Button::Left)),
            Layout::new(&node),
            search_cursor,
            &renderer,
            &mut clipboard,
            &mut shell,
            &viewport_bounds,
        );
        drop(shell);
        let mut shell = Shell::new(&mut messages);
        element.as_widget_mut().update(
            &mut tree,
            &Event::Mouse(mouse::Event::ButtonReleased(Button::Left)),
            Layout::new(&node),
            search_cursor,
            &renderer,
            &mut clipboard,
            &mut shell,
            &viewport_bounds,
        );
        drop(shell);
        let mut shell = Shell::new(&mut messages);
        element.as_widget_mut().update(
            &mut tree,
            &character_event("x", Code::KeyX),
            Layout::new(&node),
            Cursor::Unavailable,
            &renderer,
            &mut clipboard,
            &mut shell,
            &viewport_bounds,
        );
        drop(shell);
        assert_eq!(messages, [UiEvent::LibraryQuery("x".to_owned())]);
        messages.clear();

        let mut shell = Shell::new(&mut messages);
        element.as_widget_mut().update(
            &mut tree,
            &Event::Mouse(mouse::Event::ButtonPressed(Button::Left)),
            Layout::new(&node),
            picker_cursor,
            &renderer,
            &mut clipboard,
            &mut shell,
            &viewport_bounds,
        );
        assert!(
            shell.is_event_captured(),
            "the engine picker press must not click through its iced host"
        );
        drop(shell);
        assert_eq!(
            tree.state
                .downcast_ref::<State>()
                .engine
                .picker_snapshot("library2/context"),
            Some(PickerSnapshot {
                open: true,
                highlighted: Some(0),
            })
        );
        let mut shell = Shell::new(&mut messages);
        element.as_widget_mut().update(
            &mut tree,
            &character_event("y", Code::KeyY),
            Layout::new(&node),
            Cursor::Unavailable,
            &renderer,
            &mut clipboard,
            &mut shell,
            &viewport_bounds,
        );
        drop(shell);
        assert!(
            messages.is_empty(),
            "picker focus must keep ignored text away from the search document"
        );
        assert!(
            element
                .as_widget_mut()
                .overlay(
                    &mut tree,
                    Layout::new(&node),
                    &renderer,
                    &viewport_bounds,
                    Vector::ZERO,
                )
                .is_some(),
            "the open picker must escape through the host's iced overlay route"
        );

        let mut shell = Shell::new(&mut messages);
        element.as_widget_mut().update(
            &mut tree,
            &key_event(Named::ArrowDown, Code::ArrowDown),
            Layout::new(&node),
            Cursor::Unavailable,
            &renderer,
            &mut clipboard,
            &mut shell,
            &viewport_bounds,
        );
        assert!(shell.is_event_captured());
        drop(shell);
        assert_eq!(
            tree.state
                .downcast_ref::<State>()
                .engine
                .picker_snapshot("library2/context")
                .and_then(|snapshot| snapshot.highlighted),
            Some(1)
        );

        let mut shell = Shell::new(&mut messages);
        element.as_widget_mut().update(
            &mut tree,
            &key_event(Named::Enter, Code::Enter),
            Layout::new(&node),
            Cursor::Unavailable,
            &renderer,
            &mut clipboard,
            &mut shell,
            &viewport_bounds,
        );
        assert!(shell.is_event_captured());
        drop(shell);
        assert_eq!(
            messages,
            [UiEvent::Control {
                path: "library2/context".to_owned(),
                action: ControlAction::SelectIndex(1),
            }]
        );
        assert_eq!(
            tree.state
                .downcast_ref::<State>()
                .engine
                .picker_snapshot("library2/context")
                .map(|snapshot| snapshot.open),
            Some(false)
        );

        let mut shell = Shell::new(&mut messages);
        element.as_widget_mut().update(
            &mut tree,
            &key_release_event(Named::Enter, Code::Enter),
            Layout::new(&node),
            Cursor::Unavailable,
            &renderer,
            &mut clipboard,
            &mut shell,
            &viewport_bounds,
        );
        assert!(shell.is_event_captured());
        drop(shell);

        let mut shell = Shell::new(&mut messages);
        element.as_widget_mut().update(
            &mut tree,
            &key_event(Named::Enter, Code::Enter),
            Layout::new(&node),
            Cursor::Unavailable,
            &renderer,
            &mut clipboard,
            &mut shell,
            &viewport_bounds,
        );
        assert!(shell.is_event_captured());
        drop(shell);
        assert_eq!(
            tree.state
                .downcast_ref::<State>()
                .engine
                .picker_snapshot("library2/context")
                .map(|snapshot| snapshot.open),
            Some(true)
        );

        let mut shell = Shell::new(&mut messages);
        element.as_widget_mut().update(
            &mut tree,
            &key_event(Named::Enter, Code::Enter),
            Layout::new(&node),
            Cursor::Unavailable,
            &renderer,
            &mut clipboard,
            &mut shell,
            &viewport_bounds,
        );
        assert!(shell.is_event_captured());
        assert!(
            shell.is_layout_invalid(),
            "an inert captured repeat must rebuild iced's cached open overlay"
        );
        drop(shell);
        assert_eq!(
            tree.state
                .downcast_ref::<State>()
                .engine
                .picker_snapshot("library2/context")
                .map(|snapshot| snapshot.open),
            Some(true)
        );

        let mut shell = Shell::new(&mut messages);
        element.as_widget_mut().update(
            &mut tree,
            &key_event(Named::Escape, Code::Escape),
            Layout::new(&node),
            Cursor::Unavailable,
            &renderer,
            &mut clipboard,
            &mut shell,
            &viewport_bounds,
        );
        assert!(shell.is_event_captured());
        drop(shell);
        assert_eq!(
            tree.state
                .downcast_ref::<State>()
                .engine
                .picker_snapshot("library2/context")
                .map(|snapshot| snapshot.open),
            Some(false)
        );
        assert_eq!(
            messages.len(),
            1,
            "closing the picker without selection must publish nothing"
        );

        for (key, code) in [
            (Named::Delete, Code::Delete),
            (Named::Backspace, Code::Backspace),
        ] {
            let mut shell = Shell::new(&mut messages);
            element.as_widget_mut().update(
                &mut tree,
                &key_event(key, code),
                Layout::new(&node),
                Cursor::Unavailable,
                &renderer,
                &mut clipboard,
                &mut shell,
                &viewport_bounds,
            );
            assert!(
                shell.is_event_captured(),
                "the focused picker must suppress the app shortcut"
            );
        }

        let browser_cursor = Cursor::Available(Point::new(
            browser.x + browser.w / 2.0,
            browser.y + browser.h / 2.0,
        ));
        let mut shell = Shell::new(&mut messages);
        element.as_widget_mut().update(
            &mut tree,
            &Event::Mouse(mouse::Event::ButtonPressed(Button::Left)),
            Layout::new(&node),
            browser_cursor,
            &renderer,
            &mut clipboard,
            &mut shell,
            &viewport_bounds,
        );
        drop(shell);
        for (key, code) in [
            (Named::Delete, Code::Delete),
            (Named::Backspace, Code::Backspace),
        ] {
            let mut shell = Shell::new(&mut messages);
            element.as_widget_mut().update(
                &mut tree,
                &key_event(key, code),
                Layout::new(&node),
                Cursor::Unavailable,
                &renderer,
                &mut clipboard,
                &mut shell,
                &viewport_bounds,
            );
            assert!(
                !shell.is_event_captured(),
                "a key with engine focus elsewhere must reach the app as Ignored"
            );
        }
    }

    #[kithara::test]
    fn an_engine_picker_popup_captures_before_an_overlapping_window_layer() {
        let ui = compiled_gallery_library();
        let reads = FixtureReads::default();
        let CompiledNode::Module { instance, root, .. } = &ui.root else {
            panic!("gallery library fixture root must be a module");
        };
        let renderer = headless_renderer();
        let viewport = Size::new(900.0, 600.0);
        let hosted_layout = HostedLayout::new(root, ctx(&ui, &reads), builtin::skin());
        let child = super::super::node::render_engine_node(
            root,
            &[],
            *instance,
            ctx(&ui, &reads),
            builtin::skin(),
        );
        let hosted = host(child, root, ctx(&ui, &reads), builtin::skin());
        let area = Rc::new(Cell::new(Rect {
            h: 0.0,
            w: 0.0,
            x: 0.0,
            y: 0.0,
        }));
        let chrome = window_layer(OverlapWindowProgram {
            area: Rc::clone(&area),
        });
        let mut element: Element<'_, UiEvent> = Stack::with_children(vec![hosted, chrome])
            .width(Length::Fill)
            .height(Length::Fill)
            .into();
        let mut tree = Tree::new(element.as_widget());
        let mut node = element.as_widget_mut().layout(
            &mut tree,
            &renderer,
            &Limits::new(Size::ZERO, viewport),
        );

        let picker_area = hosted_layout
            .targets(
                Layout::new(&node)
                    .children()
                    .next()
                    .unwrap_or_else(|| panic!("the stack must retain the hosted document")),
                Cursor::Unavailable,
            )
            .into_iter()
            .find(|target| target.path == "library2/context")
            .map_or_else(
                || panic!("the hosted picker target must exist"),
                |target| target.hit.area(),
            );
        let popup = Rect {
            h: picker_area.h,
            w: picker_area.w,
            x: picker_area.x,
            y: picker_area.y + picker_area.h,
        };
        area.set(popup);

        let (opened, captured) = dispatch_press(
            &mut element,
            &mut tree,
            &node,
            &renderer,
            viewport,
            Point::new(
                picker_area.x + picker_area.w / 2.0,
                picker_area.y + picker_area.h / 2.0,
            ),
        );
        assert!(opened.is_empty());
        assert!(captured);
        assert!(
            tree.children[0]
                .state
                .downcast_ref::<State>()
                .engine
                .picker_snapshot("library2/context")
                .is_some_and(|snapshot| snapshot.open)
        );

        node = element.as_widget_mut().layout(
            &mut tree,
            &renderer,
            &Limits::new(Size::ZERO, viewport),
        );
        let (selected, captured) = dispatch_press(
            &mut element,
            &mut tree,
            &node,
            &renderer,
            viewport,
            Point::new(popup.x + popup.w / 2.0, popup.y + popup.h / 2.0),
        );
        assert_eq!(
            selected,
            [UiEvent::Control {
                path: "library2/context".to_owned(),
                action: ControlAction::SelectIndex(0),
            }]
        );
        assert!(captured);
        assert!(!selected.contains(&UiEvent::Window(WindowCommand::Drag)));
        assert!(
            tree.children[0]
                .state
                .downcast_ref::<State>()
                .engine
                .picker_snapshot("library2/context")
                .is_some_and(|snapshot| !snapshot.open)
        );
    }

    #[kithara::test]
    fn hosted_search_reports_two_carets_and_forwards_replacing_preedit_until_one_commit() {
        let ui = compiled_gallery_library();
        let reads = FixtureReads {
            query: "ab".to_owned(),
            ..FixtureReads::default()
        };
        let CompiledNode::Module { instance, root, .. } = &ui.root else {
            panic!("gallery library fixture root must be a module");
        };
        let renderer = headless_renderer();
        let viewport = Size::new(900.0, 600.0);
        let viewport_bounds = Rectangle::with_size(viewport);
        let child = super::super::node::render_engine_node(
            root,
            &[],
            *instance,
            ctx(&ui, &reads),
            builtin::skin(),
        );
        let mut element = host(child, root, ctx(&ui, &reads), builtin::skin());
        let mut tree = Tree::new(element.as_widget());
        let node = element.as_widget_mut().layout(
            &mut tree,
            &renderer,
            &Limits::new(Size::ZERO, viewport),
        );
        let hosted = HostedLayout::new(root, ctx(&ui, &reads), builtin::skin());
        let targets = hosted.targets(Layout::new(&node), Cursor::Unavailable);
        let search = targets
            .iter()
            .find(|target| target.path == "library2/browser/search")
            .map_or_else(
                || panic!("the search target must exist"),
                |target| target.hit.area(),
            );
        let first_position = Point::new(
            search.x + builtin::skin().tree.search_padding_x,
            search.y + search.h / 2.0,
        );
        let first_pointer = Cursor::Available(first_position);
        let mut clipboard = clipboard::Null;
        let mut messages = Vec::new();

        for event in [
            Event::Mouse(mouse::Event::ButtonPressed(Button::Left)),
            Event::Mouse(mouse::Event::ButtonReleased(Button::Left)),
        ] {
            let mut shell = Shell::new(&mut messages);
            element.as_widget_mut().update(
                &mut tree,
                &event,
                Layout::new(&node),
                first_pointer,
                &renderer,
                &mut clipboard,
                &mut shell,
                &viewport_bounds,
            );
            assert!(
                shell.is_event_captured(),
                "the hosted search must retain both pointer press and release"
            );
        }

        let mut shell = Shell::new(&mut messages);
        element.as_widget_mut().update(
            &mut tree,
            &redraw_event(),
            Layout::new(&node),
            Cursor::Unavailable,
            &renderer,
            &mut clipboard,
            &mut shell,
            &viewport_bounds,
        );
        let first_caret = match shell.input_method() {
            IcedInputMethod::Enabled {
                cursor,
                preedit: None,
                ..
            } => *cursor,
            request => panic!("focused search must enable IME without preedit: {request:?}"),
        };
        assert_eq!(
            first_caret.x,
            search.x + builtin::skin().tree.search_padding_x.floor()
        );
        drop(shell);

        let mut shell = Shell::new(&mut messages);
        element.as_widget_mut().update(
            &mut tree,
            &key_event(Named::ArrowRight, Code::ArrowRight),
            Layout::new(&node),
            Cursor::Unavailable,
            &renderer,
            &mut clipboard,
            &mut shell,
            &viewport_bounds,
        );
        assert!(shell.is_event_captured());
        drop(shell);
        assert!(messages.is_empty(), "caret movement must not publish");

        let mut shell = Shell::new(&mut messages);
        element.as_widget_mut().update(
            &mut tree,
            &redraw_event(),
            Layout::new(&node),
            Cursor::Unavailable,
            &renderer,
            &mut clipboard,
            &mut shell,
            &viewport_bounds,
        );
        let second_caret = match shell.input_method() {
            IcedInputMethod::Enabled { cursor, .. } => *cursor,
            IcedInputMethod::Disabled => panic!("moved caret must keep IME enabled"),
        };
        assert!(second_caret.x > first_caret.x);
        assert_eq!(second_caret.y, first_caret.y);
        drop(shell);

        let shifted_right = Event::Keyboard(keyboard::Event::KeyPressed {
            key: keyboard::Key::Named(Named::ArrowRight),
            modified_key: keyboard::Key::Named(Named::ArrowRight),
            physical_key: Physical::Code(Code::ArrowRight),
            location: Location::Standard,
            modifiers: IcedModifiers::SHIFT,
            text: None,
            repeat: false,
        });
        let mut shell = Shell::new(&mut messages);
        element.as_widget_mut().update(
            &mut tree,
            &shifted_right,
            Layout::new(&node),
            Cursor::Unavailable,
            &renderer,
            &mut clipboard,
            &mut shell,
            &viewport_bounds,
        );
        assert!(shell.is_event_captured());
        assert_ne!(shell.redraw_request(), window::RedrawRequest::Wait);
        drop(shell);
        assert!(messages.is_empty(), "selection changes must not publish");
        assert_eq!(
            tree.state
                .downcast_ref::<State>()
                .engine
                .text_input_snapshot("library2/browser/search")
                .and_then(|snapshot| snapshot.selection),
            Some(1..2)
        );

        for event in [
            input_method::Event::Preedit("かな".to_owned(), Some(0..3)),
            input_method::Event::Preedit("日本".to_owned(), Some(3..6)),
        ] {
            let mut shell = Shell::new(&mut messages);
            element.as_widget_mut().update(
                &mut tree,
                &Event::InputMethod(event),
                Layout::new(&node),
                Cursor::Unavailable,
                &renderer,
                &mut clipboard,
                &mut shell,
                &viewport_bounds,
            );
            assert!(shell.is_event_captured());
        }
        assert!(messages.is_empty(), "preedit replacement must not publish");

        let mut shell = Shell::new(&mut messages);
        element.as_widget_mut().update(
            &mut tree,
            &redraw_event(),
            Layout::new(&node),
            Cursor::Unavailable,
            &renderer,
            &mut clipboard,
            &mut shell,
            &viewport_bounds,
        );
        match shell.input_method() {
            IcedInputMethod::Enabled {
                cursor,
                preedit: Some(preedit),
                ..
            } => {
                assert_eq!(*cursor, second_caret);
                assert_eq!(preedit.content, "日本");
                assert_eq!(preedit.selection, Some(3..6));
            }
            request => panic!("latest preedit must reach iced at the caret: {request:?}"),
        }
        drop(shell);

        let mut shell = Shell::new(&mut messages);
        element.as_widget_mut().update(
            &mut tree,
            &Event::InputMethod(input_method::Event::Commit("日".to_owned())),
            Layout::new(&node),
            Cursor::Unavailable,
            &renderer,
            &mut clipboard,
            &mut shell,
            &viewport_bounds,
        );
        assert!(shell.is_event_captured());
        drop(shell);
        assert_eq!(messages, [UiEvent::LibraryQuery("a日".to_owned())]);
        assert!(
            tree.state
                .downcast_ref::<State>()
                .engine
                .text_input_snapshot("library2/browser/search")
                .is_some_and(|snapshot| snapshot.preedit.is_none())
        );
    }

    #[kithara::test]
    fn hosted_search_owns_delete_and_backspace_only_after_focus() {
        let ui = compiled_gallery_library();
        let reads = FixtureReads {
            query: "ab".to_owned(),
            ..FixtureReads::default()
        };
        let CompiledNode::Module { instance, root, .. } = &ui.root else {
            panic!("gallery library fixture root must be a module");
        };
        let renderer = headless_renderer();
        let viewport = Size::new(900.0, 600.0);
        let viewport_bounds = Rectangle::with_size(viewport);
        let hosted = HostedLayout::new(root, ctx(&ui, &reads), builtin::skin());

        for (key, code) in [
            (Named::Delete, Code::Delete),
            (Named::Backspace, Code::Backspace),
        ] {
            let child = super::super::node::render_engine_node(
                root,
                &[],
                *instance,
                ctx(&ui, &reads),
                builtin::skin(),
            );
            let mut element = host(child, root, ctx(&ui, &reads), builtin::skin());
            let mut tree = Tree::new(element.as_widget());
            let node = element.as_widget_mut().layout(
                &mut tree,
                &renderer,
                &Limits::new(Size::ZERO, viewport),
            );
            let search = hosted
                .targets(Layout::new(&node), Cursor::Unavailable)
                .into_iter()
                .find(|target| target.path == "library2/browser/search")
                .map_or_else(
                    || panic!("the search target must exist"),
                    |target| target.hit.area(),
                );
            let mut clipboard = clipboard::Null;
            let mut messages = Vec::new();

            let mut shell = Shell::new(&mut messages);
            element.as_widget_mut().update(
                &mut tree,
                &key_event(key, code),
                Layout::new(&node),
                Cursor::Unavailable,
                &renderer,
                &mut clipboard,
                &mut shell,
                &viewport_bounds,
            );
            assert!(!shell.is_event_captured());
            drop(shell);

            let pointer = Cursor::Available(Point::new(
                search.x + search.w - 1.0,
                search.y + search.h / 2.0,
            ));
            for event in [
                Event::Mouse(mouse::Event::ButtonPressed(Button::Left)),
                Event::Mouse(mouse::Event::ButtonReleased(Button::Left)),
            ] {
                let mut shell = Shell::new(&mut messages);
                element.as_widget_mut().update(
                    &mut tree,
                    &event,
                    Layout::new(&node),
                    pointer,
                    &renderer,
                    &mut clipboard,
                    &mut shell,
                    &viewport_bounds,
                );
            }
            messages.clear();

            let mut shell = Shell::new(&mut messages);
            element.as_widget_mut().update(
                &mut tree,
                &key_event(key, code),
                Layout::new(&node),
                Cursor::Unavailable,
                &renderer,
                &mut clipboard,
                &mut shell,
                &viewport_bounds,
            );
            assert!(shell.is_event_captured());
            drop(shell);
            assert_eq!(messages.len(), 1, "focused {key:?} must publish once");
        }
    }

    #[kithara::test]
    fn gallery_module_tabs_share_the_host_activation_component() {
        let ui = compiled_gallery_tabs();
        let reads = FixtureReads::default();
        let CompiledNode::Module {
            instance,
            module,
            root,
            ..
        } = &ui.root
        else {
            panic!("gallery module tabs fixture root must be a module");
        };

        assert_eq!(ui.resolve(*module), "gallery-module-tabs");
        let mut components = Vec::new();
        claimed_components(root, &mut components);
        assert_eq!(components, ["activation"; 5]);

        let full = super::super::node::render_compiled(&ui.root, ctx(&ui, &reads), builtin::skin());
        let full_tree = Tree::new(full.as_widget());
        assert_eq!(host_count(&full_tree), 1, "the tabs own one engine");

        let renderer = headless_renderer();
        let viewport = Size::new(500.0, 80.0);
        let child = super::super::node::render_engine_node(
            root,
            &[],
            *instance,
            ctx(&ui, &reads),
            builtin::skin(),
        );
        let mut element = host(child, root, ctx(&ui, &reads), builtin::skin());
        let mut tree = Tree::new(element.as_widget());
        let node = element.as_widget_mut().layout(
            &mut tree,
            &renderer,
            &Limits::new(Size::ZERO, viewport),
        );
        let hosted = HostedLayout::new(root, ctx(&ui, &reads), builtin::skin());
        let descriptors = hosted.descriptors();
        assert_eq!(
            descriptors.iter().map(descriptor_path).collect::<Vec<_>>(),
            [
                "modules-tabs/deck",
                "modules-tabs/deck-micro",
                "modules-tabs/global-bar",
                "modules-tabs/telemetry",
                "modules-tabs/layout",
            ]
        );
        assert!(
            descriptors
                .iter()
                .all(|descriptor| matches!(descriptor, Descriptor::Activation { .. }))
        );

        let targets = hosted.targets(Layout::new(&node), Cursor::Unavailable);
        let target = targets
            .iter()
            .find(|target| target.path == "modules-tabs/deck-micro")
            .unwrap_or_else(|| panic!("the hosted DECK MICRO target must exist"));
        let area = target.hit.area();
        assert!((area.w - 94.0).abs() < 0.001);
        assert_eq!(area.h, builtin::skin().tab_large.height);
        let cursor = Cursor::Available(Point::new(area.x + area.w / 2.0, area.y + area.h / 2.0));
        let mut clipboard = clipboard::Null;
        let mut messages = Vec::new();
        let mut shell = Shell::new(&mut messages);
        element.as_widget_mut().update(
            &mut tree,
            &Event::Mouse(mouse::Event::ButtonPressed(Button::Left)),
            Layout::new(&node),
            cursor,
            &renderer,
            &mut clipboard,
            &mut shell,
            &Rectangle::with_size(viewport),
        );
        assert!(!shell.is_event_captured());
        drop(shell);

        assert_eq!(
            messages,
            [UiEvent::Control {
                path: "modules-tabs/deck-micro".to_owned(),
                action: ControlAction::Activate,
            }]
        );
    }

    #[kithara::test]
    fn gallery_nav_shares_the_host_activation_component() {
        let ui = compiled_gallery_nav();
        let reads = FixtureReads::default();
        let CompiledNode::Module {
            instance,
            module,
            root,
            ..
        } = &ui.root
        else {
            panic!("gallery nav fixture root must be a module");
        };

        assert_eq!(ui.resolve(*module), "gallery-nav");
        let mut components = Vec::new();
        claimed_components(root, &mut components);
        assert_eq!(components, ["activation"; 25]);

        let full = super::super::node::render_compiled(&ui.root, ctx(&ui, &reads), builtin::skin());
        let full_tree = Tree::new(full.as_widget());
        assert_eq!(host_count(&full_tree), 1, "the nav owns one engine");

        let renderer = headless_renderer();
        let viewport = Size::new(198.0, 620.0);
        let child = super::super::node::render_engine_node(
            root,
            &[],
            *instance,
            ctx(&ui, &reads),
            builtin::skin(),
        );
        let mut element = host(child, root, ctx(&ui, &reads), builtin::skin());
        let mut tree = Tree::new(element.as_widget());
        let node = element.as_widget_mut().layout(
            &mut tree,
            &renderer,
            &Limits::new(Size::ZERO, viewport),
        );
        let hosted = HostedLayout::new(root, ctx(&ui, &reads), builtin::skin());
        let descriptors = hosted.descriptors();
        assert_eq!(
            descriptors.iter().map(descriptor_path).collect::<Vec<_>>(),
            [
                "gallery/atoms/item",
                "gallery/buttons/item",
                "gallery/faders/item",
                "gallery/modules/item",
                "gallery/typography/item",
                "gallery/cells/item",
                "gallery/sizes/item",
                "gallery/tokens/item",
                "gallery/micro/item",
                "gallery/mixer/item",
                "gallery/vis/item",
                "gallery/chrome/item",
                "gallery/titlebars/item",
                "gallery/table/item",
                "gallery/tree/item",
                "gallery/library2/item",
                "gallery/stress/item",
                "gallery/menu/item",
                "gallery/clock/item",
                "gallery/pivot/item",
                "gallery/shader/item",
                "gallery/objects/item",
                "gallery/motion/item",
                "gallery/sprites/item",
                "gallery/lottie/item",
            ]
        );
        assert!(
            descriptors
                .iter()
                .all(|descriptor| matches!(descriptor, Descriptor::Activation { .. }))
        );

        let targets = hosted.targets(Layout::new(&node), Cursor::Unavailable);
        let target = targets
            .iter()
            .find(|target| target.path == "gallery/buttons/item")
            .unwrap_or_else(|| panic!("the hosted buttons nav item target must exist"));
        let area = target.hit.area();
        assert_eq!((area.w, area.h), (198.0, builtin::skin().nav.item_height));
        let cursor = Cursor::Available(Point::new(area.x + area.w / 2.0, area.y + area.h / 2.0));
        let mut clipboard = clipboard::Null;
        let mut messages = Vec::new();
        let mut shell = Shell::new(&mut messages);
        element.as_widget_mut().update(
            &mut tree,
            &Event::Mouse(mouse::Event::ButtonPressed(Button::Left)),
            Layout::new(&node),
            cursor,
            &renderer,
            &mut clipboard,
            &mut shell,
            &Rectangle::with_size(viewport),
        );
        assert!(
            !shell.is_event_captured(),
            "activation publishes without retaining the engine capture slot"
        );
        drop(shell);

        assert_eq!(
            messages,
            [UiEvent::Control {
                path: "gallery/buttons/item".to_owned(),
                action: ControlAction::Activate,
            }]
        );
    }

    /// Outside a hosted subtree the item answers for itself, through the grip
    /// it declares. Nothing else reaches that arm: every nav item the
    /// repository ships is hosted, so the engine answers all of them.
    #[kithara::test]
    fn a_leaf_nav_item_publishes_an_activation_of_its_own() {
        let ui = compiled_leaf_nav_item();
        let reads = FixtureReads::default();
        let renderer = headless_renderer();
        let viewport = Size::new(198.0, 30.0);
        let mut element =
            super::super::node::render_compiled(&ui.root, ctx(&ui, &reads), builtin::skin());
        let mut tree = Tree::new(element.as_widget());
        assert_eq!(host_count(&tree), 0, "a leaf nav item owns no engine");

        let node = element.as_widget_mut().layout(
            &mut tree,
            &renderer,
            &Limits::new(Size::ZERO, viewport),
        );
        let mut clipboard = clipboard::Null;
        let mut messages = Vec::new();
        let mut shell = Shell::new(&mut messages);
        element.as_widget_mut().update(
            &mut tree,
            &Event::Mouse(mouse::Event::ButtonPressed(Button::Left)),
            Layout::new(&node),
            Cursor::Available(Point::new(99.0, 15.0)),
            &renderer,
            &mut clipboard,
            &mut shell,
            &Rectangle::with_size(viewport),
        );
        drop(shell);

        assert_eq!(
            messages,
            [UiEvent::Control {
                path: "nav/item".to_owned(),
                action: ControlAction::Activate,
            }]
        );
    }

    /// Mounts one compiled fixture in a window and hands the caller the mounted
    /// tree. Every press in these tests goes through the host the window runs,
    /// rather than through the target list alone, because what is under test is
    /// what answers.
    fn mounted<T>(
        ui: &CompiledUi,
        viewport: Size,
        act: impl FnOnce(
            &mut Element<'_, UiEvent>,
            &mut Tree,
            &layout::Node,
            &Renderer,
            Size,
            &HostedLayout,
        ) -> T,
    ) -> T {
        let reads = FixtureReads::default();
        let CompiledNode::Module { instance, root, .. } = &ui.root else {
            panic!("the fixture root must be a module");
        };
        let renderer = headless_renderer();
        let child = super::super::node::render_engine_node(
            root,
            &[],
            *instance,
            ctx(ui, &reads),
            builtin::skin(),
        );
        let mut element = host(child, root, ctx(ui, &reads), builtin::skin());
        let mut tree = Tree::new(element.as_widget());
        let node = element.as_widget_mut().layout(
            &mut tree,
            &renderer,
            &Limits::new(Size::ZERO, viewport),
        );
        let hosted = HostedLayout::new(root, ctx(ui, &reads), builtin::skin());
        act(&mut element, &mut tree, &node, &renderer, viewport, &hosted)
    }

    /// The window the clipped fixtures are mounted in: far taller than the box
    /// either of them scrolls in, so what the box cuts away still has window
    /// under it.
    fn clipped_window() -> Size {
        Size::new(198.0, 400.0)
    }

    /// The box one target publishes, wherever the layout put it.
    fn box_of(targets: &[Target<'_>], path: &str) -> Rect {
        targets
            .iter()
            .find(|target| target.path == path)
            .unwrap_or_else(|| panic!("{path} must have a target"))
            .hit
            .area()
    }

    /// The centre of that box, which is where these tests point.
    fn centre_of(targets: &[Target<'_>], path: &str) -> Point {
        let area = box_of(targets, path);
        Point::new(area.x + area.w / 2.0, area.y + area.h / 2.0)
    }

    /// A press on the blank window below a scrolling box reaches nothing: the
    /// rows the box cut away are not under that point, whatever their layout
    /// still says.
    #[kithara::test]
    fn a_press_below_a_scrolling_box_is_not_seen_by_the_rows_it_clipped() {
        let (published, at) = mounted(
            &compiled_clipped_scroll(),
            clipped_window(),
            |element, tree, node, renderer, viewport, hosted| {
                let targets = hosted.targets(Layout::new(node), Cursor::Unavailable);
                let at = centre_of(&targets, "nav/fourth");
                let (messages, _) = dispatch_press(element, tree, node, renderer, viewport, at);
                (messages, at)
            },
        );

        assert!(
            at.y > builtin::skin().nav.item_height,
            "the fourth row has to sit past the fold of a box one row tall for a press there to \
             mean anything, and the layout puts its centre at {at:?}"
        );
        assert_eq!(
            published,
            [],
            "the box scrolls one row and nothing is drawn below it, so a press on blank window \
             answered by a row is a press the box never cut"
        );
    }

    /// The other direction, so the cut is not merely deafness: inside the box
    /// the row under the pointer still answers.
    #[kithara::test]
    fn a_press_inside_a_scrolling_box_still_reaches_the_row_under_it() {
        let (published, at) = mounted(
            &compiled_clipped_scroll(),
            clipped_window(),
            |element, tree, node, renderer, viewport, hosted| {
                let targets = hosted.targets(Layout::new(node), Cursor::Unavailable);
                let at = centre_of(&targets, "nav/first");
                let (messages, _) = dispatch_press(element, tree, node, renderer, viewport, at);
                (messages, at)
            },
        );

        assert!(
            at.y < builtin::skin().nav.item_height,
            "the first row is the one the box shows, and the layout puts its centre at {at:?}"
        );
        assert_eq!(
            published,
            [UiEvent::Control {
                path: "nav/first".to_owned(),
                action: ControlAction::Activate,
            }]
        );
    }

    /// What one drag out of a scrolling box did.
    struct DraggedOut {
        /// Whether the press inside the box armed the meter there. Without this
        /// the move that follows measures nothing.
        armed: bool,
        /// What that move published.
        published: Vec<UiEvent>,
        /// Where the pointer ended up.
        at: Point,
        /// The meter's own box, which reaches past the box that scrolls it.
        held: Rect,
        /// The box of the knob below the scrolling one, which is what the
        /// pointer ended up over.
        other: Rect,
    }

    /// Presses the meter inside the scrolling box, then drags the pointer down
    /// out of that box, onto the knob below it.
    fn dragged_out_of_the_box() -> DraggedOut {
        mounted(
            &compiled_clipped_drag(),
            clipped_window(),
            |element, tree, node, renderer, viewport, hosted| {
                let targets = hosted.targets(Layout::new(node), Cursor::Unavailable);
                let held = box_of(&targets, "nav/held");
                // The box shows the top half of the meter, so a quarter of the
                // way down the meter is the middle of what is on screen.
                let start = Point::new(held.x + held.w / 2.0, held.y + held.h / 4.0);
                let at = centre_of(&targets, "nav/other");
                let other = box_of(&targets, "nav/other");
                let bounds = Rectangle::with_size(viewport);
                let mut clipboard = clipboard::Null;
                let mut armed_messages = Vec::new();
                let mut shell = Shell::new(&mut armed_messages);
                element.as_widget_mut().update(
                    tree,
                    &Event::Mouse(mouse::Event::ButtonPressed(Button::Left)),
                    Layout::new(node),
                    Cursor::Available(start),
                    renderer,
                    &mut clipboard,
                    &mut shell,
                    &bounds,
                );
                let armed = shell.is_event_captured();
                drop(shell);

                let mut published = Vec::new();
                let mut shell = Shell::new(&mut published);
                element.as_widget_mut().update(
                    tree,
                    &Event::Mouse(mouse::Event::CursorMoved { position: at }),
                    Layout::new(node),
                    Cursor::Available(at),
                    renderer,
                    &mut clipboard,
                    &mut shell,
                    &bounds,
                );
                drop(shell);

                DraggedOut {
                    armed,
                    published,
                    at,
                    held,
                    other,
                }
            },
        )
    }

    /// A drag that starts inside a box and leaves it keeps reaching the control
    /// that owns it: a gesture already in flight is the one thing the cut lets
    /// through.
    #[kithara::test]
    fn a_drag_that_leaves_the_box_it_started_in_still_reaches_its_owner() {
        let dragged = dragged_out_of_the_box();

        assert!(
            dragged.armed,
            "the press inside the box must arm the meter there, or the move that follows measures \
             nothing"
        );
        assert!(
            dragged.held.contains(dragged.at.into()),
            "the pointer has to leave the scrolling box while staying inside the meter's own box, \
             or a cut is not what would silence it: it ended at {:?} against a meter of {:?}",
            dragged.at,
            dragged.held
        );
        let [UiEvent::Control { path, action }] = dragged.published.as_slice() else {
            panic!(
                "the drag must publish one control event, and it published {:?}",
                dragged.published
            );
        };
        assert_eq!(path, "nav/held");
        assert!(
            matches!(action, ControlAction::SetScalar(_)),
            "the owner of a drag sets its own value, and it published {action:?}"
        );
    }

    /// The subtree the pointer ends up over never sees that drag: while one
    /// control owns the pointer, what happens to be under it does not answer.
    #[kithara::test]
    fn a_subtree_the_drag_passes_over_never_answers_it() {
        let dragged = dragged_out_of_the_box();

        assert!(
            dragged.other.contains(dragged.at.into()),
            "the drag has to end over the other knob for its silence to mean anything, and it \
             ended at {:?} against a box of {:?}",
            dragged.at,
            dragged.other
        );
        assert!(
            !dragged.published.iter().any(|event| matches!(
                event,
                UiEvent::Control { path, .. } if path == "nav/other"
            )),
            "the knob under the pointer answered a drag it never started: {:?}",
            dragged.published
        );
    }

    #[kithara::test]
    fn studio_overview_row_hosts_its_click_wave() {
        let ui = compiled_overview_row();
        let reads = FixtureReads::default();
        let CompiledNode::Module {
            instance,
            module,
            root,
            ..
        } = &ui.root
        else {
            panic!("overview fixture root must be a module");
        };
        let ExpandedNode::Row { children, .. } = root.as_ref() else {
            panic!("overview fixture body must be a row");
        };
        let row = &children[0];

        assert_eq!(ui.resolve(*module), "app-overview");
        assert!(ui.includes_module(*instance, &[0], "app-overview-row"));
        let mut components = Vec::new();
        claimed_components(row, &mut components);
        assert_eq!(components, ["wave"]);

        let full = super::super::node::render_compiled(&ui.root, ctx(&ui, &reads), builtin::skin());
        let full_tree = Tree::new(full.as_widget());
        assert_eq!(
            host_count(&full_tree),
            1,
            "the overview row owns one engine"
        );

        let renderer = headless_renderer();
        let viewport = Size::new(200.0, 40.0);
        let child = super::super::node::render_engine_node(
            row,
            &[0],
            *instance,
            ctx(&ui, &reads),
            builtin::skin(),
        );
        let mut element = host(child, row, ctx(&ui, &reads), builtin::skin());
        let mut tree = Tree::new(element.as_widget());
        let node = element.as_widget_mut().layout(
            &mut tree,
            &renderer,
            &Limits::new(Size::ZERO, viewport),
        );
        let hosted = HostedLayout::new(row, ctx(&ui, &reads), builtin::skin());
        let descriptors = hosted.descriptors();
        assert_eq!(descriptors.len(), 1);
        let Descriptor::Wave { path } = &descriptors[0] else {
            panic!("the overview wave must produce a wave descriptor");
        };
        assert_eq!(path, "overview/a/wave");

        let targets = hosted.targets(Layout::new(&node), Cursor::Unavailable);
        assert_eq!(
            targets.iter().map(|target| target.path).collect::<Vec<_>>(),
            ["overview/a/wave"]
        );
        let area = targets[0].hit.area();
        let cursor = Cursor::Available(Point::new(area.x + area.w * 0.25, area.y + area.h / 2.0));
        let mut clipboard = clipboard::Null;
        let mut messages = Vec::new();
        let mut shell = Shell::new(&mut messages);
        element.as_widget_mut().update(
            &mut tree,
            &Event::Mouse(mouse::Event::ButtonPressed(Button::Left)),
            Layout::new(&node),
            cursor,
            &renderer,
            &mut clipboard,
            &mut shell,
            &Rectangle::with_size(viewport),
        );
        assert!(
            !shell.is_event_captured(),
            "the click wave publishes without retaining the engine capture slot"
        );
        drop(shell);

        let outside = Point::new(area.x + area.w + 10.0, area.y + area.h / 2.0);
        let mut shell = Shell::new(&mut messages);
        element.as_widget_mut().update(
            &mut tree,
            &Event::Mouse(mouse::Event::CursorMoved { position: outside }),
            Layout::new(&node),
            Cursor::Available(outside),
            &renderer,
            &mut clipboard,
            &mut shell,
            &Rectangle::with_size(viewport),
        );
        assert!(!shell.is_event_captured());
        drop(shell);

        assert_eq!(
            messages,
            [UiEvent::Control {
                path: "overview/a/wave".to_owned(),
                action: ControlAction::SetScalar(0.25),
            }]
        );
    }

    #[kithara::test]
    fn hosted_hero_wave_keeps_grip_outside_bounds() {
        let path = "deck-a/wave";
        let child = container(Space::new().width(Length::Fill).height(Length::Fill))
            .width(Length::Fill)
            .height(Length::Fill)
            .into();
        let mut element = Element::new(Host {
            child,
            layout: HostedLayout::Control(Some(HostedControl::mounted(
                crate::render::HostedControlPlan::hero_wave_at(path, 0.75, 0.25),
                builtin::skin(),
            ))),
        });
        let renderer = headless_renderer();
        let viewport = Size::new(100.0, 40.0);
        let mut tree = Tree::new(element.as_widget());
        let node = element.as_widget_mut().layout(
            &mut tree,
            &renderer,
            &Limits::new(Size::ZERO, viewport),
        );
        let mut clipboard = clipboard::Null;
        let mut messages = Vec::new();
        let mut shell = Shell::new(&mut messages);
        element.as_widget_mut().update(
            &mut tree,
            &Event::Mouse(mouse::Event::ButtonPressed(Button::Left)),
            Layout::new(&node),
            Cursor::Available(Point::new(50.0, 20.0)),
            &renderer,
            &mut clipboard,
            &mut shell,
            &Rectangle::with_size(viewport),
        );
        assert!(shell.is_event_captured());
        drop(shell);
        assert!(messages.is_empty());

        let outside = Point::new(150.0, 20.0);
        let mut shell = Shell::new(&mut messages);
        element.as_widget_mut().update(
            &mut tree,
            &Event::Mouse(mouse::Event::CursorMoved { position: outside }),
            Layout::new(&node),
            Cursor::Available(outside),
            &renderer,
            &mut clipboard,
            &mut shell,
            &Rectangle::with_size(viewport),
        );
        assert!(shell.is_event_captured());
        drop(shell);

        assert_eq!(
            messages,
            [UiEvent::Control {
                path: path.to_owned(),
                action: ControlAction::SetScalar(0.5),
            }]
        );
    }

    #[kithara::test]
    fn outer_module_marker_survives_a_root_include_chain() {
        let mut resolver = MemResolver::default();
        resolver.insert(
            "layout.klayout.ron",
            r#"(schema: "kithara.layout", version: 1, id: "chain",
                root: Module(instance: "mixer", source: "mixer.kmodule.ron"))"#,
        );
        resolver.insert(
            "mixer.kmodule.ron",
            r#"(schema: "kithara.module", version: 1, id: "mixer",
                root: Row(children: [
                    Include(id: "strip", source: "app-strip.kmodule.ron"),
                ]))"#,
        );
        resolver.insert(
            "app-strip.kmodule.ron",
            r#"(schema: "kithara.module", version: 1, id: "app-strip",
                root: Include(id: "body", source: "strip-body.kmodule.ron"))"#,
        );
        resolver.insert(
            "strip-body.kmodule.ron",
            r#"(schema: "kithara.module", version: 1, id: "strip-body",
                root: Knob(id: "gain"))"#,
        );
        let ui = compile(
            "layout.klayout.ron",
            &resolver,
            &Registry::default(),
            builtin::skin_doc(),
            builtin::text_doc(),
            &UiConfig::default(),
        )
        .unwrap_or_else(|error| panic!("root include chain must compile: {error}"));
        let CompiledNode::Module { instance, .. } = &ui.root else {
            panic!("fixture root must be a module");
        };

        assert!(ui.includes_module(*instance, &[0], "strip-body"));
        assert!(ui.includes_module(*instance, &[0], "app-strip"));
    }

    #[kithara::test]
    fn the_app_shaped_mixer_owns_one_engine_for_both_strips() {
        let ui = compiled_fixture();
        let reads = FixtureReads::default();
        let CompiledNode::Module {
            instance,
            module,
            root,
            ..
        } = &ui.root
        else {
            panic!("fixture root must be the mixer module");
        };
        let ExpandedNode::Column { children, .. } = root.as_ref() else {
            panic!("mixer root must be a column");
        };
        let ExpandedNode::Row {
            children: strips, ..
        } = &children[0]
        else {
            panic!("mixer strips must be a row");
        };
        assert_eq!(strips.len(), 3, "two strips must surround one divider");

        assert_eq!(ui.resolve(*module), "app-mixer");
        assert!(ui.includes_module(*instance, &[0, 0], "app-strip"));
        assert!(ui.includes_module(*instance, &[0, 2], "app-strip"));
        let mut components = Vec::new();
        claimed_components(root, &mut components);
        assert_eq!(
            components,
            [
                "knob",
                "knob",
                "knob",
                "vertical-vu",
                "knob",
                "knob",
                "knob",
                "vertical-vu",
                "crossfader",
            ]
        );

        let renderer = headless_renderer();
        let viewport = Size::new(224.0, 420.0);
        let full = super::super::node::render_compiled(&ui.root, ctx(&ui, &reads), builtin::skin());
        let full_tree = Tree::new(full.as_widget());
        assert_eq!(host_count(&full_tree), 1, "the whole mixer owns one engine");

        let child = super::super::node::render_engine_node(
            root,
            &[],
            *instance,
            ctx(&ui, &reads),
            builtin::skin(),
        );
        let mut element = host(child, root, ctx(&ui, &reads), builtin::skin());
        let mut tree = Tree::new(element.as_widget());
        let node = element.as_widget_mut().layout(
            &mut tree,
            &renderer,
            &Limits::new(Size::ZERO, viewport),
        );
        let hosted = HostedLayout::new(root, ctx(&ui, &reads), builtin::skin());
        let descriptors = hosted.descriptors();
        let targets = hosted.targets(Layout::new(&node), Cursor::Unavailable);
        let expected_paths = [
            "mixer/a/high",
            "mixer/a/mid",
            "mixer/a/low",
            "mixer/a/volume",
            "mixer/b/high",
            "mixer/b/mid",
            "mixer/b/low",
            "mixer/b/volume",
            "mixer/xfade",
        ];
        assert_eq!(
            descriptors.iter().map(descriptor_path).collect::<Vec<_>>(),
            expected_paths,
            "every interactive control needs its own descriptor"
        );
        assert_eq!(
            targets.iter().map(|target| target.path).collect::<Vec<_>>(),
            expected_paths,
        );
        assert!(
            targets
                .iter()
                .all(|target| target.hit.area().w > 0.0 && target.hit.area().h > 0.0),
            "every retained component must resolve to its paint-only canvas bounds",
        );
        for target in targets.iter().filter(|target| target.path != "mixer/xfade") {
            let area = target.hit.area();
            if target.path.ends_with("/volume") {
                assert_eq!(area.w, 38.0, "the VU target must be its declared canvas");
            } else {
                assert_eq!(
                    (area.w, area.h),
                    (28.0, 39.0),
                    "a knob target must be its intrinsic canvas",
                );
            }
        }

        let high_a = targets
            .iter()
            .find(|target| target.path == "mixer/a/high")
            .unwrap_or_else(|| panic!("strip A high target must exist"));
        let high_b = targets
            .iter()
            .find(|target| target.path == "mixer/b/high")
            .unwrap_or_else(|| panic!("strip B high target must exist"));
        let center = |target: &Target<'_>| {
            let area = target.hit.area();
            Point::new(area.x + area.w / 2.0, area.y + area.h / 2.0)
        };
        let start = center(high_a);
        let over_b = center(high_b);
        let mut clipboard = clipboard::Null;
        let mut messages = Vec::new();
        let mut shell = Shell::new(&mut messages);
        element.as_widget_mut().update(
            &mut tree,
            &Event::Mouse(mouse::Event::WheelScrolled {
                delta: mouse::ScrollDelta::Lines { x: 0.0, y: -1.0 },
            }),
            Layout::new(&node),
            Cursor::Available(start),
            &renderer,
            &mut clipboard,
            &mut shell,
            &Rectangle::with_size(viewport),
        );
        assert!(
            !shell.is_event_captured(),
            "a knob wheel emission does not retain the engine capture slot"
        );
        drop(shell);
        assert_eq!(
            messages.len(),
            1,
            "a wheel on the real hosted knob must publish exactly once"
        );
        let UiEvent::Control { path, action } = &messages[0] else {
            panic!("the hosted knob wheel must publish a control event");
        };
        assert_eq!(path, "mixer/a/high");
        assert!(matches!(action, ControlAction::SetScalar(_)));
        messages.clear();

        let mut shell = Shell::new(&mut messages);
        element.as_widget_mut().update(
            &mut tree,
            &Event::Mouse(mouse::Event::ButtonPressed(Button::Left)),
            Layout::new(&node),
            Cursor::Available(start),
            &renderer,
            &mut clipboard,
            &mut shell,
            &Rectangle::with_size(viewport),
        );
        assert!(shell.is_event_captured());
        drop(shell);
        assert!(messages.is_empty(), "arming a knob must not publish");

        let mut shell = Shell::new(&mut messages);
        element.as_widget_mut().update(
            &mut tree,
            &Event::Mouse(mouse::Event::CursorMoved { position: over_b }),
            Layout::new(&node),
            Cursor::Available(over_b),
            &renderer,
            &mut clipboard,
            &mut shell,
            &Rectangle::with_size(viewport),
        );
        assert!(shell.is_event_captured());
        drop(shell);
        assert_eq!(
            messages.len(),
            1,
            "the hosted knob's paint-only child must not answer the move a second time, and strip \
             B must stay silent while strip A owns the mixer's capture slot"
        );
        let UiEvent::Control { path, action } = &messages[0] else {
            panic!("the captured strip A knob must publish a control event");
        };
        assert_eq!(path, "mixer/a/high");
        assert_eq!(action, &ControlAction::SetScalar(0.5));
    }

    #[kithara::test]
    fn the_host_retains_an_armed_component_across_fresh_descriptors() {
        let ui = compiled_fixture();
        let reads = FixtureReads::default();
        let CompiledNode::Module { instance, root, .. } = &ui.root else {
            panic!("fixture root must be the mixer module");
        };
        let ExpandedNode::Column { children, .. } = root.as_ref() else {
            panic!("mixer root must be a column");
        };
        let ExpandedNode::Row {
            children: strips, ..
        } = &children[0]
        else {
            panic!("mixer strips must be a row");
        };
        let strip = &strips[0];
        let renderer = headless_renderer();
        let viewport = Size::new(112.0, 420.0);
        let child = super::super::node::render_engine_node(
            strip,
            &[0, 0],
            *instance,
            ctx(&ui, &reads),
            builtin::skin(),
        );
        let mut element = host(child, strip, ctx(&ui, &reads), builtin::skin());
        let mut tree = Tree::new(element.as_widget());
        let node = element.as_widget_mut().layout(
            &mut tree,
            &renderer,
            &Limits::new(Size::ZERO, viewport),
        );
        let layout = HostedLayout::new(strip, ctx(&ui, &reads), builtin::skin());
        let high = layout
            .targets(Layout::new(&node), Cursor::Unavailable)
            .into_iter()
            .find(|target| target.path == "mixer/a/high")
            .unwrap_or_else(|| panic!("strip A high target must exist"));
        let area = high.hit.area();
        let start = Point::new(area.x + area.w / 2.0, area.y + area.h / 2.0);
        let mut clipboard = clipboard::Null;
        let mut messages = Vec::new();
        let mut shell = Shell::new(&mut messages);
        element.as_widget_mut().update(
            &mut tree,
            &Event::Mouse(mouse::Event::ButtonPressed(Button::Left)),
            Layout::new(&node),
            Cursor::Available(start),
            &renderer,
            &mut clipboard,
            &mut shell,
            &Rectangle::with_size(viewport),
        );
        assert!(shell.is_event_captured());
        drop(shell);
        assert!(messages.is_empty(), "arming a knob must not publish");
        drop(element);

        let refreshed_reads = FixtureReads {
            gain: 0.9,
            ..FixtureReads::default()
        };
        let next_child = super::super::node::render_engine_node(
            strip,
            &[0, 0],
            *instance,
            ctx(&ui, &refreshed_reads),
            builtin::skin(),
        );
        let mut next = host(
            next_child,
            strip,
            ctx(&ui, &refreshed_reads),
            builtin::skin(),
        );
        tree.diff(next.as_widget());
        let next_node =
            next.as_widget_mut()
                .layout(&mut tree, &renderer, &Limits::new(Size::ZERO, viewport));
        let moved = Point::new(start.x, start.y - builtin::skin().knob.drag_range * 0.25);
        let mut messages = Vec::new();
        let mut shell = Shell::new(&mut messages);
        next.as_widget_mut().update(
            &mut tree,
            &Event::Mouse(mouse::Event::CursorMoved { position: moved }),
            Layout::new(&next_node),
            Cursor::Available(moved),
            &renderer,
            &mut clipboard,
            &mut shell,
            &Rectangle::with_size(viewport),
        );
        assert!(shell.is_event_captured());
        drop(shell);

        assert_eq!(messages.len(), 1);
        let UiEvent::Control { path, action } = &messages[0] else {
            panic!("the retained knob must publish a control event");
        };
        assert_eq!(path, "mixer/a/high");
        assert_eq!(action, &ControlAction::SetScalar(0.75));
    }
}
