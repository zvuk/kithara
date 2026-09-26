use std::{
    cell::{Cell, RefCell},
    collections::{BTreeMap, VecDeque},
    rc::Rc,
    sync::{Arc, LazyLock},
};

use kithara_platform::time::Duration;
use kithara_test_utils::{
    kithara,
    test::usdt::{self, ProbeEvent},
};
use masonry::{
    app::{RenderRoot, RenderRootOptions, RenderRootSignal, WindowSizePolicy},
    core::{
        CursorIcon, Handled, Ime, PointerEvent, TextEvent, Widget, WidgetId, WidgetRef, WindowEvent,
    },
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
    TextMeasurer, built::RootParts, leaf::DragProgram,
};
use crate::{
    atoms::bar::context::Context,
    builtin,
    compile::{CompiledUi, compile},
    draw::{DrawListBuilder, Pt, Rect, Rgba},
    ids::{EndpointId, SourceUri},
    interact::{Hit, Input, Key as NeutralKey, Outcome, PointerOwnership, PointerPhase, Scroll},
    module::IconName,
    registry::{EndpointCategory, EndpointDesc, EndpointRegistry, ValueKind},
    render::{
        ControlAction, CustomSkin, DragPhase, PortalMapView, PortalTarget, ReadValue, Reads,
        ScalarRange, Skin, StereoLevels, TableCell, TableRow, TreeRow, UiEvent, WaveBucket,
        WaveformView, WindowCommand, WindowEdge, WindowLayerProgram,
        custom::CustomKinds,
        document,
        document::{Clock, Ctx},
        picker_hits,
        vis::{VisDeclaration, VisFrame},
    },
    shaping::{FontPolicy, TextContext},
    source::{MemResolver, UiConfig},
    view,
};

struct FixtureReads;

/// What the host hands the document for one frame, built from a fixture reader
/// so a test drives the clock rather than waiting for one.
fn ctx<'a>(ui: &'a CompiledUi, reads: &'a dyn Reads) -> Ctx<'a, 'a> {
    Ctx::new(
        ui,
        reads,
        &view::EMPTY,
        builtin::skin_doc(),
        Clock::default(),
    )
}

static CENSUS_PORTALS: [PortalTarget; 2] = [
    PortalTarget {
        bpm: 93.0,
        is_selected: true,
    },
    PortalTarget {
        bpm: 165.33,
        is_selected: false,
    },
];

static CENSUS_WAVE: [WaveBucket; 1] = [WaveBucket {
    high: 0.8,
    low: 0.2,
    mid: 0.5,
}];

struct VisReads {
    first: Cell<f64>,
    left: Cell<f32>,
    levels_present: Cell<bool>,
    right: Cell<f32>,
    second: Cell<f64>,
    time: Cell<f64>,
    volume: Cell<f32>,
}

impl Reads for VisReads {
    fn get(&self, endpoint: &str) -> Option<ReadValue<'_>> {
        match endpoint {
            "vis.first" => Some(ReadValue::Scalar(self.first.get())),
            "vis.second" => Some(ReadValue::Scalar(self.second.get())),
            "vis.time" => Some(ReadValue::Scalar(self.time.get())),
            "player.output.levels" if self.levels_present.get() => {
                Some(ReadValue::Stereo(StereoLevels {
                    l: self.left.get(),
                    r: self.right.get(),
                    volume: self.volume.get(),
                }))
            }
            _ => None,
        }
    }
}

struct PresetReads {
    active: Cell<&'static str>,
}

impl Reads for PresetReads {
    fn get(&self, endpoint: &str) -> Option<ReadValue<'_>> {
        (endpoint == "ui.preset").then_some(ReadValue::Text(self.active.get()))
    }
}

static LATE_TABLE_ROWS: LazyLock<Vec<TableRow<'static>>> = LazyLock::new(|| {
    vec![
        TableRow::new(
            vec![
                TableCell::text("title", "Late Arrival"),
                TableCell::text("artist", "New Artist"),
                TableCell::text("bpm", "128"),
                TableCell::number("energy", 7),
                TableCell::text("key", "Am"),
                TableCell::text("time", "03:24"),
            ],
            false,
        );
        8
    ]
});

static TREE_ROWS: [TreeRow<'static>; 8] = [TreeRow {
    label: "Late Folder",
    count: Some(8),
    expanded: Some(true),
    icon: IconName::Folder,
    muted: false,
    selected: false,
    depth: 0,
}; 8];

struct LateTrackReads {
    loaded: Cell<bool>,
}

impl Reads for LateTrackReads {
    fn get(&self, endpoint: &str) -> Option<ReadValue<'_>> {
        let id = endpoint.split_once('@').map_or(endpoint, |(id, _scope)| id);
        if id == "library.visible_tracks" {
            let rows = if self.loaded.get() {
                &LATE_TABLE_ROWS[..]
            } else {
                &[]
            };
            Some(ReadValue::Table(rows))
        } else {
            FixtureReads.get(endpoint)
        }
    }
}

struct LateTreeReads {
    query_loaded: Cell<bool>,
    rows_loaded: Cell<bool>,
}

impl Reads for LateTreeReads {
    fn get(&self, endpoint: &str) -> Option<ReadValue<'_>> {
        let id = endpoint.split_once('@').map_or(endpoint, |(id, _scope)| id);
        match id {
            "library.tree" => Some(ReadValue::Tree(if self.rows_loaded.get() {
                &TREE_ROWS
            } else {
                &[]
            })),
            "library.query" => Some(ReadValue::Text(if self.query_loaded.get() {
                "Late"
            } else {
                ""
            })),
            _ => FixtureReads.get(endpoint),
        }
    }
}

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
            "library.query" => Some(ReadValue::Text("Folder")),
            "library.scope" => Some(ReadValue::Scalar(0.0)),
            "vis.preset" => Some(ReadValue::Scalar(1.0)),
            "vis.time" => Some(ReadValue::Scalar(0.5)),
            "library.tree" => Some(ReadValue::Tree(&TREE_ROWS)),
            "demo.wave" => Some(ReadValue::Waveform(WaveformView {
                beats: &[],
                buckets: &CENSUS_WAVE,
                revision: 0,
                cues: &[],
                downbeats: &[],
                unready: &[],
                bpm: None,
                r#loop: None,
            })),
            "player.output.levels" => Some(ReadValue::Stereo(StereoLevels {
                l: 0.6,
                r: 0.4,
                volume: 0.8,
            })),
            "player.output.volume" => Some(ReadValue::Scalar(0.8)),
            "pivot.map" => Some(ReadValue::PortalMap(PortalMapView {
                master: 124.0,
                min: 88.0,
                max: 176.0,
                targets: &CENSUS_PORTALS,
            })),
            "pivot.range" => Some(ReadValue::Range(ScalarRange { min: 0.2, max: 0.8 })),
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

/// The endpoints the shipped presets name, owned by one file and shared with
/// the integration tests that compile the same documents.
mod preset_registry {
    use crate as kithara_ui;

    include!("../../../tests/common/registry.rs");
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
    /// The box the neutral host gave this node, or nothing when the room never
    /// reached it and neither host laid it out.
    placed: Option<[f64; 4]>,
    path: String,
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
    recognized: Rc<RefCell<Vec<PointerPhase>>>,
    pending: VecDeque<WheelAction>,
    long_pressed: bool,
    pressed: bool,
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
            recognized,
            pending: actions.into_iter().collect(),
            pressed: false,
            long_pressed: false,
        }
    }

    fn take(&mut self) -> Option<WheelAction> {
        self.pending.pop_front()
    }
}

impl CustomWidget for WheelEmitter {
    type Action = WheelAction;

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

    fn measure(&mut self, _text: &mut TextMeasurer<'_>, _limits: SizeLimits) -> Size2 {
        Size2::new(40.0, 40.0)
    }

    fn paint(
        &mut self,
        _list: &mut DrawListBuilder,
        _text: &mut TextMeasurer<'_>,
        _bounds: Rect,
        _skin: &CustomSkin,
    ) {
    }

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

/// Counts how often the leaf at the bottom of a stack of boxes is asked for its
/// size while the page above it is laid out once.
struct MeasureProbe {
    measures: Rc<Cell<usize>>,
}

impl CustomWidget for MeasureProbe {
    type Action = ();

    fn measure(&mut self, _text: &mut TextMeasurer<'_>, _limits: SizeLimits) -> Size2 {
        self.measures.set(self.measures.get() + 1);
        Size2::new(40.0, 40.0)
    }

    fn paint(
        &mut self,
        _list: &mut DrawListBuilder,
        _text: &mut TextMeasurer<'_>,
        _bounds: Rect,
        _skin: &CustomSkin,
    ) {
    }
}

impl CustomWidget for KeyProbe {
    type Action = ();

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

    fn measure(&mut self, _text: &mut TextMeasurer<'_>, _limits: SizeLimits) -> Size2 {
        Size2::new(40.0, 40.0)
    }

    fn paint(
        &mut self,
        _list: &mut DrawListBuilder,
        _text: &mut TextMeasurer<'_>,
        _bounds: Rect,
        _skin: &CustomSkin,
    ) {
    }
}

impl CustomWidget for FrameProbe {
    type Action = ();

    fn frame(&mut self, _elapsed: Duration) -> Option<Self::Action> {
        self.pending = false;
        None
    }

    fn measure(&mut self, _text: &mut TextMeasurer<'_>, _limits: SizeLimits) -> Size2 {
        Size2::new(40.0, 40.0)
    }

    fn paint(
        &mut self,
        _list: &mut DrawListBuilder,
        _text: &mut TextMeasurer<'_>,
        _bounds: Rect,
        _skin: &CustomSkin,
    ) {
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

    fn input(&mut self, input: Input<'_>, hit: Hit) -> Outcome<Self::Action> {
        let Input::Wheel(scroll) = input else {
            return Outcome::IGNORED;
        };
        self.observations.borrow_mut().push((scroll, hit));
        Outcome::captured()
    }

    fn measure(&mut self, _text: &mut TextMeasurer<'_>, _limits: SizeLimits) -> Size2 {
        Size2::new(40.0, 40.0)
    }

    fn paint(
        &mut self,
        _list: &mut DrawListBuilder,
        _text: &mut TextMeasurer<'_>,
        _bounds: Rect,
        _skin: &CustomSkin,
    ) {
    }
}

impl CustomWidget for CaptureProbe {
    type Action = ();

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

    fn measure(&mut self, _text: &mut TextMeasurer<'_>, _limits: SizeLimits) -> Size2 {
        Size2::new(40.0, 40.0)
    }

    fn paint(
        &mut self,
        _list: &mut DrawListBuilder,
        _text: &mut TextMeasurer<'_>,
        _bounds: Rect,
        _skin: &CustomSkin,
    ) {
    }
}

#[kithara::test]
fn masonry_layout_rects_equal_snapped_neutral_rects() {
    let reads = FixtureReads;
    let registry = preset_registry::player_registry();
    let skin = Skin::resolve_with_font_policy(
        builtin::skin_doc().clone(),
        builtin::text_doc(),
        &SourceUri("fixture:masonry-layout-parity".to_owned()),
        &builtin::resolver(),
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
            builtin::text_doc(),
            &UiConfig::default(),
            &view::EMPTY,
        )
        .unwrap_or_else(|error| panic!("builtin layout must compile: {error}"));
        for (width, height) in [(1280, 720), (960, 600), (320, 240)] {
            let expected = fixture_section(fixture, preset, width, height);
            let (output, built) = DocumentNodes::built(|| {
                document::render(
                    &ui.root,
                    ctx(&ui, &reads),
                    MasonryHost::new(ctx(&ui, &reads), &skin),
                )
            });
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
            let ids = built.in_tree(root);
            assert_eq!(
                ids.len(),
                expected.len(),
                "{preset} @ {width}x{height} did not retain exactly one real Masonry node per fixture path"
            );

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
                let Some(rect @ [x, y, rect_width, rect_height]) = expected.placed else {
                    // The room never reached this node, so the neutral host
                    // gave it no box and this one must not give it one either.
                    assert_eq!(
                        [size.width, size.height],
                        [0.0, 0.0],
                        "{preset} @ {width}x{height} path `{}` stands here and nowhere on the \
                         neutral host",
                        expected.path
                    );
                    continue;
                };
                let snapped_x = x.round();
                let snapped_y = y.round();
                let snapped_width = (x + rect_width).round() - snapped_x;
                let snapped_height = (y + rect_height).round() - snapped_y;
                assert_eq!(
                    [origin.x, origin.y, size.width, size.height],
                    [snapped_x, snapped_y, snapped_width, snapped_height],
                    "{preset} @ {width}x{height} path `{}` diverged from endpoint snapping of neutral rect {rect:?}",
                    expected.path,
                );
            }
        }
    }
}

/// A flow that measures its room, holding one cell that always stands and one
/// that waits for a wider window.
const REVEALING_BAR: &str = r#"Row(id: "bar", measure: Width, size: (w: Fill, h: Fill),
    gap: 0.0, pad: 0.0, children: [
        Spacer(id: "always", size: Some((w: Fixed(40.0), h: Fixed(20.0)))),
        Reveal(from: 200.0, child: Spacer(id: "wide", size: Some((w: Fixed(40.0), h: Fixed(20.0))))),
    ])"#;

/// The revealed cell of [`REVEALING_BAR`] on a window of the given width, and
/// whether Masonry keeps it out of the picture.
fn revealed_cell_is_hidden(width: u32) -> bool {
    let registry = fixture_registry();
    let reads = FixtureReads;
    let ui = fixture_ui("revealing-bar", REVEALING_BAR, &registry);
    let (output, built) = DocumentNodes::built(|| {
        document::render(
            &ui.root,
            ctx(&ui, &reads),
            MasonryHost::new(ctx(&ui, &reads), builtin::skin()),
        )
    });
    let mut root = masonry_root(output, width, 60);
    let wide = built.last_in(root.root());
    root.redraw()
        .unwrap_or_else(|error| panic!("the revealing bar must compose: {error}"));
    root.root()
        .get_widget(wide)
        .unwrap_or_else(|| panic!("the revealed cell must stay registered"))
        .ctx()
        .is_stashed()
}

#[kithara::test]
fn a_cell_the_room_does_not_reach_draws_nothing() {
    assert!(
        revealed_cell_is_hidden(120),
        "a cell whose band the room does not reach must be stashed: Masonry then skips it for \
         paint, and an empty box alone still lets the leaves inside it draw at the flow's origin"
    );
}

#[kithara::test]
fn a_cell_the_room_reaches_draws() {
    assert!(
        !revealed_cell_is_hidden(240),
        "a cell whose band the room reaches must stand in the picture"
    );
}

#[kithara::test]
fn a_cell_comes_back_when_the_room_grows_to_reach_it() {
    let registry = fixture_registry();
    let reads = FixtureReads;
    let ui = fixture_ui("revealing-bar-resized", REVEALING_BAR, &registry);
    let (output, built) = DocumentNodes::built(|| {
        document::render(
            &ui.root,
            ctx(&ui, &reads),
            MasonryHost::new(ctx(&ui, &reads), builtin::skin()),
        )
    });
    let mut root = masonry_root(output, 120, 60);
    let wide = built.last_in(root.root());
    root.redraw()
        .unwrap_or_else(|error| panic!("the narrow bar must compose: {error}"));
    root.handle_window_event(WindowEvent::Resize(PhysicalSize::new(240, 60)))
        .unwrap_or_else(|error| panic!("the bar must take the wider window: {error}"));
    root.redraw()
        .unwrap_or_else(|error| panic!("the widened bar must compose: {error}"));
    assert!(
        !root
            .root()
            .get_widget(wide)
            .unwrap_or_else(|| panic!("the revealed cell must stay registered"))
            .ctx()
            .is_stashed(),
        "a cell the room grew to reach must be back in the picture"
    );
}

/// The same flow as [`REVEALING_BAR`], holding the window controls in the cell
/// that waits for a wider window.
const REVEALING_CONTROLS: &str = r#"Row(id: "bar", measure: Width, size: (w: Fill, h: Fill),
    gap: 0.0, pad: 0.0, children: [
        Reveal(from: 200.0, child: WindowControls(id: "controls")),
    ])"#;

/// Whether the picture the retained host just composed carries anything, read
/// the way the control census reads it.
fn composes_a_picture<Action>(root: &mut MasonryRoot<Action>) -> bool
where
    Action: std::fmt::Debug + Send + 'static,
{
    let (scene, _) = root
        .redraw()
        .unwrap_or_else(|error| panic!("the revealing bar must compose: {error}"));
    let encoding = scene.encoding();
    !(encoding.is_empty() && encoding.resources.glyphs.is_empty())
}

/// What [`REVEALING_CONTROLS`] draws on a window of the given width, and again
/// after the window is resized to each width that follows.
fn controls_draw(width: u32, resizes: &[u32]) -> Vec<bool> {
    let registry = fixture_registry();
    let reads = FixtureReads;
    let ui = fixture_ui("revealing-controls", REVEALING_CONTROLS, &registry);
    let output = document::render(
        &ui.root,
        ctx(&ui, &reads),
        MasonryHost::new(ctx(&ui, &reads), builtin::skin()),
    );
    let mut root = masonry_root(output, width, 60);
    let mut drawn = vec![composes_a_picture(&mut root)];
    for width in resizes {
        root.handle_window_event(WindowEvent::Resize(PhysicalSize::new(*width, 60)))
            .unwrap_or_else(|error| panic!("the bar must take a {width}px window: {error}"));
        drawn.push(composes_a_picture(&mut root));
    }
    drawn
}

#[kithara::test]
fn window_controls_the_room_never_reached_draw_nothing() {
    assert_eq!(
        controls_draw(120, &[]),
        vec![false],
        "a window layer is a root of its own, so stashing the cell it belongs to never reaches          it: the box its anchor publishes is the only word it gets, and an anchor that was never          laid out publishes none"
    );
}

#[kithara::test]
fn window_controls_the_room_reaches_draw_their_row() {
    assert_eq!(
        controls_draw(240, &[]),
        vec![true],
        "a cell the room reaches must still draw the controls it holds"
    );
}

#[kithara::test]
fn window_controls_stop_drawing_when_the_room_shrinks_past_them() {
    assert_eq!(
        controls_draw(240, &[120]),
        vec![true, false],
        "an anchor that is stashed after it was laid out must take its box back, or the layer          keeps drawing the row where the cell used to stand"
    );
}

#[kithara::test]
fn a_fill_slot_centers_its_fixed_content_like_the_immediate_host() {
    let registry = fixture_registry();
    let ui = fixture_ui(
        "centered-slot",
        r#"Slot(id: "content", size: (w: Fill, h: Fill), default: [
            Spacer(id: "fixed", size: Some((w: Fixed(40.0), h: Fixed(20.0)))),
        ])"#,
        &registry,
    );
    let reads = FixtureReads;
    let state = MasonryState::default();
    let host = MasonryHost::new(ctx(&ui, &reads), builtin::skin()).with_state(state.clone());
    let output = document::render(&ui.root, ctx(&ui, &reads), host);
    let root = masonry_root(output, 200, 120);
    let fixed = state
        .widget_id("demo/fixed")
        .unwrap_or_else(|| panic!("the slot child must remain addressable"));
    let bounds = root
        .root()
        .get_widget(fixed)
        .unwrap_or_else(|| panic!("the slot child must remain mounted"))
        .ctx()
        .bounding_rect();

    assert_eq!(
        [bounds.x0, bounds.y0, bounds.width(), bounds.height()],
        [0.0, 50.0, 40.0, 20.0],
    );
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
    let host = MasonryHost::map_actions(ctx(&ui, &reads), builtin::skin(), TestAction::Document)
        .with_custom(
            "demo/custom",
            WheelEmitter::observed(expected.clone(), Rc::clone(&recognized)),
            TestAction::Wheel,
        );
    let output = document::render(&ui.root, ctx(&ui, &reads), host);
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

/// A press on content the document named by kind must reach the widget the
/// application registered, and leave as the event that registration maps it to.
/// The path route (`with_custom`) is tested above; this is the other one.
#[kithara::test]
fn a_press_reaches_the_extension_the_document_named_by_kind() {
    let registry = fixture_registry();
    let ui = fixture_ui(
        "custom-kind-fixture",
        r#"Row(size: (w: Fill, h: Fill), gap: 0.0, pad: 0.0, children: [
            Custom(id: "custom", kind: "press-extension", size: Some((w: Fixed(40.0), h: Fixed(40.0)))),
        ])"#,
        &registry,
    );
    let reads = FixtureReads;
    let kinds = press_kinds();
    let frame = ctx(&ui, &reads).with_kinds(&kinds);
    let host = MasonryHost::map_actions(frame, builtin::skin(), TestAction::Document);
    let output = document::render(&ui.root, frame, host);
    let mut root = masonry_root(output, 200, 120);

    root.handle_pointer_event(pointer_down(10.0, 50.0))
        .unwrap_or_else(|error| {
            panic!("a press on a registered extension must remain typed: {error}")
        });

    assert_eq!(
        root.take_actions(),
        [TestAction::Document(UiEvent::OpenSettings)],
    );
}

/// A cell placed at the size it was measured at is not laid out a second time.
///
/// A flow measures a cell by laying it out and then places it by laying it out
/// again, so a cell one box down is walked twice for each of the two walks
/// above it: the cost doubles per box, not per cell. The leaf here stands under
/// six of them counting the module, and was measured sixty-four times instead
/// of once - which is how the gallery page that nests deepest came to lay
/// itself out seventy-five times over.
#[kithara::test]
fn a_cell_placed_where_it_was_measured_is_laid_out_once() {
    let registry = fixture_registry();
    let ui = fixture_ui(
        "custom-fixture",
        r#"Row(size: (w: Fill, h: Fill), gap: 0.0, pad: 0.0, children: [
            Column(size: (w: Fill, h: Fill), gap: 0.0, pad: 0.0, children: [
                Row(size: (w: Fill, h: Fill), gap: 0.0, pad: 0.0, children: [
                    Column(size: (w: Fill, h: Fill), gap: 0.0, pad: 0.0, children: [
                        Row(size: (w: Fill, h: Fill), gap: 0.0, pad: 0.0, children: [
                            Spacer(id: "deep", size: Some((w: Fixed(40.0), h: Fixed(40.0)))),
                        ]),
                    ]),
                ]),
            ]),
        ])"#,
        &registry,
    );
    let reads = FixtureReads;
    let measures = Rc::new(Cell::new(0));
    let host = MasonryHost::map_actions(ctx(&ui, &reads), builtin::skin(), TestAction::Document)
        .with_custom(
            "demo/deep",
            MeasureProbe {
                measures: Rc::clone(&measures),
            },
            |()| TestAction::Document(UiEvent::OpenSettings),
        );
    let output = document::render(&ui.root, ctx(&ui, &reads), host);
    let _root = masonry_root(output, 200, 120);

    assert_eq!(
        measures.get(),
        1,
        "the leaf was measured once per box standing over it instead of once for the page"
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
    let host = MasonryHost::map_actions(ctx(&ui, &reads), builtin::skin(), TestAction::Document)
        .with_custom(
            "demo/custom",
            FrameProbe {
                paints: Rc::clone(&paints),
                pending: true,
            },
            |()| TestAction::Document(UiEvent::OpenSettings),
        );
    let output = document::render(&ui.root, ctx(&ui, &reads), host);
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
    let host = MasonryHost::map_actions(ctx(&ui, &reads), builtin::skin(), TestAction::Document)
        .with_custom(
            "demo/volume",
            WheelEmitter::new(Vec::<WheelAction>::new()),
            TestAction::Wheel,
        );
    let output = document::render(&ui.root, ctx(&ui, &reads), host);
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
    let host = MasonryHost::map_actions(ctx(&ui, &reads), builtin::skin(), TestAction::Document)
        .with_custom(
            "demo/custom",
            CaptureProbe {
                observations: Rc::clone(&observations),
            },
            |()| TestAction::Document(UiEvent::OpenSettings),
        );
    let output = document::render(&ui.root, ctx(&ui, &reads), host);
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
    let host = MasonryHost::map_actions(ctx(&ui, &reads), builtin::skin(), TestAction::Document)
        .with_custom(
            "demo/custom",
            KeyProbe {
                observed: Rc::clone(&observed),
            },
            |()| TestAction::Document(UiEvent::OpenSettings),
        );
    let output = document::render(&ui.root, ctx(&ui, &reads), host);
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
    let host = MasonryHost::map_actions(ctx(&ui, &reads), builtin::skin(), TestAction::Document)
        .with_custom(
            "demo/custom",
            ScrollProbe {
                observations: Rc::clone(&observations),
            },
            |()| TestAction::Document(UiEvent::OpenSettings),
        );
    let output = document::render(&ui.root, ctx(&ui, &reads), host);
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
    let host = MasonryHost::map_actions(ctx(&ui, &reads), builtin::skin(), TestAction::Document);
    let output = document::render(&ui.root, ctx(&ui, &reads), host);
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

/// A menu is a surface with things to press in it. The burger menu and the
/// quality picker both hang a list of pressable rows off a popover, so a press
/// that reaches the surface but never the row leaves the whole menu inert.
#[kithara::test]
fn a_press_inside_an_open_popover_reaches_the_control_it_lands_on() {
    let mut registry = fixture_registry();
    registry.insert(
        EndpointCategory::Model,
        "ui.menu.open",
        EndpointDesc::new(ValueKind::Bool),
    );
    registry.insert(
        EndpointCategory::Command,
        "ui.menu.pick",
        EndpointDesc::new(ValueKind::Trigger),
    );
    let ui = fixture_ui(
        "popover-item",
        r#"Row(size: (w: Fill, h: Fill), gap: 0.0, pad: 0.0, children: [
            Popover(
                id: "menu",
                open: Model(id: "ui.menu.open"),
                align: Start,
                anchor: Spacer(id: "anchor", size: Some((w: Fixed(40.0), h: Fixed(20.0)))),
                content: Pressable(
                    id: "item",
                    press: Command(id: "ui.menu.pick"),
                    child: Spacer(id: "item-face", size: Some((w: Fixed(100.0), h: Fixed(26.0)))),
                ),
            ),
        ])"#,
        &registry,
    );
    let reads = FixtureReads;
    let host = MasonryHost::map_actions(ctx(&ui, &reads), builtin::skin(), TestAction::Document);
    let output = document::render(&ui.root, ctx(&ui, &reads), host);
    let mut root = masonry_root(output, 200, 200);
    root.redraw()
        .unwrap_or_else(|error| panic!("popover redraw must retain typed actions: {error}"));

    let item = {
        let layer = root.root().get_layer_root(1);
        let children = layer.children();
        assert_eq!(children.len(), 1, "the popover layer has one content root");
        children[0].ctx().bounding_rect()
    };
    let at = item.center();

    root.handle_pointer_event(pointer_down(at.x, at.y))
        .unwrap_or_else(|error| panic!("menu item press must remain typed: {error}"));
    root.handle_pointer_event(pointer_up(at.x, at.y))
        .unwrap_or_else(|error| panic!("menu item release must remain typed: {error}"));

    assert_eq!(
        root.take_actions(),
        vec![TestAction::Document(UiEvent::Control {
            path: "demo/item".to_owned(),
            action: ControlAction::Activate,
        })],
        "the row under the press must activate, at {at:?} inside {item:?}"
    );
}

/// A surface floats above the document, so the room it covers belongs to it
/// alone. The burger menu and the quality picker both hang over controls the
/// document lays out after them, and an engine reads its own box without
/// asking what stands above it.
#[kithara::test]
fn an_open_popover_keeps_a_press_off_the_control_its_surface_covers() {
    let mut registry = fixture_registry();
    registry.insert(
        EndpointCategory::Model,
        "ui.menu.open",
        EndpointDesc::new(ValueKind::Bool),
    );
    registry.insert(
        EndpointCategory::Command,
        "ui.menu.pick",
        EndpointDesc::new(ValueKind::Trigger),
    );
    registry.insert(
        EndpointCategory::Command,
        "ui.page.pick",
        EndpointDesc::new(ValueKind::Trigger),
    );
    let ui = fixture_ui(
        "popover-cover",
        r#"Column(size: (w: Fill, h: Fill), gap: 0.0, pad: 0.0, children: [
            Popover(
                id: "menu",
                open: Model(id: "ui.menu.open"),
                align: Start,
                anchor: Spacer(id: "anchor", size: Some((w: Fixed(40.0), h: Fixed(20.0)))),
                content: Pressable(
                    id: "item",
                    press: Command(id: "ui.menu.pick"),
                    child: Spacer(id: "item-face", size: Some((w: Fixed(100.0), h: Fixed(26.0)))),
                ),
            ),
            Pressable(
                id: "under",
                press: Command(id: "ui.page.pick"),
                child: Spacer(id: "under-face", size: Some((w: Fill, h: Fill))),
            ),
        ])"#,
        &registry,
    );
    let reads = FixtureReads;
    let host = MasonryHost::map_actions(ctx(&ui, &reads), builtin::skin(), TestAction::Document);
    let output = document::render(&ui.root, ctx(&ui, &reads), host);
    let mut root = masonry_root(output, 200, 200);
    root.redraw()
        .unwrap_or_else(|error| panic!("popover redraw must retain typed actions: {error}"));

    let item = {
        let layer = root.root().get_layer_root(1);
        let children = layer.children();
        assert_eq!(children.len(), 1, "the popover layer has one content root");
        children[0].ctx().bounding_rect()
    };
    let at = item.center();

    root.handle_pointer_event(pointer_down(at.x, at.y))
        .unwrap_or_else(|error| panic!("menu item press must remain typed: {error}"));
    root.handle_pointer_event(pointer_up(at.x, at.y))
        .unwrap_or_else(|error| panic!("menu item release must remain typed: {error}"));

    assert_eq!(
        root.take_actions(),
        vec![TestAction::Document(UiEvent::Control {
            path: "demo/item".to_owned(),
            action: ControlAction::Activate,
        })],
        "the surface owns the press at {at:?}, not the page control it covers"
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
        ctx(&ui, &closed),
        MasonryHost::new(ctx(&ui, &closed), builtin::skin()).with_state(state.clone()),
    );
    let mut root = masonry_root(output, 200, 200);
    root.handle_pointer_event(pointer_down(70.0, 95.0))
        .unwrap_or_else(|error| panic!("opening press must remain typed: {error}"));

    let open = PopoverReads { open: true };
    let output = document::render(
        &ui.root,
        ctx(&ui, &open),
        MasonryHost::new(ctx(&ui, &open), builtin::skin()).with_state(state.clone()),
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
        ctx(&ui, &closed),
        MasonryHost::new(ctx(&ui, &closed), builtin::skin()).with_state(state.clone()),
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
        ctx(&ui, &open),
        MasonryHost::new(ctx(&ui, &open), builtin::skin()).with_state(state),
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
    let host = MasonryHost::map_actions(ctx(&ui, &reads), builtin::skin(), TestAction::Document);
    let output = document::render(&ui.root, ctx(&ui, &reads), host);
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

fn stepping_surface(registry: &dyn EndpointRegistry) -> CompiledUi {
    fixture_ui(
        "gallery-tempo",
        r#"Row(size: (w: Fill, h: Fill), gap: 0.0, pad: 0.0, children: [
            Row(
                id: "tempo",
                size: (w: Fixed(40.0), h: Fixed(40.0)),
                gap: 0.0,
                pad: 0.0,
                write: Parameter(id: "player.output.volume"),
                children: [],
            ),
        ])"#,
        registry,
    )
}

fn stepping_root(ui: &CompiledUi, reads: &FixtureReads) -> MasonryRoot<TestAction> {
    let host = MasonryHost::map_actions(ctx(ui, reads), builtin::skin(), TestAction::Document);
    let output = document::render(&ui.root, ctx(ui, reads), host);
    let mut root = masonry_root(output, 200, 120);
    root.take_platform_signals();
    root
}

#[kithara::test]
fn a_stepping_surface_keeps_its_drag_after_the_pointer_leaves_the_flow() {
    let registry = fixture_registry();
    let ui = stepping_surface(&registry);
    let reads = FixtureReads;
    let mut root = stepping_root(&ui, &reads);

    root.handle_pointer_event(pointer_down(10.0, 50.0))
        .unwrap_or_else(|error| panic!("arming the surface must remain typed: {error}"));
    root.take_actions();

    assert_eq!(
        root.handle_pointer_event(pointer_move(150.0, 34.0))
            .unwrap_or_else(|error| panic!("a drag past the edge must remain typed: {error}")),
        Handled::Yes,
    );
    assert_step(&root.take_actions(), "demo/tempo", 4.0);
}

#[kithara::test]
fn a_stepping_surface_released_off_the_flow_is_not_still_armed() {
    let registry = fixture_registry();
    let ui = stepping_surface(&registry);
    let reads = FixtureReads;
    let mut root = stepping_root(&ui, &reads);

    root.handle_pointer_event(pointer_down(10.0, 50.0))
        .unwrap_or_else(|error| panic!("arming the surface must remain typed: {error}"));
    root.handle_pointer_event(pointer_move(150.0, 34.0))
        .unwrap_or_else(|error| panic!("a drag past the edge must remain typed: {error}"));
    root.handle_pointer_event(pointer_up(150.0, 34.0))
        .unwrap_or_else(|error| panic!("releasing past the edge must remain typed: {error}"));
    root.take_actions();

    root.handle_pointer_event(pointer_move(10.0, 60.0))
        .unwrap_or_else(|error| panic!("a hover after the release must remain typed: {error}"));
    assert!(
        root.take_actions().is_empty(),
        "a released surface must not step under a bare hover"
    );
}

#[kithara::test]
fn a_knob_nested_in_a_pressable_popover_keeps_the_engine_gesture() {
    let mut registry = fixture_registry();
    registry.insert(
        EndpointCategory::Model,
        "ui.menu.open",
        EndpointDesc::new(ValueKind::Bool),
    );
    registry.insert(
        EndpointCategory::Command,
        "ui.menu.toggle",
        EndpointDesc::new(ValueKind::Trigger),
    );
    let ui = fixture_ui(
        "gallery-knobs",
        r#"Row(size: (w: Fill, h: Fill), gap: 0.0, pad: 0.0, children: [
            Popover(
                id: "menu",
                open: Model(id: "ui.menu.open"),
                at: Pointer,
                anchor: Pressable(
                    id: "menu-anchor",
                    press: Command(id: "ui.menu.toggle"),
                    child: Knob(
                        id: "volume",
                        size: Some((w: Fixed(40.0), h: Fixed(40.0))),
                        read: Parameter(id: "player.output.volume"),
                        write: Parameter(id: "player.output.volume"),
                    ),
                ),
                content: Spacer(id: "content", size: Some((w: Fixed(40.0), h: Fixed(20.0)))),
            ),
        ])"#,
        &registry,
    );
    let reads = PopoverReads { open: false };
    let state = MasonryState::default();
    let host = MasonryHost::map_actions(ctx(&ui, &reads), builtin::skin(), TestAction::Document)
        .with_state(state.clone());
    let output = document::render(&ui.root, ctx(&ui, &reads), host);
    let mut root = masonry_root(output, 200, 120);
    let volume = state
        .widget_id("demo/volume")
        .unwrap_or_else(|| panic!("the nested knob must stay addressable"));
    let bounds = root
        .root()
        .get_widget(volume)
        .unwrap_or_else(|| panic!("the nested knob must stay mounted"))
        .ctx()
        .bounding_rect();
    let start = bounds.center();

    assert_eq!(
        root.handle_pointer_event(pointer_down(start.x, start.y))
            .unwrap_or_else(|error| panic!("nested knob press must remain typed: {error}")),
        Handled::Yes,
    );
    let actions = root.take_actions();
    assert!(
        actions.is_empty(),
        "the outer pressable must not activate while the knob arms: {actions:?}"
    );
    assert_eq!(
        root.handle_pointer_event(pointer_move(start.x, start.y - 7.0))
            .unwrap_or_else(|error| panic!("nested knob drag must remain typed: {error}")),
        Handled::Yes,
    );
    assert_scalar_value(&root.take_actions(), "demo/volume", 0.85);
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
    let host = MasonryHost::map_actions(ctx(&ui, &reads), builtin::skin(), TestAction::Document);
    let output = document::render(&ui.root, ctx(&ui, &reads), host);
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
    let host = MasonryHost::map_actions(ctx(&ui, &reads), builtin::skin(), TestAction::Document);
    let output = document::render(&ui.root, ctx(&ui, &reads), host);
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
        let host =
            MasonryHost::map_actions(ctx(&ui, &reads), builtin::skin(), TestAction::Document);
        let output = document::render(&ui.root, ctx(&ui, &reads), host);
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
        let host =
            MasonryHost::map_actions(ctx(&ui, &reads), builtin::skin(), TestAction::Document);
        let (output, built) =
            DocumentNodes::built(|| document::render(&ui.root, ctx(&ui, &reads), host));
        let mut root = masonry_root(output, 200, 120);
        let control_id = built.last_in(root.root());
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

/// A press that opens a menu nobody draws answers with nothing on screen, and
/// no assertion about the engine can tell that apart from a menu that appeared:
/// the open flag is set either way. So the pointer is driven onto the closed
/// face and the scene is asked what came of it.
#[kithara::test]
fn the_scope_menu_a_press_opens_reaches_the_retained_scene() {
    let (mut root, face) = scope_strip_root();
    let closed = scene_size(&mut root);

    press_release(&mut root, face);

    assert!(
        scene_size(&mut root) > closed,
        "the retained host drew the same picture with the menu open as with it closed, so the \
         press opened a menu that is not on screen"
    );
}

/// The other direction, which is what says the growth above was the menu and
/// not something the first press woke up for good.
#[kithara::test]
fn dismissing_the_scope_menu_takes_its_drawing_off_again() {
    let (mut root, face) = scope_strip_root();
    let closed = scene_size(&mut root);
    press_release(&mut root, face);
    let open = scene_size(&mut root);

    press_release(&mut root, (150.0, 110.0));

    let dismissed = scene_size(&mut root);
    assert!(
        dismissed < open,
        "dismissing the menu left its drawing in the scene: {dismissed} against {closed} closed \
         and {open} open"
    );
}

/// The strip mounted on the retained host, with the centre of its closed scope
/// face — the one point on the strip that opens the menu.
fn scope_strip_root() -> (MasonryRoot<TestAction>, (f32, f32)) {
    /// The strip whose scope picker the retained host has to draw.
    const SCOPE_STRIP: &str = r#"Row(size: (w: Fill, h: Fill), gap: 0.0, pad: 0.0, children: [
    ContextBar(
        id: "context",
        size: Some((w: Fill, h: Fixed(26.0))),
        read: Model(id: "library.breadcrumb"),
        write: Model(id: "library.scope"),
        scope_items: ["ZVUK", "LOCAL"],
        scope: Model(id: "library.scope"),
    ),
])"#;

    let registry = fixture_registry();
    let reads = FixtureReads;
    let ui = fixture_ui("leaf-fixture", SCOPE_STRIP, &registry);
    let host = MasonryHost::map_actions(ctx(&ui, &reads), builtin::skin(), TestAction::Document);
    let (output, built) =
        DocumentNodes::built(|| document::render(&ui.root, ctx(&ui, &reads), host));
    let mut root = masonry_root(output, 200, 120);
    let control_id = built.last_in(root.root());
    root.redraw()
        .unwrap_or_else(|error| panic!("the scope strip must compose: {error}"));
    let bounds = root
        .root()
        .get_widget(control_id)
        .unwrap_or_else(|| panic!("the scope strip must stay registered"))
        .ctx()
        .bounding_rect();
    let skin = builtin::skin();
    let mut text = TextContext::from(skin.text_resources());
    let face = Context::placed(
        Context::new(skin).face_of(&mut text, ["ZVUK", "LOCAL"]),
        Rect {
            h: 0.0,
            w: 0.0,
            x: bounds.x0.as_(),
            y: bounds.y0.as_(),
        },
    );
    (root, (face.x + face.w / 2.0, face.y + face.h / 2.0))
}

/// How much the retained host drew, in the one unit a Vello scene reports.
fn scene_size(root: &mut MasonryRoot<TestAction>) -> usize {
    let (scene, _access) = root
        .redraw()
        .unwrap_or_else(|error| panic!("the retained host must draw: {error}"));
    let encoding = scene.encoding();
    encoding.path_data.len() + encoding.draw_data.len()
}

fn press_release(root: &mut MasonryRoot<TestAction>, (x, y): (f32, f32)) {
    root.handle_pointer_event(pointer_down(x.into(), y.into()))
        .unwrap_or_else(|error| panic!("the press must route: {error}"));
    root.handle_pointer_event(pointer_up(x.into(), y.into()))
        .unwrap_or_else(|error| panic!("the release must route: {error}"));
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
        let host =
            MasonryHost::map_actions(ctx(&ui, &reads), builtin::skin(), TestAction::Document);
        let output = document::render(&ui.root, ctx(&ui, &reads), host);
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
    let host = MasonryHost::map_actions(ctx(&ui, &reads), builtin::skin(), TestAction::Document)
        .with_custom(
            "demo/custom",
            CaptureProbe {
                observations: Rc::clone(&observations),
            },
            |()| TestAction::Document(UiEvent::OpenSettings),
        );
    let output = document::render(&ui.root, ctx(&ui, &reads), host);
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

fn assert_step(actions: &[TestAction], path: &str, expected: f32) {
    let [
        TestAction::Document(UiEvent::Control {
            path: actual,
            action: ControlAction::StepScalar(steps),
        }),
    ] = actions
    else {
        panic!("a stepping surface must emit exactly one typed step action: {actions:?}");
    };
    assert_eq!(actual, path);
    assert_eq!(*steps, expected);
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

/// The kind the input fixture names. The paint census registers a kind of its
/// own, so a row of it cannot start answering pointers to keep a test green.
const PRESS_KIND: &str = "press-extension";

/// Opaque, so an empty Vello scene cannot be mistaken for ink nothing can see.
const PRESS_INK: Rgba = Rgba {
    a: 1.0,
    b: 1.0,
    g: 1.0,
    r: 1.0,
};

/// An extension that claims a press and answers it with its own action.
struct PressExtension;

impl CustomWidget for PressExtension {
    type Action = ();

    fn input(&mut self, input: Input<'_>, hit: Hit) -> Outcome<Self::Action> {
        let Input::Pointer(pointer) = input else {
            return Outcome::IGNORED;
        };
        if pointer.phase == PointerPhase::Down && hit.over() {
            return Outcome::set(()).with_ownership(PointerOwnership::Claim);
        }
        Outcome::IGNORED
    }

    fn measure(&mut self, _text: &mut TextMeasurer<'_>, _limits: SizeLimits) -> Size2 {
        Size2::new(40.0, 40.0)
    }

    fn paint(
        &mut self,
        list: &mut DrawListBuilder,
        _text: &mut TextMeasurer<'_>,
        bounds: Rect,
        _skin: &CustomSkin,
    ) {
        list.fill_rect(bounds, PRESS_INK);
    }
}

fn press_kinds() -> CustomKinds {
    CustomKinds::default().with(PRESS_KIND, || PressExtension, |()| UiEvent::OpenSettings)
}

#[kithara::test]
fn retained_vis_declares_exact_logical_frames_and_continuous_repaint() {
    let mut registry = fixture_registry();
    for id in ["vis.first", "vis.second"] {
        registry.insert(
            EndpointCategory::Model,
            id,
            EndpointDesc::new(ValueKind::Scalar),
        );
    }
    let reads = VisReads {
        left: Cell::new(0.25),
        right: Cell::new(0.75),
        volume: Cell::new(0.8),
        time: Cell::new(1.25),
        first: Cell::new(0.2),
        second: Cell::new(1.8),
        levels_present: Cell::new(true),
    };
    let ui = fixture_ui(
        "vis-fixture",
        r#"Row(size: (w: Fill, h: Fill), gap: 0.0, pad: 0.0, children: [
            Vis(id: "first", read: Model(id: "vis.first"), size: Some((w: Fixed(40.0), h: Fill))),
            Vis(id: "second", read: Model(id: "vis.second"), size: Some((w: Fixed(40.0), h: Fill))),
        ])"#,
        &registry,
    );
    let output = document::render(
        &ui.root,
        ctx(&ui, &reads),
        MasonryHost::new(ctx(&ui, &reads), builtin::skin()),
    );
    let mut root = masonry_root(output, 80, 20);
    root.redraw()
        .unwrap_or_else(|error| panic!("Vis leaves must reach retained layout: {error}"));

    assert!(
        root.take_platform_signals()
            .into_iter()
            .any(|signal| matches!(signal, RenderRootSignal::RequestAnimFrame)),
        "a retained Vis leaf must request continuous animation frames"
    );
    let first = VisFrame::read(reads.get("vis.first"), &reads).expect("a valid first preset");
    let second = VisFrame::read(reads.get("vis.second"), &reads).expect("a valid second preset");
    assert_eq!((first.preset(), second.preset()), (0, 2));
    assert!((first.level() - 0.6).abs() < f32::EPSILON);
    assert_eq!(first.time(), 1.25);
    assert_eq!(
        root.vis_declarations(),
        [
            VisDeclaration::logical(first, [0.0, 0.0, 40.0, 20.0]),
            VisDeclaration::logical(second, [40.0, 0.0, 80.0, 20.0]),
        ]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>(),
        "each leaf declares its own box and the frame its reads project to"
    );
    assert_eq!(
        root.handle_pointer_event(pointer_down(20.0, 10.0))
            .unwrap_or_else(|error| panic!("Vis pointer routing must remain typed: {error}")),
        Handled::No,
        "Vis is render-only and must not claim pointer input"
    );
    assert!(root.take_actions().is_empty(), "Vis must emit no event");

    reads.first.set(1.0);
    reads.right.set(0.5);
    reads.volume.set(0.5);
    reads.time.set(9.0);
    root.refresh(ctx(&ui, &reads));
    root.redraw()
        .unwrap_or_else(|error| panic!("Vis refresh must not remount the tree: {error}"));
    let first = VisFrame::read(reads.get("vis.first"), &reads).expect("a valid first preset");
    let second = VisFrame::read(reads.get("vis.second"), &reads).expect("a valid second preset");
    assert_eq!(first.preset(), 1);
    assert!((first.level() - 0.25).abs() < f32::EPSILON);
    assert_eq!(first.time(), 9.0);
    assert_eq!(
        root.vis_declarations(),
        [
            VisDeclaration::logical(first, [0.0, 0.0, 40.0, 20.0]),
            VisDeclaration::logical(second, [40.0, 0.0, 80.0, 20.0]),
        ]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>(),
        "a refresh re-reads both frames without remounting"
    );

    reads.first.set(f64::NAN);
    root.refresh(ctx(&ui, &reads));
    assert_eq!(
        root.vis_declarations().len(),
        1,
        "an invalid preset suppresses only its own native declaration"
    );
    reads.levels_present.set(false);
    root.refresh(ctx(&ui, &reads));
    assert!(
        root.vis_declarations().is_empty(),
        "missing levels suppress every Vis declaration"
    );
}

/// A continuous Vis in a cell the flow stashes once the window is too narrow
/// to reach it.
#[kithara::test]
fn stashed_continuous_vis_stops_and_unstashing_restarts_animation_frames() {
    let registry = fixture_registry();
    let reads = FixtureReads;
    let ui = fixture_ui(
        "vis-stashing-fixture",
        r#"Row(id: "bar", measure: Width, size: (w: Fill, h: Fill), gap: 0.0, pad: 0.0, children: [
            Reveal(from: 200.0, child: Vis(id: "continuous", read: Model(id: "vis.preset"))),
        ])"#,
        &registry,
    );
    let (output, built) = DocumentNodes::built(|| {
        document::render(
            &ui.root,
            ctx(&ui, &reads),
            MasonryHost::new(ctx(&ui, &reads), builtin::skin()),
        )
    });
    let (base, _, _, _, _, _, _, _, _): RootParts = output.into();
    let signals = Rc::new(RefCell::new(Vec::new()));
    let sink = Rc::clone(&signals);
    let mut root = RenderRoot::new(
        base,
        move |signal| sink.borrow_mut().push(signal),
        render_root_options(240, 20),
    );
    let vis = built.last_in(&root);
    let resize = |root: &mut RenderRoot, width| {
        root.handle_window_event(WindowEvent::Resize(PhysicalSize::new(width, 20)));
        root.redraw();
    };

    assert!(
        take_animation_request(&signals),
        "WidgetAdded must start a continuous Vis"
    );
    resize(&mut root, 120);
    assert!(
        root.get_widget(vis)
            .is_some_and(|widget| widget.ctx().is_stashed()),
        "a window too narrow for the cell must stash the Vis leaf"
    );
    signals.borrow_mut().clear();

    root.handle_window_event(WindowEvent::AnimFrame(Duration::from_millis(16)));
    assert!(
        !take_animation_request(&signals),
        "the already-requested callback for a stashed continuous Vis must not request another frame"
    );

    resize(&mut root, 240);
    assert!(
        root.get_widget(vis)
            .is_some_and(|widget| !widget.ctx().is_stashed()),
        "the Vis leaf must be visible again before repaint restarts"
    );
    assert!(
        take_animation_request(&signals),
        "unstashing must restart the leaf repaint contract"
    );

    root.handle_window_event(WindowEvent::AnimFrame(Duration::from_millis(16)));
    assert!(
        take_animation_request(&signals),
        "the visible continuous Vis must keep scheduling frames"
    );
}

#[kithara::test]
fn a_retained_preset_press_release_is_painted_and_publishes_the_selected_name() {
    let registry = fixture_registry();
    let reads = PresetReads {
        active: Cell::new(builtin::MICRO_PRESET),
    };
    let ui = fixture_ui(
        "leaf-fixture",
        r#"Row(size: (w: Fill, h: Fill), gap: 0.0, pad: 0.0, children: [
            PresetSelector(id: "presets"),
        ])"#,
        &registry,
    );
    let host = MasonryHost::map_actions(ctx(&ui, &reads), builtin::skin(), TestAction::Document);
    let output = document::render(&ui.root, ctx(&ui, &reads), host);
    let mut root = masonry_root(output, 126, 42);
    let (idle, _) = root
        .redraw()
        .unwrap_or_else(|error| panic!("idle PresetSelector must draw: {error}"));
    let idle_draw_data = idle.encoding().draw_data.clone();

    root.handle_pointer_event(pointer_hover(31.5, 21.0))
        .unwrap_or_else(|error| panic!("PresetSelector hover must route: {error}"));
    let (hovered, _) = root
        .redraw()
        .unwrap_or_else(|error| panic!("hovered PresetSelector must draw: {error}"));
    let hovered_draw_data = hovered.encoding().draw_data.clone();
    assert_ne!(hovered_draw_data, idle_draw_data);

    assert_eq!(
        root.handle_pointer_event(pointer_down(31.5, 21.0))
            .unwrap_or_else(|error| panic!("PresetSelector press must route: {error}")),
        Handled::Yes,
    );
    assert!(root.take_actions().is_empty());
    let (pressed, _) = root
        .redraw()
        .unwrap_or_else(|error| panic!("pressed PresetSelector must draw: {error}"));
    let pressed_draw_data = pressed.encoding().draw_data.clone();
    assert_ne!(pressed_draw_data, hovered_draw_data);

    root.handle_pointer_event(pointer_up(31.5, 21.0))
        .unwrap_or_else(|error| panic!("PresetSelector release must route: {error}"));
    assert_eq!(
        root.take_actions(),
        vec![TestAction::Document(UiEvent::SelectPreset(
            builtin::MICRO_PRESET.to_owned(),
        ))]
    );
    let (released, _) = root
        .redraw()
        .unwrap_or_else(|error| panic!("released PresetSelector must draw: {error}"));
    assert_eq!(released.encoding().draw_data, hovered_draw_data);

    root.handle_pointer_event(pointer_down(94.5, 21.0))
        .unwrap_or_else(|error| panic!("second PresetSelector cell must route: {error}"));
    assert!(root.take_actions().is_empty());
    root.handle_pointer_event(pointer_up(94.5, 21.0))
        .unwrap_or_else(|error| panic!("second PresetSelector release must route: {error}"));
    assert_eq!(
        root.take_actions(),
        vec![TestAction::Document(UiEvent::SelectPreset(
            builtin::PLAYER_PRESET.to_owned(),
        ))]
    );
}

#[kithara::test]
fn retained_refresh_changes_the_active_preset_without_remounting_the_leaf() {
    let registry = fixture_registry();
    let reads = PresetReads {
        active: Cell::new(builtin::MICRO_PRESET),
    };
    let ui = fixture_ui(
        "leaf-fixture",
        r#"Row(size: (w: Fill, h: Fill), gap: 0.0, pad: 0.0, children: [
            PresetSelector(id: "presets"),
        ])"#,
        &registry,
    );
    let (output, built) = DocumentNodes::built(|| {
        document::render(
            &ui.root,
            ctx(&ui, &reads),
            MasonryHost::new(ctx(&ui, &reads), builtin::skin()),
        )
    });
    let mut root = masonry_root(output, 126, 42);
    let id = built.last_in(root.root());
    let (micro, _) = root
        .redraw()
        .unwrap_or_else(|error| panic!("MICRO PresetSelector must draw: {error}"));
    let micro_draw_data = micro.encoding().draw_data.clone();

    reads.active.set(builtin::PLAYER_PRESET);
    root.refresh(ctx(&ui, &reads));
    let (player, _) = root
        .redraw()
        .unwrap_or_else(|error| panic!("PLAYER PresetSelector must draw: {error}"));

    assert_ne!(micro_draw_data, player.encoding().draw_data);
    assert!(
        root.root().get_widget(id).is_some(),
        "refresh must update the mounted leaf rather than replace it"
    );
}

/// One scalar an object can be driven by, standing in for whatever the
/// application advances between frames.
struct DrivenReads {
    along: Cell<f64>,
}

impl Reads for DrivenReads {
    fn get(&self, endpoint: &str) -> Option<ReadValue<'_>> {
        (endpoint == "deck.view.zoom").then(|| ReadValue::Scalar(self.along.get()))
    }
}

fn driven_root(
    root: &str,
    reads: &DrivenReads,
) -> (CompiledUi, MasonryState, MasonryRoot<UiEvent>) {
    let registry = fixture_registry();
    let ui = fixture_ui("driven-fixture", root, &registry);
    let state = MasonryState::default();
    let output = document::render(
        &ui.root,
        ctx(&ui, reads),
        MasonryHost::new(ctx(&ui, reads), builtin::skin()).with_state(state.clone()),
    );
    let root = masonry_root(output, 200, 120);
    (ui, state, root)
}

/// Whether a refresh of `root` moved the object mounted at `path`.
fn refresh_moves(
    root: &mut MasonryRoot<UiEvent>,
    state: &MasonryState,
    path: &str,
    refresh: impl FnOnce(&mut MasonryRoot<UiEvent>),
) -> bool {
    let id = state
        .widget_id(path)
        .unwrap_or_else(|| panic!("`{path}` must stay addressable"));
    let _trace = usdt::scope();
    let before = usdt::events_of("masonry_object_moved").len();
    refresh(root);
    usdt::events_of("masonry_object_moved")[before..]
        .iter()
        .any(|moved| moved.field("widget") == Some(id.to_raw()))
}

const DRIVEN: &str = r#"Row(size: (w: Fill, h: Fill), gap: 0.0, pad: 0.0, children: [
    Object(
        id: "travel",
        to: (position: (100.0, 0.0)),
        phase: Model(id: "deck.view.zoom"),
        child: Spacer(id: "carried", size: Some((w: Fixed(40.0), h: Fixed(20.0)))),
    ),
])"#;

/// What a refresh is for, asked of the one thing a rebuild used to answer.
///
/// The retained host keeps its tree across frames and re-reads the document
/// into it instead. An object's pose is part of what the document says, so a
/// clock the application advanced has to reach the mounted node the same way a
/// control's value does — otherwise the immediate host animates and this one
/// stands still, and a single-frame parity capture cannot tell.
#[kithara::test]
fn a_driven_object_moves_when_the_retained_host_refreshes() {
    let reads = DrivenReads {
        along: Cell::new(0.0),
    };
    let (ui, state, mut root) = driven_root(DRIVEN, &reads);

    reads.along.set(1.0);
    assert!(refresh_moves(&mut root, &state, "demo/carried", |root| {
        root.refresh(ctx(&ui, &reads));
    }));
}

/// And an object nobody drives holds still across the same refresh, which is
/// what makes the test above a measurement rather than a statement about the
/// harness moving everything it touches.
#[kithara::test]
fn an_object_nobody_drives_keeps_its_pose_across_a_refresh() {
    const STILL: &str = r#"Row(size: (w: Fill, h: Fill), gap: 0.0, pad: 0.0, children: [
        Object(
            id: "posed",
            transform: (position: (10.0, 4.0)),
            child: Spacer(id: "carried", size: Some((w: Fixed(40.0), h: Fixed(20.0)))),
        ),
    ])"#;

    let reads = DrivenReads {
        along: Cell::new(0.0),
    };
    let (ui, state, mut root) = driven_root(STILL, &reads);

    reads.along.set(1.0);
    assert!(!refresh_moves(&mut root, &state, "demo/carried", |root| {
        root.refresh(ctx(&ui, &reads));
    }));
}

#[kithara::test]
fn a_mounted_table_draws_rows_that_arrive_during_refresh() {
    let registry = fixture_registry();
    let ui = fixture_ui(
        "late-track-list",
        r#"Row(size: (w: Fill, h: Fill), gap: 0.0, pad: 0.0, children: [
            Table(
                id: "tracks",
                read: Model(id: "library.visible_tracks"),
                columns: [(id: "title", label: "TITLE", style: Primary, width: 180.0)],
            ),
        ])"#,
        &registry,
    );
    let reads = LateTrackReads {
        loaded: Cell::new(false),
    };
    let output = document::render(
        &ui.root,
        ctx(&ui, &reads),
        MasonryHost::new(ctx(&ui, &reads), builtin::skin()),
    );
    let mut root = masonry_root(output, 240, 160);
    let (before, _) = root
        .redraw()
        .unwrap_or_else(|error| panic!("empty Table must draw its frame: {error}"));
    let before_glyphs = before.encoding().resources.glyphs.len();
    reads.loaded.set(true);
    root.refresh(ctx(&ui, &reads));
    let (after, _) = root
        .redraw()
        .unwrap_or_else(|error| panic!("refreshed Table must draw its rows: {error}"));

    assert!(after.encoding().resources.glyphs.len() > before_glyphs);
}

#[kithara::test]
fn a_mounted_table_repaints_the_row_under_the_pointer() {
    let registry = fixture_registry();
    let ui = fixture_ui(
        "hovered-track-list",
        r#"Row(size: (w: Fill, h: Fill), gap: 0.0, pad: 0.0, children: [
            Table(
                id: "tracks",
                read: Model(id: "library.visible_tracks"),
                columns: [(id: "title", label: "TITLE", style: Primary, width: 180.0)],
            ),
        ])"#,
        &registry,
    );
    let reads = LateTrackReads {
        loaded: Cell::new(true),
    };
    let output = document::render(
        &ui.root,
        ctx(&ui, &reads),
        MasonryHost::new(ctx(&ui, &reads), builtin::skin()),
    );
    let mut root = masonry_root(output, 240, 160);
    let (idle, _) = root
        .redraw()
        .unwrap_or_else(|error| panic!("idle Table must draw: {error}"));
    let idle_draw_data = idle.encoding().draw_data.clone();
    let skin = builtin::skin();
    let row_y = skin.table.header_height + skin.table.grid_gap + skin.table.row_height / 2.0;
    root.handle_pointer_event(pointer_hover(20.0, row_y.into()))
        .unwrap_or_else(|error| panic!("Table hover must route: {error}"));
    let (hovered, _) = root
        .redraw()
        .unwrap_or_else(|error| panic!("hovered Table must repaint: {error}"));

    assert_ne!(hovered.encoding().draw_data, idle_draw_data);
}

/// A wheel over a list whose engine stands above it still repaints the list.
///
/// A module the document hands to an engine mounts its controls as
/// `InputOwner::Engine`, so the engine is hosted on the module's content rather
/// than on the table inside it. The offset a wheel moves is read where the
/// table is drawn, and Masonry paints the widget that asked for paint and no
/// other, so a repaint aimed anywhere else leaves the list standing still.
#[kithara::test]
fn a_mounted_table_repaints_after_scrolling_under_a_hosted_engine() {
    /// A module id the facade hands to an engine, so the control inside it is
    /// mounted as `InputOwner::Engine` and the engine stands on the module rather
    /// than on the control. Every other fixture here names an id of its own, which
    /// is the shape where the two are the same node.
    const HOSTED_MODULE: &str = "gallery-table-tab";

    let registry = fixture_registry();
    let ui = fixture_ui(
        HOSTED_MODULE,
        r#"Row(size: (w: Fill, h: Fill), gap: 0.0, pad: 0.0, children: [
            Table(
                id: "tracks",
                read: Model(id: "library.visible_tracks"),
                columns: [(id: "title", label: "TITLE", style: Primary, width: 180.0)],
            ),
        ])"#,
        &registry,
    );
    let reads = LateTrackReads {
        loaded: Cell::new(true),
    };
    let output = document::render(
        &ui.root,
        ctx(&ui, &reads),
        MasonryHost::new(ctx(&ui, &reads), builtin::skin()),
    );
    let mut root = masonry_root(output, 240, 160);
    let (idle, _) = root
        .redraw()
        .unwrap_or_else(|error| panic!("unscrolled Table must draw: {error}"));
    let idle_draw_data = idle.encoding().draw_data.clone();
    let skin = builtin::skin();
    let row_y = skin.table.header_height + skin.table.grid_gap + skin.table.row_height / 2.0;
    root.handle_pointer_event(pointer_scroll(
        20.0,
        row_y.into(),
        ScrollDelta::LineDelta(0.0, -1.0),
    ))
    .unwrap_or_else(|error| panic!("Table scroll must route: {error}"));
    let (scrolled, _) = root
        .redraw()
        .unwrap_or_else(|error| panic!("scrolled Table must repaint: {error}"));

    assert_ne!(scrolled.encoding().draw_data, idle_draw_data);
}

#[kithara::test]
fn a_mounted_tree_refreshes_rows_and_query_independently() {
    let registry = fixture_registry();
    let ui = fixture_ui(
        "late-tree",
        r#"Row(size: (w: Fill, h: Fill), gap: 0.0, pad: 0.0, children: [
            Tree(
                id: "browser",
                read: Model(id: "library.tree"),
                query: Model(id: "library.query"),
            ),
        ])"#,
        &registry,
    );
    let reads = LateTreeReads {
        query_loaded: Cell::new(false),
        rows_loaded: Cell::new(false),
    };
    let output = document::render(
        &ui.root,
        ctx(&ui, &reads),
        MasonryHost::new(ctx(&ui, &reads), builtin::skin()),
    );
    let mut root = masonry_root(output, 240, 160);
    let _trace = usdt::scope();
    let refreshed = || {
        usdt::last("masonry_tree_refreshed")
            .map(|tree| (tree.field("rows"), tree.field("query_chars")))
    };
    let (before, _) = root
        .redraw()
        .unwrap_or_else(|error| panic!("empty Tree must draw its frame: {error}"));
    let before_draw_data = before.encoding().draw_data.clone();

    reads.rows_loaded.set(true);
    root.refresh(ctx(&ui, &reads));
    assert_eq!(
        refreshed(),
        Some((Some(TREE_ROWS.len().as_()), Some(0))),
        "row refresh must not change the independently empty query"
    );
    let (with_rows, _) = root
        .redraw()
        .unwrap_or_else(|error| panic!("refreshed Tree must draw its rows: {error}"));
    let with_rows_draw_data = with_rows.encoding().draw_data.clone();
    assert_ne!(
        with_rows_draw_data, before_draw_data,
        "the mounted scene must repaint when Tree rows arrive"
    );

    reads.query_loaded.set(true);
    root.refresh(ctx(&ui, &reads));
    assert_eq!(
        refreshed(),
        Some((
            Some(TREE_ROWS.len().as_()),
            Some("Late".chars().count().as_())
        )),
        "query refresh must retain the independently loaded rows"
    );
    let (with_query, _) = root
        .redraw()
        .unwrap_or_else(|error| panic!("refreshed Tree must draw its query: {error}"));
    assert_ne!(
        with_query.encoding().draw_data,
        with_rows_draw_data,
        "the mounted scene must repaint when the Tree query arrives"
    );
}

#[kithara::test]
fn a_mounted_tree_row_click_emits_its_typed_index_action() {
    let registry = fixture_registry();
    let ui = fixture_ui(
        "tree-row-action",
        r#"Row(size: (w: Fill, h: Fill), gap: 0.0, pad: 0.0, children: [
            Tree(
                id: "browser",
                read: Model(id: "library.tree"),
                query: Model(id: "library.query"),
            ),
        ])"#,
        &registry,
    );
    let reads = LateTreeReads {
        query_loaded: Cell::new(false),
        rows_loaded: Cell::new(true),
    };
    let output = document::render(
        &ui.root,
        ctx(&ui, &reads),
        MasonryHost::new(ctx(&ui, &reads), builtin::skin()),
    );
    let mut root = masonry_root(output, 240, 160);
    let expected = 2_usize;
    let skin = builtin::skin();
    let row_y = skin.tree.search_height
        + skin.tree.panel_padding_top
        + skin.tree.row_height * (AsPrimitive::<f32>::as_(expected) + 0.5);

    assert_eq!(
        root.handle_pointer_event(pointer_down(20.0, row_y.into()))
            .unwrap_or_else(|error| panic!("Tree row press must route: {error}")),
        Handled::Yes,
    );
    assert_eq!(
        root.take_actions(),
        vec![UiEvent::Control {
            path: "demo/browser".to_owned(),
            action: ControlAction::SelectIndex(expected),
        }],
        "the retained Tree target must preserve its control path and row index"
    );
}

#[kithara::test]
fn a_mounted_tree_search_ime_commit_emits_the_complete_query() {
    let registry = fixture_registry();
    let ui = fixture_ui(
        "tree-query-action",
        r#"Row(size: (w: Fill, h: Fill), gap: 0.0, pad: 0.0, children: [
            Tree(
                id: "browser",
                read: Model(id: "library.tree"),
                query: Model(id: "library.query"),
            ),
        ])"#,
        &registry,
    );
    let reads = LateTreeReads {
        query_loaded: Cell::new(false),
        rows_loaded: Cell::new(true),
    };
    let output = document::render(
        &ui.root,
        ctx(&ui, &reads),
        MasonryHost::new(ctx(&ui, &reads), builtin::skin()),
    );
    let mut root = masonry_root(output, 240, 160);
    let skin = builtin::skin();
    let search_x = skin.tree.search_icon_width + 1.0 + skin.tree.search_padding_x;
    let search_y = skin.tree.search_height / 2.0;

    assert_eq!(
        root.handle_pointer_event(pointer_down(search_x.into(), search_y.into()))
            .unwrap_or_else(|error| panic!("Tree search press must focus: {error}")),
        Handled::Yes,
    );
    assert!(
        root.root().focused_widget().is_some(),
        "the real Tree search target must own Masonry focus before text input"
    );
    assert_eq!(
        root.handle_pointer_event(pointer_up(search_x.into(), search_y.into()))
            .unwrap_or_else(|error| panic!("Tree search release must route: {error}")),
        Handled::Yes,
    );
    assert!(root.take_actions().is_empty());
    assert_eq!(
        root.handle_text_event(TextEvent::Ime(Ime::Commit("needle".to_owned())))
            .unwrap_or_else(|error| panic!("Tree search IME commit must route: {error}")),
        Handled::Yes,
    );
    assert_eq!(
        root.take_actions(),
        vec![UiEvent::LibraryQuery("needle".to_owned())],
        "the focused retained search target must publish the complete committed query"
    );
}

#[kithara::test]
fn a_mounted_tree_repaints_after_scrolling() {
    let registry = fixture_registry();
    let ui = fixture_ui(
        "scrolled-tree",
        r#"Row(size: (w: Fill, h: Fill), gap: 0.0, pad: 0.0, children: [
            Tree(
                id: "browser",
                read: Model(id: "library.tree"),
                query: Model(id: "library.query"),
            ),
        ])"#,
        &registry,
    );
    let reads = LateTreeReads {
        query_loaded: Cell::new(true),
        rows_loaded: Cell::new(true),
    };
    let output = document::render(
        &ui.root,
        ctx(&ui, &reads),
        MasonryHost::new(ctx(&ui, &reads), builtin::skin()),
    );
    let mut root = masonry_root(output, 240, 120);
    let (idle, _) = root
        .redraw()
        .unwrap_or_else(|error| panic!("unscrolled Tree must draw: {error}"));
    let idle_draw_data = idle.encoding().draw_data.clone();
    let skin = builtin::skin();
    let row_y = skin.tree.search_height + skin.tree.panel_padding_top + skin.tree.row_height / 2.0;
    root.handle_pointer_event(pointer_scroll(
        20.0,
        row_y.into(),
        ScrollDelta::LineDelta(0.0, -1.0),
    ))
    .unwrap_or_else(|error| panic!("Tree scroll must route: {error}"));
    let (scrolled, _) = root
        .redraw()
        .unwrap_or_else(|error| panic!("scrolled Tree must repaint: {error}"));

    assert_ne!(scrolled.encoding().draw_data, idle_draw_data);
}

#[kithara::test]
fn a_mounted_tree_repaints_the_row_under_the_pointer() {
    let registry = fixture_registry();
    let ui = fixture_ui(
        "hovered-tree",
        r#"Row(size: (w: Fill, h: Fill), gap: 0.0, pad: 0.0, children: [
            Tree(
                id: "browser",
                read: Model(id: "library.tree"),
                query: Model(id: "library.query"),
            ),
        ])"#,
        &registry,
    );
    let reads = LateTreeReads {
        query_loaded: Cell::new(true),
        rows_loaded: Cell::new(true),
    };
    let output = document::render(
        &ui.root,
        ctx(&ui, &reads),
        MasonryHost::new(ctx(&ui, &reads), builtin::skin()),
    );
    let mut root = masonry_root(output, 240, 160);
    let (idle, _) = root
        .redraw()
        .unwrap_or_else(|error| panic!("idle Tree must draw: {error}"));
    let idle_draw_data = idle.encoding().draw_data.clone();
    let skin = builtin::skin();
    let row_y = skin.tree.search_height + skin.tree.panel_padding_top + skin.tree.row_height / 2.0;
    root.handle_pointer_event(pointer_hover(20.0, row_y.into()))
        .unwrap_or_else(|error| panic!("Tree hover must route: {error}"));
    let (hovered, _) = root
        .redraw()
        .unwrap_or_else(|error| panic!("hovered Tree must repaint: {error}"));

    assert_ne!(hovered.encoding().draw_data, idle_draw_data);
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
        builtin::text_doc(),
        &UiConfig::builder()
            .custom_kinds([PRESS_KIND.to_owned()].into_iter().collect())
            .build(),
        &view::EMPTY,
    )
    .unwrap_or_else(|error| panic!("Masonry contract fixture must compile: {error}"))
}

/// The Masonry nodes one render announced as document nodes.
///
/// The host announces each node that stands for a document node as it builds
/// it, with whether the nodes it holds stand for document nodes of their own.
/// Walking the retained tree down from its root through the nodes that do gives
/// them in document order.
struct DocumentNodes(Vec<ProbeEvent>);

impl DocumentNodes {
    const PROBE: &str = "masonry_document_node";

    /// The output `render` builds, with the document nodes it announced.
    fn built<Action>(render: impl FnOnce() -> MasonryNode<Action>) -> (MasonryNode<Action>, Self) {
        let _trace = usdt::scope();
        let before = usdt::events_of(Self::PROBE).len();
        let output = render();
        let mut announced = usdt::events_of(Self::PROBE);
        (output, Self(announced.split_off(before)))
    }

    /// Every announced node in `root`'s tree, in document order.
    fn in_tree(&self, root: &RenderRoot) -> Vec<WidgetId> {
        let mut ids = Vec::new();
        self.collect(root.get_layer_root(0), &mut ids);
        ids
    }

    /// The document's last node, where a fixture's one control stands.
    fn last_in(&self, root: &RenderRoot) -> WidgetId {
        self.in_tree(root)
            .pop()
            .unwrap_or_else(|| panic!("the fixture must retain a document node"))
    }

    fn collect(&self, widget: WidgetRef<'_, dyn Widget>, ids: &mut Vec<WidgetId>) {
        let raw = widget.id().to_raw();
        let Some(node) = self.0.iter().find(|node| node.field("widget") == Some(raw)) else {
            return;
        };
        ids.push(widget.id());
        if node.field("exposes_children") == Some(1) {
            for child in widget.children() {
                self.collect(child, ids);
            }
        }
    }
}

fn masonry_root<Action>(output: MasonryNode<Action>, width: u32, height: u32) -> MasonryRoot<Action>
where
    Action: std::fmt::Debug + Send + 'static,
{
    MasonryRoot::new(output, render_root_options(width, height))
        .unwrap_or_else(|error| panic!("Masonry fixture root must retain typed actions: {error}"))
}

fn render_root_options(width: u32, height: u32) -> RenderRootOptions {
    RenderRootOptions {
        default_properties: Arc::new(default_property_set()),
        use_system_fonts: false,
        size_policy: WindowSizePolicy::User,
        size: PhysicalSize::new(width, height),
        scale_factor: 1.0,
        test_font: None,
    }
}

fn take_animation_request(signals: &Rc<RefCell<Vec<RenderRootSignal>>>) -> bool {
    signals
        .borrow_mut()
        .drain(..)
        .any(|signal| matches!(signal, RenderRootSignal::RequestAnimFrame))
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
        state,
        button: Some(MasonryPointerButton::Primary),
        pointer: pointer_info(),
    })
}

fn pointer_up(x: f64, y: f64) -> PointerEvent {
    pointer_up_with_count(x, y, 1)
}

fn pointer_up_with_count(x: f64, y: f64, count: u8) -> PointerEvent {
    let mut state = pointer_state(x, y, false);
    state.count = count;
    PointerEvent::Up(PointerButtonEvent {
        state,
        button: Some(MasonryPointerButton::Primary),
        pointer: pointer_info(),
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
        delta,
        pointer: pointer_info(),
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
        let placed = if fields.clone().next() == Some("-") {
            fields.next();
            None
        } else {
            let mut number = || {
                fields
                    .next()
                    .unwrap_or_else(|| panic!("fixture line `{line}` is missing a coordinate"))
                    .parse::<f64>()
                    .unwrap_or_else(|error| {
                        panic!("fixture line `{line}` has a bad coordinate: {error}")
                    })
            };
            Some([number(), number(), number(), number()])
        };
        rects.push(ExpectedRect {
            placed,
            path: path.to_owned(),
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
        EndpointDesc::new(ValueKind::Table),
    );
    registry.insert(
        EndpointCategory::Model,
        "library.breadcrumb",
        EndpointDesc::new(ValueKind::Text),
    );
    registry.insert(
        EndpointCategory::Model,
        "library.query",
        EndpointDesc::new(ValueKind::Text),
    );
    registry.insert(
        EndpointCategory::Model,
        "library.scope",
        EndpointDesc::new(ValueKind::Scalar),
    );
    registry.insert(
        EndpointCategory::Model,
        "library.tree",
        EndpointDesc::new(ValueKind::Tree),
    );
    registry.insert(
        EndpointCategory::Model,
        "demo.wave",
        EndpointDesc::new(ValueKind::Waveform),
    );
    registry.insert(
        EndpointCategory::Model,
        "vis.preset",
        EndpointDesc::new(ValueKind::Scalar),
    );
    registry.insert(
        EndpointCategory::Model,
        "pivot.map",
        EndpointDesc::new(ValueKind::PortalMap),
    );
    // A range reads the whole interval and writes one end of it, so the two
    // halves of its contract are two kinds under one name.
    registry.insert(
        EndpointCategory::Model,
        "pivot.range",
        EndpointDesc::new(ValueKind::Range),
    );
    registry.insert(
        EndpointCategory::Parameter,
        "pivot.range",
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

/// Four rows, stacked with no gap, each the skin's own row height.
const CLIPPED_ROWS: &str = r#"Column(gap: 0.0, children: [
    NavItem(id: "first", label: "FIRST", icon: Playlist, read: Model(id: "ui.menu.open")),
    NavItem(id: "second", label: "SECOND", icon: Playlist, read: Model(id: "ui.menu.open")),
    NavItem(id: "third", label: "THIRD", icon: Playlist, read: Model(id: "ui.menu.open")),
    NavItem(id: "fourth", label: "FOURTH", icon: Playlist, read: Model(id: "ui.menu.open")),
])"#;

/// Those rows mounted on the retained host, either inside a box one row tall or
/// with nothing around them. The pair is what makes a silent press readable: the
/// same document, the same point, and the box the only difference between them.
fn clipped_rows_root(boxed: bool) -> MasonryRoot<TestAction> {
    let height = builtin::skin().nav.item_height;
    let rows = if boxed {
        format!(
            r#"Column(gap: 0.0, pad: 0.0, size: (w: Fill, h: Fill), children: [
                Scroll(id: "rows", size: (w: Fill, h: Fixed({height})), child: {CLIPPED_ROWS}),
            ])"#
        )
    } else {
        format!(
            r#"Column(gap: 0.0, pad: 0.0, size: (w: Fill, h: Fill), children: [{CLIPPED_ROWS}])"#
        )
    };
    let mut registry = fixture_registry();
    registry.insert(
        EndpointCategory::Model,
        "ui.menu.open",
        EndpointDesc::new(ValueKind::Bool),
    );
    let reads = FixtureReads;
    let ui = fixture_ui("leaf-fixture", &rows, &registry);
    let host = MasonryHost::map_actions(ctx(&ui, &reads), builtin::skin(), TestAction::Document);
    let output = document::render(&ui.root, ctx(&ui, &reads), host);
    let mut root = masonry_root(output, 198, 200);
    root.redraw()
        .unwrap_or_else(|error| panic!("the clipped rows must compose: {error}"));
    root
}

/// The middle of the row at that index, counting from the top of the window.
fn row_middle(index: f32) -> (f32, f32) {
    let height = builtin::skin().nav.item_height;
    (99.0, height.mul_add(index, height / 2.0))
}

/// The retained host answers where its box shows a row.
#[kithara::test]
fn the_retained_box_answers_where_it_shows_a_row() {
    let mut root = clipped_rows_root(true);

    press_release(&mut root, row_middle(0.0));

    assert_eq!(
        root.take_actions(),
        vec![TestAction::Document(UiEvent::Control {
            path: "demo/first".to_owned(),
            action: ControlAction::Activate,
        })]
    );
}

/// And nowhere else: the rows it scrolled past its edge are not under a pointer
/// that never entered it. The same press with the box taken off reaches the row
/// the layout puts there, which is what says the box is what silenced it.
#[kithara::test]
fn the_retained_box_hides_the_rows_it_scrolled_past() {
    let at = row_middle(3.0);
    let mut unboxed = clipped_rows_root(false);
    press_release(&mut unboxed, at);
    assert_eq!(
        unboxed.take_actions(),
        vec![TestAction::Document(UiEvent::Control {
            path: "demo/fourth".to_owned(),
            action: ControlAction::Activate,
        })],
        "with nothing around them the rows reach that far down, or the press below measures nothing"
    );

    let mut boxed = clipped_rows_root(true);
    press_release(&mut boxed, at);

    assert_eq!(
        boxed.take_actions(),
        vec![],
        "the box is one row tall and nothing is drawn below it, so a press on blank window \
         answered by a row is a press the box never cut"
    );
}

/// A waveform with somewhere to publish its scrub, mounted alone.
///
/// The census entry for `Wave` binds only a reading, so nothing it did with a
/// pointer could ever have been seen. This one writes, which is what the decks
/// do: the overview and the hero wave both carry
/// `Command(id: "deck.transport.seek_normalized")`.
fn seeking_wave_root(extra: &str, takes_drops: bool) -> MasonryRoot<TestAction> {
    let mut registry = fixture_registry();
    registry.insert(
        EndpointCategory::Model,
        "demo.wave",
        EndpointDesc::new(ValueKind::Waveform),
    );
    registry.insert(
        EndpointCategory::Command,
        "demo.seek",
        EndpointDesc::new(ValueKind::Scalar),
    );
    registry.insert(
        EndpointCategory::Command,
        "demo.load",
        EndpointDesc::new(ValueKind::Trigger),
    );
    registry.insert(
        EndpointCategory::Model,
        "demo.drag.over",
        EndpointDesc::new(ValueKind::Bool),
    );
    let reads = FixtureReads;
    let takes = if takes_drops {
        r#"drop: Some((write: Command(id: "demo.load"), read: Model(id: "demo.drag.over"))),"#
    } else {
        ""
    };
    let mut resolver = MemResolver::default();
    resolver.insert(
        "fixture.klayout.ron",
        r#"(schema: "kithara.layout", version: 1, id: "fixture",
            root: Module(instance: "demo", source: "fixture.kmodule.ron", size: (w: Fill, h: Fill)))"#,
    );
    resolver.insert(
        "fixture.kmodule.ron",
        &format!(
            r#"(schema: "kithara.module", version: 1, id: "leaf-fixture", chrome: Plain, {takes}
            root: Column(gap: 0.0, pad: 0.0, size: (w: Fill, h: Fill), children: [
                Wave(
                    id: "wave",
                    size: Some((w: Fill, h: Fixed(80.0))),
                    read: Model(id: "demo.wave"),
                    write: Command(id: "demo.seek"),
                    {extra}
                ),
            ]))"#
        ),
    );
    let ui = compile(
        "fixture.klayout.ron",
        &resolver,
        &registry,
        builtin::skin_doc(),
        builtin::text_doc(),
        &UiConfig::default(),
        &view::EMPTY,
    )
    .unwrap_or_else(|error| panic!("the wave fixture must compile: {error}"));
    let host = MasonryHost::map_actions(ctx(&ui, &reads), builtin::skin(), TestAction::Document);
    let output = document::render(&ui.root, ctx(&ui, &reads), host);
    let mut root = masonry_root(output, 198, 200);
    root.redraw()
        .unwrap_or_else(|error| panic!("the wave must compose: {error}"));
    root
}

/// A hand drawn across the waveform seeks the track.
#[kithara::test]
fn a_hand_drawn_across_the_retained_wave_seeks() {
    for takes_drops in [false, true] {
        for extra in [
            "",
            r#"style: Hero,"#,
            r#"zoom: Model(id: "deck.view.zoom"),"#,
            r#"style: Hero, badge: Some("A"), zoom: Model(id: "deck.view.zoom"),"#,
        ] {
            let mut root = seeking_wave_root(extra, takes_drops);

            root.handle_pointer_event(pointer_down(49.0, 40.0))
                .unwrap_or_else(|error| panic!("the press must route: {error}"));
            root.handle_pointer_event(pointer_move(120.0, 40.0))
                .unwrap_or_else(|error| panic!("the move must route: {error}"));
            root.handle_pointer_event(pointer_up(120.0, 40.0))
                .unwrap_or_else(|error| panic!("the release must route: {error}"));

            let published = root.take_actions();
            assert!(
                published.iter().any(|action| matches!(
                    action,
                    TestAction::Document(UiEvent::Control {
                        action: ControlAction::SetScalar(_),
                        path,
                    }) if path.ends_with("/wave")
                )),
                "a hand drawn across a waveform that writes a seek must publish it, and the wave \
                 declared as `{extra}` in a module that takes drops={takes_drops} published \
                 {published:?}"
            );
        }
    }
}

/// The deck as the studio ships it, in the layout that ships it.
///
/// The isolated wave takes a gesture, and the shipped one does not, so the
/// difference is somewhere between them: the module takes drops, the wave is
/// adaptive and reads telemetry, a transport row sits under it, the column
/// paints a background, and the whole thing hangs off a split inside a window
/// that has resize edges.
fn studio_deck_root(reads: &DeckReads) -> (CompiledUi, MasonryRoot<TestAction>) {
    let mut registry = fixture_registry();
    registry.insert(
        EndpointCategory::Command,
        "deck.queue.load",
        EndpointDesc::new(ValueKind::Trigger).with_scope("deck"),
    );
    registry.insert(
        EndpointCategory::Model,
        "ui.drag.over",
        EndpointDesc::new(ValueKind::Bool).with_scope("deck"),
    );
    registry.insert(
        EndpointCategory::Model,
        "ui.drag.track",
        EndpointDesc::new(ValueKind::Text),
    );
    let mut resolver = MemResolver::default();
    resolver.insert(
        "studio.klayout.ron",
        r#"(schema: "kithara.layout", version: 1, id: "studio", resize_edges: true,
            dragged: Some(Model(id: "ui.drag.track")),
            root: Split(axis: Vertical, children: [
                (node: Module(instance: "bar", source: "bar.kmodule.ron",
                    size: (w: Fill, h: Fixed(42.0)))),
                (weight: 2.1, node: Split(axis: Horizontal, children: [
                    (weight: 1.0, node: Module(instance: "deck-a", source: "deck.kmodule.ron",
                        with: { "deck": "a", "letter": "A" })),
                    (weight: 1.0, node: Module(instance: "deck-b", source: "deck.kmodule.ron",
                        with: { "deck": "b", "letter": "B" })),
                ])),
            ]))"#,
    );
    resolver.insert(
        "bar.kmodule.ron",
        r#"(schema: "kithara.module", version: 1, id: "bar", chrome: Plain,
            root: Row(gap: 0.0, pad: 0.0, size: (w: Fill, h: Fill), children: [
                Slot(id: "gap", size: (w: Fill, h: Fill)),
            ]))"#,
    );
    resolver.insert(
        "deck.kmodule.ron",
        r#"(schema: "kithara.module", version: 1, id: "app-deck",
            parameters: ["deck", "letter"],
            drop: Some((
                write: Command(id: "deck.queue.load", with: { "deck": "$deck" }),
                read: Model(id: "ui.drag.over", with: { "deck": "$deck" }),
            )),
            root: Column(background: BgInset, children: [
                Wave(
                    id: "wave",
                    style: Hero,
                    badge: Some("$letter"),
                    read: Telemetry(id: "deck.playback.waveform", with: { "deck": "$deck" }),
                    write: Command(id: "deck.transport.seek_normalized", with: { "deck": "$deck" }),
                    zoom: Model(id: "deck.view.zoom"),
                ),
                Row(
                    id: "transport",
                    gap: 0.0,
                    size: (w: Fill, h: Fixed(38.0)),
                    background: BgDeep,
                    background_alpha: 0.84,
                    children: [Slot(id: "gap", size: (w: Fill, h: Fill))],
                ),
            ]))"#,
    );
    let ui = compile(
        "studio.klayout.ron",
        &resolver,
        &registry,
        builtin::skin_doc(),
        builtin::text_doc(),
        &UiConfig::default(),
        &view::EMPTY,
    )
    .unwrap_or_else(|error| panic!("the studio fixture must compile: {error}"));
    let host = MasonryHost::map_actions(ctx(&ui, reads), builtin::skin(), TestAction::Document);
    let output = document::render(&ui.root, ctx(&ui, reads), host);
    let mut root = masonry_root(output, 800, 400);
    root.redraw()
        .unwrap_or_else(|error| panic!("the first frame must draw: {error}"));
    (ui, root)
}

/// A deck whose track arrives after the tree is standing, which is the only way
/// a deck ever gets one: the module is mounted empty and something is loaded.
struct DeckReads {
    loaded: Cell<bool>,
    position: Cell<f32>,
}

impl DeckReads {
    fn loaded_at(position: f32) -> Self {
        Self {
            loaded: Cell::new(true),
            position: Cell::new(position),
        }
    }
}

impl Reads for DeckReads {
    fn get(&self, endpoint: &str) -> Option<ReadValue<'_>> {
        let id = endpoint.split_once('@').map_or(endpoint, |(id, _scope)| id);
        if id == "deck.playback.position_normalized" {
            return Some(ReadValue::Scalar(self.position.get().into()));
        }
        if id == "deck.playback.waveform" {
            return self
                .loaded
                .get()
                .then_some(ReadValue::Waveform(WaveformView {
                    beats: &[],
                    buckets: &CENSUS_WAVE,
                    revision: 0,
                    cues: &[],
                    downbeats: &[],
                    unready: &[],
                    bpm: None,
                    r#loop: None,
                }));
        }
        FixtureReads.get(endpoint)
    }
}

/// Draws a hand across the left deck's waveform and returns what it published.
fn scrub_the_deck(root: &mut MasonryRoot<TestAction>) -> Vec<TestAction> {
    root.handle_pointer_event(pointer_down(200.0, 200.0))
        .unwrap_or_else(|error| panic!("the press must route: {error}"));
    root.handle_pointer_event(pointer_move(300.0, 200.0))
        .unwrap_or_else(|error| panic!("the move must route: {error}"));
    root.handle_pointer_event(pointer_up(300.0, 200.0))
        .unwrap_or_else(|error| panic!("the release must route: {error}"));
    root.take_actions()
}

/// Whether a scrub reached the waveform.
fn published_seek(published: &[TestAction]) -> bool {
    published.iter().any(|action| {
        matches!(
            action,
            TestAction::Document(UiEvent::Control {
                action: ControlAction::SetScalar(_),
                path,
            }) if path.ends_with("/wave")
        )
    })
}

/// A hand drawn across the shipped deck's waveform seeks.
#[kithara::test]
fn a_hand_drawn_across_the_shipped_wave_seeks() {
    let reads = DeckReads::loaded_at(0.0);
    let (_ui, mut root) = studio_deck_root(&reads);

    let published = scrub_the_deck(&mut root);

    assert!(
        published_seek(&published),
        "a hand drawn across the shipped deck's waveform must publish a seek, and it published \
         {published:?}"
    );
}

/// A deck is mounted before it has a track, so a waveform that arrives after
/// the tree is standing must take a gesture the same as one that was there at
/// mount. A host that rebuilds the tree every frame gets this for free; one
/// that keeps a tree has to carry the new reading into what is already
/// mounted.
#[kithara::test]
fn a_waveform_that_arrives_after_the_mount_still_seeks() {
    let reads = DeckReads {
        loaded: Cell::new(false),
        position: Cell::new(0.0),
    };
    let (ui, mut root) = studio_deck_root(&reads);
    reads.loaded.set(true);
    root.refresh(ctx(&ui, &reads));
    root.redraw()
        .unwrap_or_else(|error| panic!("the loaded frame must draw: {error}"));

    let published = scrub_the_deck(&mut root);

    assert!(
        published_seek(&published),
        "a waveform loaded into a standing deck must take a scrub, and it published {published:?}"
    );
}

/// The seek a scrub published, if it published one.
fn seek_value(published: &[TestAction]) -> Option<f64> {
    published.iter().find_map(|action| match action {
        TestAction::Document(UiEvent::Control {
            action: ControlAction::SetScalar(value),
            path,
        }) if path.ends_with("/wave") => Some(*value),
        _ => None,
    })
}

/// A zoomed hero wave shows the window around where the track is now, so where
/// a hand lands in that window depends on the playhead. A host that keeps its
/// tree has to carry the moved playhead into the mounted gesture: otherwise the
/// same point on screen seeks to where the track was when the deck was mounted.
#[kithara::test]
fn a_scrub_seeks_by_the_window_the_wave_is_showing_now() {
    let standing_reads = DeckReads::loaded_at(0.0);
    let (ui, mut standing) = studio_deck_root(&standing_reads);
    standing_reads.position.set(0.5);
    standing.refresh(ctx(&ui, &standing_reads));
    standing
        .redraw()
        .unwrap_or_else(|error| panic!("the moved frame must draw: {error}"));
    let fresh_reads = DeckReads::loaded_at(0.5);
    let (_fresh_ui, mut fresh) = studio_deck_root(&fresh_reads);

    let standing_seek = seek_value(&scrub_the_deck(&mut standing));
    let fresh_seek = seek_value(&scrub_the_deck(&mut fresh));

    assert_eq!(
        standing_seek, fresh_seek,
        "the same hand on the same window must seek to the same place whether the deck was \
         mounted before the track moved or after it"
    );
}

/// A library that always has rows, beside a drop target that is not lit.
struct DragReads;

impl Reads for DragReads {
    fn get(&self, endpoint: &str) -> Option<ReadValue<'_>> {
        let id = endpoint.split_once('@').map_or(endpoint, |(id, _scope)| id);
        match id {
            "library.visible_tracks" => Some(ReadValue::Table(&LATE_TABLE_ROWS[..])),
            "demo.drag.over" => Some(ReadValue::Bool(false)),
            _ => FixtureReads.get(endpoint),
        }
    }
}

/// A track list beside a module that takes drops, the shape the studio ships.
fn dragging_library_root() -> MasonryRoot<TestAction> {
    let mut registry = fixture_registry();
    registry.insert(
        EndpointCategory::Command,
        "demo.load",
        EndpointDesc::new(ValueKind::Trigger),
    );
    registry.insert(
        EndpointCategory::Model,
        "demo.drag.over",
        EndpointDesc::new(ValueKind::Bool),
    );
    let reads = DragReads;
    let mut resolver = MemResolver::default();
    resolver.insert(
        "fixture.klayout.ron",
        r#"(schema: "kithara.layout", version: 1, id: "fixture",
            dragged: Some(Model(id: "library.breadcrumb")),
            root: Split(axis: Horizontal, children: [
                (node: Module(instance: "library", source: "library.kmodule.ron",
                    size: (w: Fixed(200.0), h: Fill))),
                (node: Module(instance: "deck", source: "deck.kmodule.ron",
                    size: (w: Fill, h: Fill))),
            ]))"#,
    );
    resolver.insert(
        "library.kmodule.ron",
        r#"(schema: "kithara.module", version: 1, id: "library", chrome: Plain,
            root: Column(gap: 0.0, pad: 0.0, size: (w: Fill, h: Fill), children: [
                Table(
                    id: "tracks",
                    size: (w: Fill, h: Fill),
                    read: Model(id: "library.visible_tracks"),
                    columns: [(id: "title", label: "TITLE", style: Primary, width: 180.0)],
                ),
            ]))"#,
    );
    resolver.insert(
        "deck.kmodule.ron",
        r#"(schema: "kithara.module", version: 1, id: "deck", chrome: Plain,
            drop: Some((write: Command(id: "demo.load"), read: Model(id: "demo.drag.over"))),
            root: Column(gap: 0.0, pad: 0.0, size: (w: Fill, h: Fill), children: [
                Text(id: "name", style: MicroLabel, label: "DECK", size: (w: Fill, h: Fill)),
            ]))"#,
    );
    let ui = compile(
        "fixture.klayout.ron",
        &resolver,
        &registry,
        builtin::skin_doc(),
        builtin::text_doc(),
        &UiConfig::default(),
        &view::EMPTY,
    )
    .unwrap_or_else(|error| panic!("the drag fixture must compile: {error}"));
    let host = MasonryHost::map_actions(ctx(&ui, &reads), builtin::skin(), TestAction::Document);
    let output = document::render(&ui.root, ctx(&ui, &reads), host);
    let mut root = masonry_root(output, 400, 200);
    root.redraw()
        .unwrap_or_else(|error| panic!("the drag fixture must compose: {error}"));
    root
}

/// Presses a row of the library and pulls it as far as the deck, still held.
fn carry_a_track_over_the_deck(root: &mut MasonryRoot<TestAction>) {
    root.handle_pointer_event(pointer_move(60.0, 60.0))
        .unwrap_or_else(|error| panic!("the hover must route: {error}"));
    root.handle_pointer_event(pointer_down(60.0, 60.0))
        .unwrap_or_else(|error| panic!("the press must route: {error}"));
    for at in [(64.0, 62.0), (150.0, 80.0)] {
        root.handle_pointer_event(pointer_move(at.0, at.1))
            .unwrap_or_else(|error| panic!("the move must route: {error}"));
    }
}

/// Presses a row of the library and pulls it across onto the deck.
fn drag_a_track_onto_the_deck(root: &mut MasonryRoot<TestAction>) -> Vec<TestAction> {
    carry_a_track_over_the_deck(root);
    root.handle_pointer_event(pointer_move(300.0, 100.0))
        .unwrap_or_else(|error| panic!("the move must route: {error}"));
    root.handle_pointer_event(pointer_up(300.0, 100.0))
        .unwrap_or_else(|error| panic!("the release must route: {error}"));
    root.take_actions()
}

/// Whether the published events carry this one, on a path under `instance`.
fn published_drag(published: &[TestAction], instance: &str, phase: DragPhase) -> bool {
    published.iter().any(|action| {
        matches!(
            action,
            TestAction::Document(UiEvent::Control { action, path })
                if *action == ControlAction::Drag(phase) && path.starts_with(instance)
        )
    })
}

/// The list reports the drag it started, wherever the hand then goes.
#[kithara::test]
fn a_track_pulled_out_of_the_list_reports_its_drag() {
    let published = drag_a_track_onto_the_deck(&mut dragging_library_root());

    assert!(
        published_drag(&published, "library", DragPhase::Start(1)),
        "the list a track is pulled out of must report the drag, published {published:?}"
    );
}

/// The module that takes drops reports the hand crossing into it.
#[kithara::test]
fn a_module_that_takes_drops_reports_the_hand_crossing_it() {
    let published = drag_a_track_onto_the_deck(&mut dragging_library_root());

    assert!(
        published_drag(&published, "deck", DragPhase::Over(true)),
        "a module that takes drops must report the hand above it, published {published:?}"
    );
}

/// A hand carrying a track says so, wherever on the window it has got to.
///
/// The list is the only side that knows a drag is running, and the tree asks
/// whatever sits under the pointer — which by then is the deck.
#[kithara::test]
fn a_hand_carrying_a_track_shows_it_over_a_module_it_did_not_start_in() {
    let mut root = dragging_library_root();

    carry_a_track_over_the_deck(&mut root);
    root.take_platform_signals();
    root.handle_pointer_event(pointer_move(300.0, 100.0))
        .unwrap_or_else(|error| panic!("the move must route: {error}"));

    let signals = root.take_platform_signals();
    assert!(
        signals
            .iter()
            .any(|signal| matches!(signal, RenderRootSignal::SetCursor(CursorIcon::Grabbing))),
        "a hand carrying a track over another module must still read as carrying it, \
         signalled {signals:?}"
    );
}

/// The hand lets go, and the cursor stops saying it is carrying something.
#[kithara::test]
fn the_cursor_stops_carrying_when_the_track_is_released() {
    let mut root = dragging_library_root();
    drag_a_track_onto_the_deck(&mut root);
    root.take_platform_signals();

    root.handle_pointer_event(pointer_move(310.0, 100.0))
        .unwrap_or_else(|error| panic!("the move must route: {error}"));

    let signals = root.take_platform_signals();
    assert!(
        !signals
            .iter()
            .any(|signal| matches!(signal, RenderRootSignal::SetCursor(CursorIcon::Grabbing))),
        "a released track must stop the carrying cursor, signalled {signals:?}"
    );
}

/// A window runner is handed the cursor the carrying tree asked for.
#[kithara::test]
fn taking_the_cursor_hands_over_the_shape_the_tree_asked_for() {
    let mut root = dragging_library_root();

    carry_a_track_over_the_deck(&mut root);

    assert_eq!(
        root.take_cursor(),
        Some(CursorIcon::Grabbing),
        "a runner must be handed the cursor the carrying tree asked for"
    );
}

/// Taking the cursor leaves every other platform signal where it stood.
///
/// The queue carries redraws and focus alongside the cursor, and a runner that
/// reads the cursor every event must not swallow them on the way past.
#[kithara::test]
fn taking_the_cursor_leaves_the_other_platform_signals_queued() {
    let mut whole = dragging_library_root();
    let mut split = dragging_library_root();
    for root in [&mut whole, &mut split] {
        carry_a_track_over_the_deck(root);
    }
    let rest = |signals: Vec<RenderRootSignal>| -> Vec<String> {
        signals
            .iter()
            .filter(|signal| !matches!(signal, RenderRootSignal::SetCursor(_)))
            .map(|signal| format!("{signal:?}"))
            .collect()
    };
    let expected = rest(whole.take_platform_signals());
    assert!(
        !expected.is_empty(),
        "the fixture must queue signals besides the cursor for this to say anything"
    );

    split.take_cursor();

    assert_eq!(
        rest(split.take_platform_signals()),
        expected,
        "taking the cursor must leave every other signal in order"
    );
}

/// A runner that reads the cursor twice is told nothing the second time.
#[kithara::test]
fn taking_the_cursor_twice_reports_no_second_change() {
    let mut root = dragging_library_root();
    carry_a_track_over_the_deck(&mut root);
    root.take_cursor();

    assert_eq!(
        root.take_cursor(),
        None,
        "a cursor already handed over must not be handed over again"
    );
}

/// The release reaches the list, which is the only side that knows what is held.
///
/// Nothing captures the pointer for a drag, so this is the event a host that
/// hit-tests loses: by the time the hand opens it is over the deck, and the
/// list would never hear it.
#[kithara::test]
fn a_track_released_away_from_its_list_still_reports_the_drop() {
    let published = drag_a_track_onto_the_deck(&mut dragging_library_root());

    assert!(
        published_drag(&published, "library", DragPhase::Drop),
        "a track released away from its list must still report the drop, published {published:?}"
    );
}

/// A module drawn with the full shell: a chip and a title in its header, and
/// the chevron that folds it away.
fn chrome_ui(title: &str, registry: &dyn EndpointRegistry) -> CompiledUi {
    let mut resolver = MemResolver::default();
    resolver.insert(
        "fixture.klayout.ron",
        r#"(schema: "kithara.layout", version: 1, id: "fixture",
            root: Module(instance: "demo", source: "fixture.kmodule.ron", size: (w: Fill, h: Fill)))"#,
    );
    let module = [
        r#"(schema: "kithara.module", version: 1, id: "shell", chrome: Full, chip: Some("A"), title: Some(""#,
        title,
        r#""), root: Spacer(id: "body", size: Some((w: Fill, h: Fill))))"#,
    ]
    .concat();
    resolver.insert("fixture.kmodule.ron", &module);
    compile(
        "fixture.klayout.ron",
        &resolver,
        registry,
        builtin::skin_doc(),
        builtin::text_doc(),
        &UiConfig::default(),
        &view::EMPTY,
    )
    .unwrap_or_else(|error| panic!("the chrome fixture must compile: {error}"))
}

/// Where the retained host put each cell of a full module's header, and where
/// the header itself stands.
fn header_cells(title: &str) -> (Point, MasonrySize, Vec<(Point, MasonrySize)>) {
    let registry = fixture_registry();
    let reads = FixtureReads;
    let ui = chrome_ui(title, &registry);
    let output = document::render(
        &ui.root,
        ctx(&ui, &reads),
        MasonryHost::new(ctx(&ui, &reads), builtin::skin()),
    );
    let mut root = masonry_root(output, 300, 200);
    root.redraw()
        .unwrap_or_else(|error| panic!("the module shell must compose: {error}"));
    let root = root.root();
    let mut queue = VecDeque::from([root.get_layer_root(0)]);
    let height = f64::from(builtin::skin().chrome.header_height);
    while let Some(widget) = queue.pop_front() {
        let children = widget.children();
        if widget.ctx().size().height == height && children.len() > 1 {
            return (
                widget.ctx().window_origin(),
                widget.ctx().size(),
                children
                    .into_iter()
                    .map(|cell| (cell.ctx().window_origin(), cell.ctx().size()))
                    .collect(),
            );
        }
        queue.extend(children);
    }
    panic!("a full module must draw a header holding more than one cell");
}

#[kithara::test]
fn a_full_header_holds_a_cell_for_everything_it_names() {
    let (_, _, cells) = header_cells("DECK");

    assert_eq!(
        cells.len(),
        5,
        "a header naming a chip and a title holds the chip, the title, the line after it, the \
         space that pushes the rest right, and the chevron cell: {cells:?}"
    );
}

#[kithara::test]
fn a_header_cell_grows_with_the_word_it_carries() {
    let (_, _, short) = header_cells("D");
    let (_, _, long) = header_cells("DECK MICRO");

    assert!(
        long[1].1.width > short[1].1.width,
        "a title cell must be measured from its own word: {} px for `D` and {} px for `DECK MICRO`",
        short[1].1.width,
        long[1].1.width
    );
}

#[kithara::test]
fn the_chevron_cell_stands_at_the_end_of_the_header() {
    let (origin, size, cells) = header_cells("DECK");
    let (cell_origin, cell_size) = cells[cells.len() - 1];

    assert_eq!(cell_origin.x + cell_size.width, origin.x + size.width);
}

/// A module folds away from the retained host too.
///
/// The other host answers a press anywhere on the header, through the
/// activation target it registers for it. Without the same answer here the
/// chevron is a mark that looks like a button and does nothing, and a module
/// mounted in this host can never be folded.
#[kithara::test]
fn a_press_on_a_module_header_folds_the_module_away() {
    let registry = fixture_registry();
    let reads = FixtureReads;
    let ui = chrome_ui("DECK", &registry);
    let output = document::render(
        &ui.root,
        ctx(&ui, &reads),
        MasonryHost::new(ctx(&ui, &reads), builtin::skin()),
    );
    let mut root = masonry_root(output, 300, 200);
    root.redraw()
        .unwrap_or_else(|error| panic!("the module shell must compose: {error}"));
    let (origin, size, _) = header_cells("DECK");

    root.handle_pointer_event(pointer_down(
        origin.x + size.width / 2.0,
        origin.y + size.height / 2.0,
    ))
    .unwrap_or_else(|error| panic!("the press must route: {error}"));

    assert_eq!(
        root.take_actions(),
        [UiEvent::ToggleModule("shell".to_owned())]
    );
}

#[kithara::test]
fn the_chevron_cell_takes_the_width_the_skin_gives_it() {
    let (_, _, cells) = header_cells("DECK");
    let (_, cell_size) = cells[cells.len() - 1];

    assert_eq!(
        cell_size.width,
        f64::from(builtin::skin().chrome.chevron_size)
    );
}

/// Where the application says the carried placement now stands.
struct ScenePoint {
    at: Cell<Option<Pt>>,
}

impl Reads for ScenePoint {
    fn get(&self, endpoint: &str) -> Option<ReadValue<'_>> {
        if endpoint == "scene.at" {
            return self.at.get().map(ReadValue::Point);
        }
        None
    }
}

/// A dock, a sprite that snaps onto it, and a marker standing at the stage's
/// own origin so a test measures a placement against the scene rather than
/// against whatever the window put around it.
const SCENE: &str = r#"Stage(id: "stage", size: (w: Fill, h: Fill), children: [
    Spacer(id: "mark", size: Some((w: Fixed(10.0), h: Fixed(10.0)))),
    Placed(id: "dock", at: (100.0, 24.0),
        child: Spacer(id: "post", size: Some((w: Fixed(40.0), h: Fixed(20.0))))),
    Placed(id: "carry", at: (40.0, 24.0),
        read: Model(id: "scene.at"),
        write: Parameter(id: "scene.at"),
        magnet: (to: ["dock"], within: 64.0),
        child: Spacer(id: "sprite", size: Some((w: Fixed(40.0), h: Fixed(20.0))))),
])"#;

fn scene_root(reads: &ScenePoint) -> (CompiledUi, MasonryState, MasonryRoot<UiEvent>) {
    let mut registry = fixture_registry();
    registry.insert(
        EndpointCategory::Model,
        "scene.at",
        EndpointDesc::new(ValueKind::Point),
    );
    registry.insert(
        EndpointCategory::Parameter,
        "scene.at",
        EndpointDesc::new(ValueKind::Point),
    );
    let ui = fixture_ui("scene-fixture", SCENE, &registry);
    let state = MasonryState::default();
    let output = document::render(
        &ui.root,
        ctx(&ui, reads),
        MasonryHost::new(ctx(&ui, reads), builtin::skin()).with_state(state.clone()),
    );
    let mut root = masonry_root(output, 300, 200);
    root.redraw()
        .unwrap_or_else(|error| panic!("the scene fixture must compose: {error}"));
    (ui, state, root)
}

fn window_origin(root: &MasonryRoot<UiEvent>, state: &MasonryState, path: &str) -> Point {
    let id = state
        .widget_id(path)
        .unwrap_or_else(|| panic!("`{path}` must stay addressable"));
    root.root()
        .get_widget(id)
        .unwrap_or_else(|| panic!("`{path}` must stay mounted"))
        .ctx()
        .window_origin()
}

/// Where a placement stands in its own stage, which is what the document names
/// and what the window around it must not change.
fn stands_at(root: &MasonryRoot<UiEvent>, state: &MasonryState, path: &str) -> Point {
    let origin = window_origin(root, state, path);
    let stage = window_origin(root, state, "demo/mark");
    Point::new(origin.x - stage.x, origin.y - stage.y)
}

/// Presses the middle of a placement's child and pulls it that far, leaving it
/// there.
fn carry(root: &mut MasonryRoot<UiEvent>, from: Point, by: (f64, f64)) -> Vec<UiEvent> {
    let to = Point::new(from.x + by.0, from.y + by.1);
    root.handle_pointer_event(pointer_move(from.x, from.y))
        .unwrap_or_else(|error| panic!("the hover must route: {error}"));
    root.handle_pointer_event(pointer_down(from.x, from.y))
        .unwrap_or_else(|error| panic!("the press must route: {error}"));
    root.handle_pointer_event(pointer_move(to.x, to.y))
        .unwrap_or_else(|error| panic!("the move must route: {error}"));
    root.handle_pointer_event(pointer_up(to.x, to.y))
        .unwrap_or_else(|error| panic!("the release must route: {error}"));
    root.take_actions()
}

/// The middle of the box a placement's child was laid out into.
fn middle(root: &MasonryRoot<UiEvent>, state: &MasonryState, path: &str) -> Point {
    let origin = window_origin(root, state, path);
    Point::new(origin.x + 20.0, origin.y + 10.0)
}

/// Where the last publication left the carried placement.
fn published_point(published: &[UiEvent]) -> Option<Pt> {
    published.iter().rev().find_map(|event| match event {
        UiEvent::Control {
            action: ControlAction::Place(at),
            path,
        } if path == "demo/carry" => Some(*at),
        _ => None,
    })
}

#[kithara::test]
fn a_placement_stands_at_the_point_the_document_wrote() {
    let reads = ScenePoint {
        at: Cell::new(None),
    };
    let (_, state, root) = scene_root(&reads);

    assert_eq!(
        stands_at(&root, &state, "demo/sprite"),
        Point::new(40.0, 24.0)
    );
}

/// What the retained host has to do that a rebuild used to: the point is the
/// application's, so a placement that reads one stands where the endpoint now
/// answers rather than where it was mounted.
#[kithara::test]
fn a_point_that_moves_moves_the_placement_on_a_refresh() {
    let reads = ScenePoint {
        at: Cell::new(None),
    };
    let (ui, state, mut root) = scene_root(&reads);

    reads.at.set(Some(Pt { x: 120.0, y: 60.0 }));
    root.refresh(ctx(&ui, &reads));
    root.redraw()
        .unwrap_or_else(|error| panic!("the moved scene must compose: {error}"));

    assert_eq!(
        stands_at(&root, &state, "demo/sprite"),
        Point::new(120.0, 60.0)
    );
}

/// A placement nothing carries holds still under the same drag, which is what
/// makes the drag below a measurement rather than a harness that moves whatever
/// it touches.
#[kithara::test]
fn a_placement_the_document_gave_nowhere_to_write_publishes_nothing() {
    let reads = ScenePoint {
        at: Cell::new(None),
    };
    let (_, state, mut root) = scene_root(&reads);
    let from = middle(&root, &state, "demo/post");

    let published = carry(&mut root, from, (0.0, 60.0));

    assert_eq!(published_point(&published), None);
}

/// The pointer carries the placement, and where it left it is published for the
/// application to answer with next frame.
#[kithara::test]
fn carrying_a_placement_publishes_where_the_drag_left_it() {
    let reads = ScenePoint {
        at: Cell::new(None),
    };
    let (_, state, mut root) = scene_root(&reads);
    let from = middle(&root, &state, "demo/sprite");

    let published = carry(&mut root, from, (-30.0, 60.0));

    assert_eq!(published_point(&published), Some(Pt { x: 10.0, y: 84.0 }));
}

/// A drag that ends in reach of a target the magnet names is published at that
/// target, not where the pointer left it.
#[kithara::test]
fn a_magnet_takes_a_drag_that_ends_in_reach() {
    let reads = ScenePoint {
        at: Cell::new(None),
    };
    let (_, state, mut root) = scene_root(&reads);
    let from = middle(&root, &state, "demo/sprite");

    let published = carry(&mut root, from, (50.0, 0.0));

    assert_eq!(published_point(&published), Some(Pt { x: 100.0, y: 24.0 }));
}
