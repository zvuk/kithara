mod app;
mod census;
mod gpu;
mod immediate;
mod pages;
mod retained;

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
use kithara_ui_gallery::{demo, fixture, sections};

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
