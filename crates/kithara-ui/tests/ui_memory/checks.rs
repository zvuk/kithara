//! What a host holds once a still page has settled.
//!
//! A window redraws whenever anything wakes it, and a page that has not changed
//! must cost nothing to draw again. The renderer's buffers are pooled, so the
//! pool grows to the high-water mark of everything handed to it and keeps that
//! memory for the life of the process: a host that rebuilds its picture every
//! frame therefore does not look like a leak, it looks like a large constant,
//! and no test that watches a single number over time can see it.
//!
//! So both questions are asked here, and neither is worth anything alone. The
//! ceiling catches a host that was always too expensive; the growth check
//! catches a host that pays again for a picture it already had. A defect that
//! raises the pool by a megabyte a frame passes the ceiling on a short run and
//! fails the growth check immediately.
//!
//! Each host is asked through the device itself rather than through its own
//! counters: what a renderer reports is what it asked for, while the bulk of a
//! frame is what the driver allocated underneath it.

use kithara_test_utils::kithara;
use kithara_ui::render::gpu;

use crate::{capture::Shot, fixture::Consts};

/// How much graphics memory a settled page may hold, per host.
///
/// Both are the measured cost of the gallery's heaviest page plus room for
/// driver jitter, and they are separate numbers on purpose: the two hosts
/// drive different renderers, and one total would hide which of them moved.
struct Budget;

impl Budget {
    /// Frames drawn before the pool is read, so the reading is of a settled
    /// host rather than of one still building its pipelines and atlases.
    const WARMUP: usize = 4;
    /// Frames drawn after the reading. Long enough that a per-frame cost shows
    /// as a multiple of the slack rather than as noise.
    const DRAWS: usize = 24;
    /// What the pool may drift by across [`Self::DRAWS`] unchanged frames.
    /// Not zero: a driver is free to round and to keep its own scratch.
    const SLACK_KIB: usize = 512;
    /// Everything the immediate host holds for a settled page. Measured at
    /// 26_928 KiB, and rounded up to leave room for a driver that rounds
    /// differently.
    const IMMEDIATE_KIB: u64 = 32_768;
    /// The same for the retained host, which is dominated by the compute
    /// buffers Vello sizes to the target on its first frame.
    ///
    /// This is a ratchet, not an endorsement: measured at 175_008 KiB, it is
    /// above the 120 MiB the application is allowed in total, so the retained
    /// host cannot carry a window on its own until it comes down. Pinned here
    /// so that it can only ever fall.
    const RETAINED_KIB: u64 = 180_224;
}

/// Bytes the graphics device holds, or `None` where the platform cannot say.
fn device_bytes() -> Option<u64> {
    gpu::allocated_bytes()
}

/// The pixel geometry the pages are drawn at: the window's own size at one
/// pixel to the point, so the pool measured is the pool a window would build.
fn physical() -> (u32, u32) {
    (
        num_traits::cast::AsPrimitive::as_(Consts::WIDTH),
        num_traits::cast::AsPrimitive::as_(Consts::HEIGHT),
    )
}

/// What the device held at each stage of standing a host up.
///
/// A single total says only that a host is expensive. These say which step
/// bought the memory, which is the difference between a number to argue about
/// and a place to go and fix.
struct Stages {
    /// Before anything of ours exists: the graphics device alone.
    device: u64,
    /// The renderer and its pipelines, before any page is drawn.
    renderer: u64,
    /// After the first frame, which is where atlases and pipelines are filled.
    first: u64,
    /// After the warmup, which is the reading the budget is written against.
    settled: u64,
    /// After more frames of a page that did not change.
    after: u64,
}

impl Stages {
    /// One line per step, each showing what that step alone cost.
    fn report(&self, host: &str) {
        let kib = |bytes: u64| bytes / 1024;
        eprintln!(
            "{host}: device {} KiB | renderer +{} KiB | first frame +{} KiB | warmup +{} KiB |              {} unchanged frames +{} KiB",
            kib(self.device),
            kib(self.renderer.saturating_sub(self.device)),
            kib(self.first.saturating_sub(self.renderer)),
            kib(self.settled.saturating_sub(self.first)),
            Budget::DRAWS,
            kib(self.after.saturating_sub(self.settled)),
        );
    }
}

/// The page the ceiling is written against: the heaviest the gallery ships.
fn heaviest() -> Shot {
    Shot::all()
        .into_iter()
        .last()
        .unwrap_or_else(|| panic!("the gallery must ship at least one page"))
}

/// Both hosts, one after the other in one test.
///
/// Not two tests: the device counts bytes per process, so two tests running at
/// once would each be reading the other's allocations, and the run that passed
/// would be the run whose threads happened to interleave kindly.
#[kithara::test]
fn a_settled_page_stays_inside_its_budget_on_every_host() {
    let page = heaviest();

    let iced = immediate::pool(page);
    iced.report("iced");

    #[cfg(feature = "masonry")]
    let masonry = retained::pool(page);
    #[cfg(feature = "masonry")]
    masonry.report("masonry");

    assert_growth("iced", iced.settled, iced.after);
    assert_ceiling("iced", &iced, Budget::IMMEDIATE_KIB);
    #[cfg(feature = "masonry")]
    {
        assert_growth("masonry", masonry.settled, masonry.after);
        assert_ceiling("masonry", &masonry, Budget::RETAINED_KIB);
    }
}

/// A settled host holds no more than its budget, counting only what it added
/// to a device that was already there.
fn assert_ceiling(host: &str, stages: &Stages, budget_kib: u64) {
    let held = stages.settled.saturating_sub(stages.device) / 1024;
    assert!(
        held <= budget_kib,
        "{host} holds {held} KiB for one settled page, over its {budget_kib} KiB budget; the \
         stages printed above say which step bought it"
    );
}

/// A settled host draws an unchanged page without asking the device for more.
fn assert_growth(host: &str, settled: u64, after: u64) {
    let grown = after.saturating_sub(settled);
    let slack = Budget::SLACK_KIB as u64 * 1024;
    assert!(
        grown <= slack,
        "{host} asked the device for {} KiB more across {} frames of a page that did not change, \
         which is {} KiB per frame: the picture is being rebuilt and paid for every frame instead \
         of being kept",
        grown / 1024,
        Budget::DRAWS,
        grown / 1024 / Budget::DRAWS as u64,
    );
}

/// The immediate host, which is the one the gallery's own window runs.
mod immediate {
    use std::{borrow::Cow, mem};

    use futures_lite::future::block_on;
    use iced::{
        Pixels, Size,
        advanced::{
            clipboard,
            graphics::{Shell, Viewport, text::font_system},
            mouse::Cursor,
            renderer::Style,
        },
        theme::Base as _,
        window,
    };
    use iced_renderer::fallback::Renderer as FallbackRenderer;
    use iced_runtime::{UserInterface, user_interface::Cache};
    use iced_wgpu::{
        Engine, Renderer as WgpuRenderer,
        wgpu::{
            Backends, DeviceDescriptor, Instance, InstanceDescriptor, RequestAdapterOptions,
            TextureFormat,
        },
    };
    use kithara_ui::render::fonts::{FONT_BYTES, SANS};

    use super::{Budget, Shot, Stages, device_bytes};

    /// Device bytes once the host has settled, and again after more unchanged
    /// frames.
    ///
    /// The renderer and its cache live across every draw on purpose: that is
    /// what a window does between two frames, and a fresh renderer each time
    /// would answer a different, easier question.
    pub(super) fn pool(page: Shot) -> Stages {
        let device = read();
        let mut gallery = crate::app::Gallery::mounted();
        gallery.select(page);
        let theme = crate::app::theme(gallery.skin());
        let base = theme.base();
        let logical = Size::from(crate::cli::WINDOW);
        let mut renderer = renderer();
        let mut cache = Cache::default();
        let renderer_bytes = read();

        let draw = |renderer: &mut iced::Renderer, cache: &mut Cache| {
            let mut interface = UserInterface::build(
                crate::app::view(&gallery, window::Id::unique()),
                logical,
                mem::take(cache),
                renderer,
            );
            drop(interface.update(
                &[],
                Cursor::Unavailable,
                renderer,
                &mut clipboard::Null,
                &mut Vec::new(),
            ));
            interface.draw(
                renderer,
                &theme,
                &Style {
                    text_color: base.text_color,
                },
                Cursor::Unavailable,
            );
            *cache = interface.into_cache();
            let FallbackRenderer::Primary(wgpu) = renderer else {
                panic!("this host rasterises through wgpu, which is what the window draws with")
            };
            drop(wgpu.screenshot(&viewport(), base.background_color));
        };

        draw(&mut renderer, &mut cache);
        let first = read();
        for _ in 1..Budget::WARMUP {
            draw(&mut renderer, &mut cache);
        }
        let settled = read();
        for _ in 0..Budget::DRAWS {
            draw(&mut renderer, &mut cache);
        }
        Stages {
            after: read(),
            device,
            first,
            renderer: renderer_bytes,
            settled,
        }
    }

    fn viewport() -> Viewport {
        let (width, height) = super::physical();
        Viewport::with_physical_size(Size::new(width, height), 1.0)
    }

    fn read() -> u64 {
        device_bytes().unwrap_or_else(|| panic!("the device answered once and must answer again"))
    }

    /// A renderer with the gallery's own faces registered, drawing into a
    /// texture rather than into a surface.
    fn renderer() -> iced::Renderer {
        let mut fonts = font_system()
            .write()
            .unwrap_or_else(|error| panic!("iced font system lock: {error}"));
        for bytes in FONT_BYTES {
            fonts.load_font(Cow::Borrowed(bytes));
        }
        drop(fonts);
        let instance = Instance::new(&InstanceDescriptor {
            backends: Backends::PRIMARY,
            ..InstanceDescriptor::default()
        });
        let adapter = block_on(instance.request_adapter(&RequestAdapterOptions::default()))
            .unwrap_or_else(|error| panic!("no wgpu adapter: {error}"));
        let (device, queue) = block_on(adapter.request_device(&DeviceDescriptor::default()))
            .unwrap_or_else(|error| panic!("no wgpu device: {error}"));
        let engine = Engine::new(
            &adapter,
            device,
            queue,
            TextureFormat::Rgba8UnormSrgb,
            None,
            Shell::headless(),
        );
        FallbackRenderer::Primary(WgpuRenderer::new(engine, SANS, Pixels(14.0)))
    }
}

/// The retained host, drawn through the rasteriser this toolkit already runs
/// headless.
#[cfg(feature = "masonry")]
mod retained {
    use kithara_ui::{
        app::{Config, Ui},
        builtin,
        capture::Offscreen,
    };
    use masonry::vello::peniko::Color;

    use super::{Budget, Shot, Stages, device_bytes};
    use crate::{
        custom, demo,
        fixture::resolver,
        host::{self, Gallery},
    };

    pub(super) fn pool(page: Shot) -> Stages {
        let device = read();
        let (width, height) = super::physical();
        let endpoints = demo::registry();
        let resolver = resolver();
        let kinds = custom::kinds();
        let config = Config::builder()
            .endpoints(&endpoints)
            .resolver(&resolver)
            .text(builtin::text_doc())
            .kinds(&kinds)
            .build();
        let mut off = Offscreen::new(width, height)
            .unwrap_or_else(|error| panic!("the page must rasterise: {error}"));
        let mut ui = Ui::new(Gallery::default(), config, (width, height), 1.0)
            .unwrap_or_else(|error| panic!("page {page} must mount: {error}"));
        host::stand(&mut ui, page).unwrap_or_else(|error| panic!("page {page} must open: {error}"));
        let mut rgba = Vec::new();
        let renderer_bytes = read();

        let draw = |ui: &mut Ui<Gallery>, off: &mut Offscreen, rgba: &mut Vec<u8>| {
            let frame = ui
                .render()
                .unwrap_or_else(|error| panic!("page {page} must draw: {error}"));
            off.rasterise(&frame, 1.0, Color::TRANSPARENT, rgba)
                .unwrap_or_else(|error| panic!("page {page} must rasterise: {error}"));
        };

        draw(&mut ui, &mut off, &mut rgba);
        let first = read();
        for _ in 1..Budget::WARMUP {
            draw(&mut ui, &mut off, &mut rgba);
        }
        let settled = read();
        for _ in 0..Budget::DRAWS {
            draw(&mut ui, &mut off, &mut rgba);
        }
        Stages {
            after: read(),
            device,
            first,
            renderer: renderer_bytes,
            settled,
        }
    }

    fn read() -> u64 {
        device_bytes().unwrap_or_else(|| panic!("the device answered once and must answer again"))
    }
}
