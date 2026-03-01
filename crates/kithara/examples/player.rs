//! Play with `kithara-play`: load multiple tracks and crossfade between them.
//!
//! ```
//! cargo run -p kithara --example player [URL1] [URL2] [URL3] ...
//! ```
//!
//! Controls:
//! - 1-9: switch to track by number (with crossfade)
//! - Left/Right arrows: seek ±5 seconds
//! - Up/Down arrows: volume ±5%
//! - Ctrl+C or q: stop playback
//!
//! Auto-advances to the next track when remaining time ≤ crossfade duration.

// ratatui + tokio::signal are unavailable on wasm32; this example is native-only.
#[cfg(not(target_arch = "wasm32"))]
#[path = "common/player_tui.rs"]
mod player_tui;
#[cfg(not(target_arch = "wasm32"))]
#[path = "common/tracing.rs"]
mod tracing_support;

#[cfg(not(target_arch = "wasm32"))]
mod app {
    use std::{
        env::args,
        error::Error,
        sync::{
            Arc,
            mpsc::{self, TryRecvError},
        },
        time::{Duration, Instant},
    };

    use kithara::{
        play::{Engine, EngineEvent, PlayerEvent},
        prelude::*,
    };
    use ratatui::crossterm::event::{
        self, Event as TermEvent, KeyCode, KeyEventKind, KeyModifiers,
    };
    use tokio::sync::{broadcast, broadcast::error::RecvError};
    use tracing::warn;

    const CROSSFADE_SECONDS: f32 = 5.0;
    const FILE_URL_DEFAULT: &str = "https://stream.silvercomet.top/track.mp3";
    const HLS_URL_DEFAULT: &str = "https://stream.silvercomet.top/hls/master.m3u8";
    const CONTROL_POLL_MS: u64 = 100;
    const SEEK_STEP_SECONDS: i64 = 5;
    const SEEK_STEP_SECONDS_F64: f64 = 5.0;
    const VOLUME_STEP: f32 = 0.05;
    const PROGRESS_LOG_INTERVAL: Duration = Duration::from_secs(1);

    pub(crate) type ExampleError = Box<dyn Error + Send + Sync>;
    pub(crate) type ExampleResult<T = ()> = Result<T, ExampleError>;

    enum UiMsg {
        Engine(EngineEvent),
        Note(String),
        Player(PlayerEvent),
        Source { event: Event, source: String },
    }

    struct CrossfadeClock {
        duration: Duration,
        started_at: Instant,
    }

    impl CrossfadeClock {
        fn new(duration_secs: f32) -> Self {
            Self {
                duration: Duration::from_secs_f32(duration_secs.max(0.0)),
                started_at: Instant::now(),
            }
        }

        fn progress(&self) -> f32 {
            if self.duration.is_zero() {
                return 1.0;
            }
            let progress = self.started_at.elapsed().as_secs_f32() / self.duration.as_secs_f32();
            progress.clamp(0.0, 1.0)
        }
    }

    struct ProgressLog {
        last_emit: Instant,
    }

    impl ProgressLog {
        fn new() -> Self {
            Self {
                last_emit: Instant::now() - PROGRESS_LOG_INTERVAL,
            }
        }

        fn should_emit(&mut self) -> bool {
            let now = Instant::now();
            if now.duration_since(self.last_emit) < PROGRESS_LOG_INTERVAL {
                return false;
            }
            self.last_emit = now;
            true
        }
    }

    enum ControlOutcome {
        Continue(Option<String>),
        SwitchTrack(usize),
        Quit,
    }

    fn apply_seek(
        player: &PlayerImpl,
        delta_seconds: f64,
        dashboard: &mut super::player_tui::Dashboard,
    ) -> String {
        let current = player.position_seconds().unwrap_or(0.0).max(0.0);
        let target = (current + delta_seconds).max(0.0);
        match player.seek_seconds(target) {
            Ok(()) => {
                dashboard.set_note(format!("seek {}", format_seconds(target)));
                dashboard.set_position(Duration::from_secs_f64(target));
                format!("seek target={}", format_seconds(target))
            }
            Err(err) => {
                let line = format!("seek failed target={} err={err}", format_seconds(target));
                dashboard.set_note(line.clone());
                line
            }
        }
    }

    fn apply_volume(player: &PlayerImpl, delta: f32, dashboard: &mut super::player_tui::Dashboard) {
        let volume = (player.volume() + delta).clamp(0.0, 1.0);
        player.set_volume(volume);
        dashboard.set_volume(volume);
        dashboard.set_note(format!("volume {:.0}%", volume * 100.0));
    }

    fn format_seconds(seconds: f64) -> String {
        let whole = if seconds.is_finite() && seconds > 0.0 {
            Duration::from_secs_f64(seconds).as_secs()
        } else {
            0
        };
        let minutes = whole / 60;
        let secs = whole % 60;
        format!("{minutes:02}:{secs:02}")
    }

    fn handle_key(
        key: KeyCode,
        modifiers: KeyModifiers,
        player: &PlayerImpl,
        dashboard: &mut super::player_tui::Dashboard,
    ) -> ControlOutcome {
        if modifiers.contains(KeyModifiers::CONTROL) && matches!(key, KeyCode::Char('c')) {
            return ControlOutcome::Quit;
        }
        match key {
            KeyCode::Left => ControlOutcome::Continue(Some(apply_seek(
                player,
                -SEEK_STEP_SECONDS_F64,
                dashboard,
            ))),
            KeyCode::Right => {
                ControlOutcome::Continue(Some(apply_seek(player, SEEK_STEP_SECONDS_F64, dashboard)))
            }
            KeyCode::Up => {
                apply_volume(player, VOLUME_STEP, dashboard);
                ControlOutcome::Continue(None)
            }
            KeyCode::Down => {
                apply_volume(player, -VOLUME_STEP, dashboard);
                ControlOutcome::Continue(None)
            }
            KeyCode::Char('q') => ControlOutcome::Quit,
            KeyCode::Char(c @ '1'..='9') => {
                let index = (c as usize) - ('1' as usize);
                ControlOutcome::SwitchTrack(index)
            }
            _ => ControlOutcome::Continue(None),
        }
    }

    fn is_progress_event(event: &Event) -> bool {
        matches!(
            event,
            Event::Audio(AudioEvent::PlaybackProgress { .. })
                | Event::File(FileEvent::DownloadProgress { .. })
                | Event::File(FileEvent::ByteProgress { .. })
                | Event::File(FileEvent::PlaybackProgress { .. })
                | Event::Hls(HlsEvent::DownloadProgress { .. })
                | Event::Hls(HlsEvent::ByteProgress { .. })
                | Event::Hls(HlsEvent::PlaybackProgress { .. })
        )
    }

    fn source_note(source: &str, event: &Event) -> Option<String> {
        match event {
            Event::Audio(AudioEvent::FormatDetected { spec }) => Some(format!(
                "{source} fmt {}ch {}Hz",
                spec.channels, spec.sample_rate
            )),
            Event::Audio(AudioEvent::SeekComplete { position, .. }) => Some(format!(
                "{source} seek {}",
                format_seconds(position.as_secs_f64())
            )),
            Event::Hls(HlsEvent::VariantApplied {
                to_variant, reason, ..
            }) => Some(format!("{source} abr v{to_variant} {reason:?}")),
            Event::File(FileEvent::DownloadComplete { total_bytes }) => {
                Some(format!("{source} dl done {total_bytes} bytes"))
            }
            _ => None,
        }
    }

    fn track_name(url: &str) -> String {
        url.rsplit('/').next().unwrap_or(url).to_string()
    }

    fn refresh_dashboard(dashboard: &mut super::player_tui::Dashboard, player: &PlayerImpl) {
        dashboard.set_playing(player.is_playing());
        dashboard.set_queue(player.current_index(), player.item_count());
        dashboard.set_volume(player.volume());

        let position = player.position_seconds();
        if let Some(position) = position.filter(|seconds| seconds.is_finite() && *seconds >= 0.0) {
            dashboard.set_position(Duration::from_secs_f64(position));
        } else {
            dashboard.set_position(Duration::ZERO);
        }

        let total = player.duration_seconds().and_then(|seconds| {
            if !seconds.is_finite() || seconds <= 0.0 {
                return None;
            }
            Some(Duration::from_secs_f64(seconds))
        });
        dashboard.set_total(total);
    }

    fn forward_player_events(
        mut rx: broadcast::Receiver<PlayerEvent>,
        tx: mpsc::Sender<UiMsg>,
    ) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            loop {
                match rx.recv().await {
                    Ok(event) => {
                        if tx.send(UiMsg::Player(event)).is_err() {
                            break;
                        }
                    }
                    Err(RecvError::Lagged(n)) => {
                        if tx
                            .send(UiMsg::Note(format!("player events lagged n={n}")))
                            .is_err()
                        {
                            break;
                        }
                    }
                    Err(RecvError::Closed) => break,
                }
            }
        })
    }

    fn forward_engine_events(
        mut rx: broadcast::Receiver<EngineEvent>,
        tx: mpsc::Sender<UiMsg>,
    ) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            loop {
                match rx.recv().await {
                    Ok(event) => {
                        if tx.send(UiMsg::Engine(event)).is_err() {
                            break;
                        }
                    }
                    Err(RecvError::Lagged(n)) => {
                        if tx
                            .send(UiMsg::Note(format!("engine events lagged n={n}")))
                            .is_err()
                        {
                            break;
                        }
                    }
                    Err(RecvError::Closed) => break,
                }
            }
        })
    }

    fn forward_source_events(
        mut rx: broadcast::Receiver<Event>,
        source: String,
        tx: mpsc::Sender<UiMsg>,
    ) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            loop {
                match rx.recv().await {
                    Ok(event) => {
                        if tx
                            .send(UiMsg::Source {
                                event,
                                source: source.clone(),
                            })
                            .is_err()
                        {
                            break;
                        }
                    }
                    Err(RecvError::Lagged(n)) => {
                        if tx
                            .send(UiMsg::Note(format!("{source} events lagged n={n}")))
                            .is_err()
                        {
                            break;
                        }
                    }
                    Err(RecvError::Closed) => break,
                }
            }
        })
    }

    fn switch_track(
        player: &PlayerImpl,
        index: usize,
        urls: &[String],
        ui_tx: &mpsc::Sender<UiMsg>,
        ui: &mut super::player_tui::UiSession,
        crossfade_clock: &mut Option<CrossfadeClock>,
        auto_advanced_index: &mut Option<usize>,
    ) -> ExampleResult {
        // Resource is consumed on first load (.take()), so recreate it for replay.
        let handle = tokio::runtime::Handle::current();
        let url = &urls[index];
        let resource = handle.block_on(async {
            let config = ResourceConfig::new(url)?;
            Resource::new(config).await
        })?;
        let events = resource.subscribe();
        player.replace_item(index, resource);

        // Forward events from the fresh resource.
        let label = format!("src{}", index + 1);
        let tx = ui_tx.clone();
        handle.spawn(async move {
            let mut rx = events;
            loop {
                match rx.recv().await {
                    Ok(event) => {
                        if tx
                            .send(UiMsg::Source {
                                event,
                                source: label.clone(),
                            })
                            .is_err()
                        {
                            break;
                        }
                    }
                    Err(RecvError::Lagged(_)) => {}
                    Err(RecvError::Closed) => break,
                }
            }
        });

        match player.select_item(index, true) {
            Ok(()) => {
                *crossfade_clock = Some(CrossfadeClock::new(player.crossfade_duration()));
                ui.dashboard.set_crossfade_progress(Some(0.0));
                *auto_advanced_index = None;
                let note = format!(
                    "crossfade to #{} ({:.1}s)",
                    index + 1,
                    player.crossfade_duration()
                );
                ui.dashboard.set_note(&note);
                ui.log_line(&note)?;
            }
            Err(e) => {
                let note = format!("switch failed: {e}");
                ui.dashboard.set_note(&note);
                ui.log_line(&note)?;
            }
        }
        Ok(())
    }

    fn run_ui_loop(
        player: &PlayerImpl,
        urls: Vec<String>,
        track_names: Vec<String>,
        ui_tx: mpsc::Sender<UiMsg>,
        ui_rx: &mpsc::Receiver<UiMsg>,
        stop_rx: &mpsc::Receiver<()>,
    ) -> ExampleResult {
        let track_count = track_names.len();
        let dashboard = super::player_tui::Dashboard::new(track_names);
        let mut ui = super::player_tui::UiSession::new(dashboard)?;
        ui.log_line(&format!(
            "controls: 1-{} select track, Left/Right seek {}s, Up/Down vol {:+.0}%",
            track_count.min(9),
            SEEK_STEP_SECONDS,
            VOLUME_STEP * 100.0
        ))?;
        ui.log_line("auto-advances to next track with crossfade near end of each track")?;
        ui.draw()?;

        let mut progress_log = ProgressLog::new();
        let mut crossfade_clock: Option<CrossfadeClock> = None;
        let mut auto_advanced_index: Option<usize> = None;
        let mut tick_error_logged = false;

        'ui: loop {
            match stop_rx.try_recv() {
                Ok(()) | Err(TryRecvError::Disconnected) => break,
                Err(TryRecvError::Empty) => {}
            }

            loop {
                match ui_rx.try_recv() {
                    Ok(msg) => match msg {
                        UiMsg::Engine(event) => {
                            if let EngineEvent::MasterVolumeChanged { volume } = event {
                                ui.dashboard.set_volume(volume);
                            }
                            ui.log_line(&format!("engine {event:?}"))?;
                        }
                        UiMsg::Note(note) => {
                            ui.dashboard.set_note(note.clone());
                            ui.log_line(&note)?;
                        }
                        UiMsg::Player(event) => {
                            match event {
                                PlayerEvent::CurrentItemChanged => {
                                    ui.dashboard.set_note("track changed");
                                }
                                PlayerEvent::RateChanged { rate } => {
                                    ui.dashboard.set_playing(rate > 0.0);
                                    ui.dashboard.set_note(format!("rate {rate:.2}"));
                                }
                                PlayerEvent::StatusChanged { status } => {
                                    ui.dashboard.set_note(format!("status {status:?}"));
                                }
                                PlayerEvent::VolumeChanged { volume } => {
                                    ui.dashboard.set_volume(volume);
                                    ui.dashboard
                                        .set_note(format!("volume {:.0}%", volume * 100.0));
                                }
                                _ => {}
                            }
                            ui.log_line(&format!("player {event:?}"))?;
                        }
                        UiMsg::Source { event, source } => {
                            if let Some(note) = source_note(&source, &event) {
                                ui.dashboard.set_note(note);
                            }
                            if is_progress_event(&event) {
                                if progress_log.should_emit() {
                                    ui.log_line(&format!("{source} {event:?}"))?;
                                }
                            } else {
                                ui.log_line(&format!("{source} {event:?}"))?;
                            }
                        }
                    },
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => break 'ui,
                }
            }

            match player.tick() {
                Ok(()) => {
                    tick_error_logged = false;
                }
                Err(err) => {
                    if !tick_error_logged {
                        ui.log_line(&format!("tick failed: {err}"))?;
                        ui.dashboard.set_note(format!("tick failed: {err}"));
                        tick_error_logged = true;
                    }
                }
            }

            refresh_dashboard(&mut ui.dashboard, player);

            // Crossfade progress
            if let Some(clock) = crossfade_clock.as_ref() {
                let progress = clock.progress();
                ui.dashboard.set_crossfade_progress(Some(progress));
                if progress >= 1.0 {
                    crossfade_clock = None;
                }
            } else {
                ui.dashboard.set_crossfade_progress(None);
            }

            // Auto-advance: crossfade to next track when remaining time ≤ crossfade duration
            let crossfade_secs = f64::from(player.crossfade_duration());
            if let (Some(pos), Some(dur)) = (player.position_seconds(), player.duration_seconds()) {
                if dur > crossfade_secs && pos >= dur - crossfade_secs {
                    let current = player.current_index();
                    let not_yet_advanced = auto_advanced_index != Some(current);
                    if not_yet_advanced && current + 1 < player.item_count() {
                        auto_advanced_index = Some(current);
                        let next = current + 1;
                        switch_track(
                            player,
                            next,
                            &urls,
                            &ui_tx,
                            &mut ui,
                            &mut crossfade_clock,
                            &mut auto_advanced_index,
                        )?;
                        // Restore guard (switch_track resets it for manual switches)
                        auto_advanced_index = Some(current);
                    }
                }
            }

            if event::poll(Duration::from_millis(CONTROL_POLL_MS))? {
                match event::read()? {
                    TermEvent::Key(key)
                        if matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) =>
                    {
                        match handle_key(key.code, key.modifiers, player, &mut ui.dashboard) {
                            ControlOutcome::Continue(Some(line)) => ui.log_line(&line)?,
                            ControlOutcome::Continue(None) => {}
                            ControlOutcome::SwitchTrack(index) => {
                                if index < player.item_count() {
                                    switch_track(
                                        player,
                                        index,
                                        &urls,
                                        &ui_tx,
                                        &mut ui,
                                        &mut crossfade_clock,
                                        &mut auto_advanced_index,
                                    )?;
                                }
                            }
                            ControlOutcome::Quit => break,
                        }
                    }
                    TermEvent::Resize(_, _) => {
                        ui.on_resize()?;
                    }
                    _ => {}
                }
            }

            ui.draw()?;
        }

        Ok(())
    }

    #[tokio::main(flavor = "current_thread")]
    #[cfg_attr(feature = "perf", hotpath::main)]
    pub(crate) async fn main() -> ExampleResult {
        super::tracing_support::init_tracing(&["off"], true)?;

        let urls: Vec<String> = {
            let cli: Vec<String> = args().skip(1).collect();
            if cli.is_empty() {
                vec![FILE_URL_DEFAULT.to_string(), HLS_URL_DEFAULT.to_string()]
            } else {
                cli
            }
        };

        let player = Arc::new(PlayerImpl::new(
            PlayerConfig::default().with_crossfade_duration(CROSSFADE_SECONDS),
        ));

        let (ui_tx, ui_rx) = mpsc::channel::<UiMsg>();
        let (stop_tx, stop_rx) = mpsc::channel::<()>();

        let mut forwarders = vec![
            forward_player_events(player.subscribe(), ui_tx.clone()),
            forward_engine_events(player.engine().subscribe(), ui_tx.clone()),
        ];

        let mut track_names: Vec<String> = Vec::new();
        for (i, url) in urls.iter().enumerate() {
            let name = track_name(url);
            let config = match ResourceConfig::new(url) {
                Ok(c) => c,
                Err(err) => {
                    warn!(?err, url, "invalid URL, skipping");
                    continue;
                }
            };
            match Resource::new(config).await {
                Ok(resource) => {
                    let events = resource.subscribe();
                    player.insert(resource, None);
                    let label = format!("src{}", i + 1);
                    forwarders.push(forward_source_events(events, label, ui_tx.clone()));
                    track_names.push(name);
                    let _ = ui_tx.send(UiMsg::Note(format!("loaded #{}: {url}", i + 1)));
                }
                Err(err) => {
                    warn!(?err, url, "failed to load resource, skipping");
                    let _ = ui_tx.send(UiMsg::Note(format!("skip #{}: {err}", i + 1)));
                }
            }
        }

        if track_names.is_empty() {
            return Err("no tracks loaded".into());
        }

        player.play();

        let player_for_ui = Arc::clone(&player);
        let ui_tx_for_loop = ui_tx.clone();
        let urls_for_loop = urls.clone();
        let mut ui_handle = tokio::task::spawn_blocking(move || {
            run_ui_loop(
                &player_for_ui,
                urls_for_loop,
                track_names,
                ui_tx_for_loop,
                &ui_rx,
                &stop_rx,
            )
        });

        let ui_finished = tokio::select! {
            result = &mut ui_handle => {
                result??;
                true
            }
            signal = tokio::signal::ctrl_c() => {
                if signal.is_ok() {
                    let _ = ui_tx.send(UiMsg::Note("ctrl+c received".to_string()));
                }
                false
            }
        };

        let _ = stop_tx.send(());
        if !ui_finished {
            ui_handle.await??;
        }

        player.pause();

        for forwarder in forwarders {
            forwarder.abort();
            let _ = forwarder.await;
        }

        Ok(())
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn main() -> app::ExampleResult {
    app::main()
}

#[cfg(target_arch = "wasm32")]
fn main() {}
