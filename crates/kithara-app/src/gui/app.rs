use std::{
    sync::{Arc, Mutex},
    time::Duration,
};

use iced::{
    Event as IcedEvent, Subscription, Task, Theme, event,
    keyboard::{Event as KeyboardEvent, Key, key::Named},
    time as iced_time,
};
use kithara::prelude::{Event, HlsEvent};
use kithara_queue::{Queue, QueueEvent, TrackEntry};
use tokio::sync::broadcast::error::RecvError;
use tracing::trace;

use super::{
    message::{Message, Tab},
    subscription::subscription_config,
    theme,
};
use crate::theme::gui;

/// Main GUI application state.
pub(crate) struct Kithara {
    pub(crate) queue: Arc<Queue>,
    pub(crate) palette: gui::GuiPalette,
    pub(crate) tracks_snapshot: Vec<TrackEntry>,

    // Playback state (synced from player on each tick).
    pub(crate) playing: bool,
    pub(crate) position: f32,
    pub(crate) duration: f32,
    pub(crate) volume: f32,

    // Seek state.
    pub(crate) seek_position: f32,
    pub(crate) is_seeking: bool,

    // EQ band gains in dB (one per band from eq_layout).
    pub(crate) eq_bands: Vec<f32>,

    // Playback rate.
    pub(crate) selected_rate: f32,

    // Crossfade duration in seconds.
    pub(crate) crossfade: f32,

    // Track info.
    pub(crate) current_track_index: Option<usize>,
    /// Row highlighted by a single click — second click on same row
    /// commits playback. `None` when nothing is focused.
    pub(crate) selected_track_index: Option<usize>,
    pub(crate) track_name: String,
    pub(crate) variant_label: String,
    pub(crate) shared_variant_label: Arc<Mutex<String>>,
    pub(crate) shared_abr_variants: Arc<Mutex<Vec<(usize, String)>>>,
    pub(crate) abr_variants: Vec<(usize, String)>,
    pub(crate) abr_mode_is_auto: bool,
    /// Variant index selected by user in picker (`None` = auto).
    /// Updated immediately on click. Separate from `variant_label`
    /// which reflects the actually applied variant from `VariantApplied` event.
    pub(crate) selected_variant: Option<usize>,

    // UI state.
    pub(crate) active_tab: Tab,
    pub(crate) shuffle_enabled: bool,
    pub(crate) repeat_enabled: bool,
    pub(crate) blink_counter: u8,
}

impl Kithara {
    /// Boot function for `iced::application()`.
    pub(crate) fn new(
        queue: Arc<Queue>,
        palette: gui::GuiPalette,
        config: &crate::config::AppConfig,
    ) -> (Self, Task<Message>) {
        // Feed tracks from inside iced's tokio runtime — `Loader::spawn_load`
        // uses `tokio::spawn`, which requires a running reactor.
        queue.set_tracks(crate::sources::build_sources(config));

        let volume = queue.volume();
        let crossfade = queue.crossfade_duration();
        let eq_band_count = queue.eq_band_count();

        let tracks_snapshot = queue.tracks();
        let current_track_index = tracks_snapshot.first().map(|_| 0usize);
        let track_name = tracks_snapshot
            .first()
            .map(|e| e.name.clone())
            .unwrap_or_default();

        let state = Self {
            queue,
            palette,
            tracks_snapshot,
            playing: false,
            position: 0.0,
            duration: 0.0,
            volume,
            seek_position: 0.0,
            is_seeking: false,
            eq_bands: vec![0.0; eq_band_count],
            selected_rate: 1.0,
            crossfade,
            current_track_index,
            selected_track_index: None,
            track_name,
            variant_label: String::new(),
            shared_variant_label: Arc::new(Mutex::new(String::new())),
            shared_abr_variants: Arc::new(Mutex::new(Vec::new())),
            abr_variants: Vec::new(),
            abr_mode_is_auto: true,
            selected_variant: None,
            active_tab: Tab::Playlist,
            shuffle_enabled: false,
            repeat_enabled: false,
            blink_counter: 0,
        };

        start_event_logging(&state.queue);
        start_variant_listener(
            &state.queue,
            Arc::clone(&state.shared_variant_label),
            Arc::clone(&state.shared_abr_variants),
        );

        (state, Task::none())
    }

    /// The dark + gold theme.
    pub(crate) fn theme(&self) -> Theme {
        theme::kithara_theme(&self.palette)
    }

    /// Time-tick subscription for player state sync plus keyboard. Tick
    /// interval scales with playback state to save CPU while idle — see
    /// [`subscription_config`] for rationale.
    pub(crate) fn subscription(&self) -> Subscription<Message> {
        let cfg = subscription_config(self.playing);
        let mut subs = Vec::with_capacity(2);
        subs.push(
            iced_time::every(Duration::from_millis(cfg.tick_interval_ms)).map(|_| Message::Tick),
        );
        if cfg.keyboard {
            subs.push(event::listen_with(|e, _status, _window| match e {
                IcedEvent::Keyboard(KeyboardEvent::KeyPressed {
                    key: Key::Named(Named::Delete | Named::Backspace),
                    ..
                }) => Some(Message::DeleteTrack),
                _ => None,
            }));
        }
        Subscription::batch(subs)
    }
}

/// Spawn background task that logs player and engine events.
fn start_event_logging(queue: &Queue) {
    let mut rx = queue.subscribe();

    tokio::spawn(async move {
        loop {
            match rx.recv().await {
                Ok(event) => trace!("{event:?}"),
                Err(RecvError::Lagged(n)) => trace!("events lagged: {n}"),
                Err(RecvError::Closed) => break,
            }
        }
    });
}

/// Spawn background task that tracks HLS variant changes for the GUI label.
fn start_variant_listener(
    queue: &Queue,
    variant_label: Arc<Mutex<String>>,
    shared_variants: Arc<Mutex<Vec<(usize, String)>>>,
) {
    let mut rx = queue.subscribe();

    tokio::spawn(async move {
        let mut variants: Vec<kithara::abr::VariantInfo> = Vec::new();
        loop {
            match rx.recv().await {
                Ok(Event::Hls(HlsEvent::VariantsDiscovered {
                    variants: v,
                    initial_variant,
                })) => {
                    variants.clone_from(&v);
                    let text = variant_display_label(&variants, initial_variant);
                    if let Ok(mut l) = variant_label.lock() {
                        *l = text;
                    }
                    let gui_variants: Vec<(usize, String)> = variants
                        .iter()
                        .map(|vi| {
                            let label = vi.name.clone().unwrap_or_else(|| {
                                vi.bandwidth_bps.map_or_else(
                                    || format!("v{}", vi.index),
                                    |b| format!("{}k", b / 1000),
                                )
                            });
                            (vi.index, label)
                        })
                        .collect();
                    if let Ok(mut sv) = shared_variants.lock() {
                        *sv = gui_variants;
                    }
                }
                Ok(Event::Hls(HlsEvent::VariantApplied { to_variant, .. })) => {
                    let text = variant_display_label(&variants, to_variant);
                    if let Ok(mut l) = variant_label.lock() {
                        *l = text;
                    }
                }
                Ok(Event::Queue(QueueEvent::CurrentTrackChanged { .. })) => {
                    // Reset variant cache — a new track may have its own variants.
                    variants.clear();
                    if let Ok(mut l) = variant_label.lock() {
                        l.clear();
                    }
                    if let Ok(mut sv) = shared_variants.lock() {
                        sv.clear();
                    }
                }
                Ok(_) => {}
                Err(RecvError::Lagged(_)) => continue,
                Err(RecvError::Closed) => break,
            }
        }
    });
}

fn variant_display_label(variants: &[kithara::abr::VariantInfo], index: usize) -> String {
    variants.get(index).map_or_else(
        || format!("variant {index}"),
        |v| {
            v.name.clone().unwrap_or_else(|| {
                v.bandwidth_bps.map_or_else(
                    || format!("variant {index}"),
                    |b| format!("{} kbps", b / 1000),
                )
            })
        },
    )
}
