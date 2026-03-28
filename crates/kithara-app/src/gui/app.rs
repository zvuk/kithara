use std::{
    sync::{Arc, Mutex},
    time::Duration,
};

use iced::{Subscription, Task, Theme, time as iced_time};
use kithara::{
    events::EventBus,
    prelude::{Event, HlsEvent, PlayerImpl, Resource},
};
use tokio::sync::broadcast::error::RecvError;
use tracing::info;

use super::{
    message::{Message, Tab},
    theme,
};
use crate::{
    controls::TrackLoadParams,
    playlist::{Playlist, TrackStatus},
    theme::gui,
};

/// Main GUI application state.
pub(crate) struct Kithara {
    pub(crate) player: Arc<PlayerImpl>,
    pub(crate) playlist: Arc<Playlist>,
    pub(crate) palette: gui::GuiPalette,
    pub(crate) load_params: TrackLoadParams,

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
        player: Arc<PlayerImpl>,
        playlist: Arc<Playlist>,
        load_params: TrackLoadParams,
        palette: gui::GuiPalette,
    ) -> (Self, Task<Message>) {
        let volume = player.volume();
        let crossfade = player.crossfade_duration();
        let eq_band_count = player.eq_band_count();

        // Reserve player slots for all tracks.
        player.reserve_slots(playlist.len());

        let mut state = Self {
            player,
            playlist,
            palette,
            load_params,
            playing: false,
            position: 0.0,
            duration: 0.0,
            volume,
            seek_position: 0.0,
            is_seeking: false,
            eq_bands: vec![0.0; eq_band_count],
            selected_rate: 1.0,
            crossfade,
            current_track_index: None,
            track_name: String::new(),
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

        // Start event logging inside iced's tokio runtime.
        start_event_logging(&state.player);

        // Set initial track name.
        if !state.playlist.is_empty() {
            state.current_track_index = Some(0);
            state.track_name = state.playlist.track_name(0);
            state.playlist.on_track_selected(0);
        }

        // Load all tracks. The first track uses load_track() which subscribes
        // to variant events for the GUI label/picker. Others use load_and_apply.
        let mut tasks = Vec::new();
        if !state.playlist.is_empty() {
            tasks.push(state.load_track(0));
        }
        for i in 1..state.playlist.len() {
            let params = state.load_params.clone();
            tasks.push(Task::perform(
                async move {
                    let ok = params.load_and_apply(i).await;
                    (i, ok)
                },
                |(index, ok)| {
                    if ok {
                        Message::TrackLoaded(index, Ok(()))
                    } else {
                        Message::TrackLoaded(index, Err(format!("load failed for #{}", index + 1)))
                    }
                },
            ));
        }

        (state, Task::batch(tasks))
    }

    /// The dark + gold theme.
    pub(crate) fn theme(&self) -> Theme {
        theme::kithara_theme(&self.palette)
    }

    /// 100 ms tick subscription for player state sync.
    #[expect(clippy::unused_self, reason = "iced requires &self method signature")]
    pub(crate) fn subscription(&self) -> Subscription<Message> {
        const TICK_INTERVAL_MS: u64 = 100;
        iced_time::every(Duration::from_millis(TICK_INTERVAL_MS)).map(|_| Message::Tick)
    }

    /// Load a track, select it for playback, and listen for variant events.
    pub(crate) fn load_track(&self, index: usize) -> Task<Message> {
        let params = self.load_params.clone();
        let player = Arc::clone(&self.player);
        let variant_label = Arc::clone(&self.shared_variant_label);
        let shared_variants = Arc::clone(&self.shared_abr_variants);
        Task::perform(
            async move {
                let config = params.build_config(index).map_err(|e| format!("{e}"))?;
                let event_rx = config.bus.as_ref().map(EventBus::subscribe);
                let resource = Resource::new(config).await.map_err(|e| format!("{e}"))?;
                player.replace_item(index, resource);
                params.playlist().set_status(index, TrackStatus::Loaded);
                player
                    .select_item(index, true)
                    .map_err(|e| format!("{e}"))?;

                // Listen for ABR variant changes in the background.
                if let Some(mut rx) = event_rx {
                    tokio::spawn(async move {
                        let mut variants = Vec::new();
                        while let Ok(event) = rx.recv().await {
                            match &event {
                                Event::Hls(HlsEvent::VariantsDiscovered {
                                    variants: v,
                                    initial_variant,
                                }) => {
                                    variants.clone_from(v);
                                    let text = variant_display_label(&variants, *initial_variant);
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
                                Event::Hls(HlsEvent::VariantApplied { to_variant, .. }) => {
                                    let text = variant_display_label(&variants, *to_variant);
                                    if let Ok(mut l) = variant_label.lock() {
                                        *l = text;
                                    }
                                }
                                _ => {}
                            }
                        }
                    });
                }

                Ok(())
            },
            move |result| Message::TrackLoaded(index, result),
        )
    }
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

/// Spawn background tasks that log player and engine events.
///
/// Must be called from within a tokio runtime (iced provides one).
fn start_event_logging(player: &Arc<PlayerImpl>) {
    let mut rx = player.subscribe();

    tokio::spawn(async move {
        loop {
            match rx.recv().await {
                Ok(event) => info!("{event:?}"),
                Err(RecvError::Lagged(n)) => info!("events lagged: {n}"),
                Err(RecvError::Closed) => break,
            }
        }
    });
}
