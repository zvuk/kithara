use std::time::Duration;

use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect},
    style::Style,
    text::Line,
    widgets::{Clear, Paragraph},
};

use crate::theme::tui::TuiPalette;

const MIN_PROGRESS_BAR_WIDTH: usize = 4;
const NOTE_MAX_CHARS: usize = 26;

const PROGRESS_BAR_OVERHEAD: usize = 14;

const BAR_COL_TRACK_PCT: u16 = 25;
const BAR_COL_PROGRESS_PCT: u16 = 25;
const BAR_COL_QUEUE_PCT: u16 = 12;
const BAR_COL_CROSSFADE_PCT: u16 = 10;
const BAR_COL_VOLUME_PCT: u16 = 8;
const BAR_COL_NOTE_PCT: u16 = 20;

const ELLIPSIS_LEN: usize = 3;

const PERCENT_SCALE: f32 = 100.0;

const MS_PER_SECOND: u64 = 1000;
const SECONDS_PER_MINUTE: u64 = 60;

/// TUI dashboard widget for the Kithara player.
///
/// Renders playlist, progress bar, volume, and status information
/// using ratatui inline viewport.
pub struct Dashboard {
    colors: TuiPalette,
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
    #[must_use]
    pub fn new(tracks: Vec<String>, palette: TuiPalette) -> Self {
        Self {
            colors: palette,
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

    #[must_use]
    #[expect(clippy::cast_possible_truncation)]
    pub fn height(&self) -> u16 {
        self.tracks.len() as u16 + 1
    }

    pub fn set_crossfade_progress(&mut self, progress: Option<f32>) {
        self.crossfade_progress = progress.map(|value| value.clamp(0.0, 1.0));
    }

    pub fn set_note<S: Into<String>>(&mut self, note: S) {
        self.last_note = Some(note.into());
    }

    pub fn set_playing(&mut self, playing: bool) {
        self.playing = playing;
    }

    pub fn set_position(&mut self, position: Duration) {
        self.position_ms = u64::try_from(position.as_millis()).unwrap_or(u64::MAX);
    }

    pub fn set_queue(&mut self, current_index: usize, item_count: usize) {
        self.current_index = current_index;
        self.item_count = item_count;
    }

    pub fn set_total(&mut self, total: Option<Duration>) {
        self.total_ms =
            total.map(|duration| u64::try_from(duration.as_millis()).unwrap_or(u64::MAX));
    }

    pub fn set_volume(&mut self, volume: f32) {
        self.volume = volume.clamp(0.0, 1.0);
    }

    pub fn render(&self, frame: &mut Frame) {
        let area = frame.area();
        frame.render_widget(Clear, area);

        #[expect(clippy::cast_possible_truncation)]
        let playlist_lines = self.tracks.len() as u16;
        let chunks = Layout::vertical([Constraint::Length(playlist_lines), Constraint::Length(1)])
            .split(area);

        self.render_playlist(frame, chunks[0]);
        self.render_bar(frame, chunks[1]);
    }

    fn render_playlist(&self, frame: &mut Frame, area: Rect) {
        let c = &self.colors;
        for (i, track) in self.tracks.iter().enumerate() {
            let is_active = i == self.current_index;
            let number = i + 1;
            let marker = if is_active { "▶" } else { " " };
            let text = format!(" {marker} {number}  {track}");
            let style = if is_active {
                Style::default().fg(c.accent).bg(c.bg_panel)
            } else {
                Style::default().fg(c.muted).bg(c.bg)
            };
            #[expect(clippy::cast_possible_truncation)]
            let row = Rect::new(area.x, area.y + i as u16, area.width, 1);
            let padded = fit_cell(&text, usize::from(row.width));
            frame.render_widget(Paragraph::new(Line::raw(padded)).style(style), row);
        }
    }

    fn render_bar(&self, frame: &mut Frame, area: Rect) {
        let c = &self.colors;
        let chunks = Layout::horizontal([
            Constraint::Percentage(BAR_COL_TRACK_PCT),
            Constraint::Percentage(BAR_COL_PROGRESS_PCT),
            Constraint::Percentage(BAR_COL_QUEUE_PCT),
            Constraint::Percentage(BAR_COL_CROSSFADE_PCT),
            Constraint::Percentage(BAR_COL_VOLUME_PCT),
            Constraint::Percentage(BAR_COL_NOTE_PCT),
        ])
        .split(area);

        let progress_bar_width = usize::from(chunks[1].width)
            .saturating_sub(PROGRESS_BAR_OVERHEAD)
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
                |progress| format!("xf {:>3.0}%", progress * PERCENT_SCALE),
            ),
            format!("🔉{:>3.0}%", self.volume * PERCENT_SCALE),
            clamp_text(self.last_note.as_deref().unwrap_or("-"), NOTE_MAX_CHARS),
        ];

        let style = Style::default().fg(c.text).bg(c.bg);
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

fn clamp_text(text: &str, max_chars: usize) -> String {
    if max_chars == 0 {
        return String::new();
    }
    let count = text.chars().count();
    if count <= max_chars {
        return text.to_string();
    }
    if max_chars <= ELLIPSIS_LEN {
        return text.chars().take(max_chars).collect();
    }
    let keep = max_chars - ELLIPSIS_LEN;
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
    let total_seconds = ms / MS_PER_SECOND;
    let minutes = total_seconds / SECONDS_PER_MINUTE;
    let seconds = total_seconds % SECONDS_PER_MINUTE;
    format!("{minutes:02}:{seconds:02}")
}
