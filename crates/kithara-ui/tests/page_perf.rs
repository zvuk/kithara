#![cfg(all(feature = "perf", feature = "iced", feature = "masonry"))]

//! Whole-page frame measurement on both hosts.
//!
//! The per-widget harness beside this one measures one control alone in a small
//! box. Three of the four reported symptoms are not about one control: the
//! stress page, scrolling, and the visualiser are page-level, and two of them
//! are about work that only happens once a page is rasterised. So this harness
//! mounts the gallery's own pages, drives them with the input the symptom
//! names, and rasterises every frame through the passes the application uses:
//! `ShaderPass` and `VisPass` around the Vello scene on the retained host, and
//! iced's own shader primitive prepare and draw, which only run at present, on
//! the immediate one.
//!
//! Nothing here judges. Every number is printed; the assertions are only that a
//! run did the thing it claims to measure, because a device-less, wheel-less or
//! motionless run otherwise reports a beautiful number for nothing.
//!
//! The wall clock is `kithara_platform::time::WallInstant`, the one clock the
//! platform crate leaves real on both lanes. `Instant` beside it is the virtual
//! one under flash, and a virtual clock reports a frame that took no time.

#[path = "../examples/gallery/fixture.rs"]
mod fixture;
#[path = "../examples/gallery/mock/mod.rs"]
mod mock;
#[path = "../examples/gallery/sections.rs"]
mod sections;

use std::{
    borrow::Cow,
    collections::hash_map::DefaultHasher,
    hash::{Hash as _, Hasher as _},
    mem, slice,
    sync::LazyLock,
};

use futures_lite::future::block_on;
use hotpath::{HotpathGuardBuilder, measure_block};
use iced::{
    Color, Event, Pixels, Point, Size, Theme,
    advanced::{
        clipboard,
        graphics::{Shell, Viewport, text::font_system},
        mouse::Cursor,
        renderer::Style,
    },
    mouse::{Button as MouseButton, Event as MouseEvent, ScrollDelta},
    theme::{Base as _, Palette},
    window::RedrawRequest,
};
use iced_renderer::fallback::Renderer as FallbackRenderer;
use iced_runtime::{
    UserInterface,
    user_interface::{Cache, State},
};
use iced_wgpu::{
    Engine, Renderer as WgpuRenderer,
    wgpu::{
        Backends, BufferDescriptor, BufferUsages, CommandEncoderDescriptor, Device,
        DeviceDescriptor, Extent3d, Instance, InstanceDescriptor, MapMode, PollType, Queue,
        RequestAdapterOptions, TexelCopyBufferInfo, TexelCopyBufferLayout, Texture,
        TextureDescriptor, TextureDimension, TextureFormat, TextureUsages, TextureViewDescriptor,
    },
};
use kithara_platform::time::{Duration, WallInstant as WallClock};
use kithara_test_utils::kithara;
use kithara_ui::{
    app::{App, Config, Frame, Ui},
    builtin,
    compile::{CompiledNode, CompiledUi, compile},
    draw::{PoolStats, Pt},
    expand::ExpandedNode,
    interact::{Input, MOUSE, PointerButton, PointerInput, PointerPhase, Scroll},
    registry::EndpointRegistry,
    render::{
        Clock, ReadValue, Reads, Skin, UiEvent,
        fonts::{FONT_BYTES, SANS},
        shader::ShaderPass,
        tree,
        vis::VisPass,
    },
    source::{MemResolver, UiConfig},
};
use masonry::vello::{
    AaConfig, AaSupport, RenderParams, Renderer as VelloRenderer, RendererOptions,
    peniko::Color as VelloColor, wgpu as vello_wgpu,
};
use num_traits::cast::AsPrimitive as _;

use self::{
    fixture::Consts,
    mock::MockReads,
    sections::{ModuleDemo, Tab},
};

/// The format each host rasterises into. The retained one matches the gallery's
/// own capture; iced's engine is built for a surface format, which is the sRGB
/// pair.
const IMMEDIATE_FORMAT: TextureFormat = TextureFormat::Rgba8UnormSrgb;
const RETAINED_FORMAT: vello_wgpu::TextureFormat = vello_wgpu::TextureFormat::Rgba8Unorm;

/// Frames discarded before measuring. The first carries the mount, the layout,
/// the shaping caches and every pipeline either host compiles lazily.
const WARMUP: usize = 3;

/// How many frames one direction lasts, for a wheel and for a drag alike. Both
/// run into an end and stop consuming there, which is a different code path: a
/// monotonic scroll measures a clamped no-op after about six frames, and a
/// monotonic drag measures a fader pinned at one end of its rail.
const REVERSAL: usize = 20;

/// How far one pointer move carries a drag, in page points. Small enough that a
/// frame's worth of moves stays on the rail at every sweep step, and large
/// enough that each one lands on a different pixel of it.
const DRAG_STEP: f32 = 1.5;

/// The harness's own page, mounting the one list the gallery scrolls together
/// with the visualiser, so a scroll slope measured with a visualiser on the page
/// can be compared against the same slope without one. It lives here rather than
/// in the gallery's assets because every page there joins `Shot::all()` and the
/// parity lane.
///
/// The list is the navigator's: it is the only one in the gallery whose content
/// outgrows its viewport, and a wheel over a viewport nothing overflows is a
/// measurement of a clamp rather than of a scroll.
const SCROLL_VIS_LAYOUT: &str = "perf-scroll-vis.klayout.ron";
const SCROLL_VIS_RON: &str = r#"(schema: "kithara.layout", version: 1, id: "perf-scroll-vis",
    root: Split(axis: Horizontal, children: [
        (weight: 1.0, node: Module(instance: "nav", source: "modules/nav.kmodule.ron", corners: true, size: (w: Fixed(198.0), h: Fill))),
        (weight: 1.0, node: Module(instance: "vis", source: "modules/tabs/vis.kmodule.ron", corners: true, size: (w: Fill, h: Fill))),
    ]))"#;

/// What drives a page from one frame to the next.
#[derive(Clone, Copy)]
enum Program {
    /// Nothing at all. The page is measured drawing itself unprompted, which is
    /// what makes it the control every other page is read against.
    Idle,
    /// The page's own animation, at a sweep of waveform bucket counts.
    Buckets(&'static [u16]),
    /// The page's own animation.
    Tick,
    /// `n` wheel events per frame, at a sweep of `n`.
    Wheels(&'static [usize]),
    /// A button held down for the whole run and `n` pointer moves per frame, at
    /// a sweep of `n`. This is the program the eye complains about: a fader
    /// under the hand, redrawn as fast as the host can.
    Drag(&'static [usize]),
}

impl Program {
    /// One run per sweep step, each named by the value it swept to.
    fn runs(self) -> Vec<Run> {
        match self {
            Self::Idle | Self::Tick => vec![Run::Plain],
            Self::Buckets(counts) => counts.iter().copied().map(Run::Buckets).collect(),
            Self::Wheels(counts) => counts.iter().copied().map(Run::Wheels).collect(),
            Self::Drag(counts) => counts.iter().copied().map(Run::Moves).collect(),
        }
    }
}

#[derive(Clone, Copy)]
enum Run {
    Plain,
    Buckets(u16),
    Wheels(usize),
    Moves(usize),
}

impl Run {
    fn label(self) -> String {
        match self {
            Self::Plain => "-".to_owned(),
            Self::Buckets(count) => format!("buckets={count}"),
            Self::Wheels(count) => format!("wheels={count}"),
            Self::Moves(count) => format!("moves={count}"),
        }
    }

    const fn wheels(self) -> usize {
        match self {
            Self::Plain | Self::Buckets(_) | Self::Moves(_) => 0,
            Self::Wheels(count) => count,
        }
    }

    const fn moves(self) -> usize {
        match self {
            Self::Plain | Self::Buckets(_) | Self::Wheels(_) => 0,
            Self::Moves(count) => count,
        }
    }
}

/// Which symptom a page is measured for. One route per group, so a run can ask
/// the question it is after without paying for the other two.
#[derive(Clone, Copy, Eq, PartialEq)]
enum Group {
    /// The cheap-page control and the stress page it is read against.
    Pages,
    /// The visualiser and the document shader, with their fenced variants.
    Native,
    /// The wheel sweeps.
    Scroll,
    /// The drag sweeps: a control held under the pointer and moved.
    Drag,
}

/// One page, the program that drives it, and everything the harness has to know
/// before its numbers mean anything.
struct Page {
    name: &'static str,
    group: Group,
    entry: &'static str,
    /// Which mock state the page's tick advances. The gallery's reads move the
    /// stress waveforms only on the stress tab and the visualiser only on the
    /// vis one, so a page measured under the wrong tab is a still picture.
    tab: Tab,
    frames: usize,
    program: Program,
    /// The one reading this page moves on its own, if it has one. A run that
    /// claims to measure an animating page has to show that it animated.
    moving: Option<&'static str>,
    /// Where a wheel is delivered, in logical page points. A wheel outside the
    /// viewport measures a page that was never scrolled.
    pointer_at: Pt,
    /// Whether this page also runs a fenced variant, which serialises the queue
    /// and whose totals may never be added to an unfenced frame total.
    fenced: bool,
    /// The hotpath guard each host opens for this page. Stage labels inside are
    /// the host's, so the guard name is what makes a line `host.page.stage`.
    immediate_guard: &'static str,
    retained_guard: &'static str,
}

const PAGES: &[Page] = &[
    Page {
        name: "gallery-buttons",
        group: Group::Pages,
        entry: "gallery-buttons.klayout.ron",
        tab: Tab::Buttons,
        frames: 120,
        program: Program::Idle,
        moving: None,
        pointer_at: Pt { x: 700.0, y: 400.0 },
        fenced: false,
        immediate_guard: "iced.gallery-buttons",
        retained_guard: "vello.gallery-buttons",
    },
    Page {
        // The page reported to hang the live window. Nothing on it is driven by
        // the mock, so what it measures is the cost of the page standing still:
        // a deck overview, an elapsed time and a master clock whose popover the
        // mock opens by default, which is most of the page's tree.
        name: "gallery-clock",
        group: Group::Pages,
        entry: "gallery-clock.klayout.ron",
        tab: Tab::Clock,
        frames: 120,
        program: Program::Idle,
        moving: None,
        pointer_at: Pt { x: 700.0, y: 400.0 },
        fenced: false,
        immediate_guard: "iced.gallery-clock",
        retained_guard: "vello.gallery-clock",
    },
    Page {
        name: "gallery-stress",
        group: Group::Pages,
        entry: "gallery-stress.klayout.ron",
        tab: Tab::Stress,
        frames: 120,
        program: Program::Buckets(&[8_192, 4_096, 1_024, 256]),
        moving: Some("bench.wave.0"),
        pointer_at: Pt { x: 700.0, y: 400.0 },
        fenced: false,
        immediate_guard: "iced.gallery-stress",
        retained_guard: "vello.gallery-stress",
    },
    Page {
        name: "gallery-vis",
        group: Group::Native,
        entry: "gallery-vis.klayout.ron",
        tab: Tab::Vis,
        frames: 120,
        program: Program::Tick,
        moving: Some("vis.time"),
        pointer_at: Pt { x: 700.0, y: 400.0 },
        fenced: true,
        immediate_guard: "iced.gallery-vis",
        retained_guard: "vello.gallery-vis",
    },
    Page {
        // The gallery's shader page binds two constant models, so nothing under
        // it moves; what it measures is the cost of running the document's own
        // fragment every frame regardless.
        name: "gallery-shader",
        group: Group::Native,
        entry: "gallery-shader.klayout.ron",
        tab: Tab::Shader,
        frames: 120,
        program: Program::Tick,
        moving: None,
        pointer_at: Pt { x: 700.0, y: 400.0 },
        fenced: true,
        immediate_guard: "iced.gallery-shader",
        retained_guard: "vello.gallery-shader",
    },
    Page {
        name: "gallery-pivot",
        group: Group::Scroll,
        entry: "gallery-pivot.klayout.ron",
        tab: Tab::Pivot,
        frames: 60,
        program: Program::Wheels(&[0, 1, 4, 8]),
        moving: None,
        pointer_at: Pt { x: 100.0, y: 400.0 },
        fenced: false,
        immediate_guard: "iced.gallery-pivot",
        retained_guard: "vello.gallery-pivot",
    },
    Page {
        name: "gallery-library2",
        group: Group::Scroll,
        entry: "gallery-library2.klayout.ron",
        tab: Tab::Library2,
        frames: 60,
        program: Program::Wheels(&[0, 1, 4, 8]),
        moving: None,
        pointer_at: Pt { x: 100.0, y: 400.0 },
        fenced: false,
        immediate_guard: "iced.gallery-library2",
        retained_guard: "vello.gallery-library2",
    },
    Page {
        name: "gallery-tree",
        group: Group::Scroll,
        entry: "gallery-tree.klayout.ron",
        tab: Tab::Tree,
        frames: 60,
        program: Program::Wheels(&[0, 1, 4, 8]),
        moving: None,
        pointer_at: Pt { x: 100.0, y: 400.0 },
        fenced: false,
        immediate_guard: "iced.gallery-tree",
        retained_guard: "vello.gallery-tree",
    },
    Page {
        // The horizontal fader's rail, which the gallery lays out from x=250 to
        // x=422 at y=198. The point is a constant, so the run proves it landed:
        // `mock.volume` is the fader's own reading, and a drag that missed the
        // rail leaves it where it was.
        name: "gallery-faders",
        group: Group::Drag,
        entry: "gallery-faders.klayout.ron",
        tab: Tab::Faders,
        frames: 60,
        program: Program::Drag(&[0, 1, 4, 8]),
        moving: Some("mock.volume"),
        pointer_at: Pt { x: 320.0, y: 198.0 },
        fenced: false,
        immediate_guard: "iced.gallery-faders",
        retained_guard: "vello.gallery-faders",
    },
    Page {
        // Transformed nodes with nothing running them: the mock hands each
        // object its pose and the page's clock stands still. Read against
        // `gallery-motion` it separates what a pose costs from what animating
        // one costs, and against `gallery-buttons` what a transform costs at
        // all.
        name: "gallery-objects",
        group: Group::Pages,
        entry: "gallery-objects.klayout.ron",
        tab: Tab::Objects,
        frames: 120,
        program: Program::Idle,
        moving: None,
        pointer_at: Pt { x: 700.0, y: 400.0 },
        fenced: false,
        immediate_guard: "iced.gallery-objects",
        retained_guard: "vello.gallery-objects",
    },
    // The three pages the toolkit's own motion runs on: objects posed by a
    // clock, sheets cut into frames, and artworks emitted per frame. All three
    // move off `gallery.motion.clock`, which the mock advances on their tabs,
    // so a run that measured a still page fails its own moving check.
    Page {
        name: "gallery-motion",
        group: Group::Pages,
        entry: "gallery-motion.klayout.ron",
        tab: Tab::Motion,
        frames: 120,
        program: Program::Tick,
        moving: Some("gallery.motion.clock"),
        pointer_at: Pt { x: 700.0, y: 400.0 },
        fenced: false,
        immediate_guard: "iced.gallery-motion",
        retained_guard: "vello.gallery-motion",
    },
    Page {
        name: "gallery-sprites",
        group: Group::Pages,
        entry: "gallery-sprites.klayout.ron",
        tab: Tab::Sprites,
        frames: 120,
        program: Program::Tick,
        moving: Some("gallery.motion.clock"),
        pointer_at: Pt { x: 700.0, y: 400.0 },
        fenced: false,
        immediate_guard: "iced.gallery-sprites",
        retained_guard: "vello.gallery-sprites",
    },
    Page {
        name: "gallery-lottie",
        group: Group::Pages,
        entry: "gallery-lottie.klayout.ron",
        tab: Tab::Lottie,
        frames: 120,
        program: Program::Tick,
        moving: Some("gallery.motion.clock"),
        pointer_at: Pt { x: 700.0, y: 400.0 },
        fenced: false,
        immediate_guard: "iced.gallery-lottie",
        retained_guard: "vello.gallery-lottie",
    },
    Page {
        name: "perf-scroll-vis",
        group: Group::Scroll,
        entry: SCROLL_VIS_LAYOUT,
        tab: Tab::Vis,
        frames: 60,
        program: Program::Wheels(&[0, 1, 4, 8]),
        moving: Some("vis.time"),
        pointer_at: Pt { x: 100.0, y: 400.0 },
        fenced: false,
        immediate_guard: "iced.perf-scroll-vis",
        retained_guard: "vello.perf-scroll-vis",
    },
];

/// The gallery as an application, with the page it shows fixed by the harness.
///
/// The document never changes under measurement, so no frame is spent
/// recompiling: a page that turned itself mid-run would measure the mount.
struct PageApp {
    entry: &'static str,
    published: usize,
    reads: MockReads,
    /// Whether what the document publishes is fed back into the mock.
    ///
    /// A drag is a closed loop: the pointer moves, the fader publishes a value,
    /// and the next frame draws the fader at it. Left open, the run measures a
    /// still page under a moving pointer. A wheel is not a loop - the viewport
    /// holds its own offset inside the host - and closing it there would change
    /// what the scroll runs measure: the retained library page publishes a
    /// selection with its scroll, and applying that scrolls the list back to it.
    closes_loop: bool,
}

impl PageApp {
    fn new(page: &Page, run: Run) -> Self {
        let mut reads = MockReads::default();
        reads.select_tab(page.tab);
        if let Run::Buckets(count) = run {
            reads.set_wave_buckets(count);
        }
        Self {
            closes_loop: matches!(run, Run::Moves(_)),
            entry: page.entry,
            published: 0,
            reads,
        }
    }

    /// A fingerprint of one reading, so a run can show that the value under it
    /// really moved. Scalars and waveforms are the two shapes a gallery page
    /// animates.
    fn digest_of(&self, endpoint: &str) -> Option<u64> {
        let mut hasher = DefaultHasher::new();
        match self.reads.get(endpoint)? {
            ReadValue::Scalar(value) => value.to_bits().hash(&mut hasher),
            ReadValue::Waveform(view) => {
                view.buckets.len().hash(&mut hasher);
                for bucket in view.buckets {
                    bucket.low.to_bits().hash(&mut hasher);
                    bucket.mid.to_bits().hash(&mut hasher);
                    bucket.high.to_bits().hash(&mut hasher);
                }
            }
            _ => return None,
        }
        Some(hasher.finish())
    }
}

impl App for PageApp {
    fn document(&self) -> &str {
        self.entry
    }

    fn reads<R>(&self, with: impl FnOnce(&dyn Reads) -> R) -> R {
        with(&self.reads)
    }

    fn tick(&mut self) {
        self.reads.tick();
    }

    fn update(&mut self, event: UiEvent) {
        self.published += 1;
        // A nav path would turn the page mid-run, which is the one thing the
        // harness fixes; everything else the document publishes is applied.
        if self.closes_loop
            && let UiEvent::Control { path, action } = event
            && Tab::try_from(path.as_str()).is_err()
        {
            self.reads.apply(&path, &action);
        }
    }
}

/// What one frame did, counted rather than timed.
///
/// A frame slow because it draws twice as much is a different defect from one
/// slow because it rebuilds, and these tell the two apart where a duration
/// cannot.
#[derive(Clone, Copy, Default)]
struct Census {
    /// Vello scene encoding sizes. The immediate host has no scene and leaves
    /// this unset rather than reporting a zero it never measured.
    scene: Option<Scene>,
    /// Native declarations the frame produced, retained host only.
    natives: Option<Natives>,
    pool: Pool,
    scheduled: bool,
}

/// One frame's share of the draw pool's counters. Reported apart, never summed
/// into one number: a sum hides reuse behind real allocation.
#[derive(Clone, Copy, Default)]
struct Pool {
    alloc_misses: u64,
    home_hits: u64,
    put_drops: u64,
    steal_hits: u64,
}

impl Pool {
    fn delta(before: &PoolStats, after: &PoolStats) -> Self {
        Self {
            alloc_misses: after.alloc_misses.saturating_sub(before.alloc_misses),
            home_hits: after.home_hits.saturating_sub(before.home_hits),
            put_drops: after.put_drops.saturating_sub(before.put_drops),
            steal_hits: after.steal_hits.saturating_sub(before.steal_hits),
        }
    }

    fn add(&mut self, other: Self) {
        self.alloc_misses += other.alloc_misses;
        self.home_hits += other.home_hits;
        self.put_drops += other.put_drops;
        self.steal_hits += other.steal_hits;
    }
}

#[derive(Clone, Copy, Default)]
struct Scene {
    draw_data: usize,
    draw_tags: usize,
    path_data: usize,
    transforms: usize,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct Natives {
    shaders: usize,
    vis: usize,
}

/// Everything one run produced, summed where summing is honest and kept apart
/// where it is not.
#[derive(Default)]
struct Tally {
    frames: usize,
    scheduled: usize,
    /// The last frame's scene. A scrolled page legitimately encodes a different
    /// scene every frame, so this is a snapshot, not a total.
    scene: Option<Scene>,
    natives: Option<Natives>,
    /// Whether any frame declared a different number of native draws than the
    /// first. One page cannot be measured as two.
    natives_varied: bool,
    pool: Pool,
    durations: Vec<Duration>,
}

impl Tally {
    fn push(&mut self, census: Census, elapsed: Duration) {
        self.frames += 1;
        self.scheduled += usize::from(census.scheduled);
        if let (Some(first), Some(now)) = (self.natives, census.natives) {
            self.natives_varied |= first != now;
        }
        self.natives = self.natives.or(census.natives);
        self.scene = census.scene;
        self.pool.add(census.pool);
        self.durations.push(elapsed);
    }

    /// Total, mean, shortest and longest frame, in microseconds.
    fn micros(&self) -> [u128; 4] {
        let total: u128 = self.durations.iter().map(Duration::as_micros).sum();
        let count = u128::try_from(self.durations.len().max(1)).unwrap_or(1);
        let min = self
            .durations
            .iter()
            .map(Duration::as_micros)
            .min()
            .unwrap_or_default();
        let max = self
            .durations
            .iter()
            .map(Duration::as_micros)
            .max()
            .unwrap_or_default();
        [total, total / count, min, max]
    }
}

/// One frame's worth of work on one host, from the input applied to the pixels
/// produced. Both hosts rasterise: stopping at the recorded commands would
/// report times for the visualiser passes without running them.
trait PageHost {
    /// Puts the pointer where the wheel will be delivered, once, before the
    /// warm-up. A wheel is dispatched at wherever the host last saw the
    /// pointer, so a host never told scrolls the wrong thing, or nothing.
    fn place_pointer(&mut self);

    /// Delivers one wheel detent at the page's pointer point.
    fn wheel(&mut self, lines: f32);

    /// Puts the button down where the pointer is, for the whole run. A fader
    /// only tracks the pointer while it is held, so a drag measured without
    /// this measures a page that ignored every move.
    fn press(&mut self);

    /// Moves the pointer along the rail and reports where it landed, so the
    /// caller can turn it around at the ends rather than measure a clamp.
    fn move_by(&mut self, dx: f32) -> f32;

    /// Lets the button go, once, after the last measured frame.
    fn release(&mut self);

    /// Produces and rasterises one frame.
    fn frame(&mut self) -> Census;

    /// The same, with the queue drained inside each pass. A fenced run
    /// serialises the GPU into the frame it belongs to; it is its own scenario
    /// and its totals may never be added to an unfenced total.
    fn fenced_frame(&mut self) -> Census;

    /// The pixels of the last frame this host rasterised.
    fn pixels(&mut self) -> Vec<u8>;

    /// A fingerprint of the last frame, taken at the last artefact this host
    /// owns. Two runs of one page that fingerprint alike drew the same picture,
    /// so this is what "the page moved" is read from.
    ///
    /// It is not the pixels on both hosts, and the reason is measured: the
    /// retained host hands Vello a scene, and one unchanged scene rasterises to
    /// as many as six different pixels from one call to the next. Fingerprinting
    /// its pixels reports the rasteriser's own noise as the page moving — which
    /// makes the control run flap and makes the wheel and drag assertions pass
    /// whether or not anything scrolled.
    fn picture(&mut self) -> u64;

    /// What [`PageHost::picture`] is a fingerprint of, in one word, for the
    /// lines that report it. A still scene on a page whose visualiser moves in
    /// a pass that never enters one is not a still page, and a line that said
    /// "picture" for both hosts would be claiming it is.
    fn drawn_as(&self) -> &'static str;

    /// A fingerprint of the page's own moving reading, if the page named one.
    fn reading(&self, endpoint: Option<&str>) -> Option<u64>;

    /// How many events the mounted document published, cumulative.
    fn published(&self) -> usize;
}

/// The immediate host: every frame rebuilds the element tree from the compiled
/// document, lays it out, applies the queued events, draws and presents.
struct Immediate {
    app: PageApp,
    cache: Cache,
    cursor: Cursor,
    device: Device,
    pending: Vec<Event>,
    queue: Queue,
    renderer: iced::Renderer,
    scheduled: bool,
    texture: Texture,
    theme: Theme,
    ui: CompiledUi,
    pointer_at: Pt,
}

impl Immediate {
    fn new(ui: CompiledUi, page: &Page, app: PageApp, gpu: &ImmediateGpu) -> Self {
        Self {
            app,
            cache: Cache::default(),
            cursor: Cursor::Available(Point::new(page.pointer_at.x, page.pointer_at.y)),
            device: gpu.device.clone(),
            pending: Vec::new(),
            queue: gpu.queue.clone(),
            renderer: FallbackRenderer::Primary(WgpuRenderer::new(
                gpu.engine.clone(),
                SANS,
                Pixels(14.0),
            )),
            scheduled: false,
            texture: gpu.texture(),
            theme: theme(builtin::skin()),
            ui,
            pointer_at: page.pointer_at,
        }
    }

    fn draw(&mut self) -> Census {
        let before = self.ui.draw_pool_stats();
        self.app.tick();
        let bounds = Size::new(Consts::WIDTH, Consts::HEIGHT);
        let element = measure_block!(
            "iced.view",
            self.app.reads(|reads| tree::render(
                &self.ui.root,
                &self.ui,
                reads,
                builtin::skin(),
                Clock::default()
            ))
        );
        let mut interface = measure_block!(
            "iced.build",
            UserInterface::build(
                element,
                bounds,
                mem::take(&mut self.cache),
                &mut self.renderer
            )
        );
        let mut messages: Vec<UiEvent> = Vec::new();
        self.scheduled = !self.pending.is_empty();
        let cursor = self.cursor;
        measure_block!("iced.update", {
            for event in mem::take(&mut self.pending) {
                let (state, _statuses) = interface.update(
                    slice::from_ref(&event),
                    cursor,
                    &mut self.renderer,
                    &mut clipboard::Null,
                    &mut messages,
                );
                self.scheduled |= redraw_asked(&state);
            }
        });
        let base = self.theme.base();
        measure_block!("iced.draw", {
            interface.draw(
                &mut self.renderer,
                &self.theme,
                &Style {
                    text_color: base.text_color,
                },
                cursor,
            );
        });
        self.cache = interface.into_cache();
        for event in messages {
            self.app.update(event);
        }
        Census {
            scene: None,
            natives: None,
            pool: Pool::delta(&before, &self.ui.draw_pool_stats()),
            scheduled: self.scheduled,
        }
    }

    /// Submits the recorded frame. This is where the shader primitives iced
    /// stored during `draw` are prepared, run and composited: a harness that
    /// stops before this measures a visualiser that never ran.
    fn present(&mut self) {
        let background: Color = builtin::skin().palette.bg.into();
        let texture = self.texture.clone();
        let view = texture.create_view(&TextureViewDescriptor::default());
        let viewport = Viewport::with_physical_size(Size::new(width(), height()), 1.0);
        match &mut self.renderer {
            FallbackRenderer::Primary(wgpu) => {
                wgpu.present(Some(background), IMMEDIATE_FORMAT, &view, &viewport);
            }
            FallbackRenderer::Secondary(_) => {
                panic!("the immediate page host must be built on the wgpu renderer")
            }
        }
    }
}

impl PageHost for Immediate {
    fn place_pointer(&mut self) {
        let position = Point::new(self.pointer_at.x, self.pointer_at.y);
        self.cursor = Cursor::Available(position);
        self.pending
            .push(Event::Mouse(MouseEvent::CursorMoved { position }));
    }

    fn wheel(&mut self, lines: f32) {
        self.pending.push(Event::Mouse(MouseEvent::WheelScrolled {
            delta: ScrollDelta::Lines { x: 0.0, y: lines },
        }));
    }

    fn press(&mut self) {
        self.pending
            .push(Event::Mouse(MouseEvent::ButtonPressed(MouseButton::Left)));
    }

    fn move_by(&mut self, dx: f32) -> f32 {
        self.pointer_at.x += dx;
        let position = Point::new(self.pointer_at.x, self.pointer_at.y);
        self.cursor = Cursor::Available(position);
        self.pending
            .push(Event::Mouse(MouseEvent::CursorMoved { position }));
        self.pointer_at.x
    }

    fn release(&mut self) {
        self.pending
            .push(Event::Mouse(MouseEvent::ButtonReleased(MouseButton::Left)));
    }

    fn frame(&mut self) -> Census {
        let census = self.draw();
        measure_block!("iced.encode", self.present());
        census
    }

    fn fenced_frame(&mut self) -> Census {
        let census = self.draw();
        measure_block!("iced.encode.gpu.fenced", {
            self.present();
            drain(&self.device);
        });
        census
    }

    fn pixels(&mut self) -> Vec<u8> {
        readback(&self.device, &self.queue, &self.texture)
    }

    /// The pixels themselves: what this host hands the texture comes back the
    /// same bytes every time, so a byte that differs is something drawn
    /// differently.
    fn picture(&mut self) -> u64 {
        digest(&self.pixels())
    }

    fn drawn_as(&self) -> &'static str {
        "pixels"
    }

    fn reading(&self, endpoint: Option<&str>) -> Option<u64> {
        self.app.digest_of(endpoint?)
    }

    fn published(&self) -> usize {
        self.app.published
    }
}

/// Whether a widget asked iced's runtime to come back with another frame. This
/// is the closest the immediate host has to a seam at which it could decline to
/// draw, and it does not decline: it rebuilds and draws either way.
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

/// The retained host: the tree stays mounted, input walks it, and a frame is a
/// refresh, a paint into a Vello scene, and the two native passes around it.
struct Retained<'a> {
    gpu: &'a mut RetainedGpu,
    picture: u64,
    scheduled: bool,
    ui: Ui<'a, PageApp>,
    pointer_at: Pt,
}

impl<'a> Retained<'a> {
    fn new(config: Config<'a>, page: &Page, app: PageApp, gpu: &'a mut RetainedGpu) -> Self {
        let ui = Ui::new(app, config, (width(), height()), 1.0)
            .unwrap_or_else(|error| panic!("the page-perf fixture must mount: {error}"));
        Self {
            gpu,
            picture: 0,
            scheduled: false,
            ui,
            pointer_at: page.pointer_at,
        }
    }

    /// The seam the window runner draws through: settle, ask whether the
    /// picture would change, paint, run the passes, complete. The frame is
    /// painted either way so there is always a cost to report, and whether it
    /// was wanted is counted separately.
    fn draw(&mut self, fence: bool) -> Census {
        let before = self.ui.draw_pool_stats();
        measure_block!(
            "vello.frame",
            self.ui.frame(Duration::from_millis(Consts::STRESS_TICK_MS))
        );
        self.scheduled = self.ui.needs_frame();
        let frame = measure_block!(
            "vello.render",
            self.ui
                .render()
                .unwrap_or_else(|error| panic!("the retained host must draw: {error}"))
        );
        let encoding = frame.scene().encoding();
        let scene = Scene {
            draw_data: encoding.draw_data.len(),
            draw_tags: encoding.draw_tags.len(),
            path_data: encoding.path_data.len(),
            transforms: encoding.transforms.len(),
        };
        // The scene is where this host's own drawing ends: the coordinates, what
        // is painted with them, where each is placed, and the counts that say
        // how they are grouped. Past here the picture belongs to the rasteriser.
        self.picture = {
            let mut hasher = DefaultHasher::new();
            encoding.path_data.hash(&mut hasher);
            encoding.draw_data.hash(&mut hasher);
            for placed in &encoding.transforms {
                placed.matrix.map(f32::to_bits).hash(&mut hasher);
                placed.translation.map(f32::to_bits).hash(&mut hasher);
            }
            encoding.n_paths.hash(&mut hasher);
            encoding.n_path_segments.hash(&mut hasher);
            encoding.n_clips.hash(&mut hasher);
            hasher.finish()
        };
        let natives = Natives {
            shaders: frame.shaders().len(),
            vis: frame.vis().len(),
        };
        if fence {
            self.gpu.fenced_passes(&frame);
        } else {
            self.gpu.passes(&frame);
        }
        self.ui.complete_frame();
        Census {
            scene: Some(scene),
            natives: Some(natives),
            pool: Pool::delta(&before, &self.ui.draw_pool_stats()),
            scheduled: self.scheduled,
        }
    }
}

impl PageHost for Retained<'_> {
    fn place_pointer(&mut self) {
        let at = self.pointer_at;
        self.ui.input(Input::Pointer(PointerInput::new(
            MOUSE,
            None,
            PointerPhase::Move,
            Some(at),
            1,
        )));
    }

    fn wheel(&mut self, lines: f32) {
        self.ui
            .input(Input::Wheel(Scroll::Lines { x: 0.0, y: lines }));
    }

    fn press(&mut self) {
        let at = self.pointer_at;
        self.ui.input(Input::Pointer(PointerInput::new(
            MOUSE,
            Some(PointerButton::Primary),
            PointerPhase::Down,
            Some(at),
            1,
        )));
    }

    fn move_by(&mut self, dx: f32) -> f32 {
        self.pointer_at.x += dx;
        let at = self.pointer_at;
        self.ui.input(Input::Pointer(PointerInput::new(
            MOUSE,
            None,
            PointerPhase::Move,
            Some(at),
            1,
        )));
        at.x
    }

    fn release(&mut self) {
        let at = self.pointer_at;
        self.ui.input(Input::Pointer(PointerInput::new(
            MOUSE,
            Some(PointerButton::Primary),
            PointerPhase::Up,
            Some(at),
            1,
        )));
    }

    fn frame(&mut self) -> Census {
        self.draw(false)
    }

    fn fenced_frame(&mut self) -> Census {
        self.draw(true)
    }

    fn pixels(&mut self) -> Vec<u8> {
        readback_vello(&self.gpu.device, &self.gpu.queue, &self.gpu.texture)
    }

    /// The scene of the last frame drawn, fingerprinted where it was handed
    /// over, rather than the pixels Vello then made of it.
    fn picture(&mut self) -> u64 {
        self.picture
    }

    fn drawn_as(&self) -> &'static str {
        "scene"
    }

    fn reading(&self, endpoint: Option<&str>) -> Option<u64> {
        self.ui.app().digest_of(endpoint?)
    }

    fn published(&self) -> usize {
        self.ui.app().published
    }
}

/// Everything a host is handed that is not the host: the documents, the
/// endpoints they may bind to, and nothing that differs between the two.
struct Fixture {
    registry: Box<dyn EndpointRegistry>,
    resolver: MemResolver,
}

impl Fixture {
    fn new() -> Self {
        let mut resolver = fixture::resolver();
        resolver.insert(SCROLL_VIS_LAYOUT, SCROLL_VIS_RON);
        Self {
            registry: Box::new(mock::registry()),
            resolver,
        }
    }

    fn config(&self) -> Config<'_> {
        Config::builder()
            .endpoints(self.registry.as_ref())
            .resolver(&self.resolver)
            .skin(builtin::skin())
            .skin_doc(builtin::skin_doc())
            .build()
    }

    fn compiled(&self, entry: &str) -> CompiledUi {
        compile(
            entry,
            &self.resolver,
            self.registry.as_ref(),
            builtin::skin_doc(),
            builtin::text_doc(),
            &UiConfig::default(),
        )
        .unwrap_or_else(|error| panic!("the page-perf document {entry} must compile: {error}"))
    }
}

/// A wgpu device with no window, plus iced's engine on it. Building one per page
/// would measure device creation once per page.
struct ImmediateGpu {
    device: Device,
    engine: Engine,
    queue: Queue,
}

impl ImmediateGpu {
    fn new() -> Result<Self, String> {
        let instance = Instance::new(&InstanceDescriptor {
            backends: Backends::PRIMARY,
            ..InstanceDescriptor::default()
        });
        let adapter = block_on(instance.request_adapter(&RequestAdapterOptions::default()))
            .map_err(|error| format!("no wgpu adapter: {error}"))?;
        let (device, queue) = block_on(adapter.request_device(&DeviceDescriptor::default()))
            .map_err(|error| format!("no wgpu device: {error}"))?;
        let engine = Engine::new(
            &adapter,
            device.clone(),
            queue.clone(),
            IMMEDIATE_FORMAT,
            None,
            Shell::headless(),
        );
        Ok(Self {
            device,
            engine,
            queue,
        })
    }

    fn texture(&self) -> Texture {
        self.device.create_texture(&TextureDescriptor {
            label: Some("kithara_ui.page_perf.immediate"),
            size: Extent3d {
                width: width(),
                height: height(),
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: TextureDimension::D2,
            format: IMMEDIATE_FORMAT,
            usage: TextureUsages::RENDER_ATTACHMENT | TextureUsages::COPY_SRC,
            view_formats: &[],
        })
    }
}

/// The retained host's own device, Vello renderer, and the two native passes the
/// application runs around the scene.
struct RetainedGpu {
    device: vello_wgpu::Device,
    queue: vello_wgpu::Queue,
    shaders: ShaderPass,
    texture: vello_wgpu::Texture,
    vello: VelloRenderer,
    vis: VisPass,
}

impl RetainedGpu {
    fn new() -> Result<Self, String> {
        let instance = vello_wgpu::Instance::new(&vello_wgpu::InstanceDescriptor {
            backends: vello_wgpu::Backends::PRIMARY,
            ..vello_wgpu::InstanceDescriptor::default()
        });
        let adapter =
            block_on(instance.request_adapter(&vello_wgpu::RequestAdapterOptions::default()))
                .map_err(|error| format!("no wgpu adapter: {error}"))?;
        let (device, queue) =
            block_on(adapter.request_device(&vello_wgpu::DeviceDescriptor::default()))
                .map_err(|error| format!("no wgpu device: {error}"))?;
        let vello = VelloRenderer::new(
            &device,
            RendererOptions {
                use_cpu: false,
                antialiasing_support: AaSupport::area_only(),
                num_init_threads: None,
                pipeline_cache: None,
            },
        )
        .map_err(|error| format!("vello renderer: {error}"))?;
        let texture = device.create_texture(&vello_wgpu::TextureDescriptor {
            label: Some("kithara_ui.page_perf.retained"),
            size: vello_wgpu::Extent3d {
                width: width(),
                height: height(),
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: vello_wgpu::TextureDimension::D2,
            format: RETAINED_FORMAT,
            usage: vello_wgpu::TextureUsages::STORAGE_BINDING
                | vello_wgpu::TextureUsages::RENDER_ATTACHMENT
                | vello_wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let shaders = ShaderPass::new(&device);
        let vis = VisPass::new(&device, RETAINED_FORMAT);
        Ok(Self {
            device,
            queue,
            shaders,
            texture,
            vello,
            vis,
        })
    }

    /// Shader images, then the scene, then the native visualiser draws - the
    /// order the window runner uses.
    fn passes(&mut self, frame: &Frame) {
        let view = self
            .texture
            .create_view(&vello_wgpu::TextureViewDescriptor::default());
        measure_block!(
            "vello.pass.shader",
            self.shaders
                .render(&self.device, &self.queue, &mut self.vello, frame.shaders())
        );
        measure_block!("vello.scene", self.scene(frame, &view));
        measure_block!(
            "vello.pass.vis",
            self.vis.render(
                &self.device,
                &self.queue,
                &view,
                frame.vis(),
                1.0,
                [width(), height()],
            )
        );
    }

    /// The same three passes with the queue drained after each, so each label
    /// carries its own GPU time instead of the next pass's submit.
    fn fenced_passes(&mut self, frame: &Frame) {
        let view = self
            .texture
            .create_view(&vello_wgpu::TextureViewDescriptor::default());
        measure_block!("vello.pass.shader.gpu.fenced", {
            self.shaders
                .render(&self.device, &self.queue, &mut self.vello, frame.shaders());
            drain_vello(&self.device);
        });
        measure_block!("vello.scene.gpu.fenced", {
            self.scene(frame, &view);
            drain_vello(&self.device);
        });
        measure_block!("vello.pass.vis.gpu.fenced", {
            self.vis.render(
                &self.device,
                &self.queue,
                &view,
                frame.vis(),
                1.0,
                [width(), height()],
            );
            drain_vello(&self.device);
        });
    }

    fn scene(&mut self, frame: &Frame, view: &vello_wgpu::TextureView) {
        let background: VelloColor = builtin::skin().palette.bg.into();
        self.vello
            .render_to_texture(
                &self.device,
                &self.queue,
                frame.scene(),
                view,
                &RenderParams {
                    base_color: background,
                    width: width(),
                    height: height(),
                    antialiasing_method: AaConfig::Area,
                },
            )
            .unwrap_or_else(|error| panic!("the retained host must rasterise: {error}"));
    }
}

/// Which host a case measures, and the device it needs before it can.
enum Gpu {
    Immediate(Box<ImmediateGpu>),
    Retained(Box<RetainedGpu>),
}

fn drain(device: &Device) {
    device
        .poll(PollType::Wait {
            submission_index: None,
            timeout: None,
        })
        .unwrap_or_else(|error| panic!("the immediate queue must drain: {error}"));
}

fn drain_vello(device: &vello_wgpu::Device) {
    device
        .poll(vello_wgpu::PollType::Wait)
        .unwrap_or_else(|error| panic!("the retained queue must drain: {error}"));
}

/// The page every run is laid out and rasterised at: the gallery's own window
/// size at 1x, so a page measured here is the page the application shows.
fn width() -> u32 {
    Consts::WIDTH.as_()
}

fn height() -> u32 {
    Consts::HEIGHT.as_()
}

fn unpadded_row() -> u32 {
    width() * 4
}

/// wgpu requires each copied row to start on a 256-byte boundary.
fn padded_row() -> u32 {
    unpadded_row().div_ceil(256) * 256
}

fn readback(device: &Device, queue: &Queue, texture: &Texture) -> Vec<u8> {
    let padded = padded_row();
    let buffer = device.create_buffer(&BufferDescriptor {
        label: Some("kithara_ui.page_perf.immediate.readback"),
        size: u64::from(padded) * u64::from(height()),
        usage: BufferUsages::MAP_READ | BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let mut encoder = device.create_command_encoder(&CommandEncoderDescriptor::default());
    encoder.copy_texture_to_buffer(
        texture.as_image_copy(),
        TexelCopyBufferInfo {
            buffer: &buffer,
            layout: TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(padded),
                rows_per_image: Some(height()),
            },
        },
        Extent3d {
            width: width(),
            height: height(),
            depth_or_array_layers: 1,
        },
    );
    queue.submit([encoder.finish()]);
    let slice = buffer.slice(..);
    slice.map_async(MapMode::Read, |_| {});
    drain(device);
    let mapped = slice.get_mapped_range();
    let rgba = rows(&mapped);
    drop(mapped);
    buffer.unmap();
    rgba
}

fn readback_vello(
    device: &vello_wgpu::Device,
    queue: &vello_wgpu::Queue,
    texture: &vello_wgpu::Texture,
) -> Vec<u8> {
    let padded = padded_row();
    let buffer = device.create_buffer(&vello_wgpu::BufferDescriptor {
        label: Some("kithara_ui.page_perf.retained.readback"),
        size: u64::from(padded) * u64::from(height()),
        usage: vello_wgpu::BufferUsages::MAP_READ | vello_wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let mut encoder =
        device.create_command_encoder(&vello_wgpu::CommandEncoderDescriptor::default());
    encoder.copy_texture_to_buffer(
        texture.as_image_copy(),
        vello_wgpu::TexelCopyBufferInfo {
            buffer: &buffer,
            layout: vello_wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(padded),
                rows_per_image: Some(height()),
            },
        },
        vello_wgpu::Extent3d {
            width: width(),
            height: height(),
            depth_or_array_layers: 1,
        },
    );
    queue.submit([encoder.finish()]);
    let slice = buffer.slice(..);
    slice.map_async(vello_wgpu::MapMode::Read, |_| {});
    drain_vello(device);
    let mapped = slice.get_mapped_range();
    let rgba = rows(&mapped);
    drop(mapped);
    buffer.unmap();
    rgba
}

fn rows(mapped: &[u8]) -> Vec<u8> {
    let unpadded = usize::try_from(unpadded_row()).unwrap_or(0);
    let padded = usize::try_from(padded_row()).unwrap_or(0);
    let height = usize::try_from(height()).unwrap_or(0);
    let mut rgba = Vec::with_capacity(unpadded * height);
    for row in 0..height {
        let start = row * padded;
        rgba.extend_from_slice(&mapped[start..start + unpadded]);
    }
    rgba
}

fn digest(pixels: &[u8]) -> u64 {
    let mut hasher = DefaultHasher::new();
    pixels.hash(&mut hasher);
    hasher.finish()
}

/// Whether the frame is more than one flat colour. A run that rasterised
/// nothing reports a beautiful number for nothing.
fn painted(pixels: &[u8]) -> bool {
    let Some(first) = pixels.get(..4) else {
        return false;
    };
    pixels.chunks_exact(4).any(|pixel| pixel != first)
}

/// How many `Vis` and `Shader` leaves the compiled page names, which is what the
/// retained host's declaration lists have to equal.
fn leaves(ui: &CompiledUi) -> Natives {
    fn walk(node: &ExpandedNode, found: &mut Natives) {
        match node {
            ExpandedNode::Row { children, .. }
            | ExpandedNode::Column { children, .. }
            | ExpandedNode::Slot { children, .. } => {
                for child in children {
                    walk(child, found);
                }
            }
            ExpandedNode::Optional { child, .. }
            | ExpandedNode::Pressable { child, .. }
            | ExpandedNode::Scroll { child, .. } => walk(child, found),
            ExpandedNode::Popover {
                anchor, content, ..
            } => {
                walk(anchor, found);
                walk(content, found);
            }
            ExpandedNode::Control { spec, .. } => match spec.kind() {
                "Vis" => found.vis += 1,
                "Shader" => found.shaders += 1,
                _ => {}
            },
            _ => {}
        }
    }

    let mut found = Natives::default();
    let mut stack = vec![&ui.root];
    while let Some(node) = stack.pop() {
        match node {
            CompiledNode::Split { children, .. } => {
                stack.extend(children.iter().map(|(_, child)| child));
            }
            CompiledNode::Optional { child, .. } => stack.push(child),
            CompiledNode::Module { root, .. } => walk(root, &mut found),
            _ => {}
        }
    }
    found
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

/// One run's result: the numbers, and every fact needed to say whether they are
/// about the thing the run claims to measure.
struct Outcome<'run> {
    page: &'run Page,
    run: Run,
    tally: Tally,
    expected: Natives,
    /// The page as its host drew it, before the first measured frame and after
    /// the last.
    picture: [u64; 2],
    /// Which artefact those two were taken from, for the lines that report them.
    drawn_as: &'static str,
    /// The page's own moving reading, at the same two moments.
    reading: [Option<u64>; 2],
    /// Whether the last frame is more than one flat colour.
    painted: bool,
    published: usize,
    fenced: bool,
}

/// The cheap-page control and the stress page: is the stress page's cost its
/// waveform buckets, or is it the page at all?
#[kithara::test]
#[case("iced", Host::Immediate, "gallery-buttons")]
#[case("iced", Host::Immediate, "gallery-clock")]
#[case("iced", Host::Immediate, "gallery-stress")]
#[case("vello", Host::Retained, "gallery-buttons")]
#[case("vello", Host::Retained, "gallery-clock")]
#[case("vello", Host::Retained, "gallery-stress")]
fn ui_page_perf(#[case] label: &'static str, #[case] host: Host, #[case] page: &'static str) {
    measure_one(label, host, Group::Pages, page);
}

/// The three pages that move without being touched: objects posed by a clock,
/// a sheet cut into frames, and an artwork emitted afresh every frame. The
/// buttons page above is the still control they are read against, because what
/// is asked here is what the motion itself costs on each host.
#[kithara::test]
#[case("iced", Host::Immediate, "gallery-objects")]
#[case("iced", Host::Immediate, "gallery-motion")]
#[case("iced", Host::Immediate, "gallery-sprites")]
#[case("iced", Host::Immediate, "gallery-lottie")]
#[case("vello", Host::Retained, "gallery-objects")]
#[case("vello", Host::Retained, "gallery-motion")]
#[case("vello", Host::Retained, "gallery-sprites")]
#[case("vello", Host::Retained, "gallery-lottie")]
fn ui_motion_perf(#[case] label: &'static str, #[case] host: Host, #[case] page: &'static str) {
    measure_one(label, host, Group::Pages, page);
}

/// The visualiser and the document shader, free and fenced. A fenced run
/// serialises the queue: its totals are their own scenario.
#[kithara::test]
#[case("iced", Host::Immediate, "gallery-vis")]
#[case("iced", Host::Immediate, "gallery-shader")]
#[case("vello", Host::Retained, "gallery-vis")]
#[case("vello", Host::Retained, "gallery-shader")]
fn ui_native_perf(#[case] label: &'static str, #[case] host: Host, #[case] page: &'static str) {
    measure_one(label, host, Group::Native, page);
}

/// The wheel sweeps: does a page cost more per wheel event, or per frame?
#[kithara::test]
#[case("iced", Host::Immediate, "gallery-pivot")]
#[case("iced", Host::Immediate, "gallery-library2")]
#[case("iced", Host::Immediate, "gallery-tree")]
#[case("iced", Host::Immediate, "perf-scroll-vis")]
#[case("vello", Host::Retained, "gallery-pivot")]
#[case("vello", Host::Retained, "gallery-library2")]
#[case("vello", Host::Retained, "gallery-tree")]
#[case("vello", Host::Retained, "perf-scroll-vis")]
fn ui_scroll_perf(#[case] label: &'static str, #[case] host: Host, #[case] page: &'static str) {
    measure_one(label, host, Group::Scroll, page);
}

/// The drag sweeps, and the first measurement of the symptom this campaign
/// started from: a fader that is said to move smoothly on the retained host and
/// to catch on the immediate one. Both hosts are measured at the same boundary
/// here - input applied, frame drawn, pixels rasterised - so the two columns can
/// be read against each other, which the per-widget harness cannot claim.
#[kithara::test]
#[case("iced", Host::Immediate, "gallery-faders")]
#[case("vello", Host::Retained, "gallery-faders")]
fn ui_drag_perf(#[case] label: &'static str, #[case] host: Host, #[case] page: &'static str) {
    measure_one(label, host, Group::Drag, page);
}

/// One page on one host, under one hotpath guard.
///
/// One guard per case, not per run: hotpath records nothing from a second guard
/// opened on the same thread, and libtest gives each case its own. So the guard
/// name carries the host and the page, the stage labels inside carry the host,
/// and the sweep steps within a page are told apart by the harness's own line
/// rather than by the hotpath table.
fn measure_one(label: &'static str, host: Host, group: Group, name: &'static str) {
    load_fonts();
    let page = PAGES
        .iter()
        .find(|page| page.name == name)
        .unwrap_or_else(|| panic!("{name} is not a measured page"));
    assert!(
        page.group == group,
        "{name} is measured by the route for another symptom"
    );
    let fixture = Fixture::new();
    let mut gpu = match host.gpu() {
        Ok(gpu) => gpu,
        Err(error) => {
            // Falling back to a software rasteriser here would report times for
            // the visualiser passes without running them.
            let notice = format!("{label}: NOT MEASURED, no host to measure it on: {error}");
            println!("{notice}");
            eprintln!("{notice}");
            return;
        }
    };
    let expected = leaves(&fixture.compiled(page.entry));
    let mut outcomes = Vec::new();
    {
        let _guard = HotpathGuardBuilder::new(host.guard(page))
            .functions_limit(0)
            .build();
        for run in page.program.runs() {
            outcomes.push(measure(&fixture, page, run, expected, &mut gpu, false));
            if page.fenced {
                outcomes.push(measure(&fixture, page, run, expected, &mut gpu, true));
            }
        }
    }
    // The undriven run of the same page, run for the same number of frames.
    // Whatever the page animates on its own has reached the same point in it, so
    // a picture that differs from this one differs by the wheel or the drag.
    let baseline = outcomes
        .iter()
        .find(|outcome| matches!(outcome.run, Run::Wheels(0) | Run::Moves(0)))
        .map(|outcome| outcome.picture[1]);
    for outcome in &outcomes {
        report(label, outcome);
    }
    for outcome in &outcomes {
        prove(label, outcome, baseline);
    }
}

#[derive(Clone, Copy)]
enum Host {
    Immediate,
    Retained,
}

impl Host {
    fn gpu(self) -> Result<Gpu, String> {
        match self {
            Self::Immediate => ImmediateGpu::new().map(Box::new).map(Gpu::Immediate),
            Self::Retained => RetainedGpu::new().map(Box::new).map(Gpu::Retained),
        }
    }

    const fn guard(self, page: &Page) -> &'static str {
        match self {
            Self::Immediate => page.immediate_guard,
            Self::Retained => page.retained_guard,
        }
    }
}

/// One run of one page: warm up, fingerprint, measure under a guard,
/// fingerprint again.
fn measure<'run>(
    fixture: &Fixture,
    page: &'run Page,
    run: Run,
    expected: Natives,
    gpu: &mut Gpu,
    fenced: bool,
) -> Outcome<'run> {
    let app = PageApp::new(page, run);
    let mut driver: Box<dyn PageHost + '_> = match gpu {
        Gpu::Immediate(gpu) => {
            Box::new(Immediate::new(fixture.compiled(page.entry), page, app, gpu))
        }
        Gpu::Retained(gpu) => Box::new(Retained::new(fixture.config(), page, app, gpu)),
    };
    driver.place_pointer();
    for _ in 0..WARMUP {
        driver.frame();
    }
    let picture_before = driver.picture();
    let reading_before = driver.reading(page.moving);
    let tally = frames(driver.as_mut(), page, run, fenced);
    let pixels = driver.pixels();
    Outcome {
        page,
        run,
        tally,
        expected,
        picture: [picture_before, driver.picture()],
        drawn_as: driver.drawn_as(),
        reading: [reading_before, driver.reading(page.moving)],
        painted: painted(&pixels),
        published: driver.published(),
        fenced,
    }
}

fn frames(driver: &mut dyn PageHost, page: &Page, run: Run, fence: bool) -> Tally {
    let mut tally = Tally::default();
    let dragging = run.moves() > 0;
    if dragging {
        driver.press();
    }
    for index in 0..page.frames {
        let forward = (index / REVERSAL).is_multiple_of(2);
        let lines = if forward { -1.0 } else { 1.0 };
        for _ in 0..run.wheels() {
            driver.wheel(lines);
        }
        let step = if forward { DRAG_STEP } else { -DRAG_STEP };
        for _ in 0..run.moves() {
            driver.move_by(step);
        }
        let started = WallClock::now();
        let census = if fence {
            driver.fenced_frame()
        } else {
            driver.frame()
        };
        tally.push(census, started.elapsed());
    }
    if dragging {
        driver.release();
    }
    tally
}

/// One stable line per run, with the counted facts beside the timed ones.
fn report(label: &str, outcome: &Outcome<'_>) {
    let tally = &outcome.tally;
    let [total, mean, min, max] = tally.micros();
    let scene = tally.scene.map_or_else(
        || "scene -".to_owned(),
        |scene| {
            format!(
                "tags {} path {} draw {} xform {}",
                scene.draw_tags, scene.path_data, scene.draw_data, scene.transforms
            )
        },
    );
    let natives = tally.natives.map_or_else(
        || "natives -".to_owned(),
        |natives| {
            format!(
                "vis {}/{} shaders {}/{}",
                natives.vis, outcome.expected.vis, natives.shaders, outcome.expected.shaders
            )
        },
    );
    println!(
        "{label} {:<17} {:<14} {:<6} frames {:>3} wanted {:>3} published {:>3}  {scene}  \
         {natives}  pool miss {} home {} steal {} drop {}  picture {:016x}->{:016x}  \
         us total {total} mean {mean} min {min} max {max}",
        outcome.page.name,
        outcome.run.label(),
        if outcome.fenced { "fenced" } else { "free" },
        tally.frames,
        tally.scheduled,
        outcome.published,
        tally.pool.alloc_misses,
        tally.pool.home_hits,
        tally.pool.steal_hits,
        tally.pool.put_drops,
        outcome.picture[0],
        outcome.picture[1],
    );
    if outcome.fenced {
        println!(
            "{label} {:<17} {:<14} fenced: the queue was serialised, so these totals are their \
             own scenario and may not be added to a free total",
            outcome.page.name,
            outcome.run.label(),
        );
    }
}

/// Every assertion this harness makes. Each names the thing a run claims to
/// measure; none of them is about a duration.
fn prove(label: &str, outcome: &Outcome<'_>, baseline: Option<u64>) {
    let page = outcome.page;
    let run = outcome.run.label();
    let tally = &outcome.tally;
    assert_eq!(
        tally.frames, page.frames,
        "{label} {}: the run measured a different number of frames than it scheduled",
        page.name
    );
    assert!(
        outcome.painted,
        "{label} {} {run}: the run rasterised one flat colour, so nothing was drawn to measure",
        page.name
    );
    if let Some(scene) = tally.scene {
        assert!(
            scene.draw_tags > 0,
            "{label} {} {run}: the retained scene carries no draw tags, so the frame drew nothing",
            page.name
        );
    }
    if let Some(natives) = tally.natives {
        assert!(
            !tally.natives_varied,
            "{label} {} {run}: the number of native draws changed between frames, so one page was \
             measured as two",
            page.name
        );
        assert_eq!(
            natives, outcome.expected,
            "{label} {} {run}: the retained host declared {} vis and {} shader draws for a page \
             whose document names {} and {}",
            page.name, natives.vis, natives.shaders, outcome.expected.vis, outcome.expected.shaders
        );
    }
    // A dragged page's reading is still until a hand is on it, so its undriven
    // run is a baseline rather than a failed animation.
    let driven = matches!(page.program, Program::Drag(_));
    if page.moving.is_some() && (!driven || outcome.run.moves() > 0) {
        assert_ne!(
            outcome.reading[0],
            outcome.reading[1],
            "{label} {} {run}: the reading this run is supposed to move never moved, so what was \
             measured is a still picture{}",
            page.name,
            if driven {
                " - the pointer point missed the control"
            } else {
                ""
            }
        );
    }
    match outcome.run {
        // The wheel-less run is the baseline the others are read against, and
        // whether its own picture moved is evidence about the page, not a
        // failure: a page with a caret or a visualiser moves on its own.
        Run::Wheels(0) => println!(
            "{label} {}: with no wheel the page's {} {}",
            page.name,
            outcome.drawn_as,
            if outcome.picture[0] == outcome.picture[1] {
                "stood still"
            } else {
                "moved anyway, so it animates without input"
            }
        ),
        Run::Wheels(count) => {
            let Some(baseline) = baseline else {
                panic!(
                    "{label} {}: no wheel-less run to read {count} wheels against",
                    page.name
                )
            };
            assert_ne!(
                outcome.picture[1], baseline,
                "{label} {} {run}: after the same number of frames the page looks exactly as it \
                 does with no wheel at all, so nothing scrolled",
                page.name
            );
        }
        // Same shape as the wheel: the undriven run is the control, and whether
        // it moved on its own is evidence rather than a failure.
        Run::Moves(0) => println!(
            "{label} {}: with no drag the page's {} {}",
            page.name,
            outcome.drawn_as,
            if outcome.picture[0] == outcome.picture[1] {
                "stood still"
            } else {
                "moved anyway, so it animates without input"
            }
        ),
        Run::Moves(count) => {
            let Some(baseline) = baseline else {
                panic!(
                    "{label} {}: no undriven run to read {count} moves against",
                    page.name
                )
            };
            assert_ne!(
                outcome.picture[1], baseline,
                "{label} {} {run}: after the same number of frames the page looks exactly as it \
                 does with no drag at all, so nothing was dragged",
                page.name
            );
        }
        Run::Buckets(_) | Run::Plain => {}
    }
}

/// Pins the measured table to the gallery's own list of pages.
///
/// A page that quietly left the table is not reported fast, it is not reported
/// at all. Which of the gallery's pages are worth measuring is a decision, so
/// the unmeasured ones are printed rather than asserted away; what is asserted
/// is that every measured page is one the gallery really has, or the harness's
/// own, and that it compiles.
#[kithara::test]
fn the_page_table_is_pinned_to_the_gallery() {
    let fixture = Fixture::new();
    let mut reads = MockReads::default();
    // `Tab::Modules` and the first module demo name the same document, and the
    // gallery reaches a demo through the application's own selection rather
    // than through the tab.
    let mut gallery: Vec<(usize, &'static str)> = Tab::ALL
        .iter()
        .filter(|tab| **tab != Tab::Modules)
        .map(|tab| (tab.index(), tab.entry()))
        .collect();
    gallery.extend(ModuleDemo::ALL.iter().map(|demo| {
        reads.select_module(*demo);
        (Tab::ALL.len() + demo.index(), reads.active_module().entry())
    }));
    gallery.sort_unstable();

    let unmeasured: Vec<&str> = gallery
        .iter()
        .map(|(_, entry)| *entry)
        .filter(|entry| !PAGES.iter().any(|page| page.entry == *entry))
        .collect();
    println!(
        "page_perf measures {} of the gallery's {} pages; unmeasured: {unmeasured:?}",
        gallery.len() - unmeasured.len(),
        gallery.len()
    );

    for page in PAGES {
        assert!(
            page.entry == SCROLL_VIS_LAYOUT
                || gallery.iter().any(|(_, entry)| *entry == page.entry),
            "{} names {}, which is neither a gallery page nor this harness's own",
            page.name,
            page.entry
        );
        // The gallery's reads only advance on the tab they belong to, so a
        // page mounted under the wrong one is measured as a still picture.
        let app = PageApp::new(page, Run::Plain);
        assert_eq!(
            app.reads.active_tab(),
            page.tab,
            "{} is measured under a tab whose readings it does not move",
            page.name
        );
        let natives = leaves(&fixture.compiled(page.entry));
        println!(
            "page_perf {:<17} vis {} shader {}",
            page.name, natives.vis, natives.shaders
        );
    }
}
