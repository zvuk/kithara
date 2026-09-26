use iced_wgpu::wgpu::TextureFormat;
use kithara_ui::{
    app::Config,
    builtin,
    compile::{CompiledUi, compile},
    draw::Pt,
    registry::EndpointRegistry,
    source::{MemResolver, OverlayResolver, UiConfig},
};

use crate::{Group, Page, Program, demo, fixture, sections};

/// The constants every host and page of this harness share.
pub(crate) mod consts {
    use super::{Group, Page, Program, Pt, TextureFormat};

    /// The format each host rasterises into. The retained one matches the gallery's
    /// own capture; iced's engine is built for a surface format, which is the sRGB
    /// pair.
    pub(crate) const IMMEDIATE_FORMAT: TextureFormat = TextureFormat::Rgba8UnormSrgb;
    pub(crate) const PAGES: &[Page] = &[
        Page {
            name: "gallery-buttons",
            group: Group::Pages,
            own: None,
            tab: "buttons",
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
            // the demo, so what it measures is the cost of the page standing still:
            // a deck overview, an elapsed time and a master clock whose popover the
            // demo opens by default, which is most of the page's tree.
            name: "gallery-clock",
            group: Group::Pages,
            own: None,
            tab: "clock",
            frames: 120,
            program: Program::Idle,
            moving: None,
            pointer_at: Pt { x: 700.0, y: 400.0 },
            fenced: false,
            immediate_guard: "iced.gallery-clock",
            retained_guard: "vello.gallery-clock",
        },
        Page {
            // The file table, reported to cost frames while it merely stands there
            // and to starve the visualiser beside it. Nothing on the page moves, so
            // what this measures is the price of a still table: its canvas keeps no
            // list and no tessellated geometry, and rebuilds both on every draw.
            name: "gallery-table",
            group: Group::Pages,
            own: None,
            tab: "table",
            frames: 120,
            program: Program::Idle,
            moving: None,
            pointer_at: Pt { x: 700.0, y: 400.0 },
            fenced: false,
            immediate_guard: "iced.gallery-table",
            retained_guard: "vello.gallery-table",
        },
        Page {
            name: "gallery-stress",
            group: Group::Pages,
            own: None,
            tab: "stress",
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
            own: None,
            tab: "vis",
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
            own: None,
            tab: "shader",
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
            own: None,
            tab: "pivot",
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
            own: None,
            tab: "library2",
            frames: 60,
            program: Program::Wheels(&[0, 1, 4, 8]),
            moving: None,
            pointer_at: Pt { x: 100.0, y: 400.0 },
            fenced: false,
            immediate_guard: "iced.gallery-library2",
            retained_guard: "vello.gallery-library2",
        },
        Page {
            // The file table with more rows than fit, which is the only page where
            // the table's marks cache misses on every frame. The still table page
            // prices a hit; this one prices what the key costs when it does not
            // hold and the marks have to be built anyway.
            name: "gallery-table-long",
            group: Group::Scroll,
            own: None,
            tab: "table-long",
            frames: 60,
            program: Program::Wheels(&[0, 1, 4, 8]),
            moving: None,
            pointer_at: Pt { x: 600.0, y: 400.0 },
            fenced: false,
            immediate_guard: "iced.gallery-table-long",
            retained_guard: "vello.gallery-table-long",
        },
        Page {
            name: "gallery-tree",
            group: Group::Scroll,
            own: None,
            tab: "tree",
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
            // `demo.volume` is the fader's own reading, and a drag that missed the
            // rail leaves it where it was.
            name: "gallery-faders",
            group: Group::Drag,
            own: None,
            tab: "faders",
            frames: 60,
            program: Program::Drag(&[0, 1, 4, 8]),
            moving: Some("demo.volume"),
            pointer_at: Pt { x: 320.0, y: 198.0 },
            fenced: false,
            immediate_guard: "iced.gallery-faders",
            retained_guard: "vello.gallery-faders",
        },
        Page {
            // Transformed nodes with nothing running them: the demo hands each
            // object its pose and the page's clock stands still. Read against
            // `gallery-motion` it separates what a pose costs from what animating
            // one costs, and against `gallery-buttons` what a transform costs at
            // all.
            name: "gallery-objects",
            group: Group::Pages,
            own: None,
            tab: "objects",
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
        // move off `gallery.motion.clock`, which the demo advances on their tabs,
        // so a run that measured a still page fails its own moving check.
        Page {
            name: "gallery-motion",
            group: Group::Pages,
            own: None,
            tab: "motion",
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
            own: None,
            tab: "sprites",
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
            own: None,
            tab: "lottie",
            frames: 120,
            program: Program::Tick,
            moving: Some("gallery.motion.clock"),
            pointer_at: Pt { x: 700.0, y: 400.0 },
            fenced: false,
            immediate_guard: "iced.gallery-lottie",
            retained_guard: "vello.gallery-lottie",
        },
        Page {
            name: "gallery-scene",
            group: Group::Pages,
            own: None,
            tab: "scene",
            frames: 120,
            program: Program::Tick,
            moving: Some("gallery.motion.clock"),
            pointer_at: Pt { x: 700.0, y: 400.0 },
            fenced: false,
            immediate_guard: "iced.gallery-scene",
            retained_guard: "vello.gallery-scene",
        },
        Page {
            name: "perf-scroll-vis",
            group: Group::Scroll,
            own: Some(SCROLL_VIS_LAYOUT),
            tab: "vis",
            frames: 60,
            program: Program::Wheels(&[0, 1, 4, 8]),
            moving: Some("vis.time"),
            pointer_at: Pt { x: 100.0, y: 400.0 },
            fenced: false,
            immediate_guard: "iced.perf-scroll-vis",
            retained_guard: "vello.perf-scroll-vis",
        },
    ];

    /// The harness's own page, mounting the one list the gallery scrolls together
    /// with the visualiser, so a scroll slope measured with a visualiser on the page
    /// can be compared against the same slope without one. It lives here rather than
    /// in the gallery's assets because every page there joins `Shot::all()` and the
    /// parity lane.
    ///
    /// The list is the navigator's: it is the only one in the gallery whose content
    /// outgrows its viewport, and a wheel over a viewport nothing overflows is a
    /// measurement of a clamp rather than of a scroll.
    pub(super) const SCROLL_VIS_LAYOUT: &str = "perf-scroll-vis.klayout.ron";
}

/// The gallery's nav beside a full-bleed visualiser: the wheel goes to the
/// nav's own scroll while the visualiser animates, which is the pair this
/// measures and no page of the gallery puts together.
///
/// The nav turns the screen's page state, so a layout holding it has to offer
/// the pages it names. Every one of them stands the same visualiser, because
/// what is measured here is the scroll beside it rather than the page.
fn scroll_vis_ron() -> String {
    let pages: String = sections::pages()
        .iter()
        .map(|page| {
            format!(
                r#""{page}": Module(instance: "vis", source: "modules/tabs/vis.kmodule.ron", corners: true, size: (w: Fill, h: Fill)),"#
            )
        })
        .collect();
    format!(
        r#"(schema: "kithara.layout", version: 1, id: "perf-scroll-vis",
            root: Split(axis: Horizontal, children: [
                (weight: 1.0, node: Module(instance: "nav", source: "modules/nav.kmodule.ron", corners: true, size: (w: Fixed(198.0), h: Fill))),
                (weight: 1.0, node: Tabs(state: "{state}", initial: "{initial}", pages: {{{pages}}})),
            ]))"#,
        state = sections::PAGE,
        initial = sections::first(),
    )
}

/// Everything a host is handed that is not the host: the documents, the
/// endpoints they may bind to, and nothing that differs between the two.
pub(crate) struct Fixture {
    registry: Box<dyn EndpointRegistry>,
    resolver: OverlayResolver<MemResolver, fixture::Resolver>,
}

impl Default for Fixture {
    fn default() -> Self {
        let mut extra = MemResolver::default();
        extra.insert(consts::SCROLL_VIS_LAYOUT, &scroll_vis_ron());
        Self {
            registry: Box::new(demo::registry()),
            resolver: OverlayResolver::new(extra, fixture::resolver()),
        }
    }
}

impl Fixture {
    pub(crate) fn compiled(&self, page: &Page) -> CompiledUi {
        let entry = page.document();
        compile(
            entry,
            &self.resolver,
            self.registry.as_ref(),
            builtin::skin_doc(),
            builtin::text_doc(),
            &UiConfig::default(),
            &page.standing(),
        )
        .unwrap_or_else(|error| panic!("the page-perf document {entry} must compile: {error}"))
    }

    pub(crate) fn config(&self) -> Config<'_> {
        Config::builder()
            .endpoints(self.registry.as_ref())
            .resolver(&self.resolver)
            .text(builtin::text_doc())
            .build()
    }
}
