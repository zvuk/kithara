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
use kithara_test_utils::kithara;
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

impl Interaction {
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

    fn steps(self, rect: Rect) -> Vec<Step> {
        let at = |fraction| Step::Move(inside(rect, fraction));
        match self {
            Self::Drag => vec![
                at(Self::START),
                Step::Press,
                at(Self::MIDDLE),
                at(Self::END),
                Step::Release,
            ],
            Self::Press => vec![at(Self::MIDDLE), Step::Press, Step::Release],
            Self::PressLeading => vec![at(Self::LEADING), Step::Press, Step::Release],
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
    control: &'static str,
    name: &'static str,
    interaction: Interaction,
}

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
    action: Box<dyn FnMut() -> Reading>,
    reading: Option<Reading>,
}

impl FrameWork {
    fn run(&mut self) {
        self.reading = Some((self.action)());
    }

    fn teardown(&mut self) {
        audit(
            "frame",
            std::slice::from_ref(self.reading.as_ref().expect("frame reading")),
        );
    }
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
    let backend = Backend::try_from(host)
        .unwrap_or_else(|error| panic!("{label}: NOT MEASURED, no host to measure it on: {error}"));
    // A driver borrows its fixture. Keep the one fixture for this case alive
    // for the rest of the process, outside the measured frame and allocation window.
    let fixture = Box::leak(Box::new(Fixture::new(scenario.control)));
    let reads = Rc::new(CensusReads::default());
    let rect = laid_out_rect(fixture, &reads, &backend.geometry());
    let mut driver = driver(&backend, fixture, Rc::clone(&reads));
    let _warm = driver.frame();
    let steps = scenario.interaction.steps(rect);
    let action = Box::new(move || {
        for step in &steps {
            driver.interact(*step);
        }
        let _frame = driver.frame();
        Reading {
            rect,
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
            scheduled: driver.scheduled(),
        }
    });
    FrameWork {
        action,
        reading: None,
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
#[bench::iced_crossfader(38_usize)]
#[bench::iced_fader(39_usize)]
#[bench::vello_brand(40_usize)]
#[bench::vello_spacer(41_usize)]
#[bench::vello_divider(42_usize)]
#[bench::vello_presetselector(43_usize)]
#[bench::vello_settingsbutton(44_usize)]
#[bench::vello_decksummary(45_usize)]
#[bench::vello_windowdrag(46_usize)]
#[bench::vello_titlebar(47_usize)]
#[bench::vello_windowcontrols(48_usize)]
#[bench::vello_bpm(49_usize)]
#[bench::vello_time(50_usize)]
#[bench::vello_scalar(51_usize)]
#[bench::vello_wave(52_usize)]
#[bench::vello_vis(53_usize)]
#[bench::vello_shader(54_usize)]
#[bench::vello_custom(55_usize)]
#[bench::vello_table(56_usize)]
#[bench::vello_tree(57_usize)]
#[bench::vello_search(58_usize)]
#[bench::vello_contextbar(59_usize)]
#[bench::vello_text(60_usize)]
#[bench::vello_knob(61_usize)]
#[bench::vello_chip(62_usize)]
#[bench::vello_navitem(63_usize)]
#[bench::vello_button(64_usize)]
#[bench::vello_glyph(65_usize)]
#[bench::vello_tablarge(66_usize)]
#[bench::vello_toggle(67_usize)]
#[bench::vello_checkbox(68_usize)]
#[bench::vello_segmented(69_usize)]
#[bench::vello_select(70_usize)]
#[bench::vello_statusdot(71_usize)]
#[bench::vello_swatch(72_usize)]
#[bench::vello_cell(73_usize)]
#[bench::vello_readout(74_usize)]
#[bench::vello_meter(75_usize)]
#[bench::vello_vuvertical(76_usize)]
#[bench::vello_vustereo(77_usize)]
#[bench::vello_crossfader(78_usize)]
#[bench::vello_fader(79_usize)]
fn native_frame(mut work: FrameWork) -> FrameWork {
    work.run();
    work
}

fn criterion_benchmarks(c: &mut Criterion) {
    assert!(missing_from_table().is_empty());
    assert!(unnamed_by_the_document().is_empty());
    let mut group = c.benchmark_group("frame");
    group.sample_size(10);
    for index in 0..80 {
        let host = if index < 40 { "iced" } else { "vello" };
        let scenario = &consts::SCENARIOS[index % 40];
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
