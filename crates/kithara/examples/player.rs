//! Play with `kithara-play`: start with a file URL, then crossfade to HLS after 30 seconds.
//!
//! ```
//! cargo run -p kithara --example player [FILE_URL] [HLS_URL]
//! ```
//!
//! Controls:
//! - Left/Right arrows: seek ±5 seconds
//! - Up/Down arrows: volume ±5%
//! - Ctrl+C: stop playback

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

    const SWITCH_AFTER: Duration = Duration::from_secs(30);
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
        CrossfadeStarted(Instant),
        Engine(EngineEvent),
        Note(String),
        Player(PlayerEvent),
        Source { event: Event, source: &'static str },
    }

    struct CrossfadeClock {
        duration: Duration,
        started_at: Instant,
    }

    impl CrossfadeClock {
        fn new(started_at: Instant) -> Self {
            Self {
                duration: Duration::from_secs_f32(CROSSFADE_SECONDS.max(0.0)),
                started_at,
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
        source: &'static str,
        tx: mpsc::Sender<UiMsg>,
    ) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            loop {
                match rx.recv().await {
                    Ok(event) => {
                        if tx.send(UiMsg::Source { event, source }).is_err() {
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

    fn run_ui_loop(
        player: &PlayerImpl,
        file_url: String,
        hls_url: String,
        ui_rx: &mpsc::Receiver<UiMsg>,
        stop_rx: &mpsc::Receiver<()>,
    ) -> ExampleResult {
        let dashboard = super::player_tui::Dashboard::new(file_url, hls_url);
        let mut ui = super::player_tui::UiSession::new(dashboard)?;
        ui.log_line(&format!(
            "controls: Left/Right seek {}s, Up/Down volume {:+.0}%",
            SEEK_STEP_SECONDS,
            VOLUME_STEP * 100.0
        ))?;
        ui.log_line("logs are appended above dashboard and remain scrollable")?;
        ui.draw()?;

        let mut progress_log = ProgressLog::new();
        let mut crossfade_clock: Option<CrossfadeClock> = None;
        let mut tick_error_logged = false;

        'ui: loop {
            match stop_rx.try_recv() {
                Ok(()) | Err(TryRecvError::Disconnected) => break,
                Err(TryRecvError::Empty) => {}
            }

            loop {
                match ui_rx.try_recv() {
                    Ok(msg) => match msg {
                        UiMsg::CrossfadeStarted(started_at) => {
                            crossfade_clock = Some(CrossfadeClock::new(started_at));
                            ui.dashboard.set_crossfade_progress(Some(0.0));
                            ui.dashboard
                                .set_note(format!("crossfade {:.1}s", CROSSFADE_SECONDS));
                        }
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
                                    ui.dashboard.set_note("queue advanced");
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
                            if let Some(note) = source_note(source, &event) {
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
            if let Some(clock) = crossfade_clock.as_ref() {
                let progress = clock.progress();
                ui.dashboard.set_crossfade_progress(Some(progress));
                if progress >= 1.0 {
                    crossfade_clock = None;
                }
            } else {
                ui.dashboard.set_crossfade_progress(None);
            }

            if event::poll(Duration::from_millis(CONTROL_POLL_MS))? {
                match event::read()? {
                    TermEvent::Key(key)
                        if matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) =>
                    {
                        match handle_key(key.code, key.modifiers, player, &mut ui.dashboard) {
                            ControlOutcome::Continue(Some(line)) => ui.log_line(&line)?,
                            ControlOutcome::Continue(None) => {}
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

        let mut cli_args = args().skip(1);
        let file_url = cli_args
            .next()
            .unwrap_or_else(|| FILE_URL_DEFAULT.to_string());
        let hls_url = cli_args
            .next()
            .unwrap_or_else(|| HLS_URL_DEFAULT.to_string());
        if cli_args.next().is_some() {
            warn!("Ignoring extra CLI args beyond [FILE_URL] [HLS_URL]");
        }

        let player = Arc::new(PlayerImpl::new(
            PlayerConfig::default().with_crossfade_duration(CROSSFADE_SECONDS),
        ));
        let file = Resource::new(ResourceConfig::new(&file_url)?).await?;
        let file_events = file.subscribe();
        player.insert(file, None);
        player.play();

        let (ui_tx, ui_rx) = mpsc::channel::<UiMsg>();
        let (stop_tx, stop_rx) = mpsc::channel::<()>();

        let mut forwarders = vec![
            forward_player_events(player.subscribe(), ui_tx.clone()),
            forward_engine_events(player.engine().subscribe(), ui_tx.clone()),
            forward_source_events(file_events, "file", ui_tx.clone()),
        ];

        let _ = ui_tx.send(UiMsg::Note(format!(
            "playing file, switching to hls in {}s",
            SWITCH_AFTER.as_secs()
        )));

        let player_for_ui = Arc::clone(&player);
        let file_for_ui = file_url.clone();
        let hls_for_ui = hls_url.clone();
        let mut ui_handle = tokio::task::spawn_blocking(move || {
            run_ui_loop(&player_for_ui, file_for_ui, hls_for_ui, &ui_rx, &stop_rx)
        });

        let switch_player = Arc::clone(&player);
        let switch_hls_url = hls_url.clone();
        let switch_ui_tx = ui_tx.clone();
        let switch_task = tokio::spawn(async move {
            tokio::time::sleep(SWITCH_AFTER).await;
            let _ = switch_ui_tx.send(UiMsg::Note(format!("loading hls: {switch_hls_url}")));

            let config = match ResourceConfig::new(&switch_hls_url) {
                Ok(config) => config,
                Err(err) => {
                    let _ = switch_ui_tx.send(UiMsg::Note(format!("invalid hls url: {err}")));
                    return None;
                }
            };

            let hls = match Resource::new(config).await {
                Ok(hls) => hls,
                Err(err) => {
                    let _ = switch_ui_tx.send(UiMsg::Note(format!("failed to load hls: {err}")));
                    return None;
                }
            };

            let hls_events = hls.subscribe();
            switch_player.insert(hls, None);
            switch_player.advance_to_next_item();
            switch_player.play();

            let _ = switch_ui_tx.send(UiMsg::CrossfadeStarted(Instant::now()));
            let _ = switch_ui_tx.send(UiMsg::Note(format!(
                "crossfade started ({CROSSFADE_SECONDS:.1}s)"
            )));

            Some(forward_source_events(hls_events, "hls", switch_ui_tx))
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

        if !switch_task.is_finished() {
            switch_task.abort();
        }
        match switch_task.await {
            Ok(Some(forwarder)) => forwarders.push(forwarder),
            Ok(None) => {}
            Err(err) if err.is_cancelled() => {}
            Err(err) => warn!(?err, "switch task failed"),
        }

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
