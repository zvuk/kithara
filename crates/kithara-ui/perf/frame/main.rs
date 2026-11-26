#![cfg(all(feature = "perf", feature = "iced", feature = "masonry"))]

mod fixture;
mod immediate;
mod retained;
mod scenarios;

use std::{borrow::Cow, rc::Rc, sync::LazyLock};

use criterion::{BenchmarkId, Criterion};
use futures_lite::future::block_on;
use iced::{
    Pixels, Theme,
    advanced::graphics::{Shell, text::font_system},
    theme::Palette,
};
use iced_renderer::fallback::Renderer as FallbackRenderer;
use iced_tiny_skia::Renderer as TinySkiaRenderer;
use iced_wgpu::{
    Engine, Renderer as WgpuRenderer,
    wgpu::{
        Backends, DeviceDescriptor, Instance, InstanceDescriptor, RequestAdapterOptions,
        TextureFormat,
    },
};
use kithara_ui::{
    builtin,
    compile::CompiledUi,
    draw::{Pt, Rect},
    expand::ControlSpec,
    ids::SourceUri,
    interact::{Input, MOUSE, PointerInput, PointerPhase},
    render::{
        Skin,
        fonts::{FONT_BYTES, SANS},
    },
    shaping::FontPolicy,
};

use crate::{
    fixture::{CensusReads, Fixture},
    immediate::{Immediate, laid_out_rect},
    retained::Retained,
    scenarios::consts,
};

mod types;
use types::{Interaction, Scenario, Step};

/// One frame's worth of work on one host, from the input applied to the draw
/// output produced. Nothing here rasterises: the measurement stops at the
/// commands, so a GPU on the machine cannot change the number.
trait FrameHost {
    /// Produces one frame and returns a size taken from its output, so the
    /// frame cannot be optimised away. The unit is the host's own: see
    /// [`Host::unit`].
    fn frame(&mut self) -> usize;

    /// Applies one neutral step in this host's own event vocabulary.
    fn interact(&mut self, step: Step);

    /// What the control did with the gesture, in whatever this host can see: a
    /// captured event, a published document event. Reported, never judged - a
    /// control may legitimately take a gesture and publish nothing.
    fn receipts(&self) -> usize;

    /// Whether this host asked for the frame [`Self::frame`] has just produced.
    ///
    /// A host that can decline to draw is a host whose lag is a count of
    /// frames, not a cost per frame, and the two hosts differ here.
    fn scheduled(&self) -> bool;
}

#[derive(Clone, Copy)]
enum Host {
    Immediate,
    Retained,
}

/// The device is boxed because the other variant carries nothing: a wgpu
/// `Engine` is a few hundred bytes, and every `Backend` the retained cases
/// build would otherwise be sized for a device it never opens.
enum Backend {
    Immediate(Box<Engine>),
    Retained,
}

/// Everything a host needs before it can be built. The immediate host draws
/// through wgpu, which needs a device; the retained host encodes a scene and
/// needs nothing.
impl TryFrom<Host> for Backend {
    type Error = String;

    fn try_from(host: Host) -> Result<Self, String> {
        match host {
            Host::Immediate => engine().map(Box::new).map(Self::Immediate),
            Host::Retained => Ok(Self::Retained),
        }
    }
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
            &builtin::resolver(),
            FontPolicy::Embedded,
        )
        .unwrap_or_else(|error| panic!("the frame-perf skin must resolve: {error}"))
    });
    &SKIN
}

fn theme(skin: &Skin) -> Theme {
    let palette = &skin.palette;
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
        .filter(|kind| {
            !consts::SCENARIOS
                .iter()
                .any(|scenario| scenario.name == *kind)
        })
        .collect()
}

fn unnamed_by_the_document() -> Vec<&'static str> {
    consts::SCENARIOS
        .iter()
        .map(|scenario| scenario.name)
        .filter(|name| !ControlSpec::KINDS.contains(name))
        .collect()
}

/// What one control's frame cost, and everything the harness had to be sure of
/// before that cost means anything.
struct Reading {
    name: &'static str,
    rect: Rect,
    points: Vec<Pt>,
    gestured: bool,
    scheduled: bool,
    receipts: usize,
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

/// Geometry alone cannot prove a gesture landed. A host with neither receipts nor
/// a scheduled frame measured an idle redraw instead of the declared gesture.
fn contains(rect: Rect, at: Pt) -> bool {
    at.x >= rect.x && at.x <= rect.x + rect.w && at.y >= rect.y && at.y <= rect.y + rect.h
}

fn miss(reading: &Reading) -> Option<String> {
    let rect = reading.rect;
    let viewport = Rect {
        x: 0.0,
        y: 0.0,
        w: f32::from(consts::WIDTH),
        h: f32::from(consts::HEIGHT),
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
        return Some(format!(
            "{at:?} falls outside the {}x{} fixture",
            consts::WIDTH,
            consts::HEIGHT
        ));
    }
    (reading.gestured && reading.receipts == 0 && !reading.scheduled)
        .then(|| format!("the host saw nothing at {:?} in {rect:?}", reading.points))
}

struct FrameWork {
    action: Box<dyn FnMut()>,
    driver: Box<dyn FrameHost>,
    rect: Rect,
    name: &'static str,
    steps: Vec<Step>,
    gestured: bool,
}

impl FrameWork {
    fn run(&mut self) {
        (self.action)();
    }

    fn teardown(&mut self) {
        let reading = Reading {
            rect: self.rect,
            gestured: self.gestured,
            name: self.name,
            points: self
                .steps
                .iter()
                .filter_map(|step| match step {
                    Step::Move(at) => Some(*at),
                    Step::Press | Step::Release | Step::Data => None,
                })
                .collect(),
            receipts: self.driver.receipts(),
            scheduled: self.driver.scheduled(),
        };
        audit("frame", std::slice::from_ref(&reading));
    }
}

thread_local! {
    static BACKENDS: OnceCell<Box<Backend>> = OnceCell::new();
    static FIXTURES: std::collections::BTreeMap<usize, Box<Fixture>> =
        std::collections::BTreeMap::new();
}

fn shared_backend(host: Host) -> &'static Backend {
    BACKENDS.with(|backend| {
        backend.get_or_init(|| {
            Box::new(
                Backend::try_from(host)
                    .unwrap_or_else(|error| panic!("no host to measure on: {error}")),
            )
        })
    })
}

fn shared_fixture(index: usize, scenario: &Scenario) -> &'static Fixture {
    FIXTURES.with(|fixtures| {
        fixtures
            .entry(index)
            .or_insert_with(|| Box::new(Fixture::new(scenario.control)))
    })
}

fn prepare_frame(index: usize) -> FrameWork {
    load_fonts();
    let host = if index < consts::SCENARIOS.len() {
        Host::Immediate
    } else {
        Host::Retained
    };
    let scenario = &consts::SCENARIOS[index % consts::SCENARIOS.len()];
    let label = if matches!(host, Host::Immediate) {
        "iced"
    } else {
        "vello"
    };
    let backend = shared_backend(host);
    let fixture = shared_fixture(index, scenario);
    let reads = Rc::new(CensusReads::default());
    let rect = laid_out_rect(fixture, &reads, &backend.geometry());
    let mut driver = driver(&backend, fixture, Rc::clone(&reads));
    let _warm = driver.frame();
    let steps = scenario.interaction.steps(rect);
    let action_steps = steps.clone();
    let action = Box::new(move || {
        for step in &action_steps {
            driver.interact(*step);
        }
        let _frame = driver.frame();
    });
    FrameWork {
        action,
        driver,
        rect,
        name: scenario.name,
        steps,
        gestured: matches!(
            scenario.interaction,
            Interaction::Drag | Interaction::Press | Interaction::PressLeading
        ),
    }
}

fn teardown_frame(mut work: FrameWork) {
    work.teardown();
}

#[metabench::benchmark(FRAME_PERF, "frame", "case", gungraun_setup = prepare_frame, gungraun_teardown = teardown_frame)]
#[bench::iced_brand(0_usize)]
#[bench::iced_spacer(1_usize)]
#[bench::iced_divider(2_usize)]
#[bench::iced_presetselector(3_usize)]
#[bench::iced_settingsbutton(4_usize)]
#[bench::iced_decksummary(5_usize)]
#[bench::iced_windowdrag(6_usize)]
#[bench::iced_titlebar(7_usize)]
#[bench::iced_windowcontrols(8_usize)]
#[bench::iced_bpm(9_usize)]
#[bench::iced_time(10_usize)]
#[bench::iced_scalar(11_usize)]
#[bench::iced_wave(12_usize)]
#[bench::iced_vis(13_usize)]
#[bench::iced_shader(14_usize)]
#[bench::iced_custom(15_usize)]
#[bench::iced_table(16_usize)]
#[bench::iced_tree(17_usize)]
#[bench::iced_search(18_usize)]
#[bench::iced_contextbar(19_usize)]
#[bench::iced_text(20_usize)]
#[bench::iced_knob(21_usize)]
#[bench::iced_chip(22_usize)]
#[bench::iced_navitem(23_usize)]
#[bench::iced_button(24_usize)]
#[bench::iced_glyph(25_usize)]
#[bench::iced_tablarge(26_usize)]
#[bench::iced_toggle(27_usize)]
#[bench::iced_checkbox(28_usize)]
#[bench::iced_segmented(29_usize)]
#[bench::iced_select(30_usize)]
#[bench::iced_statusdot(31_usize)]
#[bench::iced_swatch(32_usize)]
#[bench::iced_cell(33_usize)]
#[bench::iced_readout(34_usize)]
#[bench::iced_meter(35_usize)]
#[bench::iced_vuvertical(36_usize)]
#[bench::iced_vustereo(37_usize)]
#[bench::iced_fader(38_usize)]
#[bench::iced_crossfader(39_usize)]
#[bench::iced_portalmap(40_usize)]
#[bench::iced_range(41_usize)]
#[bench::iced_sprite(42_usize)]
#[bench::iced_lottie(43_usize)]
#[bench::vello_brand(44_usize)]
#[bench::vello_spacer(45_usize)]
#[bench::vello_divider(46_usize)]
#[bench::vello_presetselector(47_usize)]
#[bench::vello_settingsbutton(48_usize)]
#[bench::vello_decksummary(49_usize)]
#[bench::vello_windowdrag(50_usize)]
#[bench::vello_titlebar(51_usize)]
#[bench::vello_windowcontrols(52_usize)]
#[bench::vello_bpm(53_usize)]
#[bench::vello_time(54_usize)]
#[bench::vello_scalar(55_usize)]
#[bench::vello_wave(56_usize)]
#[bench::vello_vis(57_usize)]
#[bench::vello_shader(58_usize)]
#[bench::vello_custom(59_usize)]
#[bench::vello_table(60_usize)]
#[bench::vello_tree(61_usize)]
#[bench::vello_search(62_usize)]
#[bench::vello_contextbar(63_usize)]
#[bench::vello_text(64_usize)]
#[bench::vello_knob(65_usize)]
#[bench::vello_chip(66_usize)]
#[bench::vello_navitem(67_usize)]
#[bench::vello_button(68_usize)]
#[bench::vello_glyph(69_usize)]
#[bench::vello_tablarge(70_usize)]
#[bench::vello_toggle(71_usize)]
#[bench::vello_checkbox(72_usize)]
#[bench::vello_segmented(73_usize)]
#[bench::vello_select(74_usize)]
#[bench::vello_statusdot(75_usize)]
#[bench::vello_swatch(76_usize)]
#[bench::vello_cell(77_usize)]
#[bench::vello_readout(78_usize)]
#[bench::vello_meter(79_usize)]
#[bench::vello_vuvertical(80_usize)]
#[bench::vello_vustereo(81_usize)]
#[bench::vello_fader(82_usize)]
#[bench::vello_crossfader(83_usize)]
#[bench::vello_portalmap(84_usize)]
#[bench::vello_range(85_usize)]
#[bench::vello_sprite(86_usize)]
#[bench::vello_lottie(87_usize)]
fn native_frame(mut work: FrameWork) -> FrameWork {
    work.run();
    work
}

fn criterion_benchmarks(c: &mut Criterion) {
    assert!(missing_from_table().is_empty());
    assert!(unnamed_by_the_document().is_empty());
    let mut group = c.benchmark_group("frame");
    group.sample_size(10);
    for index in 0..(2 * consts::SCENARIOS.len()) {
        let host = if index < consts::SCENARIOS.len() {
            "iced"
        } else {
            "vello"
        };
        let scenario = &consts::SCENARIOS[index % consts::SCENARIOS.len()];
        group.bench_function(
            BenchmarkId::new(
                "case",
                format!("{host}_{}", scenario.name.to_ascii_lowercase()),
            ),
            |b| {
                let mut work = prepare_frame(index);
                b.iter(|| work.run());
                work.teardown();
            },
        );
    }
    group.finish();
}

metabench::main!(
    criterion = criterion_benchmarks,
    benchmarks = [FRAME_PERF],
    allocator = std::alloc::System,
);
