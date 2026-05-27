use kithara_queue::TrackStatus;
use num_traits::ToPrimitive;
use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{BarChart, Block, Borders, Clear, Paragraph},
};

use crate::{state::UiState, theme::tui::TuiPalette};

/// Tabs surfaced by the TUI dashboard. Mirrors the iOS reference
/// (Playlist / EQ / Settings); the older `Browser` tab was a
/// debug-only file picker and has been removed for parity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Playlist,
    Equalizer,
    Settings,
}

/// Saturating-clamp a `usize` count into `u16` for terminal-cell measurements.
/// ratatui's layout API is bounded by `u16`; a count past `u16::MAX` already
/// exceeds any displayable terminal area, so the ceiling is the truthful
/// rendering signal — best effort within what the terminal can show.
fn count_to_u16(count: usize) -> u16 {
    u16::try_from(count).unwrap_or(u16::MAX)
}

/// TUI dashboard widget for the Kithara player.
///
/// Renders playlist, EQ, settings, transport, and progress information
/// using ratatui inline viewport. Stateless w.r.t. player data — every
/// render reads from the [`UiState`] snapshot supplied by the caller.
pub struct Dashboard {
    pub active_tab: Tab,
    pub selected_eq_band: usize,
    pub selected_setting_row: usize,
    colors: TuiPalette,
    frame_count: u64,
}

impl Dashboard {
    const BAR_COL_CROSSFADE_PCT: u16 = 10;
    const BAR_COL_NOTE_PCT: u16 = 20;

    const BAR_COL_PROGRESS_PCT: u16 = 25;

    const BAR_COL_QUEUE_PCT: u16 = 12;
    const BAR_COL_TRACK_PCT: u16 = 25;
    const BAR_COL_VOLUME_PCT: u16 = 8;
    /// Frames between blink toggles (~500ms at 100ms poll).
    const BLINK_DIVISOR: u64 = 5;
    const BLINK_PERIOD: u64 = 2;
    const ELLIPSIS_LEN: usize = 3;

    /// EQ band gain bounds (matches the iOS reference: −24 … +6 dB).
    pub const EQ_GAIN_MAX: f32 = 6.0;
    pub const EQ_GAIN_MIN: f32 = -24.0;

    const MIN_PROGRESS_BAR_WIDTH: usize = 4;

    const MS_PER_SECOND: u64 = 1000;

    const NOTE_MAX_CHARS: usize = 26;
    const PERCENT_SCALE: f32 = 100.0;

    const PROGRESS_BAR_OVERHEAD: usize = 14;
    const SECONDS_PER_MINUTE: u64 = 60;

    /// Number of rows rendered inside the Settings tab body.
    pub const SETTINGS_ROW_COUNT: usize = 2;

    #[must_use]
    pub fn new(palette: TuiPalette) -> Self {
        Self {
            colors: palette,
            frame_count: 0,
            active_tab: Tab::Playlist,
            selected_eq_band: 0,
            selected_setting_row: 0,
        }
    }

    #[must_use]
    pub fn height(&self, state: &UiState) -> u16 {
        let content_lines = match self.active_tab {
            Tab::Playlist => count_to_u16(state.tracks.len()).max(1),
            Tab::Equalizer => 10,
            Tab::Settings => 6,
        };
        content_lines.saturating_add(2)
    }

    fn progress_bar(width: usize, state: &UiState) -> String {
        if width == 0 {
            return String::new();
        }
        let total_ms = seconds_to_ms(state.duration);
        let position_ms = seconds_to_ms(state.position);

        if total_ms == 0 {
            return "▱".repeat(width);
        }
        let width_u64 = u64::try_from(width).unwrap_or(u64::MAX);
        let filled_u64 = position_ms.min(total_ms).saturating_mul(width_u64) / total_ms;
        let filled = usize::try_from(filled_u64).unwrap_or(width).min(width);
        format!(
            "{}{}",
            "▰".repeat(filled),
            "▱".repeat(width.saturating_sub(filled))
        )
    }

    pub fn render(&mut self, frame: &mut Frame, state: &UiState) {
        self.frame_count = self.frame_count.wrapping_add(1);
        let area = frame.area();
        frame.render_widget(Clear, area);

        let content_height = self.height(state).saturating_sub(2);
        let chunks = Layout::vertical([
            Constraint::Length(1),
            Constraint::Length(content_height),
            Constraint::Length(1),
        ])
        .split(area);

        self.render_tabs(frame, chunks[0]);

        match self.active_tab {
            Tab::Playlist => self.render_playlist(frame, chunks[1], state),
            Tab::Equalizer => self.render_eq(frame, chunks[1], state),
            Tab::Settings => self.render_settings(frame, chunks[1], state),
        }

        self.render_bar(frame, chunks[2], state);
    }

    fn render_bar(&self, frame: &mut Frame, area: Rect, state: &UiState) {
        let c = &self.colors;
        let chunks = Layout::horizontal([
            Constraint::Percentage(Self::BAR_COL_TRACK_PCT),
            Constraint::Percentage(Self::BAR_COL_PROGRESS_PCT),
            Constraint::Percentage(Self::BAR_COL_QUEUE_PCT),
            Constraint::Percentage(Self::BAR_COL_CROSSFADE_PCT),
            Constraint::Percentage(Self::BAR_COL_VOLUME_PCT),
            Constraint::Percentage(Self::BAR_COL_NOTE_PCT),
        ])
        .split(area);

        let progress_bar_width = usize::from(chunks[1].width)
            .saturating_sub(Self::PROGRESS_BAR_OVERHEAD)
            .max(Self::MIN_PROGRESS_BAR_WIDTH);
        let icon = if state.playing { '▶' } else { '⏸' };
        let active_track = state
            .tracks
            .get(state.current_track_index.unwrap_or(0))
            .map_or("", |e| e.name.as_str());
        let queue_current = state
            .current_track_index
            .unwrap_or(0)
            .saturating_add(1)
            .max(1);
        let queue_total = state.tracks.len().max(1);

        let position_ms = seconds_to_ms(state.position);
        let total_ms = seconds_to_ms(state.duration);

        let segments = [
            format!("▌ #{queue_current} ♫{active_track}"),
            format!(
                "{icon}{}/{} {}",
                format_ms(position_ms),
                if total_ms == 0 {
                    "--:--".to_string()
                } else {
                    format_ms(total_ms)
                },
                Self::progress_bar(progress_bar_width, state)
            ),
            format!(
                "q {}/{} {}",
                queue_current.min(queue_total),
                queue_total,
                if state.playing { "play" } else { "pause" }
            ),
            state.crossfade_progress.map_or_else(
                || "xf -".to_string(),
                |progress| format!("xf {:>3.0}%", progress * Self::PERCENT_SCALE),
            ),
            format!("🔉{:>3.0}%", state.volume * Self::PERCENT_SCALE),
            clamp_text(
                state.status_note.as_deref().unwrap_or("-"),
                Self::NOTE_MAX_CHARS,
            ),
        ];

        let style = Style::default().fg(c.text).bg(c.bg);
        for (index, chunk) in chunks.iter().enumerate() {
            let text = fit_cell(&segments[index], usize::from(chunk.width));
            let widget = Paragraph::new(Line::raw(text)).style(style);
            frame.render_widget(widget, *chunk);
        }
    }

    fn render_eq(&self, frame: &mut Frame, area: Rect, state: &UiState) {
        let mut bars = Vec::new();
        for (i, &gain) in state.eq_bands.iter().enumerate() {
            let clamped = gain.clamp(Self::EQ_GAIN_MIN, Self::EQ_GAIN_MAX);
            let val = clamp_f32_to_u64((clamped - Self::EQ_GAIN_MIN).max(0.0));
            let style = if i == self.selected_eq_band {
                Style::default().fg(self.colors.accent)
            } else {
                Style::default().fg(self.colors.text)
            };
            let bar = ratatui::widgets::Bar::default()
                .value(val)
                .text_value(format!("{gain:+.1}"))
                .style(style);
            bars.push(bar);
        }

        let chart = BarChart::default()
            .data(ratatui::widgets::BarGroup::default().bars(&bars))
            .bar_width(5)
            .bar_gap(1)
            .block(Block::default().borders(Borders::ALL).title("Equalizer"));

        frame.render_widget(chart, area);
    }

    fn render_playlist(&self, frame: &mut Frame, area: Rect, state: &UiState) {
        let c = &self.colors;
        for (i, entry) in state.tracks.iter().enumerate() {
            let track_name = &entry.name;
            let is_active = Some(i) == state.current_track_index;
            let is_failed = matches!(entry.status, TrackStatus::Failed(_));
            let is_slow = matches!(entry.status, TrackStatus::Slow);
            let number = i + 1;
            let marker = if is_active { "▶" } else { " " };
            let text = format!(" {marker} {number}  {track_name}");
            let style = match (is_failed, is_slow, is_active) {
                (true, _, _) => Style::default().fg(c.danger).bg(c.bg),
                (_, true, true) => {
                    let blink_on =
                        (self.frame_count / Self::BLINK_DIVISOR).is_multiple_of(Self::BLINK_PERIOD);
                    let fg = if blink_on { c.warning } else { c.muted };
                    Style::default().fg(fg).bg(c.bg_panel)
                }
                (_, true, false) => Style::default().fg(c.warning).bg(c.bg),
                (_, false, true) => Style::default().fg(c.accent).bg(c.bg_panel),
                (_, false, false) => Style::default().fg(c.muted).bg(c.bg),
            };
            let row = Rect::new(
                area.x,
                area.y.saturating_add(count_to_u16(i)),
                area.width,
                1,
            );
            let padded = fit_cell(&text, usize::from(row.width));
            frame.render_widget(Paragraph::new(Line::raw(padded)).style(style), row);
        }
    }

    fn render_settings(&self, frame: &mut Frame, area: Rect, state: &UiState) {
        let block = Block::default().borders(Borders::ALL).title("Settings");
        let inner = block.inner(area);
        frame.render_widget(block, area);

        let layout = Layout::vertical([
            Constraint::Length(2),
            Constraint::Length(2),
            Constraint::Min(0),
        ])
        .split(inner);

        let row_style = |idx: usize| -> Style {
            if idx == self.selected_setting_row {
                Style::default()
                    .fg(self.colors.accent)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(self.colors.text)
            }
        };

        let quality_label = if state.abr_mode_is_auto {
            "Auto".to_string()
        } else if let Some(idx) = state.selected_variant {
            state
                .abr_variants
                .iter()
                .find_map(|(i, l)| (*i == idx).then(|| l.clone()))
                .unwrap_or_else(|| format!("variant {idx}"))
        } else {
            "Auto".to_string()
        };
        let quality_line = Paragraph::new(Line::from(vec![
            Span::raw("Quality  "),
            Span::styled(quality_label, Style::default().fg(self.colors.accent)),
            Span::raw("   (←/→ to switch)"),
        ]))
        .style(row_style(0));
        frame.render_widget(quality_line, layout[0]);

        let crossfade_line = Paragraph::new(Line::from(vec![
            Span::raw("Crossfade  "),
            Span::styled(
                format!("{:.1}s", state.crossfade),
                Style::default().fg(self.colors.accent),
            ),
            Span::raw("   (←/→ to adjust)"),
        ]))
        .style(row_style(1));
        frame.render_widget(crossfade_line, layout[1]);
    }

    fn render_tabs(&self, frame: &mut Frame, area: Rect) {
        let tabs = [Tab::Playlist, Tab::Equalizer, Tab::Settings];
        let mut spans = Vec::new();

        for tab in tabs {
            let label = match tab {
                Tab::Playlist => "Playlist",
                Tab::Equalizer => "EQ",
                Tab::Settings => "Settings",
            };

            let is_active = self.active_tab == tab;
            let style = if is_active {
                Style::default()
                    .fg(self.colors.bg)
                    .bg(self.colors.accent)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(self.colors.text).bg(self.colors.bg)
            };

            spans.push(Span::styled(format!(" [ {label} ] "), style));
        }

        let paragraph = Paragraph::new(Line::from(spans));
        frame.render_widget(paragraph, area);
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
    if max_chars <= Dashboard::ELLIPSIS_LEN {
        return text.chars().take(max_chars).collect();
    }
    let keep = max_chars - Dashboard::ELLIPSIS_LEN;
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
    let total_seconds = ms / Dashboard::MS_PER_SECOND;
    let minutes = total_seconds / Dashboard::SECONDS_PER_MINUTE;
    let seconds = total_seconds % Dashboard::SECONDS_PER_MINUTE;
    format!("{minutes:02}:{seconds:02}")
}

/// Convert a seconds value to milliseconds, saturating at [`u64::MAX`]
/// and clamping negatives / `NaN` to zero. Routed through `to_u64` to
/// keep the conversion free of raw `as` casts.
fn seconds_to_ms(seconds: f64) -> u64 {
    let ms = seconds.max(0.0) * 1000.0;
    if !ms.is_finite() {
        return 0;
    }
    ms.to_u64().unwrap_or(u64::MAX)
}

/// Saturating cast for non-negative `f32` gain values. `NaN` and
/// out-of-range collapse to zero / `u64::MAX` via `num_traits::to_u64`,
/// which keeps the conversion free of raw `as` casts.
fn clamp_f32_to_u64(value: f32) -> u64 {
    if !value.is_finite() || value <= 0.0 {
        return 0;
    }
    value.to_u64().unwrap_or(u64::MAX)
}
