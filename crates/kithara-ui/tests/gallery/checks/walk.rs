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
    Pixels, Size,
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

use crate::{
    app::Message,
    capture::Shot,
    cli::Host,
    sections::{self, Page},
};

/// How many frames a page is given to stop asking.
///
/// A page that settles at all settles in one or two: the retained tree lays
/// out, paints, and goes quiet. This is far past that, so a page still asking
/// here is asking forever rather than finishing late.
const FRAMES: usize = 32;

/// The toolkit a host draws through, which is what a page that never settles
/// has to be reported under: the flag names the host, the failure names the
/// drawing.
fn toolkit(host: Host) -> &'static str {
    match host {
        Host::Immediate => "iced",
        #[cfg(feature = "masonry")]
        Host::Retained => "masonry",
    }
}

/// What one page did when it was left alone.
struct Walked {
    /// Whether the page's document says it draws a different picture later.
    animates: bool,
    /// Whether the window would still be drawing this page unprompted.
    asking: bool,
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
        Size, Style, TinySkiaRenderer, UserInterface, Walked, clipboard, font_system, mem, window,
    };

    /// Steps the page through the runtime's own interface, which is what the
    /// window does: build the tree from the document, apply what is pending,
    /// draw, and ask whether another frame was requested.
    ///
    /// The software backend rasterises it. What is under test is whether the
    /// loop settles, and that answer is the same on either backend, so a
    /// machine with no graphics device is not a reason to skip the walk.
    pub(super) fn walk(page: Shot) -> Walked {
        let mut gallery = crate::app::Gallery::mounted();
        gallery.select(page);
        let animates = gallery.compiled().animates;
        let theme = crate::app::theme(gallery.skin());
        let text_color = theme.base().text_color;
        let logical = Size::from(crate::cli::WINDOW);
        let mut renderer = renderer();
        let mut cache = Cache::default();
        let mut asking = true;
        for _ in 0..FRAMES {
            // The window ticks only the pages that move, which is what the
            // gallery's subscription does. A page ticked regardless would be
            // measured moving on the strength of the harness rather than of
            // the page.
            if gallery.moves() {
                drop(crate::app::update(&mut gallery, Message::Tick));
            }
            let mut interface = UserInterface::build(
                crate::app::view(&gallery, window::Id::unique()),
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

        Walked { animates, asking }
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
        app::{App, Config, Ui},
        builtin,
        compile::compile,
        render::{Reads, Skin, UiEvent},
        view::ViewState,
    };

    use super::{FRAMES, Shot, Walked};
    use crate::{
        custom, demo,
        fixture::{consts, resolver},
        host::{self, Gallery},
        sections,
    };

    /// The gallery with its own clock held unless the page says it moves.
    ///
    /// The window ticks the application once per frame it draws, so a page
    /// whose readings the application keeps changing would be measured moving
    /// on the strength of the harness rather than of its own document — the
    /// same rule the immediate walk beside this one states, and for the same
    /// reason. What a fed page does is a separate question, asked separately.
    struct Still {
        gallery: Gallery,
        ticks: bool,
    }

    impl App for Still {
        fn reads<R>(&self, with: impl FnOnce(&dyn Reads) -> R) -> R {
            self.gallery.reads(with)
        }

        fn tick(&mut self) {
            if self.ticks {
                self.gallery.tick();
            }
        }

        delegate::delegate! {
            to self.gallery {
                fn document(&self) -> &str;
                fn skin(&self) -> &Skin;
                fn turned(&mut self, view: &ViewState);
                fn update(&mut self, event: UiEvent);
            }
        }
    }

    /// Steps one page with nothing touching it.
    ///
    /// This is the window's own loop with the window taken off, and it has to
    /// be exactly that loop or it measures nothing: the window draws only when
    /// the tree says the next frame would differ, and asks for another only
    /// when finishing this one said to. Miss either condition and a page that
    /// has stopped driving itself still looks alive here, because the harness
    /// kept calling it.
    pub(super) fn walk(page: Shot) -> Walked {
        walked(page, false)
    }

    /// Steps one page with the application feeding it every frame, which is
    /// what the window does.
    #[cfg(test)]
    pub(super) fn fed(page: Shot) -> Walked {
        walked(page, true)
    }

    fn walked(page: Shot, ticks: bool) -> Walked {
        let endpoints = demo::registry();
        let resolver = resolver();
        let kinds = custom::kinds();
        let config = Config::builder()
            .endpoints(&endpoints)
            .resolver(&resolver)
            .text(builtin::text_doc())
            .kinds(&kinds)
            .build();
        let size = (
            num_traits::cast::AsPrimitive::<u32>::as_(consts::WIDTH),
            num_traits::cast::AsPrimitive::<u32>::as_(consts::HEIGHT),
        );
        // Read from the document rather than from the host, so the two sides of
        // the comparison come from two places: what the page says, and what the
        // window then does about it.
        let animates = compile(
            sections::entry(),
            &resolver,
            &endpoints,
            builtin::skin_doc(),
            builtin::text_doc(),
            custom::config(),
            &page.standing(),
        )
        .unwrap_or_else(|error| panic!("page {} must compile: {error}", page))
        .animates;
        let still = Still {
            gallery: Gallery::default(),
            ticks: ticks || animates,
        };
        let mut ui = Ui::new(still, config, size, 1.0)
            .unwrap_or_else(|error| panic!("page {} must mount: {error}", page));
        host::stand(&mut ui, page)
            .unwrap_or_else(|error| panic!("page {} must open: {error}", page));
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
                .unwrap_or_else(|error| panic!("page {} must draw: {error}", page));
            if !ui.complete_frame() {
                asking = false;
                break;
            }
        }

        Walked { animates, asking }
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
                (walked.asking && !walked.animates).then(|| format!("{}: {}", toolkit(*host), page))
            })
        })
        .collect();

    assert!(
        spinning.is_empty(),
        "these pages ask the window for a frame forever with nothing on them declared to move, so \
         the loop never sleeps and every other page waits behind them: {spinning:?}"
    );
}

/// The other way round from the empty spin: a page the application keeps
/// feeding has to ask for the frame after it.
///
/// Nothing in the document says a reading will move — the application decides
/// that, one frame at a time — so a window that only honoured the declaration
/// would draw this page whenever some unrelated event woke it and freeze in
/// between, which is to say it would move under a travelling mouse.
#[cfg(feature = "masonry")]
#[kithara::test]
fn a_page_the_application_keeps_feeding_asks_for_the_frame_after_it() {
    let page = Shot::all()
        .into_iter()
        .find(|page| page.tab == "stress")
        .expect("the gallery must hold the stress page");
    assert!(
        retained::fed(page).asking,
        "the stress page draws readings the application moves every frame, so the window has to \
         come back for the frame after it"
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
            (walked.animates && !walked.asking).then(|| page.to_string())
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
    let walked: Vec<Page> = Shot::all().into_iter().map(|page| page.tab).collect();

    let missing: Vec<Page> = sections::pages()
        .iter()
        .copied()
        .filter(|tab| !walked.contains(tab))
        .collect();

    assert!(missing.is_empty(), "tabs never walked: {missing:?}");
}
