mod app;
mod census;
mod gpu;
mod immediate;
mod pages;
mod retained;

use std::{
    borrow::Cow,
    cell::{OnceCell, RefCell},
    rc::Rc,
    sync::LazyLock,
};

use criterion::{BenchmarkId, Criterion};
use iced::{Theme, advanced::graphics::text::font_system, theme::Palette};
use kithara_platform::time::WallInstant as WallClock;
use kithara_ui::render::{Skin, fonts::FONT_BYTES};
use kithara_ui_gallery::{demo, fixture, sections};
use pages::{Page, Program};

use crate::{
    app::PageApp,
    census::{Census, Natives, Tally, leaves},
    gpu::{Gpu, ImmediateGpu, RetainedGpu, painted},
    immediate::Immediate,
    pages::{Fixture, consts},
    retained::Retained,
};

/// One run per sweep step, each named by the value it swept to.
impl Page {
    fn open(&self, ui: &mut Ui<'_, PageApp>) {
        if self.own.is_some() {
            return;
        }
        ui.stand(sections::PAGE, self.tab).unwrap_or_else(|error| {
            panic!("the page-perf fixture must open {}: {error}", self.name)
        });
    }
}

impl From<Program> for Vec<Run> {
    fn from(program: Program) -> Self {
        match program {
            Program::Idle | Program::Tick => vec![Run::Plain],
            Program::Buckets(counts) => counts.iter().copied().map(Run::Buckets).collect(),
            Program::Wheels(counts) => counts.iter().copied().map(Run::Wheels).collect(),
            Program::Drag(counts) => counts.iter().copied().map(Run::Moves).collect(),
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

    const fn moves(self) -> usize {
        match self {
            Self::Plain | Self::Buckets(_) | Self::Wheels(_) => 0,
            Self::Moves(count) => count,
        }
    }

    const fn wheels(self) -> usize {
        match self {
            Self::Plain | Self::Buckets(_) | Self::Moves(_) => 0,
            Self::Wheels(count) => count,
        }
    }
}

trait PageHost {
    fn drawn_as(&self) -> &'static str;
    fn fenced_frame(&mut self) -> Census;
    fn frame(&mut self) -> Census;
    fn move_by(&mut self, dx: f32) -> f32;
    fn picture(&mut self) -> u64;
    fn pixels(&mut self) -> Vec<u8>;
    fn place_pointer(&mut self);
    fn press(&mut self);
    fn reading(&self, endpoint: Option<&str>) -> Option<u64>;
    fn release(&mut self);
    fn wheel(&mut self, lines: f32);
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

struct Outcome<'run> {
    page: &'run Page,
    drawn_as: &'static str,
    expected: Natives,
    run: Run,
    tally: Tally,
    reading: [Option<u64>; 2],
    picture: [u64; 2],
    painted: bool,
}

impl TryFrom<Host> for Gpu {
    type Error = String;

    fn try_from(host: Host) -> Result<Self, String> {
        match host {
            Host::Immediate => ImmediateGpu::new().map(Box::new).map(Self::Immediate),
            Host::Retained => RetainedGpu::new().map(Box::new).map(Self::Retained),
        }
    }
}

/// The two hosts a gallery page is measured on, each driven with the input
/// its symptom names and rasterised every frame through the passes the
/// application uses: `ShaderPass` and `VisPass` around the Vello scene on the
/// retained host, and iced's own shader primitive prepare and draw, which only
/// run at present, on the immediate one.
#[derive(Clone, Copy)]
enum Host {
    Immediate,
    Retained,
}

/// One run of one page: warm up, fingerprint, measure under a guard,
/// fingerprint again.
///
/// Nothing here judges. Every number is printed; the assertions are only that a
/// run did the thing it claims to measure, because a device-less, wheel-less or
/// motionless run otherwise reports a beautiful number for nothing.
fn measure(
    fixture: Rc<Fixture>,
    page: &'static Page,
    run: Run,
    expected: Natives,
    gpu: Rc<RefCell<Gpu>>,
    fenced: bool,
) -> Outcome<'static> {
    /// Frames discarded before measuring. The first carries the mount, the layout,
    /// the shaping caches and every pipeline either host compiles lazily.
    const WARMUP: usize = 3;

    let mut driver = page_driver(fixture, page, run, gpu);
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
    }
}

/// Each frame is timed by `WallInstant`, the one clock the platform crate leaves
/// real on both lanes: `Instant` is virtual under flash, and a virtual clock
/// reports a frame that took no time.
fn frames(driver: &mut dyn PageHost, page: &Page, run: Run, fence: bool) -> Tally {
    /// How many frames one direction lasts, for a wheel and for a drag alike. Both
    /// run into an end and stop consuming there, which is a different code path: a
    /// monotonic scroll measures a clamped no-op after about six frames, and a
    /// monotonic drag measures a fader pinned at one end of its rail.
    const REVERSAL: usize = 20;

    /// How far one pointer move carries a drag, in page points. Small enough that a
    /// frame's worth of moves stays on the rail at every sweep step, and large
    /// enough that each one lands on a different pixel of it.
    const DRAG_STEP: f32 = 1.5;

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

/// Check the declared frame, input, motion and completion evidence.
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
fn the_page_table_is_pinned_to_the_gallery() {
    let fixture = Fixture::default();
    let tabs = sections::pages();
    // The modules page turns a second state of its own, so its demos are pages
    // of the gallery as much as the tabs are, and are counted with them.
    let demos = sections::modules();
    let unmeasured: Vec<&str> = tabs
        .iter()
        .chain(demos)
        .copied()
        .filter(|tab| {
            !consts::PAGES
                .iter()
                .any(|page| page.own.is_none() && page.tab == *tab)
        })
        .collect();
    let offered = tabs.len() + demos.len();
    println!(
        "page_perf measures {} of the gallery's {offered} pages; unmeasured: {unmeasured:?}",
        offered - unmeasured.len()
    );

    for page in consts::PAGES {
        assert!(
            page.own.is_some() || tabs.contains(&page.tab),
            "{} names {}, which is neither a page of the gallery's screen nor this harness's own",
            page.name,
            page.tab
        );
        let natives = leaves(&fixture.compiled(page));
        println!(
            "page_perf {:<17} vis {} shader {}",
            page.name, natives.vis, natives.shaders
        );
    }
}

fn gallery_case_count() -> usize {
    consts::PAGES
        .iter()
        .map(|page| {
            usize::from(Vec::<Run>::from(page.program).len()) * 2 * if page.fenced { 2 } else { 1 }
        })
        .sum()
}

fn gallery_case(mut index: usize) -> (usize, Host, Run, bool) {
    for (page_index, page) in consts::PAGES.iter().enumerate() {
        for host in [Host::Immediate, Host::Retained] {
            for run in Vec::<Run>::from(page.program) {
                for fenced in [false, true] {
                    if fenced && !page.fenced {
                        continue;
                    }
                    if index == 0 {
                        return (page_index, host, run, fenced);
                    }
                    index -= 1;
                }
            }
        }
    }
    panic!("unknown gallery case {index}")
}

struct GalleryWork {
    driver: Box<dyn PageHost>,
    host: Host,
    page: &'static Page,
    run: Run,
    fenced: bool,
    expected: Natives,
    picture_before: u64,
    reading_before: Option<u64>,
    tally: Option<Tally>,
}

impl GalleryWork {
    fn run(&mut self) {
        self.tally = Some(frames(
            self.driver.as_mut(),
            self.page,
            self.run,
            self.fenced,
        ));
    }

    fn teardown(&mut self) {
        let outcome = Outcome {
            page: self.page,
            run: self.run,
            tally: self.tally.take().expect("gallery tally"),
            expected: self.expected,
            picture: [self.picture_before, self.driver.picture()],
            drawn_as: self.driver.drawn_as(),
            reading: [self.reading_before, self.driver.reading(self.page.moving)],
            painted: painted(&self.driver.pixels()),
        };
        let baseline = match self.run {
            Run::Wheels(count) if count > 0 => Some(
                measure(
                    Rc::new(Fixture::default()),
                    self.page,
                    Run::Wheels(0),
                    self.expected,
                    shared_gpu(self.host),
                    false,
                )
                .picture[1],
            ),
            Run::Moves(count) if count > 0 => Some(
                measure(
                    Rc::new(Fixture::default()),
                    self.page,
                    Run::Moves(0),
                    self.expected,
                    shared_gpu(self.host),
                    false,
                )
                .picture[1],
            ),
            _ => None,
        };
        prove(self.page.name, &outcome, baseline);
    }
}

thread_local! {
    static ICED_GPU: OnceCell<Rc<RefCell<Gpu>>> = OnceCell::new();
    static VELLO_GPU: OnceCell<Rc<RefCell<Gpu>>> = OnceCell::new();
}

fn shared_gpu(host: Host) -> Rc<RefCell<Gpu>> {
    let cell = match host {
        Host::Immediate => &ICED_GPU,
        Host::Retained => &VELLO_GPU,
    };
    cell.with(|gpu| {
        Rc::clone(gpu.get_or_init(|| {
            Rc::new(RefCell::new(Gpu::try_from(host).unwrap_or_else(|error| {
                panic!("no host to measure on: {error}")
            })))
        }))
    })
}

fn page_driver(
    fixture: Rc<Fixture>,
    page: &'static Page,
    run: Run,
    gpu: Rc<RefCell<Gpu>>,
) -> Box<dyn PageHost> {
    let app = PageApp::new(page, run);
    let mut borrowed = gpu.borrow_mut();
    match &mut *borrowed {
        Gpu::Immediate(gpu) => Box::new(Immediate::new(fixture.compiled(page), page, app, gpu)),
        Gpu::Retained(_) => Box::new(Retained::new(fixture, page, app, Rc::clone(&gpu))),
    }
}

fn prepare_gallery(index: usize) -> GalleryWork {
    load_fonts();
    let (page_index, host, run, fenced) = gallery_case(index);
    let page = &consts::PAGES[page_index];
    let fixture = Rc::new(Fixture::default());
    let gpu = shared_gpu(host);
    let expected = leaves(&fixture.compiled(page));
    let mut driver = page_driver(fixture, page, run, gpu);
    driver.place_pointer();
    for _ in 0..3 {
        driver.frame();
    }
    let picture_before = driver.picture();
    let reading_before = driver.reading(page.moving);
    GalleryWork {
        driver,
        host,
        page,
        run,
        fenced,
        expected,
        picture_before,
        reading_before,
        tally: None,
    }
}

fn teardown_gallery(mut work: GalleryWork) {
    work.teardown();
}

#[metabench::benchmark(GALLERY_PERF, "gallery", "case", gungraun_setup = prepare_gallery, gungraun_teardown = teardown_gallery)]
#[bench::iced_gallery_buttons_plain(0_usize)]
#[bench::vello_gallery_buttons_plain(1_usize)]
#[bench::iced_gallery_clock_plain(2_usize)]
#[bench::vello_gallery_clock_plain(3_usize)]
#[bench::iced_gallery_table_plain(4_usize)]
#[bench::vello_gallery_table_plain(5_usize)]
#[bench::iced_gallery_stress_buckets_8192(6_usize)]
#[bench::iced_gallery_stress_buckets_4096(7_usize)]
#[bench::iced_gallery_stress_buckets_1024(8_usize)]
#[bench::iced_gallery_stress_buckets_256(9_usize)]
#[bench::vello_gallery_stress_buckets_8192(10_usize)]
#[bench::vello_gallery_stress_buckets_4096(11_usize)]
#[bench::vello_gallery_stress_buckets_1024(12_usize)]
#[bench::vello_gallery_stress_buckets_256(13_usize)]
#[bench::iced_gallery_vis_plain(14_usize)]
#[bench::iced_gallery_vis_plain_fenced(15_usize)]
#[bench::vello_gallery_vis_plain(16_usize)]
#[bench::vello_gallery_vis_plain_fenced(17_usize)]
#[bench::iced_gallery_shader_plain(18_usize)]
#[bench::iced_gallery_shader_plain_fenced(19_usize)]
#[bench::vello_gallery_shader_plain(20_usize)]
#[bench::vello_gallery_shader_plain_fenced(21_usize)]
#[bench::iced_gallery_pivot_wheels_0(22_usize)]
#[bench::iced_gallery_pivot_wheels_1(23_usize)]
#[bench::iced_gallery_pivot_wheels_4(24_usize)]
#[bench::iced_gallery_pivot_wheels_8(25_usize)]
#[bench::vello_gallery_pivot_wheels_0(26_usize)]
#[bench::vello_gallery_pivot_wheels_1(27_usize)]
#[bench::vello_gallery_pivot_wheels_4(28_usize)]
#[bench::vello_gallery_pivot_wheels_8(29_usize)]
#[bench::iced_gallery_library2_wheels_0(30_usize)]
#[bench::iced_gallery_library2_wheels_1(31_usize)]
#[bench::iced_gallery_library2_wheels_4(32_usize)]
#[bench::iced_gallery_library2_wheels_8(33_usize)]
#[bench::vello_gallery_library2_wheels_0(34_usize)]
#[bench::vello_gallery_library2_wheels_1(35_usize)]
#[bench::vello_gallery_library2_wheels_4(36_usize)]
#[bench::vello_gallery_library2_wheels_8(37_usize)]
#[bench::iced_gallery_table_long_wheels_0(38_usize)]
#[bench::iced_gallery_table_long_wheels_1(39_usize)]
#[bench::iced_gallery_table_long_wheels_4(40_usize)]
#[bench::iced_gallery_table_long_wheels_8(41_usize)]
#[bench::vello_gallery_table_long_wheels_0(42_usize)]
#[bench::vello_gallery_table_long_wheels_1(43_usize)]
#[bench::vello_gallery_table_long_wheels_4(44_usize)]
#[bench::vello_gallery_table_long_wheels_8(45_usize)]
#[bench::iced_gallery_tree_wheels_0(46_usize)]
#[bench::iced_gallery_tree_wheels_1(47_usize)]
#[bench::iced_gallery_tree_wheels_4(48_usize)]
#[bench::iced_gallery_tree_wheels_8(49_usize)]
#[bench::vello_gallery_tree_wheels_0(50_usize)]
#[bench::vello_gallery_tree_wheels_1(51_usize)]
#[bench::vello_gallery_tree_wheels_4(52_usize)]
#[bench::vello_gallery_tree_wheels_8(53_usize)]
#[bench::iced_gallery_faders_moves_0(54_usize)]
#[bench::iced_gallery_faders_moves_1(55_usize)]
#[bench::iced_gallery_faders_moves_4(56_usize)]
#[bench::iced_gallery_faders_moves_8(57_usize)]
#[bench::vello_gallery_faders_moves_0(58_usize)]
#[bench::vello_gallery_faders_moves_1(59_usize)]
#[bench::vello_gallery_faders_moves_4(60_usize)]
#[bench::vello_gallery_faders_moves_8(61_usize)]
#[bench::iced_gallery_objects_plain(62_usize)]
#[bench::vello_gallery_objects_plain(63_usize)]
#[bench::iced_gallery_motion_plain(64_usize)]
#[bench::vello_gallery_motion_plain(65_usize)]
#[bench::iced_gallery_sprites_plain(66_usize)]
#[bench::vello_gallery_sprites_plain(67_usize)]
#[bench::iced_gallery_lottie_plain(68_usize)]
#[bench::vello_gallery_lottie_plain(69_usize)]
#[bench::iced_gallery_scene_plain(70_usize)]
#[bench::vello_gallery_scene_plain(71_usize)]
#[bench::iced_perf_scroll_vis_wheels_0(72_usize)]
#[bench::iced_perf_scroll_vis_wheels_1(73_usize)]
#[bench::iced_perf_scroll_vis_wheels_4(74_usize)]
#[bench::iced_perf_scroll_vis_wheels_8(75_usize)]
#[bench::vello_perf_scroll_vis_wheels_0(76_usize)]
#[bench::vello_perf_scroll_vis_wheels_1(77_usize)]
#[bench::vello_perf_scroll_vis_wheels_4(78_usize)]
#[bench::vello_perf_scroll_vis_wheels_8(79_usize)]
fn native_gallery(mut work: GalleryWork) -> GalleryWork {
    work.run();
    work
}

fn criterion_benchmarks(c: &mut Criterion) {
    the_page_table_is_pinned_to_the_gallery();
    let mut group = c.benchmark_group("gallery");
    group.sample_size(10);
    for index in 0..gallery_case_count() {
        let (page_index, host, run, fenced) = gallery_case(index);
        let page = &consts::PAGES[page_index];
        let host = match host {
            Host::Immediate => "iced",
            Host::Retained => "vello",
        };
        let run = match run {
            Run::Plain => "plain".to_owned(),
            Run::Buckets(count) => format!("buckets_{count}"),
            Run::Wheels(count) => format!("wheels_{count}"),
            Run::Moves(count) => format!("moves_{count}"),
        };
        let fenced = if fenced { "_fenced" } else { "" };
        let identity = format!("{host}_{}_{run}{fenced}", page.name).replace('-', "_");
        group.bench_function(BenchmarkId::new("case", identity), |b| {
            let mut work = prepare_gallery(index);
            b.iter(|| work.run());
            work.teardown();
        });
    }
    group.finish();
}

metabench::main!(
    criterion = criterion_benchmarks,
    benchmarks = [GALLERY_PERF],
    allocator = std::alloc::System,
);
