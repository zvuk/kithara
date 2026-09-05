//! The immediate host, kept between events so a gesture can be played to it.
//!
//! The other parity fixtures reach into one widget and hand it an event
//! directly, which is enough for a control that stands in the tree. A surface
//! does not: a popover is an overlay, and only the runtime's own interface
//! builds one. So this mounts the whole document the way a window does, and
//! keeps what the tree remembered between one event and the next.

use std::borrow::Cow;

use iced::{
    Event, Point, Size,
    advanced::{clipboard, graphics::text::font_system, mouse::Cursor},
    event,
    mouse::{self, Button, ScrollDelta},
};
use iced_runtime::{
    UserInterface,
    user_interface::{Cache, State},
};
use num_traits::cast::AsPrimitive;

use super::shared::renderer;
use crate::{
    app::App,
    compile::CompiledUi,
    draw::Pt,
    render::{Clock, ControlAction, Skin, UiEvent, fonts::FONT_BYTES, tree},
    view::ViewState,
};

/// One compiled document, drawn and answered by the immediate host.
#[derive(fieldwork::Fieldwork)]
#[fieldwork(opt_in, get)]
pub(in crate::render) struct Immediate<'a, A> {
    /// The application this host is showing.
    #[field(get, vis = "pub(in crate::render)")]
    app: A,
    /// What the interface kept of the tree it built last time. An immediate
    /// host forgets the tree between frames, so what a widget remembers of a
    /// gesture - a press it has begun, a surface it has latched - lives here
    /// and nowhere else.
    cache: Cache,
    renderer: iced::Renderer,
    size: Size,
    skin: &'a Skin,
    /// What the tree said of itself when it last answered an event.
    state: State,
    ui: &'a CompiledUi,
    /// The hand the tree last asked the window to show.
    #[field(get(copy), vis = "pub(in crate::render)")]
    hand: mouse::Interaction,
    /// The state the screen keeps for itself, which this host owns exactly as
    /// the retained one does.
    #[field(get, vis = "pub(in crate::render)")]
    view: ViewState,
}

impl<'a, A: App> Immediate<'a, A> {
    /// Mounts the document, registering the toolkit's own faces with the font
    /// system this host shapes through the way a window does on the way up.
    pub(in crate::render) fn mount(
        app: A,
        ui: &'a CompiledUi,
        skin: &'a Skin,
        size: (u32, u32),
    ) -> Self {
        let mut fonts = font_system()
            .write()
            .unwrap_or_else(|error| panic!("iced font system lock: {error}"));
        for bytes in FONT_BYTES {
            fonts.load_font(Cow::Borrowed(bytes));
        }
        drop(fonts);
        Self {
            app,
            cache: Cache::default(),
            hand: mouse::Interaction::None,
            renderer: renderer(),
            size: Size::new(size.0.as_(), size.1.as_()),
            skin,
            state: State::Outdated,
            ui,
            view: ViewState::default(),
        }
    }

    /// A whole press at one point of the window: the pointer arrives, presses
    /// and lets go, each one its own frame the way a runtime hands them over.
    pub(in crate::render) fn click_at(&mut self, at: Pt) -> bool {
        let cursor = Point::new(at.x, at.y);
        [
            Event::Mouse(mouse::Event::CursorMoved { position: cursor }),
            Event::Mouse(mouse::Event::ButtonPressed(Button::Left)),
            Event::Mouse(mouse::Event::ButtonReleased(Button::Left)),
        ]
        .into_iter()
        .fold(false, |took, event| self.play(cursor, &event) || took)
    }

    /// The pointer arrives at one point and stops there, pressing nothing.
    pub(in crate::render) fn hover_at(&mut self, at: Pt) -> bool {
        let cursor = Point::new(at.x, at.y);
        let moved = Event::Mouse(mouse::Event::CursorMoved { position: cursor });
        self.play(cursor, &moved)
    }

    /// One notch of the wheel over a point of the window, the pointer having
    /// arrived there first: a detent is aimed by wherever the hand already is,
    /// so a wheel played without a move lands where the last one left it.
    ///
    /// The arrival is aim rather than gesture, so what it took is not counted:
    /// a widget that takes every move it is offered would otherwise answer for
    /// a wheel it never read.
    pub(in crate::render) fn wheel_at(&mut self, at: Pt, notches: f32) -> bool {
        let cursor = Point::new(at.x, at.y);
        self.hover_at(at);
        let wheel = Event::Mouse(mouse::Event::WheelScrolled {
            delta: ScrollDelta::Lines { x: 0.0, y: notches },
        });
        self.play(cursor, &wheel)
    }

    /// The pointer arrives at one point and presses without letting go, which
    /// is where a drag starts. Travel is `hover_at`, which is the same move
    /// event with the button already down.
    pub(in crate::render) fn press_at(&mut self, at: Pt) -> bool {
        let cursor = Point::new(at.x, at.y);
        [
            Event::Mouse(mouse::Event::CursorMoved { position: cursor }),
            Event::Mouse(mouse::Event::ButtonPressed(Button::Left)),
        ]
        .into_iter()
        .fold(false, |took, event| self.play(cursor, &event) || took)
    }

    /// Hands the tree one event and tells the application what it published,
    /// answering whether any widget took the event for itself.
    fn play(&mut self, cursor: Point, event: &Event) -> bool {
        let (published, captured) = self.dispatch(cursor, event);
        for event in published {
            self.settle(event);
        }
        captured
    }

    /// Builds the tree, hands it one event, and keeps what the tree remembered
    /// along with the hand it asked for.
    ///
    /// A tree that reports itself outdated has no hand to give: its layout no
    /// longer answers for the pointer. The runtime rebuilds and asks again, and
    /// so does this, with no event the second time so nothing is delivered
    /// twice.
    fn dispatch(&mut self, cursor: Point, event: &Event) -> (Vec<UiEvent>, bool) {
        let (mut published, mut captured) = self.deliver(cursor, std::slice::from_ref(event));
        if matches!(self.state, State::Outdated) {
            let (again, took) = self.deliver(cursor, &[]);
            published.extend(again);
            captured |= took;
        }
        (published, captured)
    }

    /// Builds the tree, hands it the events, and keeps what it remembered,
    /// answering with what the document published and whether any widget took
    /// an event for itself.
    fn deliver(&mut self, cursor: Point, events: &[Event]) -> (Vec<UiEvent>, bool) {
        let Self {
            app,
            cache,
            hand,
            renderer,
            size,
            skin,
            state,
            ui,
            view,
        } = self;
        let element = app
            .reads(|reads| tree::render(&ui.root, ui, reads, view, skin, Clock::default(), None));
        let mut interface = UserInterface::build(element, *size, std::mem::take(cache), renderer);
        let mut published: Vec<UiEvent> = Vec::new();
        let (settled, statuses) = interface.update(
            events,
            Cursor::Available(cursor),
            renderer,
            &mut clipboard::Null,
            &mut published,
        );
        if let State::Updated {
            mouse_interaction, ..
        } = &settled
        {
            *hand = *mouse_interaction;
        }
        *state = settled;
        *cache = interface.into_cache();
        let captured = statuses
            .iter()
            .any(|status| matches!(status, event::Status::Captured));
        (published, captured)
    }

    /// Applies what the press writes to the screen's own state, then tells the
    /// application. The state a document turns for itself belongs to whichever
    /// host is showing it, so this host turns it exactly as the retained one
    /// does before the application hears anything.
    fn settle(&mut self, event: UiEvent) {
        if let UiEvent::Control { path, action } = &event
            && matches!(action, ControlAction::Activate)
            && let Some((state, write)) = self.ui.views().at(path)
        {
            self.view.apply(state, write);
        }
        self.app.update(event);
    }
}
