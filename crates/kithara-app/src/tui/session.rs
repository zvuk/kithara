use std::{error::Error as StdError, io};

use ratatui::{
    Frame, Terminal, TerminalOptions, Viewport,
    backend::CrosstermBackend,
    crossterm::terminal::{disable_raw_mode, enable_raw_mode, size},
    layout::Position,
    widgets::{Clear, Paragraph, Widget},
};

use super::dashboard::Dashboard;
use crate::state::UiState;

/// Error type returned by TUI session operations.
pub type TuiError = Box<dyn StdError + Send + Sync>;

/// Result type for TUI session operations.
pub type TuiResult<T = ()> = Result<T, TuiError>;

struct RawModeGuard;

impl RawModeGuard {
    fn new() -> TuiResult<Self> {
        enable_raw_mode()?;
        Ok(Self)
    }
}

impl Drop for RawModeGuard {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
    }
}

/// Terminal session manager for TUI mode.
///
/// Handles raw mode, inline viewport management, cursor placement,
/// and terminal resize events. Uses ratatui with an inline viewport
/// anchored to the bottom of the terminal.
pub struct UiSession {
    pub dashboard: Dashboard,
    _raw: RawModeGuard,
    terminal: Terminal<CrosstermBackend<io::Stdout>>,
}

impl UiSession {
    /// # Errors
    /// Returns an error if terminal raw mode or viewport setup fails.
    pub fn new(dashboard: Dashboard, state: &UiState) -> TuiResult<Self> {
        const BOTTOM_MARGIN: u16 = 2;
        const MIN_VIEWPORT_HEIGHT: u16 = 6;

        let raw = RawModeGuard::new()?;
        let (_, terminal_height) = size()?;
        let max_height = terminal_height
            .saturating_sub(BOTTOM_MARGIN)
            .max(MIN_VIEWPORT_HEIGHT);
        let viewport_height = dashboard.height(state).min(max_height);
        let backend = CrosstermBackend::new(io::stdout());
        let mut terminal = Terminal::with_options(
            backend,
            TerminalOptions {
                viewport: Viewport::Inline(viewport_height),
            },
        )?;
        terminal.hide_cursor()?;

        let mut session = Self {
            dashboard,
            terminal,
            _raw: raw,
        };
        session.stick_to_bottom(state)?;
        session.park_cursor_above_dashboard(state)?;
        Ok(session)
    }

    fn dashboard_height(&self, state: &UiState) -> u16 {
        self.dashboard.height(state)
    }

    /// # Errors
    /// Returns an error if terminal rendering fails.
    pub fn draw(&mut self, state: &UiState) -> TuiResult {
        self.terminal.draw(|frame: &mut Frame| {
            self.dashboard.render(frame, state);
        })?;
        self.park_cursor_above_dashboard(state)?;
        Ok(())
    }

    /// # Errors
    /// Returns an error if terminal rendering fails.
    pub fn log_line(&mut self, line: &str, state: &UiState) -> TuiResult {
        let line = line.replace('\n', " ");
        self.terminal.insert_before(1, |buf| {
            Paragraph::new(line).render(buf.area, buf);
        })?;
        self.park_cursor_above_dashboard(state)?;
        Ok(())
    }

    /// # Errors
    /// Returns an error if terminal resize handling fails.
    pub fn on_resize(&mut self, state: &UiState) -> TuiResult {
        self.terminal.autoresize()?;
        self.stick_to_bottom(state)?;
        self.park_cursor_above_dashboard(state)?;
        Ok(())
    }

    fn park_cursor_above_dashboard(&mut self, state: &UiState) -> TuiResult {
        const CURSOR_GUARD_LINES: u16 = 2;
        let terminal_height = self.terminal.size()?.height.max(1);
        let height = self.dashboard_height(state).min(terminal_height);
        let y = terminal_height.saturating_sub(height.saturating_add(CURSOR_GUARD_LINES));
        self.terminal.set_cursor_position(Position { y, x: 0 })?;
        Ok(())
    }

    fn stick_to_bottom(&mut self, state: &UiState) -> TuiResult {
        let terminal_height = self.terminal.size()?.height.max(1);
        let viewport_height = self.dashboard_height(state).min(terminal_height);
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
}

impl Drop for UiSession {
    fn drop(&mut self) {
        let _ = self.terminal.show_cursor();
    }
}
