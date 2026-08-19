//! What a page draws when its clock is held, and when its clock runs.
//!
//! A window redraws a page whenever anything wakes it, and a still page has to
//! come out the same every time: two different pictures of one moment cannot be
//! told apart from a page that moved, so the cheapest oracle there is — did the
//! picture change — would be measuring the host rather than the page.
//!
//! The two questions are one instrument used both ways round, and neither is
//! worth anything without the other: a page held at one moment must draw the
//! same picture twice, and a page whose document declares motion must draw a
//! different one once its clock has run. Without the second, a host that drew
//! nothing at all would pass the first.
//!
//! Each host is asked at the last artefact this toolkit owns, and the two are
//! not the same artefact. The immediate host is read back as pixels: what it
//! hands wgpu comes out of the texture the same bytes every time, so a pixel
//! that differs is something drawn differently. The retained host is read as the
//! scene it hands Vello, because past that point the bytes stop being ours —
//! measured here, one unchanged scene rasterises to as many as six different
//! pixels from one call to the next, so pixels would report the engine's own
//! noise as the page moving.
//!
//! That choice of artefact decides which host answers which question. Both
//! answer the still one. Only the immediate host answers the moving one, because
//! a scene cannot: the visualiser page moves in a native pass that never enters
//! one. The retained host's answer to that lives in the walk beside this, asked
//! of its loop instead of its picture.
//!
//! Neither uses the software backend the walk settles its own question on: a
//! page of plain controls comes out of that one blank but for its title, so a
//! picture taken there answers nothing about a page.

use kithara_platform::time::Duration;
use kithara_test_utils::kithara;

use super::{Consts, capture::Shot};

/// What is done to a page between one picture and the next.
#[derive(Clone, Copy, Debug)]
enum Between {
    /// Nothing at all: the still case.
    Held,
    /// The host's own clock, run for a while.
    Ticked,
}

impl Between {
    /// How many frames the moving pages are given between the two pictures. A
    /// page is free to move slowly, and this is long enough that one which
    /// moves at all has moved.
    const TICKS: usize = 30;

    /// How many of the gallery's frames pass.
    const fn ticks(self) -> usize {
        match self {
            Self::Held => 0,
            Self::Ticked => Self::TICKS,
        }
    }

    /// How far the clock moves in total.
    fn elapsed(self) -> Duration {
        Duration::from_millis(Consts::STRESS_TICK_MS) * whole(self.ticks())
    }
}

/// What one host drew for one frame.
#[derive(PartialEq)]
enum Picture {
    /// Read back out of the texture, tightly packed RGBA.
    Pixels(Vec<u8>),
    /// The streams of the scene handed to the rasteriser: the path coordinates,
    /// the draw data, and the counts that say how they are grouped.
    Scene(Vec<u32>),
}

impl Picture {
    /// How this one differs from another of the same kind, in whatever terms
    /// that kind is read in.
    fn against(&self, other: &Self) -> Option<String> {
        match (self, other) {
            (Self::Pixels(first), Self::Pixels(second)) => pixels_differ(first, second),
            (Self::Scene(first), Self::Scene(second)) => (first != second)
                .then(|| format!("{} word(s) against {}", first.len(), second.len())),
            _ => panic!("one host answers in one kind of picture"),
        }
    }
}

/// How many pixels two readbacks disagree on, and where the first of them is.
fn pixels_differ(first: &[u8], second: &[u8]) -> Option<String> {
    let width = physical().0;
    let differing: Vec<usize> = first
        .chunks_exact(4)
        .zip(second.chunks_exact(4))
        .enumerate()
        .filter_map(|(index, (left, right))| (left != right).then_some(index))
        .collect();
    let first_at = whole(*differing.first()?);
    Some(format!(
        "{} pixel(s), first at {}x{}",
        differing.len(),
        first_at % width,
        first_at / width
    ))
}

/// Which host drew the pictures.
#[derive(Clone, Copy, Debug)]
enum Host {
    /// The gallery's own window: every frame rebuilds the element tree from the
    /// compiled document, lays it out and draws it.
    Immediate,
    /// The mounted tree, refreshed and repainted in place.
    #[cfg(feature = "masonry")]
    Retained,
}

impl Host {
    const fn name(self) -> &'static str {
        match self {
            Self::Immediate => "iced",
            #[cfg(feature = "masonry")]
            Self::Retained => "masonry",
        }
    }

    /// One page, drawn `draws` times, with `between` run from one draw to the
    /// next.
    fn pictures(self, page: Shot, draws: usize, between: Between) -> Vec<Picture> {
        match self {
            Self::Immediate => immediate::pictures(page, draws, between),
            #[cfg(feature = "masonry")]
            Self::Retained => retained::pictures(page, draws, between),
        }
    }
}

/// Which hosts this build can ask. The immediate one is always compiled in —
/// the gallery is an iced application — and the retained one only with the
/// feature that brings it.
const fn hosts() -> &'static [Host] {
    &[
        Host::Immediate,
        #[cfg(feature = "masonry")]
        Host::Retained,
    ]
}

/// The pixel geometry every picture here is taken at: the window's own size at
/// one pixel to the point, so a pixel that differs is something drawn
/// differently rather than something sampled differently.
fn physical() -> (u32, u32) {
    (whole_f32(Consts::WIDTH), whole_f32(Consts::HEIGHT))
}

fn whole(value: usize) -> u32 {
    num_traits::cast::AsPrimitive::as_(value)
}

fn whole_f32(value: f32) -> u32 {
    num_traits::cast::AsPrimitive::as_(value)
}

/// Whether this page's document says something on it draws a different picture
/// later. Read from the document rather than assumed from the page's name.
fn animates(page: Shot) -> bool {
    let mut gallery = super::Gallery::mounted();
    gallery.select(page);
    gallery.compiled().animates
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

    use super::{Between, Picture, Shot, physical};
    use crate::Message;

    /// Steps the page through the runtime's own interface, which is what the
    /// window does, and reads the texture back after every draw.
    ///
    /// The renderer and its cache live across the draws on purpose: what a
    /// window does between two frames is exactly this, and a fresh renderer
    /// each time would answer a different, easier question.
    pub(super) fn pictures(page: Shot, draws: usize, between: Between) -> Vec<Picture> {
        let mut gallery = super::super::Gallery::mounted();
        gallery.select(page);
        let theme = super::super::theme(gallery.skin);
        let base = theme.base();
        let logical = super::super::window_size();
        let mut renderer = renderer();
        let mut cache = Cache::default();
        let (width, height) = physical();
        let viewport = Viewport::with_physical_size(Size::new(width, height), 1.0);

        (0..draws)
            .map(|draw| {
                if draw > 0 {
                    run(&mut gallery, between);
                }
                let mut interface = UserInterface::build(
                    super::super::view(&gallery, window::Id::unique()),
                    logical,
                    mem::take(&mut cache),
                    &mut renderer,
                );
                drop(interface.update(
                    &[],
                    Cursor::Unavailable,
                    &mut renderer,
                    &mut clipboard::Null,
                    &mut Vec::new(),
                ));
                interface.draw(
                    &mut renderer,
                    &theme,
                    &Style {
                        text_color: base.text_color,
                    },
                    Cursor::Unavailable,
                );
                cache = interface.into_cache();
                let FallbackRenderer::Primary(wgpu) = &mut renderer else {
                    panic!("this host rasterises through wgpu, which is what the window draws with")
                };
                Picture::Pixels(wgpu.screenshot(&viewport, base.background_color))
            })
            .collect()
    }

    /// The gallery's own clock is run by the message its subscription sends for
    /// the pages whose document declares motion, so that is what runs it here.
    fn run(gallery: &mut super::super::Gallery, between: Between) {
        for _ in 0..between.ticks() {
            drop(super::super::update(gallery, Message::Tick));
        }
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

/// The retained host.
#[cfg(feature = "masonry")]
mod retained {
    use kithara_platform::time::Duration;
    use kithara_ui::{
        app::{Config, Ui},
        builtin,
    };
    use masonry::vello::Scene;

    use super::{Between, Picture, Shot, physical};
    use crate::{Consts, host::Gallery, mock, resolver};

    /// Mounts the page once and repaints it, reading back the scene it handed
    /// the rasteriser. The mount lives across the draws for the same reason the
    /// immediate host's renderer does: a page remounted between two pictures
    /// answers a different question.
    pub(super) fn pictures(page: Shot, draws: usize, between: Between) -> Vec<Picture> {
        let endpoints = mock::registry();
        let resolver = resolver();
        let config = Config::builder()
            .endpoints(&endpoints)
            .resolver(&resolver)
            .skin(builtin::skin())
            .skin_doc(builtin::skin_doc())
            .text(builtin::text_doc())
            .build();
        let (width, height) = physical();
        let mut ui = Ui::new(Gallery::at(page), config, (width, height), 1.0)
            .unwrap_or_else(|error| panic!("page {} must mount: {error}", page.name()));

        (0..draws)
            .map(|draw| {
                if draw > 0 {
                    run(&mut ui, between);
                }
                let frame = ui
                    .render()
                    .unwrap_or_else(|error| panic!("page {} must draw: {error}", page.name()));
                Picture::Scene(streams(frame.scene()))
            })
            .collect()
    }

    /// The scene as the numbers that describe it: where every path goes, what
    /// every draw says, and how many of each there are. Bitwise comparable, and
    /// the last thing this toolkit decides before the rasteriser has its say.
    fn streams(scene: &Scene) -> Vec<u32> {
        let encoding = scene.encoding();
        let mut streams =
            Vec::with_capacity(encoding.path_data.len() + encoding.draw_data.len() + 3);
        streams.extend_from_slice(&encoding.path_data);
        streams.extend_from_slice(&encoding.draw_data);
        streams.extend([encoding.n_paths, encoding.n_path_segments, encoding.n_clips]);
        streams
    }

    /// The window's own loop between two frames: a still page is not stepped at
    /// all, and a moving one is stepped at the rate the window tells the pass.
    fn run(ui: &mut Ui<Gallery>, between: Between) {
        for _ in 0..between.ticks() {
            ui.frame(Duration::from_millis(Consts::STRESS_TICK_MS));
        }
    }
}

#[kithara::test]
fn a_page_held_at_one_moment_rasterises_the_same_twice() {
    /// Three draws rather than two: the first is also the first time this host
    /// has drawn the page, so a difference between the second and the third
    /// separates warming up from drawing differently every time.
    const DRAWS: usize = 3;

    let unsteady: Vec<String> = hosts()
        .iter()
        .flat_map(|host| {
            Shot::all().into_iter().filter_map(move |page| {
                let shots = host.pictures(page, DRAWS, Between::Held);
                (1..DRAWS).find_map(|draw| {
                    shots[draw - 1].against(&shots[draw]).map(|difference| {
                        format!(
                            "{}: {}: draw {draw} differs from draw {} by {difference}",
                            host.name(),
                            page.name(),
                            draw + 1,
                        )
                    })
                })
            })
        })
        .collect();

    assert!(
        unsteady.is_empty(),
        "these pages draw two different pictures of the same moment, so nothing can tell them \
         apart from a page that moved: {unsteady:#?}"
    );
}

/// The immediate host alone, and for two reasons that both come from the
/// artefact each host is read at.
///
/// The retained one is read as a scene, and a scene cannot answer this: the
/// visualiser page moves in a native pass that never enters one, so a page
/// genuinely animating would be reported frozen. That host's own answer to
/// this question is next door in `walk`, where a tree that has stopped
/// animating is a tree that has stopped asking the window for frames — which
/// is the same claim asked of the loop rather than of the picture.
#[kithara::test]
fn a_page_that_declares_motion_draws_a_different_picture_once_its_clock_has_run() {
    let host = Host::Immediate;

    let frozen: Vec<String> = Shot::all()
        .into_iter()
        .filter(|page| animates(*page))
        .filter_map(|page| {
            let shots = host.pictures(page, 2, Between::Ticked);
            shots[0]
                .against(&shots[1])
                .is_none()
                .then(|| format!("{}: {}", host.name(), page.name()))
        })
        .collect();

    assert!(
        frozen.is_empty(),
        "these pages say something on them moves and then draw the same picture after {:?} of \
         their host's own clock, so nothing on them is animating: {frozen:#?}",
        Between::Ticked.elapsed(),
    );
}
