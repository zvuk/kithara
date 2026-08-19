#![cfg(all(feature = "perf", feature = "iced", feature = "masonry"))]

use std::{borrow::Cow, cell::Cell, collections::BTreeMap, mem, rc::Rc, slice, sync::LazyLock};

use futures_lite::future::block_on;
use hotpath::HotpathGuardBuilder;
use iced::{
    Event, Pixels, Point, Size, Theme,
    advanced::{
        clipboard,
        graphics::{Shell, text::font_system},
        layout::{Layout, Limits},
        mouse::Cursor,
        renderer::Style,
        widget::Tree,
    },
    event::Status,
    mouse::{Button, Event as MouseEvent},
    theme::{Base as _, Palette},
    window::RedrawRequest,
};
use iced_renderer::fallback::Renderer as FallbackRenderer;
use iced_runtime::{
    UserInterface,
    user_interface::{Cache, State},
};
use iced_tiny_skia::Renderer as TinySkiaRenderer;
use iced_wgpu::{
    Engine, Renderer as WgpuRenderer,
    wgpu::{
        Backends, DeviceDescriptor, Instance, InstanceDescriptor, RequestAdapterOptions,
        TextureFormat,
    },
};
use kithara_platform::time::Duration;
use kithara_test_utils::kithara;
use kithara_ui::{
    app::{App, Config, Ui},
    builtin,
    compile::{CompiledUi, compile},
    draw::{Pt, Rect},
    expand::ControlSpec,
    ids::{EndpointId, SourceUri},
    interact::{Input, MOUSE, PointerInput, PointerPhase},
    registry::{EndpointCategory, EndpointDesc, EndpointRegistry, ValueKind},
    render::{
        Clock, PortalMapView, PortalTarget, ReadValue, Reads, ScalarRange, Skin, StereoLevels,
        TableCell, TableRow, TreeIcon, TreeRow, UiEvent, WaveBucket, WaveformView,
        fonts::{FONT_BYTES, SANS},
        tree,
    },
    shaping::FontPolicy,
    source::{MemResolver, UiConfig},
};

const LAYOUT: &str = "fixture.klayout.ron";
const MODULE: &str = "fixture.kmodule.ron";
const WIDTH: u16 = 240;
const HEIGHT: u16 = 120;

const LAYOUT_RON: &str = r#"(schema: "kithara.layout", version: 1, id: "fixture",
    root: Module(instance: "demo", source: "fixture.kmodule.ron", size: (w: Fill, h: Fill)))"#;

/// The one source the census table names beside a control. Only the shader row
/// asks for it.
const SOURCES: &[(&str, &str)] = &[(
    "census.wgsl",
    r"
@fragment
fn fs_main(@builtin(position) position: vec4<f32>) -> @location(0) vec4<f32> {
    return vec4<f32>(kithara.level.x, position.x / kithara.viewport.x, 0.0, 1.0);
}
",
)];

/// What a frame is asked to answer for. A drag also presses, so the classes are
/// tested widest-first wherever one has to be chosen.
#[derive(Clone, Copy)]
enum Interaction {
    Drag,
    Press,
    /// A press on the leading part of the box rather than the middle of it, for
    /// a control whose box is mostly a reading and whose one gesture sits at
    /// one end. Pressing the middle of such a control is a press on the label,
    /// which correctly answers nothing on either host.
    PressLeading,
    DataChange,
}

/// One neutral step both hosts translate into their own event vocabulary.
#[derive(Clone, Copy)]
enum Step {
    Move(Pt),
    Press,
    Release,
    Data,
}

/// Where a gesture acts, as fractions of the control's own laid-out rect. A
/// point derived this way is inside the control whatever size its layout gave
/// it; a fixed coordinate is inside only the controls big enough to reach it,
/// and a control it misses is measured redrawing while claiming to be dragged.
const START: (f32, f32) = (0.25, 0.25);
const MIDDLE: (f32, f32) = (0.5, 0.5);
const END: (f32, f32) = (0.75, 0.75);
/// Far enough in to clear the strip's own padding and its icon, and well short
/// of where the reading beside the control starts. At this fixture's 240 that
/// is x=48, against a scope chip the skin puts at 30 and runs about 50 wide.
const LEADING: (f32, f32) = (0.2, 0.5);

impl Interaction {
    fn steps(self, rect: Rect) -> Vec<Step> {
        let at = |fraction| Step::Move(inside(rect, fraction));
        match self {
            Self::Drag => vec![at(START), Step::Press, at(MIDDLE), at(END), Step::Release],
            Self::Press => vec![at(MIDDLE), Step::Press, Step::Release],
            Self::PressLeading => vec![at(LEADING), Step::Press, Step::Release],
            Self::DataChange => vec![Step::Data],
        }
    }
}

/// One point of the rect, named by where in it the gesture wants to be.
fn inside(rect: Rect, (x, y): (f32, f32)) -> Pt {
    Pt {
        x: rect.x + rect.w * x,
        y: rect.y + rect.h * y,
    }
}

fn contains(rect: Rect, at: Pt) -> bool {
    at.x >= rect.x && at.x <= rect.x + rect.w && at.y >= rect.y && at.y <= rect.y + rect.h
}

/// One control, mounted alone, and the interaction its frame is driven by.
///
/// `name` is the document name of the control, and `control` is the same RON
/// the paint and gesture censuses mount it from. `interaction` follows the
/// gesture census: a control that carries a drag is measured dragging, one that
/// only presses is measured pressing, and the rest are measured redrawing after
/// their reading moved.
struct Scenario {
    name: &'static str,
    control: &'static str,
    interaction: Interaction,
}

const SCENARIOS: &[Scenario] = &[
    Scenario {
        name: "Brand",
        control: r#"Brand(id: "control")"#,
        interaction: Interaction::DataChange,
    },
    Scenario {
        name: "Spacer",
        control: r#"Spacer(id: "control")"#,
        interaction: Interaction::DataChange,
    },
    Scenario {
        name: "Divider",
        control: r#"Divider(id: "control")"#,
        interaction: Interaction::DataChange,
    },
    Scenario {
        name: "PresetSelector",
        control: r#"PresetSelector(id: "control")"#,
        interaction: Interaction::Press,
    },
    Scenario {
        name: "SettingsButton",
        control: r#"SettingsButton(id: "control")"#,
        interaction: Interaction::Press,
    },
    Scenario {
        name: "DeckSummary",
        control: r#"DeckSummary(id: "control")"#,
        interaction: Interaction::DataChange,
    },
    Scenario {
        name: "WindowDrag",
        control: r#"WindowDrag(id: "control")"#,
        interaction: Interaction::Drag,
    },
    Scenario {
        name: "TitleBar",
        control: r#"TitleBar(id: "control", label: "KITHARA")"#,
        interaction: Interaction::DataChange,
    },
    Scenario {
        name: "WindowControls",
        control: r#"WindowControls(id: "control")"#,
        interaction: Interaction::Press,
    },
    Scenario {
        name: "Bpm",
        control: r#"Bpm(id: "control", placeholder: Some("time"))"#,
        interaction: Interaction::DataChange,
    },
    Scenario {
        name: "Time",
        control: r#"Time(id: "control", read: Model(id: "deck.view.zoom"))"#,
        interaction: Interaction::DataChange,
    },
    Scenario {
        name: "Scalar",
        control: r#"Scalar(id: "control", read: Model(id: "deck.view.zoom"))"#,
        interaction: Interaction::DataChange,
    },
    Scenario {
        name: "Wave",
        control: r#"Wave(id: "control", read: Model(id: "mock.wave"))"#,
        interaction: Interaction::Press,
    },
    Scenario {
        name: "Vis",
        control: r#"Vis(id: "control", read: Model(id: "vis.preset"))"#,
        interaction: Interaction::DataChange,
    },
    Scenario {
        name: "Shader",
        control: r#"Shader(id: "control", source: "census.wgsl", uniforms: { "level": Model(id: "deck.view.zoom") })"#,
        interaction: Interaction::DataChange,
    },
    Scenario {
        name: "Table",
        control: r#"Table(id: "control", read: Model(id: "library.visible_tracks"), columns: [(id: "title", label: "TITLE", style: Primary, width: 180.0)])"#,
        interaction: Interaction::Drag,
    },
    Scenario {
        name: "Tree",
        control: r#"Tree(id: "control", read: Model(id: "library.tree"), query: Model(id: "library.query"))"#,
        interaction: Interaction::Drag,
    },
    Scenario {
        name: "ContextBar",
        control: r#"ContextBar(id: "control", read: Model(id: "library.breadcrumb"), scope_items: ["ALL", "MINE"], scope: Model(id: "library.scope"), write: Model(id: "library.scope"))"#,
        interaction: Interaction::PressLeading,
    },
    Scenario {
        name: "Text",
        control: r#"Text(id: "control", label: Some("HELLO"))"#,
        interaction: Interaction::DataChange,
    },
    Scenario {
        name: "Knob",
        control: r#"Knob(id: "control", read: Parameter(id: "player.output.volume"), write: Parameter(id: "player.output.volume"))"#,
        interaction: Interaction::Drag,
    },
    Scenario {
        name: "Chip",
        control: r#"Chip(id: "control", label: "A", read: Model(id: "ui.menu.open"))"#,
        interaction: Interaction::Press,
    },
    Scenario {
        name: "NavItem",
        control: r#"NavItem(id: "control", label: "LIBRARY", icon: Playlist, read: Model(id: "ui.menu.open"))"#,
        interaction: Interaction::Press,
    },
    Scenario {
        name: "Button",
        control: r#"Button(id: "control", label: "PLAY", read: Model(id: "ui.menu.open"))"#,
        interaction: Interaction::Press,
    },
    Scenario {
        name: "Glyph",
        control: r#"Glyph(id: "control", icon: Playlist)"#,
        interaction: Interaction::DataChange,
    },
    Scenario {
        name: "TabLarge",
        control: r#"TabLarge(id: "control", label: "MIXER", read: Model(id: "ui.menu.open"))"#,
        interaction: Interaction::Press,
    },
    Scenario {
        name: "Toggle",
        control: r#"Toggle(id: "control", read: Model(id: "ui.menu.open"))"#,
        interaction: Interaction::Press,
    },
    Scenario {
        name: "Checkbox",
        control: r#"Checkbox(id: "control", read: Model(id: "ui.menu.open"))"#,
        interaction: Interaction::Press,
    },
    Scenario {
        name: "Segmented",
        control: r#"Segmented(id: "control", items: ["A", "B"], read: Model(id: "library.scope"))"#,
        interaction: Interaction::Press,
    },
    Scenario {
        name: "Select",
        control: r#"Select(id: "control", label: "QUALITY")"#,
        interaction: Interaction::DataChange,
    },
    Scenario {
        name: "StatusDot",
        control: r#"StatusDot(id: "control", label: "LIVE")"#,
        interaction: Interaction::DataChange,
    },
    Scenario {
        name: "Swatch",
        control: r#"Swatch(id: "control", role: Accent, label: "ACCENT")"#,
        interaction: Interaction::DataChange,
    },
    Scenario {
        name: "Cell",
        control: r#"Cell(id: "control", label: Some("A1"))"#,
        interaction: Interaction::DataChange,
    },
    Scenario {
        name: "Readout",
        control: r#"Readout(id: "control", label: Some("BPM"), read: Model(id: "library.breadcrumb"))"#,
        interaction: Interaction::DataChange,
    },
    Scenario {
        name: "Meter",
        control: r#"Meter(id: "control", read: Model(id: "deck.view.zoom"))"#,
        interaction: Interaction::DataChange,
    },
    Scenario {
        name: "VuVertical",
        control: r#"VuVertical(id: "control", read: Telemetry(id: "player.output.levels"))"#,
        interaction: Interaction::Drag,
    },
    Scenario {
        name: "VuStereo",
        control: r#"VuStereo(id: "control", read: Telemetry(id: "player.output.levels"))"#,
        interaction: Interaction::Drag,
    },
    Scenario {
        name: "Fader",
        control: r#"Fader(id: "control", read: Parameter(id: "player.output.volume"), write: Parameter(id: "player.output.volume"))"#,
        interaction: Interaction::Drag,
    },
    Scenario {
        name: "Crossfader",
        control: r#"Crossfader(id: "control", read: Parameter(id: "player.output.volume"), write: Parameter(id: "player.output.volume"))"#,
        interaction: Interaction::Drag,
    },
    Scenario {
        name: "PortalMap",
        control: r#"PortalMap(id: "control", read: Model(id: "pivot.map"))"#,
        interaction: Interaction::DataChange,
    },
    Scenario {
        name: "Range",
        control: r#"Range(id: "control", read: Model(id: "pivot.range"), write: Parameter(id: "pivot.range"))"#,
        interaction: Interaction::Drag,
    },
    // A sheet is cut once and a frame of it is one picture draw; an artwork is
    // read once and every frame of it is emitted afresh. Both are driven by a
    // reading that moves, which is the frame their hosts actually pay for.
    Scenario {
        name: "Sprite",
        control: r#"Sprite(id: "control", sheet: "spinner", seconds: 1.6, read: Model(id: "deck.view.zoom"))"#,
        interaction: Interaction::DataChange,
    },
    Scenario {
        name: "Lottie",
        control: r#"Lottie(id: "control", artwork: "pulse", seconds: 1.6, read: Model(id: "deck.view.zoom"))"#,
        interaction: Interaction::DataChange,
    },
];

static PORTALS: [PortalTarget; 2] = [
    PortalTarget {
        bpm: 93.0,
        is_selected: true,
    },
    PortalTarget {
        bpm: 165.33,
        is_selected: false,
    },
];

static WAVE: [WaveBucket; 4] = [WaveBucket {
    high: 0.8,
    low: 0.2,
    mid: 0.5,
}; 4];

static TREE_ROWS: [TreeRow<'static>; 8] = [TreeRow {
    label: "Folder",
    count: Some(8),
    expanded: Some(true),
    icon: TreeIcon::Folder,
    muted: false,
    selected: false,
    depth: 0,
}; 8];

static TABLE_ROWS: LazyLock<Vec<TableRow<'static>>> = LazyLock::new(|| {
    vec![TableRow::new(vec![TableCell::text("title", "Midnight Circuit")], false); 8]
});

/// The readings every census control binds to, all moved by one call so a
/// data-change frame is a frame the reading really did move under.
struct CensusReads {
    flag: Cell<bool>,
    level: Cell<f32>,
    scalar: Cell<f64>,
    text: Cell<&'static str>,
}

impl Default for CensusReads {
    fn default() -> Self {
        Self {
            flag: Cell::new(true),
            level: Cell::new(0.4),
            scalar: Cell::new(0.25),
            text: Cell::new("All Tracks"),
        }
    }
}

impl CensusReads {
    fn bump(&self) {
        let scalar = self.scalar.get();
        self.scalar
            .set(if scalar > 0.9 { 0.1 } else { scalar + 0.05 });
        let level = self.level.get();
        self.level.set(if level > 0.9 { 0.1 } else { level + 0.05 });
        self.flag.set(!self.flag.get());
        self.text.set(if self.flag.get() {
            "All Tracks"
        } else {
            "Folder"
        });
    }
}

impl Reads for CensusReads {
    fn get(&self, endpoint: &str) -> Option<ReadValue<'_>> {
        let id = endpoint.split_once('@').map_or(endpoint, |(id, _scope)| id);
        match id {
            "deck.playback.tempo" | "deck.track.title" | "library.breadcrumb" | "library.query" => {
                Some(ReadValue::Text(self.text.get()))
            }
            "deck.playback.playing"
            | "deck.playback.looping"
            | "deck.playback.synced"
            | "ui.menu.open" => Some(ReadValue::Bool(self.flag.get())),
            "deck.playback.reverse" => Some(ReadValue::Bool(!self.flag.get())),
            "deck.playback.position_normalized"
            | "deck.view.zoom"
            | "library.scope"
            | "player.output.volume"
            | "vis.preset" => Some(ReadValue::Scalar(self.scalar.get())),
            "library.tree" => Some(ReadValue::Tree(&TREE_ROWS)),
            "library.visible_tracks" => Some(ReadValue::Table(&TABLE_ROWS[..])),
            "deck.playback.waveform" | "mock.wave" => Some(ReadValue::Waveform(WaveformView {
                beats: &[],
                buckets: &WAVE,
                revision: 0,
                cues: &[],
                downbeats: &[],
                bpm: None,
                r#loop: None,
            })),
            "player.output.levels" => Some(ReadValue::Stereo(StereoLevels {
                l: self.level.get(),
                r: 1.0 - self.level.get(),
                volume: self.level.get(),
            })),
            "pivot.map" => Some(ReadValue::PortalMap(PortalMapView {
                master: 88.0 + self.level.get() * 88.0,
                min: 88.0,
                max: 176.0,
                targets: &PORTALS,
            })),
            "pivot.range" => Some(ReadValue::Range(ScalarRange {
                min: self.level.get() * 0.4,
                max: 0.6 + self.level.get() * 0.4,
            })),
            _ => None,
        }
    }
}

#[derive(Default)]
struct CensusRegistry {
    endpoints: BTreeMap<(EndpointCategory, EndpointId), EndpointDesc>,
}

impl CensusRegistry {
    fn insert(&mut self, category: EndpointCategory, id: &str, description: EndpointDesc) {
        self.endpoints
            .insert((category, EndpointId(id.to_owned())), description);
    }
}

impl EndpointRegistry for CensusRegistry {
    fn endpoint(&self, category: EndpointCategory, id: &EndpointId) -> Option<&EndpointDesc> {
        self.endpoints.get(&(category, id.clone()))
    }
}

fn census_registry() -> CensusRegistry {
    let mut registry = CensusRegistry::default();
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
    models(&mut registry);
    registry
}

fn models(registry: &mut CensusRegistry) {
    for (id, kind) in [
        ("deck.view.zoom", ValueKind::Scalar),
        ("library.breadcrumb", ValueKind::Text),
        ("library.query", ValueKind::Text),
        ("library.scope", ValueKind::Scalar),
        ("library.tree", ValueKind::Tree),
        ("library.visible_tracks", ValueKind::Table),
        ("mock.wave", ValueKind::Waveform),
        ("pivot.map", ValueKind::PortalMap),
        ("ui.menu.open", ValueKind::Bool),
        ("vis.preset", ValueKind::Scalar),
    ] {
        registry.insert(EndpointCategory::Model, id, EndpointDesc::new(kind));
    }
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
}

/// Everything a host is handed that is not the host: the documents, the
/// endpoints they may bind to, and nothing that differs between the two.
struct Fixture {
    registry: CensusRegistry,
    resolver: MemResolver,
}

impl Fixture {
    fn new(control: &str) -> Self {
        let mut resolver = MemResolver::default();
        resolver.insert(LAYOUT, LAYOUT_RON);
        resolver.insert(
            MODULE,
            &format!(
                r#"(schema: "kithara.module", version: 1, id: "census", chrome: Plain,
                    root: Row(size: (w: Fill, h: Fill), gap: 0.0, pad: 0.0, children: [{control}]))"#
            ),
        );
        for (name, body) in SOURCES {
            resolver.insert(name, body);
        }
        Self {
            registry: census_registry(),
            resolver,
        }
    }

    fn config(&self) -> Config<'_> {
        Config::builder()
            .endpoints(&self.registry)
            .resolver(&self.resolver)
            .skin(skin())
            .skin_doc(builtin::skin_doc())
            .build()
    }

    fn compiled(&self) -> CompiledUi {
        compile(
            LAYOUT,
            &self.resolver,
            &self.registry,
            builtin::skin_doc(),
            builtin::text_doc(),
            &UiConfig::default(),
        )
        .unwrap_or_else(|error| panic!("the frame-perf fixture must compile: {error}"))
    }
}

/// One frame's worth of work on one host, from the input applied to the draw
/// output produced. Nothing here rasterises: the measurement stops at the
/// commands, so a GPU on the machine cannot change the number.
trait FrameHost {
    /// Applies one neutral step in this host's own event vocabulary.
    fn interact(&mut self, step: Step);

    /// Produces one frame and returns a size taken from its output, so the
    /// frame cannot be optimised away. The unit is the host's own: see
    /// [`Host::unit`].
    fn frame(&mut self) -> usize;

    /// Whether this host asked for the frame [`Self::frame`] has just produced.
    ///
    /// A host that can decline to draw is a host whose lag is a count of
    /// frames, not a cost per frame, and the two hosts differ here.
    fn scheduled(&self) -> bool;

    /// What the control did with the gesture, in whatever this host can see: a
    /// captured event, a published document event. Reported, never judged - a
    /// control may legitimately take a gesture and publish nothing.
    fn receipts(&self) -> usize;
}

/// The immediate host: every frame rebuilds the element tree from the compiled
/// document, lays it out, applies the queued events and draws.
struct Immediate {
    cache: Cache,
    captured: usize,
    cursor: Cursor,
    pending: Vec<(Event, Cursor)>,
    reads: Rc<CensusReads>,
    renderer: iced::Renderer,
    scheduled: bool,
    theme: Theme,
    ui: CompiledUi,
}

impl Immediate {
    /// The renderer is the one the application draws through. tiny-skia has no
    /// custom-shader primitive: under it the Vis and Shader passes never run
    /// and their frame times are times for work that did not happen.
    fn new(fixture: &Fixture, reads: Rc<CensusReads>, engine: &Engine) -> Self {
        Self {
            cache: Cache::default(),
            captured: 0,
            cursor: Cursor::Unavailable,
            pending: Vec::new(),
            reads,
            renderer: FallbackRenderer::Primary(WgpuRenderer::new(
                engine.clone(),
                SANS,
                Pixels(14.0),
            )),
            scheduled: false,
            theme: theme(skin()),
            ui: fixture.compiled(),
        }
    }
}

impl FrameHost for Immediate {
    fn interact(&mut self, step: Step) {
        match step {
            Step::Move(at) => {
                let position = Point::new(at.x, at.y);
                self.cursor = Cursor::Available(position);
                self.pending.push((
                    Event::Mouse(MouseEvent::CursorMoved { position }),
                    self.cursor,
                ));
            }
            Step::Press => self.pending.push((
                Event::Mouse(MouseEvent::ButtonPressed(Button::Left)),
                self.cursor,
            )),
            Step::Release => self.pending.push((
                Event::Mouse(MouseEvent::ButtonReleased(Button::Left)),
                self.cursor,
            )),
            Step::Data => self.reads.bump(),
        }
    }

    fn frame(&mut self) -> usize {
        let bounds = Size::new(f32::from(WIDTH), f32::from(HEIGHT));
        let element = tree::render(
            &self.ui.root,
            &self.ui,
            self.reads.as_ref(),
            skin(),
            Clock::default(),
        );
        let mut interface = UserInterface::build(
            element,
            bounds,
            mem::take(&mut self.cache),
            &mut self.renderer,
        );
        let mut messages: Vec<UiEvent> = Vec::new();
        self.scheduled = !self.pending.is_empty();
        for (event, at) in mem::take(&mut self.pending) {
            let (state, statuses) = interface.update(
                slice::from_ref(&event),
                at,
                &mut self.renderer,
                &mut clipboard::Null,
                &mut messages,
            );
            self.captured += statuses
                .iter()
                .filter(|status| matches!(status, Status::Captured))
                .count();
            self.scheduled |= redraw_asked(&state);
        }
        let base = self.theme.base();
        interface.draw(
            &mut self.renderer,
            &self.theme,
            &Style {
                text_color: base.text_color,
            },
            self.cursor,
        );
        self.cache = interface.into_cache();
        acquisitions(&self.ui)
    }

    /// The immediate host has no seam at which it can decline to draw: it
    /// rebuilds the element tree and draws every time it is asked. What it can
    /// report is what iced's runtime would have been told - an event arrived,
    /// or a widget asked for another frame.
    fn scheduled(&self) -> bool {
        self.scheduled
    }

    fn receipts(&self) -> usize {
        self.captured
    }
}

/// Whether a widget asked iced's runtime to come back with another frame.
fn redraw_asked(state: &State) -> bool {
    match state {
        State::Outdated
        | State::Updated {
            redraw_request: RedrawRequest::NextFrame | RedrawRequest::At(_),
            ..
        } => true,
        State::Updated {
            redraw_request: RedrawRequest::Wait,
            ..
        } => false,
    }
}

/// The rect the fixture's layout gives the one control in it.
///
/// This is the only laid-out rect an integration test can reach. The retained
/// host resolves a control's rect from its own mounted widget tree, but the
/// call that does it (`Ui::rect_of`) is `#[cfg(test)]` inside the crate, and
/// the index it reads is only filled there too, so both hosts are driven at
/// the geometry measured here and each is made to prove it was hit.
fn laid_out_rect(fixture: &Fixture, reads: &CensusReads, renderer: &iced::Renderer) -> Rect {
    let ui = fixture.compiled();
    let mut element = tree::render(&ui.root, &ui, reads, skin(), Clock::default());
    let mut tree = Tree::new(element.as_widget());
    let bounds = Size::new(f32::from(WIDTH), f32::from(HEIGHT));
    let node =
        element
            .as_widget_mut()
            .layout(&mut tree, renderer, &Limits::new(Size::ZERO, bounds));
    control_rect(&ui, Layout::new(&node))
}

/// The fixture is fixed: a plain module whose root is a Row that fills it and
/// holds the control alone. That shape is two wrappers - the row's own size,
/// then its padding - above the control, and every step asserts, so a change
/// in how a Row is wrapped fails here instead of silently naming another node.
fn control_rect(ui: &CompiledUi, root: Layout<'_>) -> Rect {
    assert!(
        !ui.resize_edges && ui.dragged.is_none(),
        "the frame-perf layout must not wrap its module in a window layer"
    );
    let mut at = root;
    for wrapper in ["the row's own size", "the row's padding", "the control"] {
        at = only_child(at, wrapper);
    }
    let bounds = at.bounds();
    Rect {
        x: bounds.x,
        y: bounds.y,
        w: bounds.width,
        h: bounds.height,
    }
}

fn only_child<'layout>(layout: Layout<'layout>, what: &str) -> Layout<'layout> {
    let mut children = layout.children();
    let Some(first) = children.next() else {
        panic!("{what} must be the one child of the node above it, which has none");
    };
    assert!(
        children.next().is_none(),
        "{what} must be the one child of the node above it, which has more"
    );
    first
}

/// The retained host: the tree stays mounted, input walks it, and a frame is a
/// refresh plus a paint pass into a Vello scene.
struct Retained<'fixture> {
    published: Rc<Cell<usize>>,
    reads: Rc<CensusReads>,
    scheduled: bool,
    ui: Ui<'fixture, CensusApp>,
}

impl<'fixture> Retained<'fixture> {
    fn new(fixture: &'fixture Fixture, reads: Rc<CensusReads>) -> Self {
        let published = Rc::new(Cell::new(0));
        let app = CensusApp {
            published: Rc::clone(&published),
            reads: Rc::clone(&reads),
        };
        let size = (u32::from(WIDTH), u32::from(HEIGHT));
        let ui = Ui::new(app, fixture.config(), size, 1.0)
            .unwrap_or_else(|error| panic!("the frame-perf fixture must mount: {error}"));
        Self {
            published,
            reads,
            scheduled: false,
            ui,
        }
    }
}

impl FrameHost for Retained<'_> {
    fn interact(&mut self, step: Step) {
        match step {
            Step::Move(at) => self.ui.input(packet(PointerPhase::Move, Some(at))),
            Step::Press => self.ui.input(packet(PointerPhase::Down, None)),
            Step::Release => self.ui.input(packet(PointerPhase::Up, None)),
            Step::Data => self.reads.bump(),
        }
    }

    /// The seam the window runner draws through: settle, ask whether the
    /// picture would change, paint. The frame is painted either way here so
    /// there is always a cost to report, and whether it was wanted is
    /// [`Self::scheduled`].
    fn frame(&mut self) -> usize {
        self.ui.frame(Duration::from_millis(16));
        self.scheduled = self.ui.needs_frame();
        let frame = self
            .ui
            .render()
            .unwrap_or_else(|error| panic!("the retained host must draw: {error}"));
        let encoding = frame.scene().encoding();
        let size = encoding.path_data.len() + encoding.draw_data.len();
        self.ui.complete_frame();
        size
    }

    fn scheduled(&self) -> bool {
        self.scheduled
    }

    fn receipts(&self) -> usize {
        self.published.get()
    }
}

/// The application the retained host drives. It shows one document and answers
/// the same readings the immediate host is handed directly.
struct CensusApp {
    published: Rc<Cell<usize>>,
    reads: Rc<CensusReads>,
}

impl App for CensusApp {
    fn document(&self) -> &str {
        LAYOUT
    }

    fn reads<R>(&self, with: impl FnOnce(&dyn Reads) -> R) -> R {
        with(self.reads.as_ref())
    }

    fn update(&mut self, _event: UiEvent) {
        self.published.set(self.published.get() + 1);
    }
}

#[derive(Clone, Copy)]
enum Host {
    Immediate,
    Retained,
}

impl Host {
    /// What one unit of this host's frame output is. The two are different
    /// quantities of different things: comparing them across hosts says
    /// nothing, and this exists so a report cannot forget that.
    const fn unit(self) -> &'static str {
        match self {
            Self::Immediate => "draw-pool acquisitions",
            Self::Retained => "scene-encoding bytes",
        }
    }

    /// Everything the host needs before it can be built. The immediate host
    /// draws through wgpu, which needs a device; the retained host encodes a
    /// scene and needs nothing.
    fn backend(self) -> Result<Backend, String> {
        match self {
            Self::Immediate => engine().map(Box::new).map(Backend::Immediate),
            Self::Retained => Ok(Backend::Retained),
        }
    }
}

/// The device is boxed because the other variant carries nothing: a wgpu
/// `Engine` is a few hundred bytes, and every `Backend` the retained cases
/// build would otherwise be sized for a device it never opens.
enum Backend {
    Immediate(Box<Engine>),
    Retained,
}

impl Backend {
    /// The renderer the fixture's geometry is measured through. Laying out is
    /// text metrics, which both iced renderers take from the one shared font
    /// system; nothing is drawn through this one. The immediate host measures
    /// through its own, and the retained host, which has no iced renderer at
    /// all, measures through the software one.
    fn geometry(&self) -> iced::Renderer {
        match self {
            Self::Immediate(engine) => FallbackRenderer::Primary(WgpuRenderer::new(
                Engine::clone(engine),
                SANS,
                Pixels(14.0),
            )),
            Self::Retained => {
                FallbackRenderer::Secondary(TinySkiaRenderer::new(SANS, Pixels(14.0)))
            }
        }
    }
}

fn driver<'fixture>(
    backend: &Backend,
    fixture: &'fixture Fixture,
    reads: Rc<CensusReads>,
) -> Box<dyn FrameHost + 'fixture> {
    match backend {
        Backend::Immediate(engine) => Box::new(Immediate::new(fixture, reads, engine)),
        Backend::Retained => Box::new(Retained::new(fixture, reads)),
    }
}

/// A wgpu device with no window, shared by every control on the immediate
/// host. Building one per control would measure device creation forty times.
fn engine() -> Result<Engine, String> {
    let instance = Instance::new(&InstanceDescriptor {
        backends: Backends::PRIMARY,
        ..InstanceDescriptor::default()
    });
    let adapter = block_on(instance.request_adapter(&RequestAdapterOptions::default()))
        .map_err(|error| format!("no wgpu adapter: {error}"))?;
    let (device, queue) = block_on(adapter.request_device(&DeviceDescriptor::default()))
        .map_err(|error| format!("no wgpu device: {error}"))?;
    Ok(Engine::new(
        &adapter,
        device,
        queue,
        TextureFormat::Rgba8UnormSrgb,
        None,
        Shell::headless(),
    ))
}

fn packet(phase: PointerPhase, at: Option<Pt>) -> Input<'static> {
    Input::Pointer(PointerInput::new(MOUSE, None, phase, at, 1))
}

/// Draw buffers this document has taken from its pool. The immediate host
/// rebuilds its list every frame, so this grows with every frame drawn.
fn acquisitions(ui: &CompiledUi) -> usize {
    let stats = ui.draw_pool_stats();
    let total = stats.alloc_misses + stats.home_hits + stats.steal_hits;
    usize::try_from(total).unwrap_or(usize::MAX)
}

fn skin() -> &'static Skin {
    static SKIN: LazyLock<Skin> = LazyLock::new(|| {
        Skin::resolve_with_font_policy(
            builtin::skin_doc().clone(),
            builtin::text_doc(),
            &SourceUri("fixture:ui-frame-perf".to_owned()),
            FontPolicy::Embedded,
        )
        .unwrap_or_else(|error| panic!("the frame-perf skin must resolve: {error}"))
    });
    &SKIN
}

fn theme(skin: &Skin) -> Theme {
    let palette = skin.palette;
    Theme::custom(
        "Kithara".to_owned(),
        Palette {
            background: palette.bg.into(),
            text: palette.text.into(),
            primary: palette.accent.into(),
            success: palette.success.into(),
            danger: palette.danger.into(),
            warning: palette.warning.into(),
        },
    )
}

fn load_fonts() {
    static LOADED: LazyLock<()> = LazyLock::new(|| {
        let mut fonts = font_system()
            .write()
            .expect("iced font system lock must not be poisoned");
        for bytes in FONT_BYTES {
            fonts.load_font(Cow::Borrowed(bytes));
        }
    });
    LazyLock::force(&LOADED);
}

fn missing_from_table() -> Vec<&'static str> {
    ControlSpec::KINDS
        .iter()
        .copied()
        .filter(|kind| !SCENARIOS.iter().any(|scenario| scenario.name == *kind))
        .collect()
}

fn unnamed_by_the_document() -> Vec<&'static str> {
    SCENARIOS
        .iter()
        .map(|scenario| scenario.name)
        .filter(|name| !ControlSpec::KINDS.contains(name))
        .collect()
}

#[kithara::test]
fn every_control_the_document_can_name_has_a_frame_scenario() {
    let missing = missing_from_table();
    assert!(
        missing.is_empty(),
        "no frame is measured for {missing:?}; a control with no scenario is not reported slow, it is not reported at all"
    );
}

#[kithara::test]
fn the_scenario_table_names_no_control_the_document_cannot() {
    let unnamed = unnamed_by_the_document();
    assert!(
        unnamed.is_empty(),
        "the frame-perf table names {unnamed:?}, which `ControlSpec` does not"
    );
}

/// What one control's frame cost, and everything the harness had to be sure of
/// before that cost means anything.
struct Reading {
    gestured: bool,
    name: &'static str,
    points: Vec<Pt>,
    receipts: usize,
    rect: Rect,
    scheduled: bool,
    size: usize,
}

/// Measures one frame per control on each host. It records durations and makes
/// no claim about them: the comparison this feeds is between the two hosts on
/// the same machine, and a threshold here would judge the machine.
#[kithara::test]
#[case("iced", Host::Immediate)]
#[case("vello", Host::Retained)]
fn ui_frame_perf(#[case] label: &'static str, #[case] host: Host) {
    load_fonts();
    let backend = match host.backend() {
        Ok(backend) => backend,
        Err(error) => {
            // Falling back to the software rasteriser here would report times
            // for the Vis and Shader passes without running them.
            let notice = format!("{label}: NOT MEASURED, no host to measure it on: {error}");
            println!("{notice}");
            eprintln!("{notice}");
            return;
        }
    };
    let readings = {
        let _guard = HotpathGuardBuilder::new(label).functions_limit(0).build();
        measure(&backend)
    };
    report(label, host, &readings);
    audit(label, &readings);
}

fn measure(backend: &Backend) -> Vec<Reading> {
    let geometry = backend.geometry();
    let mut readings = Vec::with_capacity(SCENARIOS.len());
    for scenario in SCENARIOS {
        let fixture = Fixture::new(scenario.control);
        let reads = Rc::new(CensusReads::default());
        let rect = laid_out_rect(&fixture, &reads, &geometry);
        let mut driver = driver(backend, &fixture, Rc::clone(&reads));
        // The first frame carries the mount, the layout and the shaping caches.
        // Measuring it would measure the setup instead of the frame.
        let _warm = driver.frame();
        let steps = scenario.interaction.steps(rect);
        let size = hotpath::measure_block!(scenario.name, {
            for step in &steps {
                driver.interact(*step);
            }
            driver.frame()
        });
        readings.push(Reading {
            gestured: matches!(
                scenario.interaction,
                Interaction::Drag | Interaction::Press | Interaction::PressLeading
            ),
            name: scenario.name,
            points: steps
                .iter()
                .filter_map(|step| match step {
                    Step::Move(at) => Some(*at),
                    Step::Press | Step::Release | Step::Data => None,
                })
                .collect(),
            receipts: driver.receipts(),
            rect,
            scheduled: driver.scheduled(),
            size,
        });
    }
    readings
}

/// Prints one row per control, then the two totals that are about the host
/// rather than about a control.
fn report(label: &str, host: Host, readings: &[Reading]) {
    for reading in readings {
        let rect = reading.rect;
        println!(
            "{label} {:<15} rect {:7.2} {:7.2} {:7.2} {:7.2}  points {}  scheduled {:<5}  receipts {:<3}  out {}",
            reading.name,
            rect.x,
            rect.y,
            rect.w,
            rect.h,
            reading.points.len(),
            reading.scheduled,
            reading.receipts,
            reading.size,
        );
    }
    let scheduled = readings.iter().filter(|reading| reading.scheduled).count();
    let silent: Vec<&str> = readings
        .iter()
        .filter(|reading| reading.size == 0)
        .map(|reading| reading.name)
        .collect();
    println!(
        "{label}: {scheduled} of {} frames the host asked for, {} measured",
        readings.len(),
        readings.len()
    );
    println!(
        "{label}: {} {} over {} controls, a unit no other host shares",
        readings.iter().map(|reading| reading.size).sum::<usize>(),
        host.unit(),
        readings.len()
    );
    println!(
        "{label}: {} control(s) whose frame produced nothing: {silent:?}",
        silent.len()
    );
}

/// Fails naming every control the gesture did not land on. A pointer that
/// misses is measured as an idle redraw while the row still says "drag", and
/// nothing downstream can tell the two apart.
fn audit(label: &str, readings: &[Reading]) {
    let missed: Vec<String> = readings
        .iter()
        .filter_map(|reading| Some(format!("{}: {}", reading.name, miss(reading)?)))
        .collect();
    assert!(
        missed.is_empty(),
        "{label}: the gesture cannot reach {missed:#?}"
    );
}

fn miss(reading: &Reading) -> Option<String> {
    let rect = reading.rect;
    let viewport = Rect {
        x: 0.0,
        y: 0.0,
        w: f32::from(WIDTH),
        h: f32::from(HEIGHT),
    };
    if ![rect.x, rect.y, rect.w, rect.h]
        .iter()
        .all(|edge| edge.is_finite())
    {
        return Some(format!("its rect is not finite: {rect:?}"));
    }
    if rect.w <= 0.0 || rect.h <= 0.0 {
        return Some(format!("its rect has no area to act in: {rect:?}"));
    }
    if let Some(at) = reading.points.iter().find(|at| !contains(rect, **at)) {
        return Some(format!("{at:?} falls outside its own rect {rect:?}"));
    }
    if let Some(at) = reading.points.iter().find(|at| !contains(viewport, **at)) {
        return Some(format!("{at:?} falls outside the {WIDTH}x{HEIGHT} fixture"));
    }
    // Geometry says the point is on the control. This says the host agrees: a
    // gesture that neither published anything nor asked for another frame
    // reached nothing, and its row is an idle redraw wearing a drag's name.
    (reading.gestured && reading.receipts == 0 && !reading.scheduled)
        .then(|| format!("the host saw nothing at {:?} in {rect:?}", reading.points))
}
