use std::convert::Infallible;

use iced::{Element, Task, Theme, theme, window, window::Id};
use kithara_platform::time::Duration;
use kithara_ui::{
    builtin,
    compile::{CompiledUi, compile},
    registry::EndpointRegistry,
    render::{Clock, ControlAction, Skin, UiEvent, WindowCommand, custom::CustomKinds, tree},
    skin::SkinDoc,
    view::{Screens, ViewState},
};

use crate::{
    capture::{Capture, Shot},
    demo::DemoReads,
    fixture::{Resolver, consts, resolver},
    sections,
};

#[derive(Clone, Debug)]
pub(crate) enum Message {
    Tick,
    Ui(UiEvent),
    /// Move to the next page to photograph, or finish and exit.
    CaptureNext,
    /// The page is on screen; ask the window for its pixels.
    CaptureShoot(Shot),
    /// Write one page to disk.
    CaptureSave(Shot, window::Screenshot),
}

pub(crate) struct Gallery {
    /// This host's own reading of time, advanced by the same step the tick
    /// subscription fires at, so a document bound to it moves with the page.
    pub(crate) clock: Clock,
    /// The extensions this application registers, offered to whichever host
    /// draws the page that names one.
    pub(crate) kinds: CustomKinds,
    pub(crate) reads: DemoReads,
    /// How far that clock moves in one frame.
    pub(crate) step: Duration,
    pub(crate) window_id: Id,
    pub(crate) capture: Option<Capture>,
    /// The screen the gallery shows, and the pages of it compiled before.
    pub(crate) screens: Screens,
    /// State the documents keep for themselves, which no endpoint of this
    /// application answers.
    pub(crate) view: ViewState,
}

impl Gallery {
    /// Selects the next page and lets one frame render before the shot.
    fn capture_next(&mut self) -> Task<Message> {
        let Some(capture) = self.capture.as_mut() else {
            return Task::none();
        };
        let Some(shot) = capture.next() else {
            capture.report();
            return iced::exit();
        };
        self.select(shot);
        Task::done(Message::CaptureShoot(shot))
    }

    fn capture_save(&mut self, shot: Shot, image: &window::Screenshot) -> Task<Message> {
        let Some(capture) = self.capture.as_mut() else {
            return Task::none();
        };
        match capture.save(shot, image) {
            Ok(path) => println!("captured {} ({} left)", path.display(), capture.remaining()),
            Err(error) => eprintln!("capture failed: {error}"),
        }
        Task::done(Message::CaptureNext)
    }

    pub(crate) const fn compiled(&self) -> &CompiledUi {
        self.screens.shown()
    }

    /// Compiles every page again against the skin the gallery has turned to.
    ///
    /// A document is compiled against a skin and not merely painted with one:
    /// what a page measures comes from the skin's own numbers, so a skin
    /// changed at runtime is a set of pages built again.
    pub(crate) fn dress(&mut self) {
        let resolver = resolver();
        let endpoints = crate::demo::registry();
        let skin = self.skin().document();
        let ui = compiled(&resolver, &endpoints, skin, &self.view);
        self.screens.reset(ui);
    }

    /// The gallery with no window of iced's: the offscreen capture rasterises
    /// the same documents itself, and never opens one.
    pub(crate) fn mounted() -> Self {
        let resolver = resolver();
        let endpoints = crate::demo::registry();
        let skin = builtin::skin().document();
        let view = ViewState::default();
        Self {
            screens: Screens::new(
                crate::custom::config().screen_cache,
                compiled(&resolver, &endpoints, skin, &view),
            ),
            window_id: Id::unique(),
            clock: Clock::default(),
            step: Duration::from_millis(consts::STRESS_TICK_MS),
            reads: DemoReads::default(),
            kinds: crate::custom::kinds(),
            capture: None,
            view,
        }
    }

    /// Whether the page the gallery is showing reads differently next frame,
    /// which is the window's whole reason to come back for one.
    ///
    /// Two things move a page and only one of them is written down. The
    /// document declares its own motion, and the compiled page carries that
    /// declaration; the application also hands over readings it moved itself,
    /// which no document can declare because the application decides them one
    /// frame at a time. A window that honoured the declaration alone froze the
    /// second kind between unrelated events.
    pub(crate) fn moves(&self) -> bool {
        self.compiled().animates || self.reads.feeds()
    }

    /// Applies whatever the press at `path` writes to the screen's own state,
    /// then shows the page that state now stands at.
    pub(crate) fn press(&mut self, path: &str) {
        let Self { screens, view, .. } = self;
        if let Some((state, write)) = screens.shown().views().at(path) {
            view.apply(state, write);
        }
        self.turn();
    }

    /// Turns to the page a shot names, as freshly as the retained host mounts
    /// one: that host builds a page its own, so a page opens here at nothing
    /// on the clock and nothing behind it. Carrying the page before it over
    /// would photograph the two hosts at two different moments, and a film of
    /// a page would open wherever the page before it left off.
    pub(crate) fn select(&mut self, shot: Shot) {
        self.clock = Clock::default();
        self.reads = DemoReads::default();
        self.view = shot.standing();
        self.turn();
    }

    /// The skin the gallery is dressed in, read off the same state every page
    /// is read from, so turning a page cannot undress it.
    pub(crate) fn skin(&self) -> &'static Skin {
        self.reads.skin()
    }

    /// One frame of the gallery's own time: the clock a document binds to,
    /// and the application's own reading of how far along it is.
    pub(crate) fn tick(&mut self) {
        self.clock = self.clock.advance(self.step);
        self.reads.tick();
    }

    /// Shows the page the screen's state stands at, and tells the demo model
    /// which page that is: a page with a feed of its own is fed only while it
    /// is the page on screen.
    fn turn(&mut self) {
        let Self {
            reads,
            screens,
            view,
            ..
        } = self;
        let resolver = resolver();
        let endpoints = crate::demo::registry();
        let skin = reads.skin().document();
        screens
            .show(view, || {
                Ok::<_, Infallible>(compiled(&resolver, &endpoints, skin, view))
            })
            .unwrap_or_else(|never| match never {});
        let page = screens.shown().views().standing(view, sections::PAGE);
        if let Some(page) = page.and_then(sections::named) {
            reads.show(page);
        }
    }
}

pub(crate) fn update(state: &mut Gallery, message: Message) -> Task<Message> {
    match message {
        Message::Tick => {
            state.tick();
            Task::none()
        }
        Message::Ui(UiEvent::Control { path, action }) => {
            if matches!(action, ControlAction::Activate) {
                state.press(&path);
            }
            let was = state.reads.active_skin();
            state.reads.apply(&path, &action);
            if state.reads.active_skin() != was {
                state.dress();
            }
            Task::none()
        }
        Message::Ui(UiEvent::LibraryQuery(query)) => {
            state.reads.set_library_query(query);
            Task::none()
        }
        Message::Ui(UiEvent::ToggleModule(module)) => {
            state.reads.toggle_module(module);
            Task::none()
        }
        Message::Ui(UiEvent::Window(command)) => match command {
            WindowCommand::Drag => window::drag(state.window_id),
            WindowCommand::Minimize => window::minimize(state.window_id, true),
            WindowCommand::ToggleMaximize => window::toggle_maximize(state.window_id),
            WindowCommand::Close => iced::exit(),
            _ => Task::none(),
        },
        Message::Ui(_) => Task::none(),
        Message::CaptureNext => state.capture_next(),
        Message::CaptureShoot(shot) => {
            window::screenshot(state.window_id).map(move |image| Message::CaptureSave(shot, image))
        }
        Message::CaptureSave(shot, image) => state.capture_save(shot, &image),
    }
}

pub(crate) fn view(state: &Gallery, _window: Id) -> Element<'_, Message> {
    tree::render(
        &state.compiled().root,
        state.compiled(),
        &state.reads,
        &state.view,
        state.skin(),
        state.clock,
        Some(&state.kinds),
    )
    .map(Message::Ui)
}

/// The gallery's screen as it stands for `view`.
fn compiled(
    resolver: &Resolver,
    endpoints: &dyn EndpointRegistry,
    skin: &SkinDoc,
    view: &ViewState,
) -> CompiledUi {
    let entry = sections::entry();
    compile(
        entry,
        resolver,
        endpoints,
        skin,
        builtin::text_doc(),
        crate::custom::config(),
        view,
    )
    .unwrap_or_else(|error| panic!("the gallery document {entry} must compile: {error}"))
}

pub(crate) fn theme(skin: &Skin) -> Theme {
    let palette = skin.palette;
    Theme::custom(
        "Kithara".to_owned(),
        theme::Palette {
            background: palette.bg.into(),
            text: palette.text.into(),
            primary: palette.accent.into(),
            success: palette.success.into(),
            danger: palette.danger.into(),
            warning: palette.warning.into(),
        },
    )
}
