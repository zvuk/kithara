use std::{sync::Arc, time::Duration};

use iced::{Subscription, Task, Theme};
use kithara::{
    play::Engine,
    prelude::{PlayerImpl, Resource, ResourceConfig},
};
use tokio::sync::broadcast::error::RecvError;
use tracing::info;

use crate::{
    message::{Message, Tab},
    playlist::Playlist,
    theme,
};

/// Main application state.
pub(crate) struct Kithara {
    pub(crate) player: Arc<PlayerImpl>,
    pub(crate) playlist: Arc<Playlist>,

    // Playback state (synced from player on each tick).
    pub(crate) playing: bool,
    pub(crate) position: f32,
    pub(crate) duration: f32,
    pub(crate) volume: f32,

    // Seek state.
    pub(crate) seek_position: f32,
    pub(crate) is_seeking: bool,

    // EQ bands (Low / Mid / High), in dB.
    pub(crate) eq_bands: [f32; 3],

    // Crossfade duration in seconds.
    pub(crate) crossfade: f32,

    // Track info.
    pub(crate) current_track_index: Option<usize>,
    pub(crate) track_name: String,

    // UI state.
    pub(crate) active_tab: Tab,
    pub(crate) shuffle_enabled: bool,
    pub(crate) repeat_enabled: bool,
    pub(crate) autoplay: bool,
}

impl Kithara {
    /// Boot function for `iced::application()`.
    pub(crate) fn new(
        player: Arc<PlayerImpl>,
        tracks: Vec<String>,
        autoplay: bool,
    ) -> (Self, Task<Message>) {
        let playlist = Arc::new(Playlist::new(tracks));
        let volume = player.volume();
        let crossfade = player.crossfade_duration();

        let mut state = Self {
            player,
            playlist,
            playing: false,
            position: 0.0,
            duration: 0.0,
            volume,
            seek_position: 0.0,
            is_seeking: false,
            eq_bands: [0.0; 3],
            crossfade,
            current_track_index: None,
            track_name: String::new(),
            active_tab: Tab::Playlist,
            shuffle_enabled: false,
            repeat_enabled: false,
            autoplay,
        };

        // Start event logging inside iced's tokio runtime.
        start_event_logging(&state.player);

        // Initialize first track if available.
        let task = if !state.playlist.is_empty() {
            state.current_track_index = Some(0);
            state.track_name = state.playlist.track_name(0);
            state.playlist.on_track_selected(0);
            state.load_track(0)
        } else {
            Task::none()
        };

        (state, task)
    }

    /// The dark + gold theme.
    #[expect(clippy::unused_self, reason = "iced requires &self method signature")]
    pub(crate) fn theme(&self) -> Theme {
        theme::kithara_theme()
    }

    /// 100 ms tick subscription for player state sync.
    #[expect(clippy::unused_self, reason = "iced requires &self method signature")]
    pub(crate) fn subscription(&self) -> Subscription<Message> {
        iced::time::every(Duration::from_millis(100)).map(|_| Message::Tick)
    }

    /// Asynchronously load a track by playlist index.
    pub(crate) fn load_track(&self, index: usize) -> Task<Message> {
        let Some(path) = self.playlist.track_path(index) else {
            return Task::none();
        };

        let path = path.to_string();
        let player = Arc::clone(&self.player);
        let autoplay = self.playing || self.autoplay;

        Task::perform(
            async move {
                let config = ResourceConfig::new(&path).map_err(|e| format!("{e}"))?;
                let resource = Resource::new(config).await.map_err(|e| format!("{e}"))?;

                // Single-slot model: always use slot 0 to avoid index overflow.
                if player.item_count() > 0 {
                    player.replace_item(0, resource);
                } else {
                    player.insert(resource, None);
                }
                player
                    .select_item(0, autoplay)
                    .map_err(|e| format!("{e}"))?;
                Ok(())
            },
            Message::TrackLoaded,
        )
    }
}

/// Spawn background tasks that log player and engine events.
///
/// Must be called from within a tokio runtime (iced provides one).
fn start_event_logging(player: &Arc<PlayerImpl>) {
    let mut player_rx = player.subscribe();
    let mut engine_rx = player.engine().subscribe();

    tokio::spawn(async move {
        loop {
            tokio::select! {
                result = player_rx.recv() => match result {
                    Ok(event) => info!("[player] {event:?}"),
                    Err(RecvError::Lagged(n)) => info!("[player] events lagged: {n}"),
                    Err(RecvError::Closed) => break,
                },
                result = engine_rx.recv() => match result {
                    Ok(event) => info!("[engine] {event:?}"),
                    Err(RecvError::Lagged(n)) => info!("[engine] events lagged: {n}"),
                    Err(RecvError::Closed) => break,
                },
            }
        }
    });
}
