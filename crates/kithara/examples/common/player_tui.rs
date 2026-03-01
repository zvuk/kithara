use std::{error::Error, io, time::Duration};

use ratatui::{
    Frame, Terminal, TerminalOptions, Viewport,
    backend::CrosstermBackend,
    crossterm::terminal::{disable_raw_mode, enable_raw_mode, size},
    layout::{Constraint, Layout, Position, Rect},
    style::{Color, Style},
    text::Line,
    widgets::{Clear, Paragraph, Widget},
};

const MIN_PROGRESS_BAR_WIDTH: usize = 4;
const NOTE_MAX_CHARS: usize = 26;
const CURSOR_GUARD_LINES: u16 = 2;

type ExampleError = Box<dyn Error + Send + Sync>;
type ExampleResult<T = ()> = Result<T, ExampleError>;

pub(crate) struct Dashboard {
    crossfade_progress: Option<f32>,
    current_index: usize,
    item_count: usize,
    last_note: Option<String>,
    playing: bool,
    position_ms: u64,
    total_ms: Option<u64>,
    tracks: Vec<String>,
    volume: f32,
}

impl Dashboard {
    pub(crate) fn new(tracks: Vec<String>) -> Self {
        Self {
            crossfade_progress: None,
            current_index: 0,
            item_count: 0,
            last_note: None,
            playing: false,
            position_ms: 0,
            total_ms: None,
            tracks,
            volume: 1.0,
        }
    }

    pub(crate) fn height(&self) -> u16 {
        self.tracks.len() as u16 + 1
    }

    pub(crate) fn set_crossfade_progress(&mut self, progress: Option<f32>) {
        self.crossfade_progress = progress.map(|value| value.clamp(0.0, 1.0));
    }

    pub(crate) fn set_note(&mut self, note: impl Into<String>) {
        self.last_note = Some(note.into());
    }

    pub(crate) fn set_playing(&mut self, playing: bool) {
        self.playing = playing;
    }

    pub(crate) fn set_position(&mut self, position: Duration) {
        self.position_ms = u64::try_from(position.as_millis()).unwrap_or(u64::MAX);
    }

    pub(crate) fn set_queue(&mut self, current_index: usize, item_count: usize) {
        self.current_index = current_index;
        self.item_count = item_count;
    }

    pub(crate) fn set_total(&mut self, total: Option<Duration>) {
        self.total_ms =
            total.map(|duration| u64::try_from(duration.as_millis()).unwrap_or(u64::MAX));
    }

    pub(crate) fn set_volume(&mut self, volume: f32) {
        self.volume = volume.clamp(0.0, 1.0);
    }

    pub(crate) fn render(&self, frame: &mut Frame) {
        let area = frame.area();
        frame.render_widget(Clear, area);

        let playlist_lines = self.tracks.len() as u16;
        let chunks = Layout::vertical([Constraint::Length(playlist_lines), Constraint::Length(1)])
            .split(area);

        self.render_playlist(frame, chunks[0]);
        self.render_bar(frame, chunks[1]);
    }

    fn render_playlist(&self, frame: &mut Frame, area: Rect) {
        let bg = Color::Rgb(20, 24, 30);
        let active_bg = Color::Rgb(40, 44, 55);

        for (i, track) in self.tracks.iter().enumerate() {
            let is_active = i == self.current_index;
            let number = i + 1;
            let marker = if is_active { "▶" } else { " " };
            let text = format!(" {marker} {number}  {track}");
            let style = if is_active {
                Style::default().fg(Color::White).bg(active_bg)
            } else {
                Style::default().fg(Color::DarkGray).bg(bg)
            };
            let row = Rect::new(area.x, area.y + i as u16, area.width, 1);
            let padded = fit_cell(&text, usize::from(row.width));
            frame.render_widget(Paragraph::new(Line::raw(padded)).style(style), row);
        }
    }

    fn render_bar(&self, frame: &mut Frame, area: Rect) {
        let chunks = Layout::horizontal([
            Constraint::Percentage(25),
            Constraint::Percentage(25),
            Constraint::Percentage(12),
            Constraint::Percentage(10),
            Constraint::Percentage(8),
            Constraint::Percentage(20),
        ])
        .split(area);

        let progress_bar_width = usize::from(chunks[1].width)
            .saturating_sub(14)
            .max(MIN_PROGRESS_BAR_WIDTH);
        let icon = if self.playing { '▶' } else { '⏸' };
        let active_track = self
            .tracks
            .get(self.current_index)
            .map_or("-", |s| s.as_str());
        let queue_current = self.current_index.saturating_add(1).max(1);
        let queue_total = self.item_count.max(1);
        let segments = [
            format!("▌ #{} ♫{}", queue_current, active_track),
            format!(
                "{icon}{}/{} {}",
                format_ms(self.position_ms),
                self.total_ms.map_or_else(|| "--:--".to_string(), format_ms),
                self.progress_bar(progress_bar_width)
            ),
            format!(
                "q {}/{} {}",
                queue_current.min(queue_total),
                queue_total,
                if self.playing { "play" } else { "pause" }
            ),
            self.crossfade_progress.map_or_else(
                || "xf -".to_string(),
                |progress| format!("xf {:>3.0}%", progress * 100.0),
            ),
            format!("🔉{:>3.0}%", self.volume * 100.0),
            clamp_text(self.last_note.as_deref().unwrap_or("-"), NOTE_MAX_CHARS),
        ];

        let style = Style::default().fg(Color::White).bg(Color::Rgb(20, 24, 30));
        for (index, chunk) in chunks.iter().enumerate() {
            let text = fit_cell(&segments[index], usize::from(chunk.width));
            let widget = Paragraph::new(Line::raw(text)).style(style);
            frame.render_widget(widget, *chunk);
        }
    }

    fn progress_bar(&self, width: usize) -> String {
        if width == 0 {
            return String::new();
        }
        let Some(total_ms) = self.total_ms else {
            return "▱".repeat(width);
        };
        if total_ms == 0 {
            return "▱".repeat(width);
        }
        let width_u64 = u64::try_from(width).unwrap_or(u64::MAX);
        let filled_u64 = self.position_ms.min(total_ms).saturating_mul(width_u64) / total_ms;
        let filled = usize::try_from(filled_u64).unwrap_or(width).min(width);
        format!(
            "{}{}",
            "▰".repeat(filled),
            "▱".repeat(width.saturating_sub(filled))
        )
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

pub(crate) struct UiSession {
    _raw: RawModeGuard,
    pub(crate) dashboard: Dashboard,
    terminal: Terminal<CrosstermBackend<io::Stdout>>,
}

impl UiSession {
    pub(crate) fn new(dashboard: Dashboard) -> ExampleResult<Self> {
        let raw = RawModeGuard::new()?;
        let (_, terminal_height) = size()?;
        let max_height = terminal_height.saturating_sub(2).max(6);
        let viewport_height = dashboard.height().min(max_height);
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
            dashboard,
            terminal,
        };
        session.stick_to_bottom()?;
        session.park_cursor_above_dashboard()?;
        Ok(session)
    }

    pub(crate) fn draw(&mut self) -> ExampleResult {
        self.terminal.draw(|frame| self.dashboard.render(frame))?;
        self.park_cursor_above_dashboard()?;
        Ok(())
    }

    pub(crate) fn log_line(&mut self, line: &str) -> ExampleResult {
        let line = line.replace('\n', " ");
        self.terminal.insert_before(1, |buf| {
            Paragraph::new(line).render(buf.area, buf);
        })?;
        self.park_cursor_above_dashboard()?;
        Ok(())
    }

    pub(crate) fn on_resize(&mut self) -> ExampleResult {
        self.terminal.autoresize()?;
        self.stick_to_bottom()?;
        self.park_cursor_above_dashboard()?;
        Ok(())
    }

    fn dashboard_height(&self) -> u16 {
        self.dashboard.height()
    }

    fn stick_to_bottom(&mut self) -> ExampleResult {
        let terminal_height = self.terminal.size()?.height.max(1);
        let viewport_height = self.dashboard_height().min(terminal_height);
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

    fn park_cursor_above_dashboard(&mut self) -> ExampleResult {
        let terminal_height = self.terminal.size()?.height.max(1);
        let height = self.dashboard_height().min(terminal_height);
        let y = terminal_height.saturating_sub(height.saturating_add(CURSOR_GUARD_LINES));
        self.terminal.set_cursor_position(Position { x: 0, y })?;
        Ok(())
    }
}

impl Drop for UiSession {
    fn drop(&mut self) {
        let _ = self.terminal.show_cursor();
    }
}

fn clamp_text(text: &str, max_chars: usize) -> String {
    if max_chars == 0 {
        return String::new();
    }
    let count = text.chars().count();
    if count <= max_chars {
        return text.to_string();
    }
    if max_chars <= 3 {
        return text.chars().take(max_chars).collect();
    }
    let keep = max_chars - 3;
    let mut out: String = text.chars().take(keep).collect();
    out.push_str("...");
    out
}

fn fit_cell(text: &str, width: usize) -> String {
    if width == 0 {
        return String::new();
    }
    let mut out = clamp_text(text, width);
    let used = out.chars().count();
    if used < width {
        out.push_str(&" ".repeat(width - used));
    }
    out
}

fn format_ms(ms: u64) -> String {
    let total_seconds = ms / 1000;
    let minutes = total_seconds / 60;
    let seconds = total_seconds % 60;
    format!("{minutes:02}:{seconds:02}")
}
