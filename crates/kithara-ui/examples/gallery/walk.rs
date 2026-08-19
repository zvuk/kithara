//! Every page of the gallery, walked with nothing touching it.
//!
//! A window drives itself: it asks for a frame, draws it, and asks again for as
//! long as something is moving. Two ways that goes wrong are invisible to a
//! screenshot, because a single frame of either is the right picture.
//!
//! A page that asks forever while nothing on it moves is an empty spin: the
//! window never sleeps, a core burns, and every other page pays for it in the
//! same event loop. A page that moves and stops asking is worse to use than to
//! measure: it animates only while some unrelated event wakes the window, so it
//! moves under a travelling mouse and freezes the moment the mouse stops.
//!
//! What a page does is checked against what its document says it will do. That
//! answer is one walk of the compiled tree — an object placed by an endpoint, a
//! control that paints every frame of its own accord, or a binding that reads
//! the host's own clock — and it is spelled out per control, so a control added
//! tomorrow is classified here or does not compile.
//!
//! Comparing two pictures instead cannot be the oracle *here*, and the reason
//! is this walk's own rasteriser rather than the pages: through the software
//! backend a page of plain controls comes out blank but for its title, so what
//! that comparison measured was the handful of glyphs the backend does draw.
//! The picture oracle lives beside this, in `steady`, on the backend the window
//! actually draws with.
//!
//! Both hosts are walked, because the gallery's own window is the immediate one
//! and the retained one answers a different question: whether a page settles is
//! a property of the host's loop, and the two loops sleep for different
//! reasons. The immediate walk rasterises through the software backend, so a
//! machine with no graphics device still runs it.
//!
//! A page that hangs outright needs no oracle of its own: the walk lays out and
//! rasterises every page on both hosts, so a frame that never returns is a test
//! that never finishes, and the runner ends it. What needs saying in an assert
//! is only the two failures a finished frame can hide.
//!
//! The page list is [`Shot::all`], the same one the captures walk, so a page
//! that is photographed is a page that is walked.

use std::{borrow::Cow, mem};

use iced::{
    Pixels,
    advanced::{clipboard, graphics::text::font_system, mouse::Cursor, renderer::Style},
    window,
    window::RedrawRequest,
};
use iced_renderer::fallback::Renderer as FallbackRenderer;
use iced_runtime::{
    UserInterface,
    user_interface::{Cache, State},
};
use iced_tiny_skia::Renderer as TinySkiaRenderer;
use kithara_test_utils::kithara;
use kithara_ui::render::fonts::{FONT_BYTES, SANS};

use super::{Message, capture::Shot, sections::Tab};

/// How many frames a page is given to stop asking.
///
/// A page that settles at all settles in one or two: the retained tree lays
/// out, paints, and goes quiet. This is far past that, so a page still asking
/// here is asking forever rather than finishing late.
const FRAMES: usize = 32;

/// Which loop the page was stepped in. The retained one is a variant only where
/// the feature that brings that host is on, so a build without it has no arm
/// that could be reached and none that sits unused.
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
    fn name(self) -> &'static str {
        match self {
            Self::Immediate => "iced",
            #[cfg(feature = "masonry")]
            Self::Retained => "masonry",
        }
    }
}

/// What one page did when it was left alone.
struct Walked {
    /// Whether the window would still be drawing this page unprompted.
    asking: bool,
    /// Whether the page's document says it draws a different picture later.
    animates: bool,
}

/// Steps one page on the named host with nothing touching it.
fn walk(host: Host, page: Shot) -> Walked {
    match host {
        Host::Immediate => immediate::walk(page),
        #[cfg(feature = "masonry")]
        Host::Retained => retained::walk(page),
    }
}

/// The immediate host, which is the one the gallery's own window runs.
mod immediate {
    use iced::theme::Base as _;

    use super::{
        Cache, Cow, Cursor, FONT_BYTES, FRAMES, FallbackRenderer, Message, Pixels, SANS, Shot,
        Style, TinySkiaRenderer, UserInterface, Walked, clipboard, font_system, mem, window,
    };

    /// Steps the page through the runtime's own interface, which is what the
    /// window does: build the tree from the document, apply what is pending,
    /// draw, and ask whether another frame was requested.
    ///
    /// The software backend rasterises it. What is under test is whether the
    /// loop settles, and that answer is the same on either backend, so a
    /// machine with no graphics device is not a reason to skip the walk.
    pub(super) fn walk(page: Shot) -> Walked {
        let mut gallery = super::super::Gallery::mounted();
        gallery.select(page);
        let animates = gallery.compiled().animates;
        let theme = super::super::theme(gallery.skin);
        let text_color = theme.base().text_color;
        let logical = super::super::window_size();
        let mut renderer = renderer();
        let mut cache = Cache::default();
        let mut asking = true;
        for _ in 0..FRAMES {
            // The window ticks only the pages whose document says they move,
            // which is what the gallery's subscription does. A page ticked
            // regardless would be measured moving on the strength of the
            // harness rather than of its own document.
            if animates {
                drop(super::super::update(&mut gallery, Message::Tick));
            }
            let mut interface = UserInterface::build(
                super::super::view(&gallery, window::Id::unique()),
                logical,
                mem::take(&mut cache),
                &mut renderer,
            );
            let (state, _statuses) = interface.update(
                &[],
                Cursor::Unavailable,
                &mut renderer,
                &mut clipboard::Null,
                &mut Vec::new(),
            );
            interface.draw(
                &mut renderer,
                &theme,
                &Style { text_color },
                Cursor::Unavailable,
            );
            cache = interface.into_cache();
            if !super::redraw_asked(&state) {
                asking = false;
                break;
            }
        }

        Walked { asking, animates }
    }

    /// A renderer with the gallery's own faces registered, drawing into memory.
    fn renderer() -> iced::Renderer {
        let mut fonts = font_system()
            .write()
            .unwrap_or_else(|error| panic!("iced font system lock: {error}"));
        for bytes in FONT_BYTES {
            fonts.load_font(Cow::Borrowed(bytes));
        }
        drop(fonts);

        FallbackRenderer::Secondary(TinySkiaRenderer::new(SANS, Pixels(14.0)))
    }
}

/// The retained host.
#[cfg(feature = "masonry")]
mod retained {
    use kithara_platform::time::Duration;
    use kithara_ui::{
        app::{App as _, Config, Ui},
        builtin,
        compile::compile,
        source::UiConfig,
    };

    use super::{FRAMES, Shot, Walked};
    use crate::{Consts, host::Gallery, mock, resolver};

    /// Steps one page with nothing touching it.
    ///
    /// This is the window's own loop with the window taken off, and it has to
    /// be exactly that loop or it measures nothing: the window draws only when
    /// the tree says the next frame would differ, and asks for another only
    /// when finishing this one said to. Miss either condition and a page that
    /// has stopped driving itself still looks alive here, because the harness
    /// kept calling it.
    pub(super) fn walk(page: Shot) -> Walked {
        let endpoints = mock::registry();
        let resolver = resolver();
        let config = Config::builder()
            .endpoints(&endpoints)
            .resolver(&resolver)
            .skin(builtin::skin())
            .skin_doc(builtin::skin_doc())
            .text(builtin::text_doc())
            .build();
        let size = (
            num_traits::cast::AsPrimitive::<u32>::as_(Consts::WIDTH),
            num_traits::cast::AsPrimitive::<u32>::as_(Consts::HEIGHT),
        );
        let at = Gallery::at(page);
        // Read from the document rather than from the host, so the two sides of
        // the comparison come from two places: what the page says, and what the
        // window then does about it.
        let animates = compile(
            at.document(),
            &resolver,
            &endpoints,
            builtin::skin_doc(),
            builtin::text_doc(),
            &UiConfig::default(),
        )
        .unwrap_or_else(|error| panic!("page {} must compile: {error}", page.name()))
        .animates;
        let mut ui = Ui::new(at, config, size, 1.0)
            .unwrap_or_else(|error| panic!("page {} must mount: {error}", page.name()));
        // One frame at sixty a second, which is what the window tells the pass.
        let frame = Duration::from_millis(16);
        // The window asks for the first frame itself, once the surface is up.
        let mut asking = true;
        for _ in 0..FRAMES {
            ui.frame(frame);
            if !ui.needs_frame() {
                asking = false;
                break;
            }
            ui.render()
                .unwrap_or_else(|error| panic!("page {} must draw: {error}", page.name()));
            if !ui.complete_frame() {
                asking = false;
                break;
            }
        }

        Walked { asking, animates }
    }
}

/// Whether a widget asked iced's runtime to come back with another frame. This
/// is the seam at which the window decides between drawing again and sleeping
/// until something happens, so it is what "still asking" means on this host.
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

/// Which hosts this build can walk. The immediate one is always compiled in —
/// the gallery is an iced application — and the retained one only with the
/// feature that brings it.
const fn hosts() -> &'static [Host] {
    &[
        Host::Immediate,
        #[cfg(feature = "masonry")]
        Host::Retained,
    ]
}

/// The empty spin: the window never sleeps and nothing on the page moves.
#[kithara::test]
fn no_page_asks_for_frames_its_document_never_declared() {
    let spinning: Vec<String> = hosts()
        .iter()
        .flat_map(|host| {
            Shot::all().into_iter().filter_map(move |page| {
                let walked = walk(*host, page);
                (walked.asking && !walked.animates)
                    .then(|| format!("{}: {}", host.name(), page.name()))
            })
        })
        .collect();

    assert!(
        spinning.is_empty(),
        "these pages ask the window for a frame forever with nothing on them declared to move, so \
         the loop never sleeps and every other page waits behind them: {spinning:?}"
    );
}

/// The other direction: a page that says it moves and stops asking has stopped
/// animating, and only an unrelated event will move it again.
///
/// The retained host alone, because the two hosts are moved by different
/// things. There, a frame is asked for by the mounted tree, so a tree that
/// stopped asking has stopped animating and this is the whole question. The
/// immediate host is driven from outside instead: the gallery subscribes to a
/// tick for exactly the pages whose document declares motion, every message
/// redraws, and `redraw_request` stays `Wait` throughout — which is why every
/// moving page reads as "not asking" here and means nothing. That this host
/// really does draw a different picture as its clock runs is measured where it
/// can be: `page_perf` fingerprints the first and last frame of the moving
/// pages on both hosts.
#[cfg(feature = "masonry")]
#[kithara::test]
fn every_page_that_declares_motion_keeps_asking_for_frames() {
    let stalled: Vec<String> = Shot::all()
        .into_iter()
        .filter_map(|page| {
            let walked = walk(Host::Retained, page);
            (walked.animates && !walked.asking).then(|| page.name())
        })
        .collect();

    assert!(
        stalled.is_empty(),
        "these pages say something on them moves and then stop asking the window for frames, so \
         they animate only while something unrelated keeps waking it — a mouse crossing the \
         window, and nothing when it stops: {stalled:?}"
    );
}

/// Every page the gallery shows is walked, so a page added tomorrow is checked
/// rather than merely photographed.
#[kithara::test]
fn every_tab_the_gallery_shows_is_walked() {
    let walked: Vec<Tab> = Shot::all().into_iter().map(|page| page.tab).collect();

    let missing: Vec<Tab> = Tab::ALL
        .into_iter()
        .filter(|tab| !walked.contains(tab))
        .collect();

    assert!(missing.is_empty(), "tabs never walked: {missing:?}");
}
