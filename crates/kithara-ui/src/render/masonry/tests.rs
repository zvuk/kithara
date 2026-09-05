use std::{
    cell::{Cell, RefCell},
    collections::{BTreeMap, VecDeque},
    rc::Rc,
    sync::{Arc, LazyLock},
};

use kithara_platform::time::Duration;
use kithara_test_utils::kithara;
use masonry::{
    app::{RenderRoot, RenderRootOptions, RenderRootSignal, WindowSizePolicy},
    core::{CursorIcon, Handled, Ime, PointerEvent, TextEvent, WindowEvent},
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
    TextMeasurer, built::RootParts, leaf::DragProgram, node::Node,
};
use crate::{
    atoms::bar::context::Context,
    builtin,
    compile::{CompiledUi, compile},
    draw::{DrawListBuilder, Pt, Rect, Rgba},
    geom::Transform,
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
    },
    shaping::{FontPolicy, TextContext},
    skin::parse_skin_over,
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

    include!("../../../tests/common/mod.rs");
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
            let output = document::render(
                &ui.root,
                ctx(&ui, &reads),
                MasonryHost::new(ctx(&ui, &reads), &skin),
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
    let output = document::render(
        &ui.root,
        ctx(&ui, &reads),
        MasonryHost::new(ctx(&ui, &reads), builtin::skin()),
    );
    let wide = *output
        .document_ids()
        .last()
        .unwrap_or_else(|| panic!("the fixture must retain the revealed cell"));
    let mut root = masonry_root(output, width, 60);
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
    let output = document::render(
        &ui.root,
        ctx(&ui, &reads),
        MasonryHost::new(ctx(&ui, &reads), builtin::skin()),
    );
    let wide = *output
        .document_ids()
        .last()
        .unwrap_or_else(|| panic!("the fixture must retain the revealed cell"));
    let mut root = masonry_root(output, 120, 60);
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
        let output = document::render(&ui.root, ctx(&ui, &reads), host);
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
    let registry = fixture_registry();
    let reads = FixtureReads;
    let ui = fixture_ui("leaf-fixture", SCOPE_STRIP, &registry);
    let host = MasonryHost::map_actions(ctx(&ui, &reads), builtin::skin(), TestAction::Document);
    let output = document::render(&ui.root, ctx(&ui, &reads), host);
    let control_id = *output
        .document_ids()
        .last()
        .unwrap_or_else(|| panic!("the scope strip must have a real control node"));
    let mut root = masonry_root(output, 200, 120);
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

/// Whether this host has a painter for a control, or still mounts it as a
/// correctly-sized empty box.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Paints {
    Yes,
    /// A native pass draws this control after the Vello scene.
    Native,
    /// There is no picture to draw. A window-drag region is a place the hand
    /// grabs the window by, and the immediate host draws nothing for it either,
    /// so an empty scene here is the control working rather than a gap. That
    /// claim is checked on both hosts rather than taken on trust, by
    /// `the_retained_window_drag_region_carries_the_drag_and_draws_nothing` here
    /// and `a_drag_surface_carries_the_window_and_draws_nothing` in
    /// `render::window::surface`.
    Nothing,
}

/// Every control the shared base draws, and whether Masonry draws it today.
///
/// Native output is counted separately from Vello output, so an intentionally
/// empty Vello scene cannot make a working second-pass control look undrawn.
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
        Paints::Yes,
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
    (
        "Wave",
        Paints::Yes,
        r#"Wave(id: "control", read: Model(id: "demo.wave"))"#,
    ),
    (
        "Vis",
        Paints::Native,
        r#"Vis(id: "control", read: Model(id: "vis.preset"))"#,
    ),
    (
        "Sprite",
        Paints::Yes,
        r#"Sprite(id: "control", sheet: "spinner", seconds: 1.6, read: Model(id: "ui.clock.seconds"))"#,
    ),
    (
        "Lottie",
        Paints::Yes,
        r#"Lottie(id: "control", artwork: "pulse", seconds: 1.6, read: Model(id: "ui.clock.seconds"))"#,
    ),
    (
        // Unlike `Vis`, a shader is not a second pass beside the scene: the
        // retained host encodes the image draw into the Vello scene itself, and
        // the GPU pass fills the very image that draw points at.
        "Shader",
        Paints::Yes,
        r#"Shader(id: "control", source: "census.wgsl", uniforms: { "level": Model(id: "deck.view.zoom") })"#,
    ),
    (
        // What it draws is the application's, so this says only that the host
        // reached the registered widget and replayed what it drew.
        "Custom",
        Paints::Yes,
        r#"Custom(id: "control", kind: "census-extension")"#,
    ),
    (
        "Table",
        Paints::Yes,
        r#"Table(id: "control", read: Model(id: "library.visible_tracks"), columns: [(id: "title", label: "TITLE", style: Primary, width: 180.0)])"#,
    ),
    (
        "Tree",
        Paints::Yes,
        r#"Tree(id: "control", read: Model(id: "library.tree"), query: Model(id: "library.query"))"#,
    ),
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
    (
        "PortalMap",
        Paints::Yes,
        r#"PortalMap(id: "control", read: Model(id: "pivot.map"))"#,
    ),
    (
        "Range",
        Paints::Yes,
        r#"Range(id: "control", read: Model(id: "pivot.range"), write: Parameter(id: "pivot.range"))"#,
    ),
];

/// The kind the census document names, registered below so the `Custom` row
/// draws the extension it stands for rather than the empty box a host falls to
/// when the registry it was handed does not hold the name.
const CENSUS_KIND: &str = "census-extension";

/// Opaque, so an empty Vello scene cannot be mistaken for ink nothing can see.
const CENSUS_INK: Rgba = Rgba {
    a: 1.0,
    b: 1.0,
    g: 1.0,
    r: 1.0,
};

/// A registered extension that paints, so the `Custom` row answers for the
/// mount path rather than for a widget that chose to draw nothing.
struct CensusExtension;

impl CustomWidget for CensusExtension {
    type Action = ();

    fn measure(&mut self, _text: &mut TextMeasurer<'_>, _limits: SizeLimits) -> Size2 {
        Size2::new(40.0, 40.0)
    }

    fn paint(
        &mut self,
        list: &mut DrawListBuilder,
        _text: &mut TextMeasurer<'_>,
        bounds: Rect,
        skin: &CustomSkin,
    ) {
        list.fill_rect(bounds, skin.color("ink").unwrap_or(CENSUS_INK));
    }
}

fn census_kinds() -> CustomKinds {
    CustomKinds::default().with(CENSUS_KIND, || CensusExtension, |()| UiEvent::OpenSettings)
}

/// The kind the input fixture names. Registered apart from the census one so a
/// row of the paint census cannot start answering pointers to keep a test
/// green.
const PRESS_KIND: &str = "press-extension";

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
        list.fill_rect(bounds, CENSUS_INK);
    }
}

fn press_kinds() -> CustomKinds {
    CustomKinds::default().with(PRESS_KIND, || PressExtension, |()| UiEvent::OpenSettings)
}

/// The sources the census table names beside the controls themselves. Only the
/// shader row needs one; an entry nobody asks for costs the other rows nothing.
const CENSUS_SOURCES: &[(&str, &str)] = &[(
    "census.wgsl",
    r"
@fragment
fn fs_main(@builtin(position) position: vec4<f32>) -> @location(0) vec4<f32> {
    return vec4<f32>(kithara.level.x, position.x / kithara.viewport.x, 0.0, 1.0);
}
",
)];

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
        builtin::text_doc(),
        &SourceUri("fixture:masonry-control-census".to_owned()),
        &builtin::resolver(),
        FontPolicy::Embedded,
    )
    .unwrap_or_else(|error| panic!("the census skin must resolve: {error}"));
    let kinds = census_kinds();
    let observed = CONTROL_CENSUS
        .iter()
        .map(|(name, _, control)| {
            let ui = fixture_ui_with_sources(
                "census",
                &format!(
                    r#"Row(size: (w: Fill, h: Fill), gap: 0.0, pad: 0.0, children: [{control}])"#
                ),
                &registry,
                CENSUS_SOURCES,
            );
            let frame = ctx(&ui, &reads).with_kinds(&kinds);
            let output = document::render(&ui.root, frame, MasonryHost::new(frame, &skin));
            let mut root = masonry_root(output, 240, 120);
            let (scene, _) = root.redraw().unwrap_or_else(|error| {
                panic!("`{name}` must reach a Masonry paint pass: {error}")
            });
            let encoding = scene.encoding();
            let paints = if !(encoding.is_empty() && encoding.resources.glyphs.is_empty()) {
                Paints::Yes
            } else if !root.vis_declarations().is_empty() {
                Paints::Native
            } else {
                Paints::Nothing
            };
            (*name, paints)
        })
        .collect::<Vec<_>>();
    let expected = CONTROL_CENSUS
        .iter()
        .map(|(name, paints, _)| (*name, *paints))
        .collect::<Vec<_>>();

    assert_eq!(
        observed, expected,
        "the census is stale — move a row when its painter lands, and never leave the census \
         describing a host it no longer matches"
    );
}

/// A skin dressing the census extension in one named colour, so what an
/// extension is drawn in can be changed without changing the extension.
fn dressed(ink: &str) -> Skin {
    let origin = SourceUri("fixture:masonry-dressed-extension".to_owned());
    let text = format!(
        r#"(schema: "kithara.skin", version: 1, id: "dressed",
            custom: {{ "{CENSUS_KIND}": {{ "ink": Color("{ink}") }} }})"#
    );
    let document = parse_skin_over(builtin::skin_doc(), &text, &origin)
        .unwrap_or_else(|error| panic!("the dressing patch must parse: {error}"));
    Skin::resolve_with_font_policy(
        document,
        builtin::text_doc(),
        &origin,
        &builtin::resolver(),
        FontPolicy::Embedded,
    )
    .unwrap_or_else(|error| panic!("the dressed skin must resolve: {error}"))
}

/// What this host draws for a mounted extension under one skin.
fn extension_paint(skin: &Skin) -> Vec<u32> {
    let registry = fixture_registry();
    let reads = FixtureReads;
    let kinds = census_kinds();
    let ui = fixture_ui(
        "dressed",
        &format!(
            r#"Row(size: (w: Fill, h: Fill), gap: 0.0, pad: 0.0, children: [
                Custom(id: "drawn", kind: "{CENSUS_KIND}")])"#
        ),
        &registry,
    );
    let frame = ctx(&ui, &reads).with_kinds(&kinds);
    let output = document::render(&ui.root, frame, MasonryHost::new(frame, skin));
    let mut root = masonry_root(output, 240, 120);
    let (scene, _) = root
        .redraw()
        .unwrap_or_else(|error| panic!("the dressed extension must be drawn: {error}"));
    scene.encoding().draw_data.clone()
}

/// The dressing is taken from the skin the leaf was mounted under, so two
/// skins draw one extension two ways without the extension knowing either.
#[kithara::test]
fn a_mounted_extension_is_drawn_in_what_the_skin_dresses_its_kind_in() {
    assert_ne!(
        extension_paint(&dressed("#ff0000")),
        extension_paint(&dressed("#0000ff")),
        "the two skins dress this kind in two colours, so an extension painting the same under \
         both is reading neither"
    );
}

/// The mounted input contract, observed from both leaf adapters and the engine
/// plan they share. Keeping this beside the paint census makes a new
/// `ControlSpec` incomplete until it names both its picture and its gestures.
///
/// It enumerates kinds of control, so what a group declares over itself is
/// outside it by construction: a stepping surface has no row here and cannot
/// get one. Those are pinned as a gesture played to both hosts, in
/// `render::parity::hand`.
mod gesture_census {
    use std::rc::Rc;

    use kithara_test_utils::kithara;
    use num_traits::cast::AsPrimitive;

    use super::{
        super::controls::Retained, CENSUS_SOURCES, CONTROL_CENSUS, FixtureReads, FixtureRegistry,
        Handled, LATE_TABLE_ROWS, MasonryHost, MasonryRoot, MasonryState, PointerEvent, Pt,
        ScrollDelta, fixture_registry, fixture_ui_with_sources, masonry_root, pointer_down,
        pointer_move, pointer_scroll, pointer_up,
    };
    use crate::{
        app::App,
        builtin,
        compile::{CompiledNode, CompiledUi},
        draw::Rect,
        expand::{Binding, ControlSpec, ExpandedNode},
        ids::{InternId, SourceUri},
        interact::Gestures,
        mount,
        registry::{EndpointCategory, EndpointDesc, EndpointRegistry, ValueKind},
        render::{
            Clock, ReadValue, Reads, Skin, UiEvent,
            controls::{Draws, Gesture, Paint, Reading},
            document::{self, Ctx},
            hosted::hosted_control_plan,
            masonry::{HostAction, Painted},
            parity::{
                immediate::Immediate,
                shared::{renderer, snapped},
            },
            tree,
        },
        shaping::FontPolicy,
        view,
    };

    #[derive(Clone, Copy)]
    struct Row {
        name: &'static str,
        gestures: Gestures,
    }

    const ROWS: &[Row] = &[
        Row {
            name: "Brand",
            gestures: Gestures::empty(),
        },
        Row {
            name: "Spacer",
            gestures: Gestures::empty(),
        },
        Row {
            name: "Divider",
            gestures: Gestures::empty(),
        },
        Row {
            name: "PresetSelector",
            gestures: Gestures::PRESS,
        },
        Row {
            name: "SettingsButton",
            gestures: Gestures::PRESS,
        },
        Row {
            name: "DeckSummary",
            gestures: Gestures::empty(),
        },
        Row {
            name: "WindowDrag",
            gestures: Gestures::DRAG,
        },
        Row {
            name: "TitleBar",
            gestures: Gestures::empty(),
        },
        Row {
            name: "WindowControls",
            gestures: Gestures::PRESS,
        },
        Row {
            name: "Bpm",
            gestures: Gestures::empty(),
        },
        Row {
            name: "Time",
            gestures: Gestures::empty(),
        },
        Row {
            name: "Scalar",
            gestures: Gestures::empty(),
        },
        Row {
            name: "Wave",
            gestures: Gestures::PRESS,
        },
        Row {
            name: "Vis",
            gestures: Gestures::empty(),
        },
        Row {
            name: "Sprite",
            gestures: Gestures::empty(),
        },
        Row {
            name: "Lottie",
            gestures: Gestures::empty(),
        },
        Row {
            name: "Shader",
            gestures: Gestures::empty(),
        },
        Row {
            // The document binds a custom control to nothing, so the toolkit
            // recognises nothing over it: whatever it answers, it answers for
            // itself, through the registry it was mounted from.
            name: "Custom",
            gestures: Gestures::empty(),
        },
        Row {
            name: "Table",
            gestures: Gestures::DRAG.union(Gestures::WHEEL),
        },
        Row {
            name: "Tree",
            gestures: Gestures::DRAG
                .union(Gestures::KEYBOARD)
                .union(Gestures::WHEEL),
        },
        Row {
            name: "ContextBar",
            gestures: Gestures::PRESS.union(Gestures::KEYBOARD),
        },
        Row {
            name: "Text",
            gestures: Gestures::empty(),
        },
        Row {
            name: "Knob",
            gestures: Gestures::DRAG
                .union(Gestures::DOUBLE_CLICK)
                .union(Gestures::WHEEL),
        },
        Row {
            name: "Chip",
            gestures: Gestures::PRESS,
        },
        Row {
            name: "NavItem",
            gestures: Gestures::PRESS,
        },
        Row {
            name: "Button",
            gestures: Gestures::PRESS,
        },
        Row {
            name: "Glyph",
            gestures: Gestures::empty(),
        },
        Row {
            name: "TabLarge",
            gestures: Gestures::PRESS,
        },
        Row {
            name: "Toggle",
            gestures: Gestures::PRESS,
        },
        Row {
            name: "Checkbox",
            gestures: Gestures::PRESS,
        },
        Row {
            name: "Segmented",
            gestures: Gestures::PRESS,
        },
        Row {
            name: "Select",
            gestures: Gestures::empty(),
        },
        Row {
            name: "StatusDot",
            gestures: Gestures::empty(),
        },
        Row {
            name: "Swatch",
            gestures: Gestures::empty(),
        },
        Row {
            name: "Cell",
            gestures: Gestures::empty(),
        },
        Row {
            name: "Readout",
            gestures: Gestures::empty(),
        },
        Row {
            name: "Meter",
            gestures: Gestures::empty(),
        },
        Row {
            name: "VuVertical",
            gestures: Gestures::DRAG,
        },
        Row {
            name: "VuStereo",
            gestures: Gestures::DRAG,
        },
        Row {
            name: "Fader",
            gestures: Gestures::DRAG,
        },
        Row {
            name: "Crossfader",
            gestures: Gestures::DRAG,
        },
        Row {
            name: "PortalMap",
            gestures: Gestures::empty(),
        },
        Row {
            name: "Range",
            gestures: Gestures::DRAG,
        },
    ];

    #[derive(Clone, Copy, Default)]
    struct Observed {
        immediate: Gestures,
        retained: Gestures,
        special: Gestures,
    }

    struct Probe<'a> {
        skin: &'a Skin,
        reading: Reading<'a>,
    }

    trait ProbeControl {
        fn observe(&self, probe: Probe<'_>) -> Observed;
    }

    impl<Control> ProbeControl for Control
    where
        Control: Draws,
        Control::Painter: Retained + 'static,
    {
        fn observe(&self, probe: Probe<'_>) -> Observed {
            let immediate = self.data(probe.reading).map_or(Gestures::empty(), |data| {
                let grip = self.grip(probe.skin, &data);
                Gesture::with_grip(
                    "control",
                    Paint::new(self.painter(probe.skin), data, probe.skin),
                    grip,
                    self.index_event(),
                )
                .map_or_else(|_| Gestures::empty(), |gesture| gesture.gestures())
            });
            let retained = self.data(probe.reading).map_or(Gestures::empty(), |data| {
                let grip = self.grip(probe.skin, &data);
                Painted::new(self.painter(probe.skin), data, probe.skin)
                    .interactive(
                        grip,
                        "control".to_owned(),
                        Rc::new(HostAction::new),
                        self.index_event(),
                    )
                    .gestures()
            });
            Observed {
                immediate,
                retained,
                special: Gestures::empty(),
            }
        }
    }

    macro_rules! passive {
        ($($control:ty),+ $(,)?) => {
            $(impl ProbeControl for $control {
                fn observe(&self, _probe: Probe<'_>) -> Observed {
                    Observed::default()
                }
            })+
        };
    }

    passive!(
        mount::TitleBar,
        mount::Text<'_>,
        mount::Custom,
        mount::Shader<'_>,
        mount::Vis,
        mount::Table<'_>,
        mount::Tree<'_>,
    );

    impl ProbeControl for mount::Drag {
        fn observe(&self, _probe: Probe<'_>) -> Observed {
            Observed {
                special: Gestures::DRAG,
                ..Observed::default()
            }
        }
    }

    impl ProbeControl for mount::Controls {
        fn observe(&self, _probe: Probe<'_>) -> Observed {
            Observed {
                special: Gestures::PRESS,
                ..Observed::default()
            }
        }
    }

    struct Apply<'a> {
        probe: Probe<'a>,
    }

    impl Apply<'_> {
        fn apply<Control: ProbeControl>(self, control: &Control) -> Observed {
            control.observe(self.probe)
        }
    }

    fn mounted(
        path: InternId,
        spec: &ControlSpec,
        read: Option<&Binding>,
        ctx: Ctx<'_, '_>,
        skin: &Skin,
    ) -> (Gestures, Gestures) {
        let value = read.and_then(|binding| ctx.read(binding));
        let leaf = mount::controls!(
            spec,
            Apply {
                probe: Probe {
                    reading: Reading {
                        ctx,
                        scope: ctx.scope(read),
                        skin,
                        value: value.as_ref(),
                    },
                    skin,
                },
            }
        );
        let engine = hosted_control_plan(path, spec, read, ctx, skin)
            .map_or(Gestures::empty(), |plan| plan.gestures());
        (
            leaf.immediate.union(engine).union(leaf.special),
            leaf.retained.union(engine).union(leaf.special),
        )
    }

    fn find_control(node: &ExpandedNode) -> Option<(InternId, &ControlSpec, Option<&Binding>)> {
        match node {
            ExpandedNode::Control {
                path, spec, read, ..
            } => Some((*path, spec, read.as_ref())),
            ExpandedNode::Object { child, .. }
            | ExpandedNode::Optional { child, .. }
            | ExpandedNode::Placed { child, .. }
            | ExpandedNode::Pressable { child, .. }
            | ExpandedNode::Reveal { child, .. }
            | ExpandedNode::Scroll { child, .. } => find_control(child),
            ExpandedNode::Adaptive { base, steps, .. } => find_control(base)
                .or_else(|| steps.iter().find_map(|(_, branch)| find_control(branch))),
            ExpandedNode::Row { children, .. }
            | ExpandedNode::Column { children, .. }
            | ExpandedNode::Slot { children, .. }
            | ExpandedNode::Stage { children, .. } => children.iter().find_map(find_control),
            ExpandedNode::Popover {
                anchor, content, ..
            } => find_control(anchor).or_else(|| find_control(content)),
        }
    }

    fn compiled_control(ui: &CompiledUi) -> (InternId, &ControlSpec, Option<&Binding>) {
        let root = match &ui.root {
            CompiledNode::Module { root, .. } => root,
            CompiledNode::Optional { child, .. } => {
                let CompiledNode::Module { root, .. } = child.as_ref() else {
                    panic!("the census fixture must compile to one module");
                };
                root
            }
            CompiledNode::Adaptive { .. } | CompiledNode::Split { .. } => {
                panic!("the census fixture must contain one module")
            }
        };
        find_control(root).unwrap_or_else(|| panic!("the census fixture must contain a control"))
    }

    /// A census short by a control agrees with the other census, which is short
    /// by the same one, and neither reports a gap. Only the document contract
    /// can say what the full set is.
    #[kithara::test]
    fn the_census_covers_every_control_the_document_can_name() {
        let mut censused = CONTROL_CENSUS
            .iter()
            .map(|(name, _, _)| *name)
            .collect::<Vec<_>>();
        let mut declared = ControlSpec::KINDS.to_vec();
        censused.sort_unstable();
        declared.sort_unstable();

        assert_eq!(
            censused, declared,
            "every `ControlSpec` variant needs a census row saying what it draws"
        );
    }

    #[kithara::test]
    fn every_control_names_the_same_mounted_gestures_in_both_hosts() {
        let painted = CONTROL_CENSUS
            .iter()
            .map(|(name, _, _)| *name)
            .collect::<Vec<_>>();
        let gestured = ROWS.iter().map(|row| row.name).collect::<Vec<_>>();
        assert_eq!(
            gestured, painted,
            "the paint and gesture censuses must cover the same controls in the same order"
        );

        let registry = census_registry();
        let reads = FixtureReads;
        let skin = census_skin();

        for (row, (_, _, control)) in ROWS.iter().zip(CONTROL_CENSUS) {
            let ui = fixture_ui_with_sources(
                "gesture-census",
                &format!(
                    r#"Row(size: (w: Fill, h: Fill), gap: 0.0, pad: 0.0, children: [{control}])"#
                ),
                &registry,
                CENSUS_SOURCES,
            );
            let (path, spec, read) = compiled_control(&ui);
            assert_eq!(
                spec.kind(),
                row.name,
                "the census row for {} mounts a different control than it names",
                row.name
            );
            let (immediate, retained) = mounted(path, spec, read, super::ctx(&ui, &reads), &skin);
            assert_eq!(
                immediate, row.gestures,
                "{} changed its iced gesture contract",
                row.name
            );
            assert_eq!(
                retained, row.gestures,
                "{} changed its Masonry gesture contract",
                row.name
            );
        }
    }

    fn census_registry() -> FixtureRegistry {
        let mut registry = fixture_registry();
        registry.insert(
            EndpointCategory::Model,
            "ui.menu.open",
            EndpointDesc::new(ValueKind::Bool),
        );
        registry
    }

    fn census_skin() -> Skin {
        Skin::resolve_with_font_policy(
            builtin::skin_doc().clone(),
            builtin::text_doc(),
            &SourceUri("fixture:gesture-census".to_owned()),
            &builtin::resolver(),
            FontPolicy::Embedded,
        )
        .unwrap_or_else(|error| panic!("the census skin must resolve: {error}"))
    }

    const DRIVEN_WIDTH: u32 = 240;
    const DRIVEN_HEIGHT: u32 = 120;

    /// The readings a driven control needs to have anything under the hand.
    ///
    /// A table with no rows takes no press for a reason that has nothing to do
    /// with the host, so the driven census binds the rows the rest of the
    /// fixture leaves out.
    struct DrivenReads;

    impl Reads for DrivenReads {
        fn get(&self, endpoint: &str) -> Option<ReadValue<'_>> {
            let id = endpoint.split_once('@').map_or(endpoint, |(id, _scope)| id);
            if id == "library.visible_tracks" {
                return Some(ReadValue::Table(&LATE_TABLE_ROWS));
            }
            FixtureReads.get(endpoint)
        }
    }

    /// The single promise one driven sequence measures.
    ///
    /// A control that names a drag also names a press, and the two are
    /// separate promises: measuring them together lets one cover for the
    /// other. Each is driven on its own root by the event that carries it.
    ///
    /// A drag takes two moves, not one: `ItemDrag` spends the first fixing the
    /// point the travel is measured from, so a single move is below every
    /// threshold by construction and would measure the sequence, not the host.
    #[derive(Clone, Copy, Debug)]
    enum Named {
        Press,
        Drag,
        Wheel,
    }

    impl Named {
        fn declared_by(self, gestures: Gestures) -> bool {
            match self {
                Self::Press => gestures.contains(Gestures::PRESS),
                Self::Drag => gestures.contains(Gestures::DRAG),
                Self::Wheel => gestures.contains(Gestures::WHEEL),
            }
        }
    }

    /// What the retained host did with the event carrying the promise.
    ///
    /// Neither observable is sound alone: a control can take an event and
    /// change only its own state, and the root answers `Handled::No` for a
    /// path that emitted nothing at all.
    #[derive(Clone, Copy, Debug, Default)]
    struct Answer {
        acted: bool,
        handled: bool,
    }

    impl Answer {
        fn or(self, other: Self) -> Self {
            Self {
                acted: self.acted || other.acted,
                handled: self.handled || other.handled,
            }
        }

        fn silent(self) -> bool {
            !self.acted && !self.handled
        }
    }

    fn take(root: &mut MasonryRoot<UiEvent>, event: PointerEvent) -> Answer {
        let handled = root
            .handle_pointer_event(event)
            .unwrap_or_else(|error| panic!("driven input must stay typed: {error}"));
        Answer {
            acted: !root.take_actions().is_empty(),
            handled: handled == Handled::Yes,
        }
    }

    /// The middle of the box the control was actually laid out into.
    ///
    /// Window chrome answers before the tree and has no document leaf to
    /// address; the middle of the root is on it, because it is the only thing
    /// mounted.
    fn aim(
        root: &MasonryRoot<UiEvent>,
        state: &MasonryState,
        across: f64,
        down: f64,
    ) -> (f64, f64) {
        state
            .widget_id("demo/control")
            .and_then(|id| root.root().get_widget(id))
            .map_or_else(
                || {
                    (
                        f64::from(DRIVEN_WIDTH) * across,
                        f64::from(DRIVEN_HEIGHT) * down,
                    )
                },
                |widget| {
                    let origin = widget.ctx().window_origin();
                    let size = widget.ctx().size();
                    (
                        origin.x + size.width * across,
                        origin.y + size.height * down,
                    )
                },
            )
    }

    /// The interior of a box, in fractions of its own width and height.
    ///
    /// A control is not uniformly live: a strip answers on its crumbs and not
    /// in the gap between them, and a table answers on a row. Aiming at one
    /// point measures where the aim landed, not what the control takes, so
    /// every point gets its own root and the control answers if any does.
    const AIMS: &[f64] = &[0.25, 0.5, 0.75];

    /// Whether the press alone keeps this promise.
    ///
    /// `HostLayer::handle` answers `Down` and nothing else, because
    /// `WindowCommand::Drag` gives the gesture to the window manager: no move
    /// ever comes back for the toolkit to answer. Both hosts share that layer,
    /// so this is the contract rather than a retained-host gap. It is named
    /// here instead of skipped, so the census fails the day a handover starts
    /// answering moves.
    fn handed_over(name: &str, named: Named) -> bool {
        name == "WindowDrag" && matches!(named, Named::Drag)
    }

    /// The document a census row is driven in: the control alone, filling the
    /// window.
    fn driven_document(control: &str, registry: &dyn EndpointRegistry) -> CompiledUi {
        fixture_ui_with_sources(
            "gesture-drive",
            &format!(r#"Row(size: (w: Fill, h: Fill), gap: 0.0, pad: 0.0, children: [{control}])"#),
            registry,
            CENSUS_SOURCES,
        )
    }

    /// The retained host with the document mounted and laid out, beside the
    /// state that says where each path was put.
    fn driven_root(ui: &CompiledUi, skin: &Skin) -> (MasonryRoot<UiEvent>, MasonryState) {
        let reads = DrivenReads;
        let state = MasonryState::default();
        let host = MasonryHost::new(super::ctx(ui, &reads), skin).with_state(state.clone());
        let output = document::render(&ui.root, super::ctx(ui, &reads), host);
        let mut root = masonry_root(output, DRIVEN_WIDTH, DRIVEN_HEIGHT);
        root.redraw()
            .unwrap_or_else(|error| panic!("the driven control must lay out: {error}"));
        (root, state)
    }

    fn driven(named: Named, control: &str, registry: &dyn EndpointRegistry, skin: &Skin) -> Answer {
        let ui = driven_document(control, registry);
        let mut answer = Answer::default();
        for across in AIMS {
            for down in AIMS {
                let (mut root, state) = driven_root(&ui, skin);
                let (x, y) = aim(&root, &state, *across, *down);
                answer = answer.or(at(named, &mut root, x, y));
            }
        }
        answer
    }

    fn at(named: Named, root: &mut MasonryRoot<UiEvent>, x: f64, y: f64) -> Answer {
        match named {
            Named::Press => {
                let down = take(root, pointer_down(x, y));
                let up = take(root, pointer_up(x, y));
                down.or(up)
            }
            Named::Drag => {
                let _down = take(root, pointer_down(x, y));
                let first = take(root, pointer_move(x + 2.0, y));
                first.or(take(root, pointer_move(x + 24.0, y)))
            }
            Named::Wheel => take(
                root,
                pointer_scroll(x, y, ScrollDelta::LineDelta(0.0, -2.0)),
            ),
        }
    }

    /// An application standing in for the one a driven row has none of.
    ///
    /// The immediate host keeps nothing between frames, so what a control did
    /// with a gesture shows only in what the document published and in whether
    /// the tree took the event. This keeps the first; the driver answers the
    /// second.
    struct Driven<'a> {
        published: Vec<UiEvent>,
        skin: &'a Skin,
    }

    impl Reads for Driven<'_> {
        fn get(&self, endpoint: &str) -> Option<ReadValue<'_>> {
            DrivenReads.get(endpoint)
        }
    }

    impl App for Driven<'_> {
        fn document(&self) -> &str {
            "fixture.klayout.ron"
        }

        fn reads<R>(&self, with: impl FnOnce(&dyn Reads) -> R) -> R {
            with(self)
        }

        fn skin(&self) -> &Skin {
            self.skin
        }

        fn update(&mut self, event: UiEvent) {
            self.published.push(event);
        }
    }

    /// How far apart the points the immediate census drives are.
    ///
    /// Four pixels is under the smallest box any control in the census was
    /// laid out into, so a control that answers anywhere is reached.
    const SWEEP: f32 = 4.0;

    /// Plays one gesture at one point and says what the document did with it.
    ///
    /// A drag is measured by its travel, not by the press that starts it: a
    /// control that answers the press and drops every move would otherwise
    /// pass as a control that drags. The retained twin discards the same press
    /// for the same reason.
    ///
    /// The travel is two moves rather than one because a recognizer may spend
    /// the first fixing what the rest are measured from, and a drag played as
    /// a single move is then below every threshold by construction.
    fn played(named: Named, host: &mut Immediate<'_, Driven<'_>>, at: Pt) -> Answer {
        match named {
            Named::Press => {
                let handled = host.click_at(at);
                Answer {
                    acted: !host.app().published.is_empty(),
                    handled,
                }
            }
            Named::Drag => {
                host.press_at(at);
                let started = host.app().published.len();
                let first = host.hover_at(Pt {
                    x: at.x + 2.0,
                    y: at.y,
                });
                let second = host.hover_at(Pt {
                    x: at.x + 24.0,
                    y: at.y,
                });
                Answer {
                    acted: host.app().published.len() > started,
                    handled: first || second,
                }
            }
            Named::Wheel => {
                let handled = host.wheel_at(at, -2.0);
                Answer {
                    acted: !host.app().published.is_empty(),
                    handled,
                }
            }
        }
    }

    /// Drives the same gesture over the whole window on the immediate host.
    ///
    /// The retained twin aims at the box it laid the control into, because it
    /// keeps a tree that can be asked. This host keeps none, and borrowing the
    /// other host's box would make a control that answers on both look silent
    /// here the moment the two lay it out differently - a question about
    /// geometry, answered as if it were one about gestures. So this sweeps the
    /// window instead, and the control answers if any point of it does.
    fn driven_immediate(
        named: Named,
        control: &str,
        registry: &dyn EndpointRegistry,
        skin: &Skin,
    ) -> Answer {
        let ui = driven_document(control, registry);
        let (width, height): (f32, f32) = (DRIVEN_WIDTH.as_(), DRIVEN_HEIGHT.as_());
        let mut y = SWEEP / 2.0;
        while y < height {
            let mut x = SWEEP / 2.0;
            while x < width {
                let app = Driven {
                    published: Vec::new(),
                    skin,
                };
                let mut host = Immediate::mount(app, &ui, skin, (DRIVEN_WIDTH, DRIVEN_HEIGHT));
                let answer = played(named, &mut host, Pt { x, y });
                if !answer.silent() {
                    return answer;
                }
                x += SWEEP;
            }
            y += SWEEP;
        }
        Answer::default()
    }

    /// Drives, on the retained host, the pointer gesture each control names.
    ///
    /// The census beside this one compares the two hosts' declarations. Saying
    /// a gesture is not answering it: a control declared a drag both hosts
    /// agreed on while the retained one dropped every move, and no test could
    /// see it, because the words matched. This mounts each control alone,
    /// finds the box it was laid out into, and pushes real input at it.
    #[kithara::test]
    fn every_control_answers_the_pointer_gesture_it_names_on_the_retained_host() {
        let registry = census_registry();
        let skin = census_skin();

        let mut observed = Vec::new();
        let mut expected = Vec::new();
        for (row, (_, _, control)) in ROWS.iter().zip(CONTROL_CENSUS) {
            for named in [Named::Press, Named::Drag, Named::Wheel] {
                if !named.declared_by(row.gestures) {
                    continue;
                }
                let answers = !driven(named, control, &registry, &skin).silent();
                observed.push(format!("{} {named:?}: {answers}", row.name));
                expected.push(format!(
                    "{} {named:?}: {}",
                    row.name,
                    !handed_over(row.name, named)
                ));
            }
        }

        assert_eq!(
            observed, expected,
            "the retained host answers a different set of pointer gestures than the controls name"
        );
    }

    /// Drives, on the immediate host, the pointer gesture each control names.
    ///
    /// The twin of the census above. The two hosts route a pointer through
    /// machinery with nothing in common - one against boxes read out of a tree
    /// it keeps, the other by letting iced walk a tree it rebuilt - and a
    /// control that answers on one and not the other draws exactly the same
    /// picture. Driving both against the one table each declared its gestures
    /// in is what makes that visible.
    #[kithara::test]
    fn every_control_answers_the_pointer_gesture_it_names_on_the_immediate_host() {
        let registry = census_registry();
        let skin = census_skin();

        let mut observed = Vec::new();
        let mut expected = Vec::new();
        for (row, (_, _, control)) in ROWS.iter().zip(CONTROL_CENSUS) {
            for named in [Named::Press, Named::Drag, Named::Wheel] {
                if !named.declared_by(row.gestures) {
                    continue;
                }
                let answers = !driven_immediate(named, control, &registry, &skin).silent();
                observed.push(format!("{} {named:?}: {answers}", row.name));
                expected.push(format!(
                    "{} {named:?}: {}",
                    row.name,
                    !handed_over(row.name, named)
                ));
            }
        }

        assert_eq!(
            observed, expected,
            "the immediate host answers a different set of pointer gestures than the controls name"
        );
    }

    /// The box the retained host laid a control into.
    fn retained_box(control: &str, registry: &dyn EndpointRegistry, skin: &Skin) -> Rect {
        let ui = driven_document(control, registry);
        let (root, state) = driven_root(&ui, skin);
        let widget = state
            .widget_id("demo/control")
            .and_then(|id| root.root().get_widget(id))
            .unwrap_or_else(|| panic!("the retained host must mount {control} as a leaf"));
        let origin = widget.ctx().window_origin();
        let size = widget.ctx().size();
        Rect {
            x: origin.x.as_(),
            y: origin.y.as_(),
            w: size.width.as_(),
            h: size.height.as_(),
        }
    }

    /// The box the immediate host laid the same control into.
    ///
    /// The hosts disagree on how many nodes a control is: the retained one
    /// mounts a single widget carrying the resolved size, and the immediate one
    /// wraps the control's own element in a container of that size. Descending
    /// through every node that stands alone reaches the surface both hosts
    /// paint, and stopping at the first node that splits keeps a control that
    /// lays out children - only `Tree` does - measured as the surface they are
    /// painted on.
    fn immediate_box(control: &str, registry: &dyn EndpointRegistry, skin: &Skin) -> Rect {
        use iced::{
            Size,
            advanced::{
                layout::{Layout, Limits},
                widget::Tree,
            },
        };

        let ui = driven_document(control, registry);
        let reads = DrivenReads;
        let mut element = tree::render(
            &ui.root,
            &ui,
            &reads,
            &view::EMPTY,
            skin,
            Clock::default(),
            None,
        );
        let mut state = Tree::new(element.as_widget());
        let node = element.as_widget_mut().layout(
            &mut state,
            &renderer(),
            &Limits::new(
                Size::ZERO,
                Size::new(DRIVEN_WIDTH.as_(), DRIVEN_HEIGHT.as_()),
            ),
        );
        let mut layout = Layout::new(&node);
        loop {
            let mut children = layout.children();
            let Some(only) = children.next() else { break };
            if children.next().is_some() {
                break;
            }
            layout = only;
        }
        let bounds = layout.bounds();
        Rect {
            x: bounds.x,
            y: bounds.y,
            w: bounds.width,
            h: bounds.height,
        }
    }

    /// A button given a box paints all of it, on both hosts.
    ///
    /// Every `Button` a shipped document names declares a size, and the census
    /// above cannot see that shape: it drives each control as the document
    /// leaves it. The retained host mounts one widget of the declared box; the
    /// immediate host wraps the button's own element in a container of that box
    /// and lets the element ask for a width of its own, so a button that asks
    /// for the width of its word is painted narrower than the box the document
    /// gave it.
    #[kithara::test]
    fn a_button_given_a_box_paints_all_of_it_on_both_hosts() {
        let registry = census_registry();
        let skin = census_skin();
        let control = r#"Button(id: "control", label: "PLAY", size: (w: Fixed(72.0), h: Fixed(28.0)), read: Model(id: "ui.menu.open"))"#;

        assert_eq!(
            snapped(immediate_box(control, &registry, &skin)),
            snapped(retained_box(control, &registry, &skin)),
            "the two hosts paint a button given the same box differently"
        );
    }

    /// Both hosts lay the same control into the same box.
    ///
    /// A declared size reaches the two hosts through separate tables -
    /// `length_for` on the immediate one, `control_length` on the retained one
    /// - and a control whose painter measures its own width is where the two
    /// can part: one gives the parent the painter's box and the other replaces
    /// it with the skin's. No shipped document names such a size, so only a
    /// census over every control keeps the two tables answering alike.
    #[kithara::test]
    fn every_control_is_laid_out_into_the_same_box_on_both_hosts() {
        let registry = census_registry();
        let skin = census_skin();

        let mut retained = Vec::new();
        let mut immediate = Vec::new();
        for (name, _, control) in CONTROL_CENSUS {
            retained.push(format!(
                "{name}: {:?}",
                snapped(retained_box(control, &registry, &skin))
            ));
            immediate.push(format!(
                "{name}: {:?}",
                snapped(immediate_box(control, &registry, &skin))
            ));
        }

        assert_eq!(
            retained, immediate,
            "the two hosts lay the same control into different boxes"
        );
    }
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
    let declarations = root.vis_declarations();
    assert_eq!(declarations.len(), 2);
    assert_eq!(
        declarations
            .iter()
            .map(|vis| vis.rect())
            .collect::<Vec<_>>(),
        vec![[0.0, 0.0, 40.0, 20.0], [40.0, 0.0, 80.0, 20.0]]
    );
    assert_eq!(declarations[0].frame().preset(), 0);
    assert_eq!(declarations[1].frame().preset(), 2);
    assert!((declarations[0].frame().level() - 0.6).abs() < f32::EPSILON);
    assert_eq!(declarations[0].frame().time(), 1.25);
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
    let refreshed = root.vis_declarations();
    assert_eq!(refreshed.len(), 2);
    assert_eq!(refreshed[0].frame().preset(), 1);
    assert!((refreshed[0].frame().level() - 0.25).abs() < f32::EPSILON);
    assert_eq!(refreshed[0].frame().time(), 9.0);

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

#[kithara::test]
fn stashed_continuous_vis_stops_and_unstashing_restarts_animation_frames() {
    let registry = fixture_registry();
    let reads = FixtureReads;
    let ui = fixture_ui(
        "vis-stashing-fixture",
        r#"Row(size: (w: Fill, h: Fill), gap: 0.0, pad: 0.0, children: [
            Vis(id: "continuous", read: Model(id: "vis.preset")),
        ])"#,
        &registry,
    );
    let output = document::render(
        &ui.root,
        ctx(&ui, &reads),
        MasonryHost::new(ctx(&ui, &reads), builtin::skin()),
    );
    let [_, parent, vis] = output.document_ids() else {
        panic!("the fixture must retain exactly the document root, one parent, and one Vis leaf")
    };
    let parent = *parent;
    let vis = *vis;
    let (base, _, _, _, _, _, _, _, _): RootParts = output.into();
    let signals = Rc::new(RefCell::new(Vec::new()));
    let sink = Rc::clone(&signals);
    let mut root = RenderRoot::new(
        base,
        move |signal| sink.borrow_mut().push(signal),
        render_root_options(80, 20),
    );

    assert!(
        take_animation_request(&signals),
        "WidgetAdded must start a continuous Vis"
    );
    root.edit_widget(parent, |mut widget| {
        let mut node = widget.downcast::<Node>();
        Node::set_child_stashed(&mut node, 0, true);
    });
    assert!(
        root.get_widget(vis)
            .is_some_and(|widget| widget.ctx().is_stashed()),
        "the test must exercise Masonry's real stashed state"
    );
    signals.borrow_mut().clear();

    root.handle_window_event(WindowEvent::AnimFrame(Duration::from_millis(16)));
    assert!(
        !take_animation_request(&signals),
        "the already-requested callback for a stashed continuous Vis must not request another frame"
    );

    root.edit_widget(parent, |mut widget| {
        let mut node = widget.downcast::<Node>();
        Node::set_child_stashed(&mut node, 0, false);
    });
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
    let output = document::render(
        &ui.root,
        ctx(&ui, &reads),
        MasonryHost::new(ctx(&ui, &reads), builtin::skin()),
    );
    let id = *output
        .document_ids()
        .last()
        .unwrap_or_else(|| panic!("PresetSelector must retain one leaf"));
    let mut root = masonry_root(output, 126, 42);
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

fn placed_at(root: &MasonryRoot<UiEvent>, state: &MasonryState, path: &str) -> Transform {
    let id = state
        .widget_id(path)
        .unwrap_or_else(|| panic!("`{path}` must stay addressable"));
    root.root()
        .get_widget(id)
        .unwrap_or_else(|| panic!("`{path}` must stay mounted"))
        .downcast::<Node>()
        .unwrap_or_else(|| panic!("`{path}` must be a document node"))
        .transform()
}

/// A module id the facade hands to an engine, so the control inside it is
/// mounted as `InputOwner::Engine` and the engine stands on the module rather
/// than on the control. Every other fixture here names an id of its own, which
/// is the shape where the two are the same node.
const HOSTED_MODULE: &str = "gallery-table-tab";

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
    let start = placed_at(&root, &state, "demo/carried");

    reads.along.set(1.0);
    root.refresh(ctx(&ui, &reads));

    assert_ne!(placed_at(&root, &state, "demo/carried"), start);
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
    let start = placed_at(&root, &state, "demo/carried");

    reads.along.set(1.0);
    root.refresh(ctx(&ui, &reads));

    assert_eq!(placed_at(&root, &state, "demo/carried"), start);
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
    let (before, _) = root
        .redraw()
        .unwrap_or_else(|error| panic!("empty Tree must draw its frame: {error}"));
    let before_draw_data = before.encoding().draw_data.clone();
    assert_eq!(
        root.tree_picture("demo/browser"),
        Some((0, String::new())),
        "the mounted Tree must retain the initially empty rows and query"
    );

    reads.rows_loaded.set(true);
    root.refresh(ctx(&ui, &reads));
    assert_eq!(
        root.tree_picture("demo/browser"),
        Some((TREE_ROWS.len(), String::new())),
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
        root.tree_picture("demo/browser"),
        Some((TREE_ROWS.len(), "Late".to_owned())),
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
    fixture_ui_with_options(module_id, root, registry, false, &[])
}

fn fixture_ui_with_resize(
    module_id: &str,
    root: &str,
    registry: &dyn EndpointRegistry,
) -> CompiledUi {
    fixture_ui_with_options(module_id, root, registry, true, &[])
}

/// A fixture whose document names sources of its own. A shader is a file the
/// resolver has to answer rather than a node written inline, so a census row
/// that mounts one needs its module beside the layout.
fn fixture_ui_with_sources(
    module_id: &str,
    root: &str,
    registry: &dyn EndpointRegistry,
    sources: &[(&str, &str)],
) -> CompiledUi {
    fixture_ui_with_options(module_id, root, registry, false, sources)
}

fn fixture_ui_with_options(
    module_id: &str,
    root: &str,
    registry: &dyn EndpointRegistry,
    resize_edges: bool,
    sources: &[(&str, &str)],
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
    for (name, body) in sources {
        resolver.insert(name, body);
    }
    compile(
        "fixture.klayout.ron",
        &resolver,
        registry,
        builtin::skin_doc(),
        builtin::text_doc(),
        &UiConfig::builder()
            .custom_kinds(
                [CENSUS_KIND.to_owned(), PRESS_KIND.to_owned()]
                    .into_iter()
                    .collect(),
            )
            .build(),
        &view::EMPTY,
    )
    .unwrap_or_else(|error| panic!("Masonry contract fixture must compile: {error}"))
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
