use std::{
    cell::{Cell, RefCell},
    collections::{BTreeMap, VecDeque},
    rc::Rc,
    sync::Arc,
};

use kithara_platform::time::Duration;
use kithara_test_utils::kithara;
use masonry::{
    app::{RenderRootOptions, RenderRootSignal, WindowSizePolicy},
    core::{CursorIcon, Handled, PointerEvent, TextEvent, WindowEvent},
    dpi::{PhysicalPosition, PhysicalSize},
    kurbo::{Point, Size as MasonrySize},
    theme::default_property_set,
    ui_events::{
        ScrollDelta,
        keyboard::{Key, NamedKey},
        pointer::{
            PointerButton as MasonryPointerButton, PointerButtonEvent, PointerButtons, PointerId,
            PointerInfo, PointerScrollEvent, PointerState, PointerType, PointerUpdate,
        },
    },
};
use num_traits::cast::AsPrimitive;

use super::{
    CustomWidget, MasonryHost, MasonryNode, MasonryRoot, MasonryState, Repaint, Size2, SizeLimits,
    TextMeasurer, leaf::DragProgram,
};
use crate::{
    atoms::bar::context::Context,
    builtin,
    compile::{CompiledUi, compile},
    draw::{DrawListBuilder, Pt, Rect},
    ids::{EndpointId, SourceUri},
    interact::{Hit, Input, Key as NeutralKey, Outcome, PointerOwnership, PointerPhase, Scroll},
    registry::{EndpointCategory, EndpointDesc, EndpointRegistry, ValueKind},
    render::{
        ControlAction, ReadValue, Reads, Skin, StereoLevels, UiEvent, WindowCommand, WindowEdge,
        WindowLayerProgram, document, picker_hits,
    },
    source::{MemResolver, UiConfig},
    text::{FontPolicy, TextContext},
};

struct FixtureReads;

impl Reads for FixtureReads {
    fn get(&self, endpoint: &str) -> Option<ReadValue<'_>> {
        let id = endpoint.split_once('@').map_or(endpoint, |(id, _scope)| id);
        match id {
            "deck.playback.tempo" => Some(ReadValue::Text("128.0")),
            "deck.track.title" => Some(ReadValue::Text("Midnight Circuit")),
            "deck.playback.playing"
            | "deck.playback.looping"
            | "deck.playback.synced"
            | "ui.menu.open" => Some(ReadValue::Bool(true)),
            "deck.playback.reverse" => Some(ReadValue::Bool(false)),
            "deck.playback.position_normalized" => Some(ReadValue::Scalar(0.375)),
            "deck.view.zoom" => Some(ReadValue::Scalar(0.25)),
            "library.breadcrumb" => Some(ReadValue::Text("All Tracks")),
            "library.scope" => Some(ReadValue::Scalar(0.0)),
            "player.output.levels" => Some(ReadValue::Stereo(StereoLevels {
                l: 0.6,
                r: 0.4,
                volume: 0.8,
            })),
            "player.output.volume" => Some(ReadValue::Scalar(0.8)),
            _ => None,
        }
    }
}

struct PopoverReads {
    open: bool,
}

impl Reads for PopoverReads {
    fn get(&self, endpoint: &str) -> Option<ReadValue<'_>> {
        let id = endpoint.split_once('@').map_or(endpoint, |(id, _scope)| id);
        if id == "ui.menu.open" {
            Some(ReadValue::Bool(self.open))
        } else {
            FixtureReads.get(endpoint)
        }
    }
}

#[derive(Default)]
struct FixtureRegistry {
    endpoints: BTreeMap<(EndpointCategory, EndpointId), EndpointDesc>,
}

impl FixtureRegistry {
    fn insert(&mut self, category: EndpointCategory, id: &str, description: EndpointDesc) {
        self.endpoints
            .insert((category, EndpointId(id.to_owned())), description);
    }
}

impl EndpointRegistry for FixtureRegistry {
    fn endpoint(&self, category: EndpointCategory, id: &EndpointId) -> Option<&EndpointDesc> {
        self.endpoints.get(&(category, id.clone()))
    }
}

#[derive(Debug)]
struct ExpectedRect {
    path: String,
    x: f64,
    y: f64,
    width: f64,
    height: f64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Regularity {
    Regular,
    Irregular,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Mode {
    Velocity,
    Probability,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Rotation(i16);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ViewState {
    mode: Mode,
    selected_round: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum WheelAction {
    CellPress {
        round: usize,
        cell: usize,
        regularity: Regularity,
        active: bool,
    },
    CellLongPress {
        round: usize,
        cell: usize,
        regularity: Regularity,
        active: bool,
    },
    ModeEdit {
        round: usize,
        cell: usize,
        mode: Mode,
        value: u8,
    },
    RoundRotate {
        round: usize,
        rotation: Rotation,
    },
    ModeReset {
        round: usize,
        cell: usize,
        mode: Mode,
    },
    ViewStateRequest {
        view_state: ViewState,
    },
}

#[derive(Debug, PartialEq)]
enum TestAction {
    Document(UiEvent),
    Wheel(WheelAction),
}

struct WheelEmitter {
    pending: VecDeque<WheelAction>,
    pressed: bool,
    long_pressed: bool,
    recognized: Rc<RefCell<Vec<PointerPhase>>>,
}

impl WheelEmitter {
    fn new(actions: impl IntoIterator<Item = WheelAction>) -> Self {
        Self::observed(actions, Rc::default())
    }

    fn observed(
        actions: impl IntoIterator<Item = WheelAction>,
        recognized: Rc<RefCell<Vec<PointerPhase>>>,
    ) -> Self {
        Self {
            pending: actions.into_iter().collect(),
            pressed: false,
            long_pressed: false,
            recognized,
        }
    }

    fn take(&mut self) -> Option<WheelAction> {
        self.pending.pop_front()
    }
}

impl CustomWidget for WheelEmitter {
    type Action = WheelAction;

    fn measure(&mut self, _text: &mut TextMeasurer<'_>, _limits: SizeLimits) -> Size2 {
        Size2::new(40.0, 40.0)
    }

    fn input(&mut self, input: Input<'_>, hit: Hit) -> Outcome<Self::Action> {
        let Input::Pointer(pointer) = input else {
            return Outcome::IGNORED;
        };
        match pointer.phase {
            PointerPhase::Down if hit.over() => {
                if self.pending.is_empty() {
                    return Outcome::IGNORED;
                }
                self.pressed = true;
                self.long_pressed = false;
                self.recognized.borrow_mut().push(PointerPhase::Down);
                let action = matches!(self.pending.front(), Some(WheelAction::CellPress { .. }))
                    .then(|| self.take())
                    .flatten();
                let outcome = action.map_or_else(Outcome::captured, Outcome::set);
                outcome.with_ownership(PointerOwnership::Claim)
            }
            PointerPhase::Move if self.pressed && self.long_pressed => {
                self.recognized
                    .borrow_mut()
                    .push(PointerPhase::MoveLongPress);
                matches!(self.pending.front(), Some(WheelAction::ModeEdit { .. }))
                    .then(|| self.take())
                    .flatten()
                    .map_or(Outcome::IGNORED, Outcome::set)
            }
            PointerPhase::Up if self.pressed => {
                self.pressed = false;
                self.recognized.borrow_mut().push(PointerPhase::Up);
                matches!(self.pending.front(), Some(WheelAction::RoundRotate { .. }))
                    .then(|| self.take())
                    .flatten()
                    .map_or(Outcome::IGNORED, Outcome::set)
                    .with_ownership(PointerOwnership::Release)
            }
            PointerPhase::DoubleClick if hit.over() => {
                self.pressed = false;
                self.long_pressed = false;
                self.recognized.borrow_mut().push(PointerPhase::DoubleClick);
                matches!(self.pending.front(), Some(WheelAction::ModeReset { .. }))
                    .then(|| self.take())
                    .flatten()
                    .map_or(Outcome::IGNORED, Outcome::set)
                    .with_ownership(PointerOwnership::Release)
            }
            PointerPhase::Leave if self.pressed => {
                self.pressed = false;
                self.recognized.borrow_mut().push(PointerPhase::Leave);
                matches!(
                    self.pending.front(),
                    Some(WheelAction::ViewStateRequest { .. })
                )
                .then(|| self.take())
                .flatten()
                .map_or(Outcome::IGNORED, Outcome::set)
                .with_ownership(PointerOwnership::Release)
            }
            _ => Outcome::IGNORED,
        }
    }

    fn frame(&mut self, _elapsed: Duration) -> Option<Self::Action> {
        if !self.pressed || self.long_pressed {
            return None;
        }
        self.long_pressed = true;
        self.recognized.borrow_mut().push(PointerPhase::LongPress);
        matches!(
            self.pending.front(),
            Some(WheelAction::CellLongPress { .. })
        )
        .then(|| self.take())
        .flatten()
    }

    fn paint(&mut self, _list: &mut DrawListBuilder, _text: &mut TextMeasurer<'_>, _bounds: Rect) {}

    fn repaint(&self) -> Repaint {
        if self.pressed && !self.long_pressed {
            Repaint::Continuous
        } else {
            Repaint::None
        }
    }
}

type PointerObservations = Rc<RefCell<Vec<(PointerPhase, Option<Pt>, bool)>>>;

struct CaptureProbe {
    observations: PointerObservations,
}

type ScrollObservations = Rc<RefCell<Vec<(Scroll, Hit)>>>;

struct ScrollProbe {
    observations: ScrollObservations,
}

struct FrameProbe {
    paints: Rc<Cell<usize>>,
    pending: bool,
}

struct KeyProbe {
    observed: Rc<RefCell<Vec<&'static str>>>,
}

impl CustomWidget for KeyProbe {
    type Action = ();

    fn measure(&mut self, _text: &mut TextMeasurer<'_>, _limits: SizeLimits) -> Size2 {
        Size2::new(40.0, 40.0)
    }

    fn input(&mut self, input: Input<'_>, _hit: Hit) -> Outcome<Self::Action> {
        let event = match input {
            Input::ModifiersChanged(_) => "modifiers",
            Input::KeyPressed {
                key: NeutralKey::Enter,
                ..
            } => "key",
            _ => return Outcome::IGNORED,
        };
        self.observed.borrow_mut().push(event);
        Outcome::captured()
    }

    fn paint(&mut self, _list: &mut DrawListBuilder, _text: &mut TextMeasurer<'_>, _bounds: Rect) {}
}

impl CustomWidget for FrameProbe {
    type Action = ();

    fn measure(&mut self, _text: &mut TextMeasurer<'_>, _limits: SizeLimits) -> Size2 {
        Size2::new(40.0, 40.0)
    }

    fn frame(&mut self, _elapsed: Duration) -> Option<Self::Action> {
        self.pending = false;
        None
    }

    fn paint(&mut self, _list: &mut DrawListBuilder, _text: &mut TextMeasurer<'_>, _bounds: Rect) {
        self.paints.set(self.paints.get() + 1);
    }

    fn repaint(&self) -> Repaint {
        if self.pending {
            Repaint::NextFrame
        } else {
            Repaint::None
        }
    }
}

impl CustomWidget for ScrollProbe {
    type Action = ();

    fn measure(&mut self, _text: &mut TextMeasurer<'_>, _limits: SizeLimits) -> Size2 {
        Size2::new(40.0, 40.0)
    }

    fn input(&mut self, input: Input<'_>, hit: Hit) -> Outcome<Self::Action> {
        let Input::Wheel(scroll) = input else {
            return Outcome::IGNORED;
        };
        self.observations.borrow_mut().push((scroll, hit));
        Outcome::captured()
    }

    fn paint(&mut self, _list: &mut DrawListBuilder, _text: &mut TextMeasurer<'_>, _bounds: Rect) {}
}

impl CustomWidget for CaptureProbe {
    type Action = ();

    fn measure(&mut self, _text: &mut TextMeasurer<'_>, _limits: SizeLimits) -> Size2 {
        Size2::new(40.0, 40.0)
    }

    fn input(&mut self, input: Input<'_>, hit: Hit) -> Outcome<Self::Action> {
        let Input::Pointer(pointer) = input else {
            return Outcome::IGNORED;
        };
        self.observations
            .borrow_mut()
            .push((pointer.phase, pointer.at, hit.over()));
        if pointer.phase == PointerPhase::Down && hit.over() {
            return Outcome::captured().with_ownership(PointerOwnership::Claim);
        }
        if pointer.phase == PointerPhase::Up {
            return Outcome::IGNORED.with_ownership(PointerOwnership::Release);
        }
        Outcome::IGNORED
    }

    fn paint(&mut self, _list: &mut DrawListBuilder, _text: &mut TextMeasurer<'_>, _bounds: Rect) {}
}

#[kithara::test]
fn masonry_layout_rects_equal_snapped_neutral_rects() {
    let reads = FixtureReads;
    let registry = fixture_registry();
    let skin = Skin::resolve_with_font_policy(
        builtin::skin_doc().clone(),
        &SourceUri("fixture:masonry-layout-parity".to_owned()),
        FontPolicy::Embedded,
    )
    .unwrap_or_else(|error| panic!("builtin layout fixture skin must resolve: {error}"));

    for (preset, fixture) in [
        (
            builtin::MICRO_PRESET,
            include_str!("../../../tests/fixtures/layout/micro.rects"),
        ),
        (
            builtin::PLAYER_PRESET,
            include_str!("../../../tests/fixtures/layout/player.rects"),
        ),
    ] {
        let ui = compile(
            preset,
            &builtin::resolver(),
            &registry,
            builtin::skin_doc(),
            &UiConfig::default(),
        )
        .unwrap_or_else(|error| panic!("builtin layout must compile: {error}"));
        for (width, height) in [(1280, 720), (960, 600), (320, 240)] {
            let expected = fixture_section(fixture, preset, width, height);
            let output = document::render(
                &ui.root,
                &ui,
                &reads,
                builtin::skin_doc(),
                MasonryHost::new(&ui, &reads, &skin),
            );
            let ids = output.document_ids().to_vec();
            assert_eq!(
                ids.len(),
                expected.len(),
                "{preset} @ {width}x{height} did not retain exactly one real Masonry node per fixture path"
            );
            let mut raw_ids = ids.iter().map(|id| id.to_raw()).collect::<Vec<_>>();
            raw_ids.sort_unstable();
            raw_ids.dedup();
            assert_eq!(
                raw_ids.len(),
                ids.len(),
                "{preset} @ {width}x{height} reused a Masonry WidgetId"
            );
            let root = MasonryRoot::new(
                output,
                RenderRootOptions {
                    default_properties: Arc::new(default_property_set()),
                    use_system_fonts: false,
                    size_policy: WindowSizePolicy::User,
                    size: PhysicalSize::new(width, height),
                    scale_factor: 1.0,
                    test_font: None,
                },
            )
            .unwrap_or_else(|error| panic!("Masonry root must retain typed actions: {error}"));
            let root = root.root();

            for (id, expected) in ids.into_iter().zip(expected) {
                let widget = root.get_widget(id).unwrap_or_else(|| {
                    panic!(
                        "{preset} @ {width}x{height} path `{}` is not in the real Masonry tree",
                        expected.path
                    )
                });
                let transform = widget.ctx().window_transform().as_coeffs();
                assert_eq!(
                    [transform[0], transform[1], transform[2], transform[3]],
                    [1.0, 0.0, 0.0, 1.0],
                    "{preset} @ {width}x{height} path `{}` used affine compensation",
                    expected.path
                );
                let origin = widget.ctx().window_origin();
                let size = widget.ctx().size();
                let snapped_x = expected.x.round();
                let snapped_y = expected.y.round();
                let snapped_width = (expected.x + expected.width).round() - snapped_x;
                let snapped_height = (expected.y + expected.height).round() - snapped_y;
                assert_eq!(
                    [origin.x, origin.y, size.width, size.height],
                    [snapped_x, snapped_y, snapped_width, snapped_height],
                    "{preset} @ {width}x{height} path `{}` diverged from endpoint snapping of neutral rect {:?}",
                    expected.path,
                    [expected.x, expected.y, expected.width, expected.height]
                );
            }
        }
    }
}

#[kithara::test]
fn wheel_actions_round_trip_through_the_public_custom_contract() {
    let expected = wheel_actions();
    let recognized = Rc::new(RefCell::new(Vec::new()));
    let registry = fixture_registry();
    let ui = fixture_ui(
        "custom-fixture",
        r#"Row(size: (w: Fill, h: Fill), gap: 0.0, pad: 0.0, children: [
            Spacer(id: "custom", size: Some((w: Fixed(40.0), h: Fixed(40.0)))),
        ])"#,
        &registry,
    );
    let reads = FixtureReads;
    let host = MasonryHost::map_actions(&ui, &reads, builtin::skin(), TestAction::Document)
        .with_custom(
            "demo/custom",
            WheelEmitter::observed(expected.clone(), Rc::clone(&recognized)),
            TestAction::Wheel,
        );
    let output = document::render(&ui.root, &ui, &reads, builtin::skin_doc(), host);
    let mut root = masonry_root(output, 200, 120);

    root.handle_pointer_event(pointer_down(10.0, 50.0))
        .unwrap_or_else(|error| panic!("wheel press must remain typed: {error}"));
    root.handle_window_event(WindowEvent::AnimFrame(Duration::from_millis(600)))
        .unwrap_or_else(|error| panic!("wheel long press must remain typed: {error}"));
    root.handle_pointer_event(pointer_move(20.0, 55.0))
        .unwrap_or_else(|error| panic!("wheel long-press move must remain typed: {error}"));
    root.handle_pointer_event(pointer_up(20.0, 55.0))
        .unwrap_or_else(|error| panic!("wheel drag release must remain typed: {error}"));
    root.handle_pointer_event(pointer_down_with_count(10.0, 50.0, 2))
        .unwrap_or_else(|error| panic!("wheel double press must remain typed: {error}"));
    root.handle_pointer_event(pointer_up_with_count(10.0, 50.0, 2))
        .unwrap_or_else(|error| panic!("wheel double click must remain typed: {error}"));
    root.handle_pointer_event(pointer_down(10.0, 50.0))
        .unwrap_or_else(|error| panic!("wheel terminal press must remain typed: {error}"));
    root.handle_pointer_event(pointer_leave())
        .unwrap_or_else(|error| panic!("wheel leave must remain typed: {error}"));

    assert_eq!(
        root.take_actions(),
        expected
            .into_iter()
            .map(TestAction::Wheel)
            .collect::<Vec<_>>(),
        "all six consumer-owned variants must leave MasonryRoot with their concrete type and payload"
    );
    assert_eq!(
        *recognized.borrow(),
        [
            PointerPhase::Down,
            PointerPhase::LongPress,
            PointerPhase::MoveLongPress,
            PointerPhase::Up,
            PointerPhase::Down,
            PointerPhase::DoubleClick,
            PointerPhase::Down,
            PointerPhase::Leave,
        ],
        "consumer state must retain the gesture and recognize long-press phases without toolkit types"
    );
}

#[kithara::test]
fn a_frame_that_transitions_to_no_repaint_still_paints_its_result() {
    let registry = fixture_registry();
    let ui = fixture_ui(
        "custom-fixture",
        r#"Row(size: (w: Fill, h: Fill), gap: 0.0, pad: 0.0, children: [
            Spacer(id: "custom", size: Some((w: Fixed(40.0), h: Fixed(40.0)))),
        ])"#,
        &registry,
    );
    let reads = FixtureReads;
    let paints = Rc::new(Cell::new(0));
    let host = MasonryHost::map_actions(&ui, &reads, builtin::skin(), TestAction::Document)
        .with_custom(
            "demo/custom",
            FrameProbe {
                paints: Rc::clone(&paints),
                pending: true,
            },
            |()| TestAction::Document(UiEvent::OpenSettings),
        );
    let output = document::render(&ui.root, &ui, &reads, builtin::skin_doc(), host);
    let mut root = masonry_root(output, 200, 120);
    root.redraw()
        .unwrap_or_else(|error| panic!("initial frame probe paint must remain typed: {error}"));
    let before = paints.get();

    root.handle_window_event(WindowEvent::AnimFrame(Duration::from_millis(16)))
        .unwrap_or_else(|error| panic!("frame probe update must remain typed: {error}"));
    root.redraw()
        .unwrap_or_else(|error| panic!("completed frame probe paint must remain typed: {error}"));

    assert_eq!(paints.get(), before + 1);
    assert!(
        !root.root().needs_anim(),
        "NextFrame must paint the completed state without becoming continuous"
    );
}

#[kithara::test]
fn custom_mounting_replaces_an_actionable_controls_builtin_owner() {
    let registry = fixture_registry();
    let ui = fixture_ui(
        "gallery-knobs",
        r#"Row(size: (w: Fill, h: Fill), gap: 0.0, pad: 0.0, children: [
            Knob(
                id: "volume",
                size: Some((w: Fixed(40.0), h: Fixed(40.0))),
                read: Parameter(id: "player.output.volume"),
                write: Parameter(id: "player.output.volume"),
            ),
        ])"#,
        &registry,
    );
    let reads = FixtureReads;
    let host = MasonryHost::map_actions(&ui, &reads, builtin::skin(), TestAction::Document)
        .with_custom(
            "demo/volume",
            WheelEmitter::new(Vec::<WheelAction>::new()),
            TestAction::Wheel,
        );
    let output = document::render(&ui.root, &ui, &reads, builtin::skin_doc(), host);
    let mut root = masonry_root(output, 200, 120);

    assert_eq!(
        root.handle_pointer_event(pointer_down(10.0, 50.0))
            .unwrap_or_else(|error| panic!("custom replacement press must remain typed: {error}")),
        Handled::No,
        "an inert custom replacement must not fall through to the knob's built-in engine"
    );
    assert!(root.take_actions().is_empty());
}

#[kithara::test]
fn custom_pointer_capture_keeps_moves_after_the_pointer_leaves() {
    let registry = fixture_registry();
    let ui = fixture_ui(
        "custom-fixture",
        r#"Row(size: (w: Fill, h: Fill), gap: 0.0, pad: 0.0, children: [
            Spacer(id: "custom", size: Some((w: Fixed(40.0), h: Fixed(40.0)))),
        ])"#,
        &registry,
    );
    let reads = FixtureReads;
    let observations = PointerObservations::default();
    let host = MasonryHost::map_actions(&ui, &reads, builtin::skin(), TestAction::Document)
        .with_custom(
            "demo/custom",
            CaptureProbe {
                observations: Rc::clone(&observations),
            },
            |()| TestAction::Document(UiEvent::OpenSettings),
        );
    let output = document::render(&ui.root, &ui, &reads, builtin::skin_doc(), host);
    let mut root = masonry_root(output, 200, 120);
    root.take_platform_signals();

    assert_eq!(
        root.handle_pointer_event(pointer_down(10.0, 50.0))
            .unwrap_or_else(|error| panic!("pointer claim must remain typed: {error}")),
        Handled::Yes,
    );
    assert!(
        root.take_platform_signals()
            .into_iter()
            .all(|signal| !matches!(signal, RenderRootSignal::StartIme)),
        "a pointer-only custom component must not start a platform IME session"
    );
    assert_eq!(
        root.handle_pointer_event(pointer_move(150.0, 50.0))
            .unwrap_or_else(|error| panic!("captured move must remain typed: {error}")),
        Handled::Yes,
    );
    assert_eq!(
        root.handle_pointer_event(pointer_leave())
            .unwrap_or_else(|error| panic!("captured leave must remain typed: {error}")),
        Handled::Yes,
    );
    assert_eq!(
        root.handle_pointer_event(pointer_up(150.0, 50.0))
            .unwrap_or_else(|error| panic!("pointer release must remain typed: {error}")),
        Handled::Yes,
    );
    assert_eq!(
        root.handle_pointer_event(pointer_move(150.0, 50.0))
            .unwrap_or_else(|error| panic!("post-release move must remain typed: {error}")),
        Handled::No,
    );

    assert_eq!(
        *observations.borrow(),
        vec![
            (PointerPhase::Down, Some(Pt { x: 10.0, y: 50.0 }), true,),
            (PointerPhase::Move, Some(Pt { x: 150.0, y: 50.0 }), false,),
            (PointerPhase::Leave, None, false,),
            (PointerPhase::Up, Some(Pt { x: 150.0, y: 50.0 }), false,),
        ],
        "the claimant receives outside motion and leave until it explicitly gives the pointer back"
    );
    assert!(root.take_actions().is_empty());
}

#[kithara::test]
fn a_modifier_capturing_custom_still_receives_the_actual_key_packet() {
    let registry = fixture_registry();
    let ui = fixture_ui(
        "custom-fixture",
        r#"Row(size: (w: Fill, h: Fill), gap: 0.0, pad: 0.0, children: [
            Spacer(id: "custom", size: Some((w: Fixed(40.0), h: Fixed(40.0)))),
        ])"#,
        &registry,
    );
    let reads = FixtureReads;
    let observed = Rc::new(RefCell::new(Vec::new()));
    let host = MasonryHost::map_actions(&ui, &reads, builtin::skin(), TestAction::Document)
        .with_custom(
            "demo/custom",
            KeyProbe {
                observed: Rc::clone(&observed),
            },
            |()| TestAction::Document(UiEvent::OpenSettings),
        );
    let output = document::render(&ui.root, &ui, &reads, builtin::skin_doc(), host);
    let mut root = masonry_root(output, 200, 120);
    root.handle_pointer_event(pointer_down(10.0, 50.0))
        .unwrap_or_else(|error| panic!("custom focus press must remain typed: {error}"));

    assert_eq!(
        root.handle_text_event(TextEvent::key_down(Key::Named(NamedKey::Enter)))
            .unwrap_or_else(|error| panic!("custom key press must remain typed: {error}")),
        Handled::Yes,
    );
    assert_eq!(*observed.borrow(), ["modifiers", "key"]);
}

#[kithara::test]
fn custom_component_receives_neutral_line_and_pixel_wheel_input() {
    let registry = fixture_registry();
    let ui = fixture_ui(
        "custom-fixture",
        r#"Row(size: (w: Fill, h: Fill), gap: 0.0, pad: 0.0, children: [
            Spacer(id: "custom", size: Some((w: Fixed(40.0), h: Fixed(40.0)))),
        ])"#,
        &registry,
    );
    let reads = FixtureReads;
    let observations = ScrollObservations::default();
    let host = MasonryHost::map_actions(&ui, &reads, builtin::skin(), TestAction::Document)
        .with_custom(
            "demo/custom",
            ScrollProbe {
                observations: Rc::clone(&observations),
            },
            |()| TestAction::Document(UiEvent::OpenSettings),
        );
    let output = document::render(&ui.root, &ui, &reads, builtin::skin_doc(), host);
    let mut root = masonry_root(output, 200, 120);

    assert_eq!(
        root.handle_pointer_event(pointer_scroll(
            10.0,
            50.0,
            ScrollDelta::LineDelta(1.5, -2.0),
        ))
        .unwrap_or_else(|error| panic!("line wheel input must remain typed: {error}")),
        Handled::Yes,
    );
    assert_eq!(
        root.handle_pointer_event(pointer_scroll(
            10.0,
            50.0,
            ScrollDelta::PixelDelta(PhysicalPosition::new(-7.5, 4.25)),
        ))
        .unwrap_or_else(|error| panic!("pixel wheel input must remain typed: {error}")),
        Handled::Yes,
    );

    let observations = observations.borrow();
    assert_eq!(observations.len(), 2);
    assert_eq!(observations[0].0, Scroll::Lines { x: 1.5, y: -2.0 });
    assert_eq!(observations[1].0, Scroll::Pixels { x: -7.5, y: 4.25 });
    assert!(observations.iter().all(|(_, hit)| hit.over()));
}

#[kithara::test]
fn popover_layer_places_exactly_and_owns_inside_and_outside_presses() {
    let mut registry = fixture_registry();
    registry.insert(
        EndpointCategory::Model,
        "ui.menu.open",
        EndpointDesc::new(ValueKind::Bool),
    );
    let ui = fixture_ui_with_resize(
        "popover-fixture",
        r#"Row(size: (w: Fill, h: Fill), gap: 0.0, pad: 0.0, children: [
            Popover(
                id: "menu",
                open: Model(id: "ui.menu.open"),
                align: Start,
                anchor: Spacer(id: "anchor", size: Some((w: Fixed(40.0), h: Fixed(20.0)))),
                content: Spacer(id: "content", size: Some((w: Fixed(100.0), h: Fixed(60.0)))),
            ),
        ])"#,
        &registry,
    );
    let reads = FixtureReads;
    let host = MasonryHost::map_actions(&ui, &reads, builtin::skin(), TestAction::Document);
    let output = document::render(&ui.root, &ui, &reads, builtin::skin_doc(), host);
    let mut root = masonry_root(output, 200, 200);
    root.redraw()
        .unwrap_or_else(|error| panic!("popover redraw must retain typed actions: {error}"));

    {
        let layer = root.root().get_layer_root(1);
        assert_eq!(layer.ctx().size(), MasonrySize::new(200.0, 200.0));
        let children = layer.children();
        assert_eq!(children.len(), 1, "the native layer has one content root");
        let content = children[0];
        assert_eq!(content.ctx().size(), MasonrySize::new(100.0, 60.0));
        assert_eq!(
            content.ctx().window_origin(),
            Point::new(1.0, 113.0),
            "the surface begins at (0, 110): one border pixel and a two-pixel cap precede content"
        );
    }

    assert_eq!(
        root.handle_pointer_event(pointer_down(50.0, 130.0))
            .unwrap_or_else(|error| panic!("inside popover press must remain typed: {error}")),
        Handled::Yes,
    );
    assert_eq!(
        root.handle_pointer_event(pointer_up(50.0, 130.0))
            .unwrap_or_else(|error| panic!("inside popover release must remain typed: {error}")),
        Handled::No,
    );
    assert!(
        root.take_actions().is_empty(),
        "content owns presses inside the exact popover surface"
    );

    assert_eq!(
        root.handle_pointer_event(pointer_down(199.0, 130.0))
            .unwrap_or_else(|error| panic!("topmost resize press must remain typed: {error}")),
        Handled::Yes,
    );
    assert_eq!(
        root.take_actions(),
        vec![TestAction::Document(UiEvent::Window(
            WindowCommand::Resize(WindowEdge::East),
        ))],
        "the topmost window edge must win without also dismissing the open popover"
    );
    root.handle_pointer_event(pointer_up(199.0, 130.0))
        .unwrap_or_else(|error| panic!("resize release must remain typed: {error}"));

    assert_eq!(
        root.handle_pointer_event(pointer_down(150.0, 130.0))
            .unwrap_or_else(|error| panic!("outside dismiss must remain typed: {error}")),
        Handled::Yes,
    );
    assert_eq!(
        root.take_actions(),
        vec![TestAction::Document(UiEvent::Control {
            path: "demo/menu".to_owned(),
            action: ControlAction::Activate,
        })],
        "the layer owns an outside press and emits the neutral dismiss event once"
    );
}

#[kithara::test]
fn pointer_popover_retains_each_opening_origin_across_rebuilds() {
    let mut registry = fixture_registry();
    registry.insert(
        EndpointCategory::Model,
        "ui.menu.open",
        EndpointDesc::new(ValueKind::Bool),
    );
    let ui = fixture_ui_with_resize(
        "popover-fixture",
        r#"Row(size: (w: Fill, h: Fill), gap: 0.0, pad: 0.0, children: [
            Popover(
                id: "menu",
                open: Model(id: "ui.menu.open"),
                at: Pointer,
                align: Start,
                anchor: Spacer(id: "anchor", size: Some((w: Fixed(40.0), h: Fixed(20.0)))),
                content: Spacer(id: "content", size: Some((w: Fixed(100.0), h: Fixed(60.0)))),
            ),
        ])"#,
        &registry,
    );
    let state = MasonryState::default();
    let closed = PopoverReads { open: false };
    let output = document::render(
        &ui.root,
        &ui,
        &closed,
        builtin::skin_doc(),
        MasonryHost::new(&ui, &closed, builtin::skin()).with_state(state.clone()),
    );
    let mut root = masonry_root(output, 200, 200);
    root.handle_pointer_event(pointer_down(70.0, 95.0))
        .unwrap_or_else(|error| panic!("opening press must remain typed: {error}"));

    let open = PopoverReads { open: true };
    let output = document::render(
        &ui.root,
        &ui,
        &open,
        builtin::skin_doc(),
        MasonryHost::new(&ui, &open, builtin::skin()).with_state(state.clone()),
    );
    let mut root = masonry_root(output, 200, 200);
    root.redraw()
        .unwrap_or_else(|error| panic!("pointer popover redraw must remain typed: {error}"));

    let layer = root.root().get_layer_root(1);
    let children = layer.children();
    assert_eq!(children.len(), 1, "the pointer layer has one content root");
    assert_eq!(
        children[0].ctx().window_origin(),
        Point::new(70.0, 98.0),
        "the latest pointer press owns the surface origin across the closed-to-open rebuild"
    );

    let output = document::render(
        &ui.root,
        &ui,
        &closed,
        builtin::skin_doc(),
        MasonryHost::new(&ui, &closed, builtin::skin()).with_state(state.clone()),
    );
    let mut root = masonry_root(output, 200, 200);
    assert_eq!(
        root.handle_text_event(TextEvent::key_down(Key::Named(NamedKey::Escape)))
            .unwrap_or_else(|error| panic!("closed Escape must remain typed: {error}")),
        Handled::No,
        "a closed popover must give Escape back to the document"
    );
    assert!(root.take_actions().is_empty());
    root.handle_pointer_event(pointer_down(10.0, 100.0))
        .unwrap_or_else(|error| panic!("second opening press must remain typed: {error}"));
    root.handle_pointer_event(pointer_up(10.0, 100.0))
        .unwrap_or_else(|error| panic!("second opening release must remain typed: {error}"));
    root.handle_pointer_event(pointer_down(199.0, 100.0))
        .unwrap_or_else(|error| panic!("resize press must remain typed: {error}"));
    root.handle_pointer_event(pointer_up(199.0, 100.0))
        .unwrap_or_else(|error| panic!("resize release must remain typed: {error}"));

    let output = document::render(
        &ui.root,
        &ui,
        &open,
        builtin::skin_doc(),
        MasonryHost::new(&ui, &open, builtin::skin()).with_state(state),
    );
    let mut root = masonry_root(output, 200, 200);
    root.redraw()
        .unwrap_or_else(|error| panic!("reopened pointer popover must remain typed: {error}"));
    let layer = root.root().get_layer_root(1);
    let children = layer.children();
    assert_eq!(children.len(), 1, "the reopened layer has one content root");
    assert_eq!(
        children[0].ctx().window_origin(),
        Point::new(10.0, 103.0),
        "a reopen must consume the second content press without banking the later window-owned resize press"
    );
}

#[kithara::test]
fn hosted_module_retains_its_engine_owned_knob_outside_the_control() {
    let registry = fixture_registry();
    let ui = fixture_ui(
        "gallery-knobs",
        r#"Row(size: (w: Fill, h: Fill), gap: 0.0, pad: 0.0, children: [
            Knob(
                id: "volume",
                size: Some((w: Fixed(40.0), h: Fixed(40.0))),
                read: Parameter(id: "player.output.volume"),
                write: Parameter(id: "player.output.volume"),
            ),
        ])"#,
        &registry,
    );
    let reads = FixtureReads;
    let host = MasonryHost::map_actions(&ui, &reads, builtin::skin(), TestAction::Document);
    let output = document::render(&ui.root, &ui, &reads, builtin::skin_doc(), host);
    let mut root = masonry_root(output, 200, 120);

    root.take_platform_signals();
    assert_eq!(
        root.handle_pointer_event(pointer_hover(10.0, 50.0))
            .unwrap_or_else(|error| panic!("engine-owned hover must remain typed: {error}")),
        Handled::No,
    );
    assert!(
        root.take_platform_signals()
            .iter()
            .any(|signal| matches!(signal, RenderRootSignal::SetCursor(CursorIcon::NsResize))),
        "the engine-owned knob must expose the shared vertical-resize cursor"
    );
    assert_eq!(
        root.handle_pointer_event(pointer_down(10.0, 50.0))
            .unwrap_or_else(|error| panic!("engine-owned input must remain typed: {error}")),
        Handled::Yes,
    );
    assert!(
        root.take_actions().is_empty(),
        "arming a relative knob must not flatten it into activation"
    );
    assert_eq!(
        root.handle_pointer_event(pointer_move(150.0, 43.0))
            .unwrap_or_else(|error| panic!("captured engine move must remain typed: {error}")),
        Handled::Yes,
    );
    assert_scalar_value(&root.take_actions(), "demo/volume", 0.85);
    assert_eq!(
        root.handle_pointer_event(pointer_up(150.0, 43.0))
            .unwrap_or_else(|error| panic!("engine release must remain typed: {error}")),
        Handled::Yes,
    );
    assert_eq!(
        root.handle_pointer_event(pointer_move(150.0, 43.0))
            .unwrap_or_else(|error| panic!("released engine move must remain typed: {error}")),
        Handled::No,
    );
    assert!(root.take_actions().is_empty());
}

/// The settings button has no endpoint to activate: pressing it says something
/// to the document instead. The leaf that draws it also says it, so the answer
/// does not depend on a second wiring the two hosts could disagree about.
#[kithara::test]
fn the_settings_button_leaf_opens_settings_on_press() {
    let registry = fixture_registry();
    let ui = fixture_ui(
        "settings-fixture",
        r#"Row(size: (w: Fill, h: Fill), gap: 0.0, pad: 0.0, children: [
            SettingsButton(id: "settings", size: Some((w: Fixed(40.0), h: Fixed(40.0)))),
        ])"#,
        &registry,
    );
    let reads = FixtureReads;
    let host = MasonryHost::map_actions(&ui, &reads, builtin::skin(), TestAction::Document);
    let output = document::render(&ui.root, &ui, &reads, builtin::skin_doc(), host);
    let mut root = masonry_root(output, 200, 120);

    assert_eq!(
        root.handle_pointer_event(pointer_down(10.0, 50.0))
            .unwrap_or_else(|error| panic!("settings press must remain typed: {error}")),
        Handled::Yes,
    );
    assert_eq!(
        root.take_actions(),
        vec![TestAction::Document(UiEvent::OpenSettings)]
    );

    root.handle_pointer_event(pointer_up(10.0, 50.0))
        .unwrap_or_else(|error| panic!("settings release must remain typed: {error}"));
    assert!(
        root.take_actions().is_empty(),
        "the press opened settings; letting go must not open them a second time"
    );
}

#[kithara::test]
fn leaf_owned_knob_uses_scalar_drag_wheel_reset_and_cursor() {
    let registry = fixture_registry();
    let ui = fixture_ui(
        "leaf-fixture",
        r#"Row(size: (w: Fill, h: Fill), gap: 0.0, pad: 0.0, children: [
            Knob(
                id: "volume",
                size: Some((w: Fixed(40.0), h: Fixed(40.0))),
                read: Parameter(id: "player.output.volume"),
                write: Parameter(id: "player.output.volume"),
            ),
        ])"#,
        &registry,
    );
    let reads = FixtureReads;
    let host = MasonryHost::map_actions(&ui, &reads, builtin::skin(), TestAction::Document);
    let output = document::render(&ui.root, &ui, &reads, builtin::skin_doc(), host);
    let mut root = masonry_root(output, 200, 120);

    root.take_platform_signals();
    assert_eq!(
        root.handle_pointer_event(pointer_hover(10.0, 50.0))
            .unwrap_or_else(|error| panic!("leaf-owned hover must remain typed: {error}")),
        Handled::No,
    );
    assert!(
        root.take_platform_signals()
            .iter()
            .any(|signal| matches!(signal, RenderRootSignal::SetCursor(CursorIcon::NsResize))),
        "the leaf-owned knob must expose the shared vertical-resize cursor"
    );
    assert_eq!(
        root.handle_pointer_event(pointer_scroll(
            10.0,
            50.0,
            ScrollDelta::LineDelta(0.0, -1.0),
        ))
        .unwrap_or_else(|error| panic!("leaf-owned wheel must remain typed: {error}")),
        Handled::Yes,
    );
    assert_scalar_value(
        &root.take_actions(),
        "demo/volume",
        builtin::skin().knob.wheel_step.mul_add(1.0, 0.8),
    );

    assert_eq!(
        root.handle_pointer_event(pointer_down(10.0, 50.0))
            .unwrap_or_else(|error| panic!("first reset press must remain typed: {error}")),
        Handled::Yes,
    );
    assert_eq!(
        root.handle_pointer_event(pointer_up(10.0, 50.0))
            .unwrap_or_else(|error| panic!("first reset release must remain typed: {error}")),
        Handled::Yes,
    );
    assert!(root.take_actions().is_empty());
    assert_eq!(
        root.handle_pointer_event(pointer_down_with_count(10.0, 50.0, 2))
            .unwrap_or_else(|error| panic!("second reset press must remain typed: {error}")),
        Handled::Yes,
    );
    assert_scalar_value(&root.take_actions(), "demo/volume", 0.5);
    assert_eq!(
        root.handle_pointer_event(pointer_up_with_count(10.0, 50.0, 2))
            .unwrap_or_else(|error| panic!("double-click release must remain typed: {error}")),
        Handled::No,
    );
    assert!(root.take_actions().is_empty());

    assert_eq!(
        root.handle_pointer_event(pointer_down(10.0, 50.0))
            .unwrap_or_else(|error| panic!("leaf-owned press must remain typed: {error}")),
        Handled::Yes,
    );
    assert!(
        root.take_actions().is_empty(),
        "the leaf-owned relative knob must arm without publishing"
    );
    assert_eq!(
        root.handle_pointer_event(pointer_move(150.0, 43.0))
            .unwrap_or_else(|error| panic!("captured leaf move must remain typed: {error}")),
        Handled::Yes,
    );
    // The double click above reset the knob, so it draws 0.5 and the drag that
    // follows counts from there — not from the 0.8 its endpoint reported when
    // the tree was built and has not reported since.
    assert_scalar_value(
        &root.take_actions(),
        "demo/volume",
        0.5 + (50.0 - 43.0) / builtin::skin().knob.drag_range,
    );
    assert_eq!(
        root.handle_pointer_event(pointer_up(150.0, 43.0))
            .unwrap_or_else(|error| panic!("leaf-owned release must remain typed: {error}")),
        Handled::Yes,
    );
    assert_eq!(
        root.handle_pointer_event(pointer_move(150.0, 43.0))
            .unwrap_or_else(|error| panic!("released leaf move must remain typed: {error}")),
        Handled::No,
    );
    assert!(root.take_actions().is_empty());
}

#[kithara::test]
fn double_click_terminates_engine_and_leaf_owned_scalar_capture() {
    let registry = fixture_registry();
    let reads = FixtureReads;
    for module_id in ["gallery-knobs", "leaf-fixture"] {
        let ui = fixture_ui(
            module_id,
            r#"Row(size: (w: Fill, h: Fill), gap: 0.0, pad: 0.0, children: [
                Knob(
                    id: "volume",
                    size: Some((w: Fixed(40.0), h: Fixed(40.0))),
                    read: Parameter(id: "player.output.volume"),
                    write: Parameter(id: "player.output.volume"),
                ),
            ])"#,
            &registry,
        );
        let host = MasonryHost::map_actions(&ui, &reads, builtin::skin(), TestAction::Document);
        let output = document::render(&ui.root, &ui, &reads, builtin::skin_doc(), host);
        let mut root = masonry_root(output, 200, 120);

        assert_eq!(
            root.handle_pointer_event(pointer_down_with_count(10.0, 50.0, 2))
                .unwrap_or_else(|error| panic!("{module_id} second press must route: {error}")),
            Handled::Yes,
        );
        assert_eq!(
            root.handle_pointer_event(pointer_up_with_count(10.0, 50.0, 2))
                .unwrap_or_else(|error| panic!("{module_id} double click must route: {error}")),
            Handled::Yes,
        );
        assert!(root.take_actions().is_empty());
        assert_eq!(
            root.handle_pointer_event(pointer_move(20.0, 40.0))
                .unwrap_or_else(|error| panic!("{module_id} released move must route: {error}")),
            Handled::No,
            "DoubleClick is terminal and must release both the retained recognizer and Masonry capture"
        );
        assert!(root.take_actions().is_empty());
    }
}

#[kithara::test]
fn picker_portal_honours_engine_and_leaf_owners_beneath_the_root_window_layer() {
    let registry = fixture_registry();
    let reads = FixtureReads;
    for module_id in ["gallery-library2-tab", "leaf-fixture"] {
        let ui = fixture_ui_with_resize(
            module_id,
            r#"Row(size: (w: Fill, h: Fill), gap: 0.0, pad: 0.0, children: [
                ContextBar(
                    id: "context",
                    size: Some((w: Fill, h: Fixed(26.0))),
                    read: Model(id: "library.breadcrumb"),
                    write: Model(id: "library.scope"),
                    scope_items: ["ZVUK", "LOCAL"],
                    scope: Model(id: "library.scope"),
                ),
            ])"#,
            &registry,
        );
        let host = MasonryHost::map_actions(&ui, &reads, builtin::skin(), TestAction::Document);
        let output = document::render(&ui.root, &ui, &reads, builtin::skin_doc(), host);
        let control_id = *output
            .document_ids()
            .last()
            .unwrap_or_else(|| panic!("{module_id} picker must have a real control node"));
        let mut root = masonry_root(output, 200, 120);
        root.redraw()
            .unwrap_or_else(|error| panic!("{module_id} picker must compose: {error}"));

        let bounds = root
            .root()
            .get_widget(control_id)
            .unwrap_or_else(|| panic!("{module_id} picker node must stay registered"))
            .ctx()
            .bounding_rect();
        let skin = builtin::skin();
        let bounds_x: f32 = bounds.x0.as_();
        let bounds_y: f32 = bounds.y0.as_();
        // Where the strip drew its face, asked of the painter that drew it —
        // an anchor worked out a second time here would agree with the host
        // only until one of the two changed.
        let mut text = TextContext::from(skin.text_resources());
        let anchor = Context::placed(
            Context::new(skin).face_of(&mut text, ["ZVUK", "LOCAL"]),
            Rect {
                h: 0.0,
                w: 0.0,
                x: bounds_x,
                y: bounds_y,
            },
        );
        let center = |area: Rect| (area.x + area.w / 2.0, area.y + area.h / 2.0);
        let (anchor_x, anchor_y) = center(anchor);
        assert_eq!(
            root.handle_pointer_event(pointer_down(anchor_x.into(), anchor_y.into()))
                .unwrap_or_else(|error| panic!("{module_id} picker must open: {error}")),
            Handled::Yes,
        );
        root.handle_pointer_event(pointer_up(anchor_x.into(), anchor_y.into()))
            .unwrap_or_else(|error| panic!("{module_id} picker release must route: {error}"));
        assert!(root.take_actions().is_empty());

        let option = picker_hits(anchor, skin.tree.scope_item_height, 2)
            .into_iter()
            .next()
            .map_or_else(
                || panic!("{module_id} picker must expose its first option"),
                |region| region.area(),
            );
        let (option_x, option_y) = center(option);
        assert_eq!(
            root.handle_pointer_event(pointer_down(option_x.into(), option_y.into()))
                .unwrap_or_else(|error| panic!("{module_id} option must route: {error}")),
            Handled::Yes,
        );
        assert_eq!(
            root.take_actions(),
            vec![TestAction::Document(UiEvent::Control {
                path: "demo/context".to_owned(),
                action: ControlAction::SelectIndex(0),
            })],
            "{module_id} must preserve the typed option action outside the anchor"
        );

        root.handle_pointer_event(pointer_up(option_x.into(), option_y.into()))
            .unwrap_or_else(|error| panic!("{module_id} option release must route: {error}"));
        root.handle_pointer_event(pointer_down(anchor_x.into(), anchor_y.into()))
            .unwrap_or_else(|error| panic!("{module_id} picker must reopen: {error}"));
        root.handle_pointer_event(pointer_up(anchor_x.into(), anchor_y.into()))
            .unwrap_or_else(|error| panic!("{module_id} picker release must route: {error}"));
        assert_eq!(
            root.handle_pointer_event(pointer_down(199.0, 60.0))
                .unwrap_or_else(|error| panic!("{module_id} root resize must route: {error}")),
            Handled::Yes,
        );
        assert_eq!(
            root.take_actions(),
            vec![TestAction::Document(UiEvent::Window(
                WindowCommand::Resize(WindowEdge::East),
            ))],
            "{module_id} root resize must answer before the open picker"
        );
        assert_eq!(
            root.handle_pointer_event(pointer_move(150.0, 60.0))
                .unwrap_or_else(|error| panic!(
                    "{module_id} captured resize move must route: {error}"
                )),
            Handled::Yes,
            "the real Masonry capture target must keep priority over the picker portal"
        );
        root.handle_pointer_event(pointer_up(150.0, 60.0))
            .unwrap_or_else(|error| panic!("{module_id} resize release must route: {error}"));
        assert_eq!(
            root.handle_pointer_event(pointer_down(150.0, 60.0))
                .unwrap_or_else(|error| panic!("{module_id} picker dismiss must route: {error}")),
            Handled::Yes,
        );
        assert!(
            root.take_actions().is_empty(),
            "{module_id} picker must still own a non-window outside dismissal"
        );
        assert_eq!(
            root.root().focused_widget(),
            None,
            "outside dismissal must resign the picker owner's Masonry focus"
        );
        root.handle_pointer_event(pointer_up(150.0, 60.0))
            .unwrap_or_else(|error| {
                panic!("{module_id} picker dismiss release must route: {error}")
            });
        root.handle_pointer_event(pointer_down(anchor_x.into(), anchor_y.into()))
            .unwrap_or_else(|error| panic!("{module_id} keyboard picker must open: {error}"));
        root.handle_pointer_event(pointer_up(anchor_x.into(), anchor_y.into()))
            .unwrap_or_else(|error| {
                panic!("{module_id} keyboard picker release must route: {error}")
            });
        assert_eq!(
            root.handle_text_event(TextEvent::key_down(Key::Named(NamedKey::ArrowDown)))
                .unwrap_or_else(|error| panic!("{module_id} picker arrow must route: {error}")),
            Handled::Yes,
        );
        assert_eq!(
            root.handle_text_event(TextEvent::key_down(Key::Named(NamedKey::Enter)))
                .unwrap_or_else(|error| panic!("{module_id} picker Enter must route: {error}")),
            Handled::Yes,
        );
        assert_eq!(
            root.take_actions(),
            vec![TestAction::Document(UiEvent::Control {
                path: "demo/context".to_owned(),
                action: ControlAction::SelectIndex(1),
            })],
            "{module_id} must preserve engine focus and typed keyboard selection"
        );
    }
}

#[kithara::test]
fn titlebar_window_layer_honours_engine_and_leaf_owned_modules() {
    let registry = fixture_registry();
    let reads = FixtureReads;
    for module_id in ["gallery-buttons-tab", "leaf-fixture"] {
        let ui = fixture_ui(
            module_id,
            r#"Row(size: (w: Fill, h: Fill), gap: 0.0, pad: 0.0, children: [
                TitleBar(
                    id: "title",
                    label: "KITHARA",
                    size: Some((w: Fill, h: Fixed(40.0))),
                ),
            ])"#,
            &registry,
        );
        let host = MasonryHost::map_actions(&ui, &reads, builtin::skin(), TestAction::Document);
        let output = document::render(&ui.root, &ui, &reads, builtin::skin_doc(), host);
        let mut root = masonry_root(output, 200, 120);
        root.redraw()
            .unwrap_or_else(|error| panic!("{module_id} titlebar must compose: {error}"));

        assert_eq!(
            root.handle_pointer_event(pointer_down(80.0, 60.0))
                .unwrap_or_else(|error| panic!("{module_id} titlebar press must route: {error}")),
            Handled::Yes,
        );
        assert_eq!(
            root.take_actions(),
            vec![TestAction::Document(UiEvent::Window(WindowCommand::Drag))],
            "{module_id} must retain the neutral window action in its native leaf layer"
        );
    }
}

#[kithara::test]
fn outer_resize_layer_precedes_content_only_on_the_window_edge() {
    let registry = fixture_registry();
    let ui = fixture_ui_with_resize(
        "custom-fixture",
        r#"Row(size: (w: Fill, h: Fill), gap: 0.0, pad: 0.0, children: [
            Spacer(id: "custom", size: Some((w: Fixed(40.0), h: Fixed(40.0)))),
        ])"#,
        &registry,
    );
    let reads = FixtureReads;
    let observations = PointerObservations::default();
    let host = MasonryHost::map_actions(&ui, &reads, builtin::skin(), TestAction::Document)
        .with_custom(
            "demo/custom",
            CaptureProbe {
                observations: Rc::clone(&observations),
            },
            |()| TestAction::Document(UiEvent::OpenSettings),
        );
    let output = document::render(&ui.root, &ui, &reads, builtin::skin_doc(), host);
    let mut root = masonry_root(output, 200, 120);

    assert_eq!(
        root.handle_pointer_event(pointer_down(0.0, 50.0))
            .unwrap_or_else(|error| panic!("window-edge press must remain typed: {error}")),
        Handled::Yes,
    );
    assert_eq!(
        root.take_actions(),
        vec![TestAction::Document(UiEvent::Window(
            WindowCommand::Resize(WindowEdge::West),
        ))],
        "the outer layer must own the west edge before document content"
    );
    assert!(
        observations.borrow().is_empty(),
        "the custom leaf behind the resize edge must not also see the press"
    );
    root.handle_pointer_event(pointer_up(0.0, 50.0))
        .unwrap_or_else(|error| panic!("window-edge release must remain typed: {error}"));

    assert_eq!(
        root.handle_pointer_event(pointer_down(5.0, 50.0))
            .unwrap_or_else(|error| panic!(
                "content press inside the edge must remain typed: {error}"
            )),
        Handled::Yes,
    );
    assert_eq!(
        *observations.borrow(),
        vec![(PointerPhase::Down, Some(Pt { x: 5.0, y: 50.0 }), true,)],
        "one pixel inside the four-pixel resize frame must return ownership to document content"
    );
    assert!(root.take_actions().is_empty());
}

fn wheel_actions() -> Vec<WheelAction> {
    vec![
        WheelAction::CellPress {
            round: 1,
            cell: 3,
            regularity: Regularity::Regular,
            active: true,
        },
        WheelAction::CellLongPress {
            round: 2,
            cell: 5,
            regularity: Regularity::Irregular,
            active: false,
        },
        WheelAction::ModeEdit {
            round: 3,
            cell: 7,
            mode: Mode::Velocity,
            value: 96,
        },
        WheelAction::RoundRotate {
            round: 4,
            rotation: Rotation(-17),
        },
        WheelAction::ModeReset {
            round: 5,
            cell: 11,
            mode: Mode::Probability,
        },
        WheelAction::ViewStateRequest {
            view_state: ViewState {
                mode: Mode::Probability,
                selected_round: 6,
            },
        },
    ]
}

fn assert_scalar_value(actions: &[TestAction], path: &str, expected: f32) {
    let [
        TestAction::Document(UiEvent::Control {
            path: actual,
            action: ControlAction::SetScalar(value),
        }),
    ] = actions
    else {
        panic!("a retained knob gesture must emit exactly one typed scalar action: {actions:?}");
    };
    assert_eq!(actual, path);
    assert_eq!(*value, f64::from(expected));
}

/// Whether this host has a painter for a control, or still mounts it as a
/// correctly-sized empty box.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Paints {
    Yes,
    NotYet,
    /// There is no picture to draw. A window-drag region is a place the hand
    /// grabs the window by, and the immediate host draws nothing for it either,
    /// so an empty scene here is the control working rather than a gap. That
    /// claim is checked by `a_window_drag_region_has_no_picture_on_either_host`
    /// rather than taken on trust.
    Nothing,
}

impl Paints {
    const fn draws(self) -> bool {
        matches!(self, Self::Yes)
    }
}

/// Every control the shared base draws, and whether Masonry draws it today.
///
/// The `NotYet` rows are the remaining work, stated once and checked rather
/// than described. A control that starts drawing fails this test until its row
/// moves to `Yes`; a control that stops drawing fails it immediately.
///
/// Every `ControlSpec` variant has a row. A census that covered only the
/// controls someone remembered to add left the rest invisible: not drawn, and
/// not reported as undrawn either.
const CONTROL_CENSUS: &[(&str, Paints, &str)] = &[
    ("Brand", Paints::Yes, r#"Brand(id: "control")"#),
    ("Spacer", Paints::Yes, r#"Spacer(id: "control")"#),
    ("Divider", Paints::Yes, r#"Divider(id: "control")"#),
    (
        "PresetSelector",
        Paints::NotYet,
        r#"PresetSelector(id: "control")"#,
    ),
    (
        "SettingsButton",
        Paints::Yes,
        r#"SettingsButton(id: "control")"#,
    ),
    ("DeckSummary", Paints::Yes, r#"DeckSummary(id: "control")"#),
    (
        "WindowDrag",
        Paints::Nothing,
        r#"WindowDrag(id: "control")"#,
    ),
    (
        "TitleBar",
        Paints::Yes,
        r#"TitleBar(id: "control", label: "KITHARA")"#,
    ),
    (
        "WindowControls",
        Paints::Yes,
        r#"WindowControls(id: "control")"#,
    ),
    (
        // The placeholder names which stand-in a deck shows when no tempo was
        // measured, not a word to display; `time` is the one the shipped decks
        // ask for.
        "Bpm",
        Paints::Yes,
        r#"Bpm(id: "control", placeholder: Some("time"))"#,
    ),
    (
        "Time",
        Paints::Yes,
        r#"Time(id: "control", read: Model(id: "deck.view.zoom"))"#,
    ),
    (
        "Scalar",
        Paints::Yes,
        r#"Scalar(id: "control", read: Model(id: "deck.view.zoom"))"#,
    ),
    ("Wave", Paints::Yes, r#"Wave(id: "control")"#),
    ("Vis", Paints::NotYet, r#"Vis(id: "control")"#),
    (
        "TrackList",
        Paints::NotYet,
        r#"TrackList(id: "control", read: Model(id: "library.visible_tracks"), columns: Some([Title]))"#,
    ),
    ("Tree", Paints::NotYet, r#"Tree(id: "control")"#),
    (
        // The path in view is the strip's own reading, and the fixture never
        // bound one: a strip with no path names nothing, so it drew nothing
        // for a reason that had nothing to do with this host.
        "ContextBar",
        Paints::Yes,
        r#"ContextBar(id: "control", read: Model(id: "library.breadcrumb"), scope_items: ["ALL", "MINE"], scope: Model(id: "library.scope"), write: Model(id: "library.scope"))"#,
    ),
    (
        "Text",
        Paints::Yes,
        r#"Text(id: "control", label: Some("HELLO"))"#,
    ),
    (
        "Knob",
        Paints::Yes,
        r#"Knob(id: "control", read: Parameter(id: "player.output.volume"), write: Parameter(id: "player.output.volume"))"#,
    ),
    (
        "Chip",
        Paints::Yes,
        r#"Chip(id: "control", label: "A", read: Model(id: "ui.menu.open"))"#,
    ),
    (
        "NavItem",
        Paints::Yes,
        r#"NavItem(id: "control", label: "LIBRARY", icon: Playlist, read: Model(id: "ui.menu.open"))"#,
    ),
    (
        "Button",
        Paints::Yes,
        r#"Button(id: "control", label: "PLAY", read: Model(id: "ui.menu.open"))"#,
    ),
    (
        "Glyph",
        Paints::Yes,
        r#"Glyph(id: "control", icon: Playlist)"#,
    ),
    (
        "TabLarge",
        Paints::Yes,
        r#"TabLarge(id: "control", label: "MIXER", read: Model(id: "ui.menu.open"))"#,
    ),
    (
        "Toggle",
        Paints::Yes,
        r#"Toggle(id: "control", read: Model(id: "ui.menu.open"))"#,
    ),
    (
        "Checkbox",
        Paints::Yes,
        r#"Checkbox(id: "control", read: Model(id: "ui.menu.open"))"#,
    ),
    (
        "Segmented",
        Paints::Yes,
        r#"Segmented(id: "control", items: ["A", "B"], read: Model(id: "library.scope"))"#,
    ),
    (
        "Select",
        Paints::Yes,
        r#"Select(id: "control", label: "QUALITY")"#,
    ),
    (
        "StatusDot",
        Paints::Yes,
        r#"StatusDot(id: "control", label: "LIVE")"#,
    ),
    (
        "Swatch",
        Paints::Yes,
        r#"Swatch(id: "control", role: Accent, label: "ACCENT")"#,
    ),
    (
        "Cell",
        Paints::Yes,
        r#"Cell(id: "control", label: Some("A1"))"#,
    ),
    (
        "Readout",
        Paints::Yes,
        r#"Readout(id: "control", label: Some("BPM"), read: Model(id: "library.breadcrumb"))"#,
    ),
    (
        "Meter",
        Paints::Yes,
        r#"Meter(id: "control", read: Model(id: "deck.view.zoom"))"#,
    ),
    (
        "VuVertical",
        Paints::Yes,
        r#"VuVertical(id: "control", read: Telemetry(id: "player.output.levels"))"#,
    ),
    (
        "VuStereo",
        Paints::Yes,
        r#"VuStereo(id: "control", read: Telemetry(id: "player.output.levels"))"#,
    ),
    (
        "Fader",
        Paints::Yes,
        r#"Fader(id: "control", read: Parameter(id: "player.output.volume"), write: Parameter(id: "player.output.volume"))"#,
    ),
    (
        "Crossfader",
        Paints::Yes,
        r#"Crossfader(id: "control", read: Parameter(id: "player.output.volume"), write: Parameter(id: "player.output.volume"))"#,
    ),
];

/// Mounts one control on its own and asks Masonry to draw it. A document that
/// holds nothing else has nothing else to contribute, so an empty scene means
/// that control drew nothing.
#[kithara::test]
fn masonry_draws_every_control_the_census_claims_it_draws() {
    let mut registry = fixture_registry();
    registry.insert(
        EndpointCategory::Model,
        "ui.menu.open",
        EndpointDesc::new(ValueKind::Bool),
    );
    let reads = FixtureReads;
    let skin = Skin::resolve_with_font_policy(
        builtin::skin_doc().clone(),
        &SourceUri("fixture:masonry-control-census".to_owned()),
        FontPolicy::Embedded,
    )
    .unwrap_or_else(|error| panic!("the census skin must resolve: {error}"));
    let observed = CONTROL_CENSUS
        .iter()
        .map(|(name, _, control)| {
            let ui = fixture_ui(
                "census",
                &format!(
                    r#"Row(size: (w: Fill, h: Fill), gap: 0.0, pad: 0.0, children: [{control}])"#
                ),
                &registry,
            );
            let output = document::render(
                &ui.root,
                &ui,
                &reads,
                builtin::skin_doc(),
                MasonryHost::new(&ui, &reads, &skin),
            );
            let mut root = masonry_root(output, 240, 120);
            let (scene, _) = root.redraw().unwrap_or_else(|error| {
                panic!("`{name}` must reach a Masonry paint pass: {error}")
            });
            let encoding = scene.encoding();
            (
                *name,
                !(encoding.is_empty() && encoding.resources.glyphs.is_empty()),
            )
        })
        .collect::<Vec<_>>();
    let expected = CONTROL_CENSUS
        .iter()
        .map(|(name, paints, _)| (*name, paints.draws()))
        .collect::<Vec<_>>();

    assert_eq!(
        observed, expected,
        "the census is stale — move a row when its painter lands, and never leave the census \
         describing a host it no longer matches"
    );
}

/// The one census row that claims a control has no picture at all.
///
/// `Paints::Nothing` is the only way a row can sit beside an empty scene
/// without being read as unfinished, so the claim behind it is checked rather
/// than trusted: the region this host mounts draws nothing and earns its place
/// by carrying the drag. The immediate host's half of the same claim is
/// `a_drag_surface_carries_the_window_and_draws_nothing`.
#[kithara::test]
fn the_retained_window_drag_region_carries_the_drag_and_draws_nothing() {
    let bounds = Rect {
        h: 40.0,
        w: 200.0,
        x: 0.0,
        y: 0.0,
    };
    let pointer = Some(Pt { x: 10.0, y: 10.0 });
    let layer = DragProgram.layer(&(), bounds, pointer);

    assert!(layer.draw().commands().is_empty());
    assert_eq!(layer.action_at(pointer), Some(&WindowCommand::Drag));
}

fn fixture_ui(module_id: &str, root: &str, registry: &dyn EndpointRegistry) -> CompiledUi {
    fixture_ui_with_options(module_id, root, registry, false)
}

fn fixture_ui_with_resize(
    module_id: &str,
    root: &str,
    registry: &dyn EndpointRegistry,
) -> CompiledUi {
    fixture_ui_with_options(module_id, root, registry, true)
}

fn fixture_ui_with_options(
    module_id: &str,
    root: &str,
    registry: &dyn EndpointRegistry,
    resize_edges: bool,
) -> CompiledUi {
    let mut resolver = MemResolver::default();
    let resize_edges = if resize_edges {
        ", resize_edges: true"
    } else {
        ""
    };
    let layout = [
        r#"(schema: "kithara.layout", version: 1, id: "fixture""#,
        resize_edges,
        r#",
            root: Module(
                instance: "demo",
                source: "fixture.kmodule.ron",
                size: (w: Fill, h: Fill),
            ))"#,
    ]
    .concat();
    resolver.insert("fixture.klayout.ron", &layout);
    let module = [
        r#"(schema: "kithara.module", version: 1, id: ""#,
        module_id,
        r#"", chrome: Plain, root: "#,
        root,
        ")",
    ]
    .concat();
    resolver.insert("fixture.kmodule.ron", &module);
    compile(
        "fixture.klayout.ron",
        &resolver,
        registry,
        builtin::skin_doc(),
        &UiConfig::default(),
    )
    .unwrap_or_else(|error| panic!("Masonry contract fixture must compile: {error}"))
}

fn masonry_root<Action>(output: MasonryNode<Action>, width: u32, height: u32) -> MasonryRoot<Action>
where
    Action: std::fmt::Debug + Send + 'static,
{
    MasonryRoot::new(
        output,
        RenderRootOptions {
            default_properties: Arc::new(default_property_set()),
            use_system_fonts: false,
            size_policy: WindowSizePolicy::User,
            size: PhysicalSize::new(width, height),
            scale_factor: 1.0,
            test_font: None,
        },
    )
    .unwrap_or_else(|error| panic!("Masonry fixture root must retain typed actions: {error}"))
}

fn pointer_info() -> PointerInfo {
    PointerInfo {
        pointer_id: Some(PointerId::PRIMARY),
        persistent_device_id: None,
        pointer_type: PointerType::Mouse,
    }
}

fn pointer_state(x: f64, y: f64, pressed: bool) -> PointerState {
    let mut buttons = PointerButtons::new();
    if pressed {
        buttons.insert(MasonryPointerButton::Primary);
    }
    PointerState {
        position: PhysicalPosition::new(x, y),
        buttons,
        count: 1,
        scale_factor: 1.0,
        ..PointerState::default()
    }
}

fn pointer_down(x: f64, y: f64) -> PointerEvent {
    pointer_down_with_count(x, y, 1)
}

fn pointer_down_with_count(x: f64, y: f64, count: u8) -> PointerEvent {
    let mut state = pointer_state(x, y, true);
    state.count = count;
    PointerEvent::Down(PointerButtonEvent {
        button: Some(MasonryPointerButton::Primary),
        pointer: pointer_info(),
        state,
    })
}

fn pointer_up(x: f64, y: f64) -> PointerEvent {
    pointer_up_with_count(x, y, 1)
}

fn pointer_up_with_count(x: f64, y: f64, count: u8) -> PointerEvent {
    let mut state = pointer_state(x, y, false);
    state.count = count;
    PointerEvent::Up(PointerButtonEvent {
        button: Some(MasonryPointerButton::Primary),
        pointer: pointer_info(),
        state,
    })
}

fn pointer_move(x: f64, y: f64) -> PointerEvent {
    PointerEvent::Move(PointerUpdate {
        pointer: pointer_info(),
        current: pointer_state(x, y, true),
        coalesced: Vec::new(),
        predicted: Vec::new(),
    })
}

fn pointer_hover(x: f64, y: f64) -> PointerEvent {
    PointerEvent::Move(PointerUpdate {
        pointer: pointer_info(),
        current: pointer_state(x, y, false),
        coalesced: Vec::new(),
        predicted: Vec::new(),
    })
}

fn pointer_scroll(x: f64, y: f64, delta: ScrollDelta) -> PointerEvent {
    PointerEvent::Scroll(PointerScrollEvent {
        pointer: pointer_info(),
        delta,
        state: pointer_state(x, y, false),
    })
}

fn pointer_leave() -> PointerEvent {
    PointerEvent::Leave(pointer_info())
}

fn fixture_section(fixture: &str, preset: &str, width: u32, height: u32) -> Vec<ExpectedRect> {
    let header = format!("# {preset} @ {width}x{height}");
    let mut active = false;
    let mut rects = Vec::new();
    for line in fixture.lines() {
        if line.starts_with('#') {
            if active {
                break;
            }
            active = line == header;
            continue;
        }
        if !active {
            continue;
        }
        let mut fields = line.split_whitespace();
        let Some(path) = fields.next() else {
            continue;
        };
        let mut number = || {
            fields
                .next()
                .unwrap_or_else(|| panic!("fixture line `{line}` is missing a coordinate"))
                .parse::<f64>()
                .unwrap_or_else(|error| {
                    panic!("fixture line `{line}` has a bad coordinate: {error}")
                })
        };
        rects.push(ExpectedRect {
            path: path.to_owned(),
            x: number(),
            y: number(),
            width: number(),
            height: number(),
        });
        assert!(
            fields.next().is_none(),
            "fixture line `{line}` has an extra coordinate"
        );
    }
    assert!(active, "fixture section `{header}` is missing");
    rects
}

fn fixture_registry() -> FixtureRegistry {
    let mut registry = FixtureRegistry::default();
    for id in [
        "deck.transport.jump_back",
        "deck.transport.jump_forward",
        "deck.transport.set_cue",
        "deck.transport.toggle_loop",
        "deck.transport.toggle_play",
        "deck.transport.toggle_reverse",
        "deck.transport.toggle_sync",
        "deck.view.zoom_in",
        "deck.view.zoom_out",
    ] {
        registry.insert(
            EndpointCategory::Command,
            id,
            EndpointDesc::new(ValueKind::Trigger).with_scope("deck"),
        );
    }
    registry.insert(
        EndpointCategory::Command,
        "deck.transport.seek_normalized",
        EndpointDesc::new(ValueKind::Scalar).with_scope("deck"),
    );
    for id in [
        "deck.playback.looping",
        "deck.playback.playing",
        "deck.playback.reverse",
        "deck.playback.synced",
    ] {
        registry.insert(
            EndpointCategory::Telemetry,
            id,
            EndpointDesc::new(ValueKind::Bool).with_scope("deck"),
        );
    }
    for id in ["deck.playback.tempo", "deck.track.title"] {
        registry.insert(
            EndpointCategory::Telemetry,
            id,
            EndpointDesc::new(ValueKind::Text).with_scope("deck"),
        );
    }
    registry.insert(
        EndpointCategory::Telemetry,
        "deck.playback.position_normalized",
        EndpointDesc::new(ValueKind::Scalar).with_scope("deck"),
    );
    registry.insert(
        EndpointCategory::Telemetry,
        "deck.playback.waveform",
        EndpointDesc::new(ValueKind::Waveform).with_scope("deck"),
    );
    registry.insert(
        EndpointCategory::Telemetry,
        "player.output.levels",
        EndpointDesc::new(ValueKind::Stereo),
    );
    registry.insert(
        EndpointCategory::Parameter,
        "player.output.volume",
        EndpointDesc::new(ValueKind::Scalar),
    );
    registry.insert(
        EndpointCategory::Model,
        "deck.view.zoom",
        EndpointDesc::new(ValueKind::Scalar),
    );
    registry.insert(
        EndpointCategory::Model,
        "library.visible_tracks",
        EndpointDesc::new(ValueKind::TrackList),
    );
    registry.insert(
        EndpointCategory::Model,
        "library.breadcrumb",
        EndpointDesc::new(ValueKind::Text),
    );
    registry.insert(
        EndpointCategory::Model,
        "library.scope",
        EndpointDesc::new(ValueKind::Scalar),
    );
    insert_stream_endpoints(&mut registry);
    registry
}

fn insert_stream_endpoints(registry: &mut FixtureRegistry) {
    registry.insert(
        EndpointCategory::Telemetry,
        "deck.stream.quality_hidden",
        EndpointDesc::new(ValueKind::Bool).with_scope("deck"),
    );
    registry.insert(
        EndpointCategory::Model,
        "deck.stream.quality",
        EndpointDesc::new(ValueKind::Text).with_scope("deck"),
    );
    registry.insert(
        EndpointCategory::Model,
        "deck.stream.quality_menu",
        EndpointDesc::new(ValueKind::Bool).with_scope("deck"),
    );
    registry.insert(
        EndpointCategory::Command,
        "deck.stream.toggle_quality_menu",
        EndpointDesc::new(ValueKind::Trigger).with_scope("deck"),
    );
    registry.insert(
        EndpointCategory::Model,
        "deck.stream.variant_active",
        EndpointDesc::new(ValueKind::Bool)
            .with_scope("deck")
            .with_scope("variant"),
    );
    registry.insert(
        EndpointCategory::Telemetry,
        "deck.stream.variant_hidden",
        EndpointDesc::new(ValueKind::Bool)
            .with_scope("deck")
            .with_scope("variant"),
    );
    for id in ["deck.stream.variant_label", "deck.stream.variant_sub"] {
        registry.insert(
            EndpointCategory::Telemetry,
            id,
            EndpointDesc::new(ValueKind::Text)
                .with_scope("deck")
                .with_scope("variant"),
        );
    }
    registry.insert(
        EndpointCategory::Command,
        "deck.stream.select_variant",
        EndpointDesc::new(ValueKind::Trigger)
            .with_scope("deck")
            .with_scope("variant"),
    );
}
