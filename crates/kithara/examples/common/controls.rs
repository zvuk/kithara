use std::{
    error::Error,
    io,
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};

#[cfg(feature = "hls")]
use kithara::prelude::HlsEvent;
use kithara::prelude::{AudioEvent, Event, FileEvent};
use ratatui::{
    Terminal, TerminalOptions, Viewport,
    backend::CrosstermBackend,
    crossterm::{
        event::{self, Event as TermEvent, KeyCode, KeyEventKind},
        terminal::{disable_raw_mode, enable_raw_mode, size},
    },
    widgets::{Clear, Paragraph, Widget},
};
use rodio::{OutputStreamBuilder, Sink, Source};
use tokio::sync::broadcast::error::RecvError;

use crate::tui::{DASHBOARD_HEIGHT, Dashboard};

type ExampleError = Box<dyn Error + Send + Sync>;
type ExampleResult<T = ()> = Result<T, ExampleError>;

const CONTROL_POLL_MS: u64 = 100;
const SEEK_STEP_SECONDS: i64 = 5;
const VOLUME_STEP: f32 = 0.05;

enum UiMsg {
    Event(Event),
    Note(String),
}

pub(crate) struct UiConfig {
    pub(crate) source_label: &'static str,
    pub(crate) total_duration: Option<Duration>,
    pub(crate) track: String,
}

impl UiConfig {
    pub(crate) fn new(source_label: &'static str, track: String) -> Self {
        Self {
            source_label,
            total_duration: None,
            track,
        }
    }

    pub(crate) fn with_total_duration(mut self, total_duration: Option<Duration>) -> Self {
        self.total_duration = total_duration;
        self
    }
}

struct RawModeGuard;

impl RawModeGuard {
    fn new() -> ExampleResult<Self> {
        enable_raw_mode()?;
        Ok(Self)
    }
}

impl Drop for RawModeGuard {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
    }
}

struct UiSession {
    _raw: RawModeGuard,
    dashboard: Dashboard,
    terminal: Terminal<CrosstermBackend<io::Stdout>>,
}

impl UiSession {
    fn draw(&mut self) -> ExampleResult {
        self.stick_to_bottom()?;
        self.terminal.draw(|frame| self.dashboard.render(frame))?;
        Ok(())
    }

    fn stick_to_bottom(&mut self) -> ExampleResult {
        let terminal_height = self.terminal.size()?.height.max(1);
        let viewport_height = DASHBOARD_HEIGHT.min(terminal_height);
        let cursor_y = self.terminal.get_cursor_position()?.y;
        let target_top = terminal_height.saturating_sub(viewport_height);
        let pad = target_top.saturating_sub(cursor_y);
        if pad == 0 {
            return Ok(());
        }
        self.terminal.insert_before(pad, |buf| {
            Clear.render(buf.area, buf);
        })?;
        Ok(())
    }

    fn log_line(&mut self, line: &str) -> ExampleResult {
        let line = line.replace('\n', " ");
        self.stick_to_bottom()?;
        self.terminal.insert_before(1, |buf| {
            Paragraph::new(line).render(buf.area, buf);
        })?;
        Ok(())
    }

    fn new(config: UiConfig, volume: f32) -> ExampleResult<Self> {
        let raw = RawModeGuard::new()?;
        let (_, terminal_height) = size()?;
        let max_height = terminal_height.saturating_sub(2).max(6);
        let viewport_height = DASHBOARD_HEIGHT.min(max_height);
        let backend = CrosstermBackend::new(io::stdout());
        let mut terminal = Terminal::with_options(
            backend,
            TerminalOptions {
                viewport: Viewport::Inline(viewport_height),
            },
        )?;
        terminal.hide_cursor()?;
        let mut session = Self {
            _raw: raw,
            dashboard: Dashboard::new(config.source_label, config.track, volume),
            terminal,
        };
        session.stick_to_bottom()?;
        session.log_line(&format!(
            "controls: Left/Right seek {SEEK_STEP_SECONDS}s, Up/Down volume {:+.0}%",
            VOLUME_STEP * 100.0
        ))?;
        session.log_line("logs are appended above dashboard and remain scrollable")?;
        Ok(session)
    }
}

impl Drop for UiSession {
    fn drop(&mut self) {
        let _ = self.terminal.show_cursor();
    }
}

fn apply_seek(sink: &Sink, delta_secs: i64, dashboard: &mut Dashboard) -> String {
    let delta = Duration::from_secs(delta_secs.unsigned_abs());
    let current = sink.get_pos();
    let target = if delta_secs.is_negative() {
        current.saturating_sub(delta)
    } else {
        current.saturating_add(delta)
    };

    match sink.try_seek(target) {
        Ok(()) => {
            dashboard.track_seek(target);
            format!("seek target={target:?}")
        }
        Err(err) => {
            let line = format!("seek failed target={target:?} err={err}");
            dashboard.push_note(line.clone());
            line
        }
    }
}

fn apply_seek_log(sink: &Sink, delta_secs: i64, dashboard: &mut Dashboard) -> Option<String> {
    let line = apply_seek(sink, delta_secs, dashboard);
    if line.starts_with("seek failed") {
        return Some(line);
    }
    None
}

fn apply_volume(sink: &Sink, delta: f32, dashboard: &mut Dashboard) {
    let volume = (sink.volume() + delta).clamp(0.0, 1.0);
    sink.set_volume(volume);
    dashboard.set_volume(volume);
    dashboard.push_note(format!("volume {:.0}%", volume * 100.0));
}

enum KeyOutcome {
    Ignored,
    Handled { log_line: Option<String> },
}

fn handle_key(key: KeyCode, sink: &Sink, dashboard: &mut Dashboard) -> KeyOutcome {
    match key {
        KeyCode::Left => KeyOutcome::Handled {
            log_line: apply_seek_log(sink, -SEEK_STEP_SECONDS, dashboard),
        },
        KeyCode::Right => KeyOutcome::Handled {
            log_line: apply_seek_log(sink, SEEK_STEP_SECONDS, dashboard),
        },
        KeyCode::Up => {
            apply_volume(sink, VOLUME_STEP, dashboard);
            KeyOutcome::Handled { log_line: None }
        }
        KeyCode::Down => {
            apply_volume(sink, -VOLUME_STEP, dashboard);
            KeyOutcome::Handled { log_line: None }
        }
        _ => KeyOutcome::Ignored,
    }
}

fn loggable_event(event: &Event) -> bool {
    match event {
        Event::Audio(AudioEvent::PlaybackProgress { .. }) => false,
        Event::File(FileEvent::DownloadProgress { .. })
        | Event::File(FileEvent::ByteProgress { .. })
        | Event::File(FileEvent::PlaybackProgress { .. }) => false,
        #[cfg(feature = "hls")]
        Event::Hls(HlsEvent::DownloadProgress { .. })
        | Event::Hls(HlsEvent::ByteProgress { .. })
        | Event::Hls(HlsEvent::PlaybackProgress { .. }) => false,
        _ => true,
    }
}

fn is_progress_event(event: &Event) -> bool {
    match event {
        Event::Audio(AudioEvent::PlaybackProgress { .. }) => true,
        Event::File(FileEvent::DownloadProgress { .. })
        | Event::File(FileEvent::ByteProgress { .. })
        | Event::File(FileEvent::PlaybackProgress { .. }) => true,
        #[cfg(feature = "hls")]
        Event::Hls(HlsEvent::DownloadProgress { .. })
        | Event::Hls(HlsEvent::ByteProgress { .. })
        | Event::Hls(HlsEvent::PlaybackProgress { .. }) => true,
        _ => false,
    }
}

struct ProgressLog {
    last_emit: Instant,
}

impl ProgressLog {
    fn new() -> Self {
        Self {
            last_emit: Instant::now() - Duration::from_secs(2),
        }
    }

    fn should_emit(&mut self) -> bool {
        let now = Instant::now();
        if now.duration_since(self.last_emit) < Duration::from_secs(1) {
            return false;
        }
        self.last_emit = now;
        true
    }
}

pub(crate) async fn play_with_controls<S>(
    source: S,
    mut events_rx: tokio::sync::broadcast::Receiver<Event>,
    ui: UiConfig,
) -> ExampleResult
where
    S: Source + Send + 'static,
{
    let (stop_tx, stop_rx) = mpsc::channel::<()>();
    let (events_tx, events_ui_rx) = mpsc::channel::<UiMsg>();

    let forwarder = tokio::spawn(async move {
        loop {
            match events_rx.recv().await {
                Ok(event) => {
                    if events_tx.send(UiMsg::Event(event)).is_err() {
                        break;
                    }
                }
                Err(RecvError::Lagged(n)) => {
                    if events_tx
                        .send(UiMsg::Note(format!("events lagged n={n}")))
                        .is_err()
                    {
                        break;
                    }
                }
                Err(RecvError::Closed) => break,
            }
        }
    });

    let mut playback = tokio::task::spawn_blocking(move || -> ExampleResult {
        let output = OutputStreamBuilder::open_default_stream()?;
        let sink = Sink::connect_new(output.mixer());
        sink.append(source);
        let initial_total_duration = ui.total_duration;
        let mut ui_session = UiSession::new(ui, sink.volume()).ok();
        let mut progress_log = ProgressLog::new();
        if let Some(session) = ui_session.as_mut() {
            session.dashboard.set_total_duration(initial_total_duration);
            session.draw()?;
        }

        loop {
            if stop_rx.try_recv().is_ok() {
                break;
            }
            if sink.empty() {
                break;
            }

            while let Ok(msg) = events_ui_rx.try_recv() {
                if let Some(session) = ui_session.as_mut() {
                    match msg {
                        UiMsg::Event(event) => {
                            session.dashboard.track_event(&event);
                            if loggable_event(&event) {
                                session.log_line(&format!("{event:?}"))?;
                            } else if is_progress_event(&event) && progress_log.should_emit() {
                                session.log_line(&session.dashboard.progress_log_line())?;
                            }
                        }
                        UiMsg::Note(note) => {
                            session.dashboard.push_note(note.clone());
                            session.log_line(&note)?;
                        }
                    }
                }
            }

            if let Some(session) = ui_session.as_mut() {
                session.dashboard.set_position(sink.get_pos());
                if event::poll(Duration::from_millis(CONTROL_POLL_MS))? {
                    match event::read()? {
                        TermEvent::Key(key)
                            if matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) =>
                        {
                            match handle_key(key.code, &sink, &mut session.dashboard) {
                                KeyOutcome::Handled { log_line } => {
                                    if let Some(line) = log_line {
                                        session.log_line(&line)?;
                                    }
                                }
                                KeyOutcome::Ignored => {}
                            }
                        }
                        TermEvent::Resize(_, _) => {
                            session.terminal.autoresize()?;
                            session.stick_to_bottom()?;
                        }
                        _ => {}
                    }
                }
                // Keep the dashboard anchored even if external writers print to the terminal.
                session.draw()?;
            } else {
                thread::sleep(Duration::from_millis(CONTROL_POLL_MS));
            }
        }

        sink.stop();
        Ok(())
    });

    let playback_finished = tokio::select! {
        _ = tokio::signal::ctrl_c() => false,
        result = &mut playback => {
            result??;
            true
        }
    };

    let _ = stop_tx.send(());
    if !playback_finished {
        playback.await??;
    }

    forwarder.abort();
    let _ = forwarder.await;

    Ok(())
}
