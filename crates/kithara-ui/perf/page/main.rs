#![cfg(all(feature = "perf", feature = "iced", feature = "masonry"))]

mod app;
mod census;
#[path = "../../examples/gallery/demo/mod.rs"]
mod demo;
#[path = "../../examples/gallery/fixture.rs"]
mod fixture;
mod gpu;
mod immediate;
mod pages;
mod retained;
#[path = "../../examples/gallery/sections.rs"]
mod sections;

use std::{borrow::Cow, sync::LazyLock};

use hotpath::HotpathGuardBuilder;
use iced::{Theme, advanced::graphics::text::font_system, theme::Palette};
use kithara_platform::time::WallInstant as WallClock;
use kithara_test_utils::kithara;
use kithara_ui::{
    app::Ui,
    draw::Pt,
    render::{Skin, fonts::FONT_BYTES},
    view::ViewState,
};

use crate::{
    app::PageApp,
    census::{Census, Natives, Tally, leaves},
    gpu::{Gpu, ImmediateGpu, RetainedGpu, painted},
    immediate::Immediate,
    pages::{Fixture, consts},
    retained::Retained,
};

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

/// One run per sweep step, each named by the value it swept to.
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
    /// The hotpath guard each host opens for this page. Stage labels inside are
    /// the host's, so the guard name is what makes a line `host.page.stage`.
    immediate_guard: &'static str,
    name: &'static str,
    retained_guard: &'static str,
    group: Group,
    /// The one reading this page moves on its own, if it has one. A run that
    /// claims to measure an animating page has to show that it animated.
    moving: Option<&'static str>,
    /// The document this page is the harness's own, when it is not one of the
    /// pages the gallery's screen offers.
    own: Option<&'static str>,
    /// Which demo state the page's tick advances. The gallery's reads move the
    /// stress waveforms only on the stress tab and the visualiser only on the
    /// vis one, so a page measured under the wrong tab is a still picture.
    tab: sections::Page,
    program: Program,
    /// Where a wheel is delivered, in logical page points. A wheel outside the
    /// viewport measures a page that was never scrolled.
    pointer_at: Pt,
    /// Whether this page also runs a fenced variant, which serialises the queue
    /// and whose totals may never be added to an unfenced frame total.
    fenced: bool,
    frames: usize,
}

impl Page {
    /// The document this page is read from: the gallery's one screen, unless
    /// the harness wrote a page of its own.
    fn document(&self) -> &'static str {
        self.own.unwrap_or_else(sections::entry)
    }

    /// Turns a mounted screen to this page. A page the harness wrote is the
    /// whole document, so there is nothing to turn.
    fn open(&self, ui: &mut Ui<'_, PageApp>) {
        if self.own.is_some() {
            return;
        }
        ui.stand(sections::PAGE, self.tab).unwrap_or_else(|error| {
            panic!("the page-perf fixture must open {}: {error}", self.name)
        });
    }

    /// The screen's own state standing at this page, which is how the one
    /// screen the gallery ships is opened at the page under measurement.
    fn standing(&self) -> ViewState {
        let mut view = ViewState::default();
        if self.own.is_none() {
            view.stand(sections::PAGE, self.tab);
        }
        view
    }
}

/// One frame's worth of work on one host, from the input applied to the pixels
/// produced. Both hosts rasterise: stopping at the recorded commands would
/// report times for the visualiser passes without running them.
trait PageHost {
    /// What [`PageHost::picture`] is a fingerprint of, in one word, for the
    /// lines that report it. A still scene on a page whose visualiser moves in
    /// a pass that never enters one is not a still page, and a line that said
    /// "picture" for both hosts would be claiming it is.
    fn drawn_as(&self) -> &'static str;

    /// The same, with the queue drained inside each pass. A fenced run
    /// serialises the GPU into the frame it belongs to; it is its own scenario
    /// and its totals may never be added to an unfenced total.
    fn fenced_frame(&mut self) -> Census;

    /// Produces and rasterises one frame.
    fn frame(&mut self) -> Census;

    /// Moves the pointer along the rail and reports where it landed, so the
    /// caller can turn it around at the ends rather than measure a clamp.
    fn move_by(&mut self, dx: f32) -> f32;

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

    /// The pixels of the last frame this host rasterised.
    fn pixels(&mut self) -> Vec<u8>;

    /// Puts the pointer where the wheel will be delivered, once, before the
    /// warm-up. A wheel is dispatched at wherever the host last saw the
    /// pointer, so a host never told scrolls the wrong thing, or nothing.
    fn place_pointer(&mut self);

    /// Puts the button down where the pointer is, for the whole run. A fader
    /// only tracks the pointer while it is held, so a drag measured without
    /// this measures a page that ignored every move.
    fn press(&mut self);

    /// How many events the mounted document published, cumulative.
    fn published(&self) -> usize;

    /// A fingerprint of the page's own moving reading, if the page named one.
    fn reading(&self, endpoint: Option<&str>) -> Option<u64>;

    /// Lets the button go, once, after the last measured frame.
    fn release(&mut self);

    /// Delivers one wheel detent at the page's pointer point.
    fn wheel(&mut self, lines: f32);
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
    /// Which artefact those two were taken from, for the lines that report them.
    drawn_as: &'static str,
    expected: Natives,
    run: Run,
    tally: Tally,
    /// The page's own moving reading, at the same two moments.
    reading: [Option<u64>; 2],
    /// The page as its host drew it, before the first measured frame and after
    /// the last.
    picture: [u64; 2],
    fenced: bool,
    /// Whether the last frame is more than one flat colour.
    painted: bool,
    published: usize,
}

/// The cheap-page control and the stress page: is the stress page's cost its
/// waveform buckets, or is it the page at all?
#[kithara::test]
#[case("iced", Host::Immediate, "gallery-buttons")]
#[case("iced", Host::Immediate, "gallery-clock")]
#[case("iced", Host::Immediate, "gallery-table")]
#[case("iced", Host::Immediate, "gallery-stress")]
#[case("vello", Host::Retained, "gallery-buttons")]
#[case("vello", Host::Retained, "gallery-clock")]
#[case("vello", Host::Retained, "gallery-table")]
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
#[case("iced", Host::Immediate, "gallery-scene")]
#[case("vello", Host::Retained, "gallery-objects")]
#[case("vello", Host::Retained, "gallery-motion")]
#[case("vello", Host::Retained, "gallery-sprites")]
#[case("vello", Host::Retained, "gallery-lottie")]
#[case("vello", Host::Retained, "gallery-scene")]
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
#[case("iced", Host::Immediate, "gallery-table-long")]
#[case("iced", Host::Immediate, "perf-scroll-vis")]
#[case("vello", Host::Retained, "gallery-pivot")]
#[case("vello", Host::Retained, "gallery-library2")]
#[case("vello", Host::Retained, "gallery-tree")]
#[case("vello", Host::Retained, "gallery-table-long")]
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
    let page = consts::PAGES
        .iter()
        .find(|page| page.name == name)
        .unwrap_or_else(|| panic!("{name} is not a measured page"));
    assert!(
        page.group == group,
        "{name} is measured by the route for another symptom"
    );
    let fixture = Fixture::default();
    let mut gpu = match Gpu::try_from(host) {
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
    let expected = leaves(&fixture.compiled(page));
    let mut outcomes = Vec::new();
    {
        let _guard = HotpathGuardBuilder::new(host.guard(page))
            .functions_limit(0)
            .build();
        for run in Vec::<Run>::from(page.program) {
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

impl Host {
    const fn guard(self, page: &Page) -> &'static str {
        match self {
            Self::Immediate => page.immediate_guard,
            Self::Retained => page.retained_guard,
        }
    }
}

/// One run of one page: warm up, fingerprint, measure under a guard,
/// fingerprint again.
///
/// Nothing here judges. Every number is printed; the assertions are only that a
/// run did the thing it claims to measure, because a device-less, wheel-less or
/// motionless run otherwise reports a beautiful number for nothing.
fn measure<'run>(
    fixture: &Fixture,
    page: &'run Page,
    run: Run,
    expected: Natives,
    gpu: &mut Gpu,
    fenced: bool,
) -> Outcome<'run> {
    /// Frames discarded before measuring. The first carries the mount, the layout,
    /// the shaping caches and every pipeline either host compiles lazily.
    const WARMUP: usize = 3;

    let app = PageApp::new(page, run);
    let mut driver: Box<dyn PageHost + '_> = match gpu {
        Gpu::Immediate(gpu) => Box::new(Immediate::new(fixture.compiled(page), page, app, gpu)),
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
