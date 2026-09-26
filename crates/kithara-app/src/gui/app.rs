use arc_swap::ArcSwap;
use iced::{
    Color, Event as IcedEvent, Subscription, Task, Theme, event,
    event::Status,
    keyboard::Event as KeyboardEvent,
    theme::{Base, Style},
    time as iced_time, window,
};
use kithara::platform::{sync::Arc, time::Duration, tokio::sync::mpsc::UnboundedSender};
use tracing::warn;

use super::{
    frontend::{Boot, window_settings},
    message::Message,
    overlay::Overlay,
    subscription,
    subscription::subscription_config,
    theme,
    ui::AppUi,
};
use crate::{
    catalog::Catalog,
    engine::{Command, EngineSnapshot, Envelope},
    theme::gui,
};

/// Main GUI application state.
pub(crate) struct Kithara {
    /// The compiled UI and its host-owned view state.
    pub(crate) ui: AppUi,
    pub(crate) snapshot: Arc<EngineSnapshot>,
    /// The app's track list; decks load from it.
    pub(crate) catalog: Catalog,
    pub(crate) palette: gui::GuiPalette,
    /// The app window; window-chrome commands execute against it.
    pub(crate) window_id: window::Id,
    /// Highlighted catalog row, shared by every deck's load buttons.
    pub(crate) selected_track: Option<usize>,
    published: Arc<EngineSnapshot>,
    snapshots: Arc<ArcSwap<EngineSnapshot>>,
    overlay: Overlay,
    commands: UnboundedSender<Envelope>,
    seq: u64,
}

impl Kithara {
    /// Boot function for `iced::daemon()`. Opens the app window.
    pub(crate) fn new(boot: Boot) -> (Self, Task<Message>) {
        let (window_id, open) = window::open(window_settings(boot.ui.window_min()));
        (Self::mounted(boot, window_id), open.discard())
    }

    /// The same state without a window of iced's: a host that owns its own
    /// window mounts the application through here.
    pub(crate) fn mounted(boot: Boot, window_id: window::Id) -> Self {
        let published = boot.snapshots.load_full();
        let mut state = Self {
            snapshot: Arc::clone(&published),
            published,
            snapshots: boot.snapshots,
            commands: boot.commands,
            catalog: boot.catalog,
            ui: boot.ui,
            palette: boot.palette.into(),
            window_id,
            overlay: Overlay::default(),
            selected_track: None,
            seq: 0,
        };
        state.refresh();
        state
    }

    pub(crate) fn refresh(&mut self) {
        self.published = self.snapshots.load_full();
        self.overlay.retire(self.published.applied_seq);
        self.snapshot = self.overlay.over(&self.published);
        self.ui.cache.refresh(&self.snapshot, &self.catalog);
    }

    pub(crate) fn send(&mut self, command: Command) {
        self.seq += 1;
        self.overlay.record(self.seq, &command);
        let envelope = Envelope {
            command,
            seq: self.seq,
        };
        if let Err(error) = self.commands.send(envelope) {
            warn!(seq = error.0.seq, "the engine stopped taking commands");
        }
    }

    /// The window paints no ground of its own: the document lays down the page
    /// in the shape the skin gives the window, and whatever the shape leaves
    /// out is the desktop behind it.
    pub(crate) fn style(_state: &Self, theme: &Theme) -> Style {
        Style {
            background_color: Color::TRANSPARENT,
            text_color: theme.base().text_color,
        }
    }

    /// Time-tick subscription for player state sync plus keyboard. Tick
    /// interval scales with playback state to save CPU while idle.
    pub(crate) fn subscription(&self) -> Subscription<Message> {
        const SUBSCRIPTION_CAPACITY: usize = 4;
        let playing = self.snapshot.decks.iter().any(|deck| deck.playing);
        let cfg = subscription_config(playing);
        let mut subs: Vec<Subscription<Message>> = Vec::with_capacity(SUBSCRIPTION_CAPACITY);
        subs.push(
            iced_time::every(Duration::from_millis(cfg.tick_interval_ms)).map(|_| Message::Tick),
        );
        subs.push(window::close_requests().map(|_| Message::WindowCloseRequested));
        subs.push(window::resize_events().map(|(_, size)| Message::WindowResized(size)));
        if cfg.is_keyboard_enabled {
            subs.push(event::listen_with(|e, status, _window| match e {
                IcedEvent::Keyboard(KeyboardEvent::KeyPressed {
                    ref key, modifiers, ..
                }) if status == Status::Ignored => subscription::shortcut(key, modifiers),
                _ => None,
            }));
        }
        Subscription::batch(subs)
    }

    /// The dark + gold theme.
    pub(crate) fn theme(&self, _window: window::Id) -> Theme {
        theme::kithara_theme(&self.palette)
    }

    /// Window title.
    pub(crate) fn title(_state: &Self, _window: window::Id) -> String {
        "Kithara".to_string()
    }
}
