use std::collections::VecDeque;

/// Maximum history entries retained for [`NavigationState::prev`].
const MAX_HISTORY_SIZE: usize = 100;

/// Behavior when the queue reaches the last track.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub enum RepeatMode {
    /// Stop after the last track; [`NavigationState::next`] returns `None`.
    #[default]
    Off,
    /// Repeat the currently selected track.
    One,
    /// Loop back to the first track.
    All,
}

/// Pure-logic navigation state: current index, history, shuffle, repeat.
///
/// Mirrors `kithara-app::playlist::PlaylistState`. Caller owns locking;
/// methods take `&mut self` so the surrounding [`Queue`](crate::Queue) can
/// decide the lock granularity.
#[derive(Debug, Default)]
pub struct NavigationState {
    current_index: Option<usize>,
    history: VecDeque<usize>,
    shuffle_enabled: bool,
    repeat_mode: RepeatMode,
}

impl NavigationState {
    /// New empty state: no current track, history empty, shuffle off,
    /// [`RepeatMode::Off`].
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Current track index, if selected.
    #[must_use]
    pub fn current_index(&self) -> Option<usize> {
        self.current_index
    }

    /// Shuffle flag.
    #[must_use]
    pub fn is_shuffle_enabled(&self) -> bool {
        self.shuffle_enabled
    }

    /// Current repeat mode.
    #[must_use]
    pub fn repeat_mode(&self) -> RepeatMode {
        self.repeat_mode
    }

    /// Enable / disable shuffle.
    pub fn set_shuffle(&mut self, on: bool) {
        self.shuffle_enabled = on;
    }

    /// Set repeat mode.
    pub fn set_repeat(&mut self, mode: RepeatMode) {
        self.repeat_mode = mode;
    }

    /// Record an explicit selection. If the previously-current track is
    /// different, it is pushed onto history (deduped against the tail).
    pub fn select(&mut self, idx: usize) {
        if let Some(current) = self.current_index
            && current != idx
            && self.history.back() != Some(&current)
        {
            add_to_history(&mut self.history, current);
        }
        self.current_index = Some(idx);
    }

    /// Advance to the next track.
    ///
    /// Returns `None` when the queue is empty or when the end has been
    /// reached with [`RepeatMode::Off`]. With [`RepeatMode::All`] wraps to
    /// index `0`. With [`RepeatMode::One`] returns the current index.
    pub fn next(&mut self, len: usize) -> Option<usize> {
        if len == 0 {
            return None;
        }
        let Some(current) = self.current_index else {
            self.current_index = Some(0);
            return Some(0);
        };
        if self.repeat_mode == RepeatMode::One {
            return Some(current);
        }
        add_to_history(&mut self.history, current);
        let next = if current + 1 < len {
            current + 1
        } else {
            match self.repeat_mode {
                RepeatMode::All => 0,
                RepeatMode::Off | RepeatMode::One => return None,
            }
        };
        self.current_index = Some(next);
        Some(next)
    }

    /// Go back to the previous track. Returns `None` when at index `0` or
    /// when no track has been selected yet.
    pub fn prev(&mut self) -> Option<usize> {
        let current = self.current_index?;
        if current == 0 {
            return None;
        }
        let prev = current - 1;
        self.current_index = Some(prev);
        Some(prev)
    }
}

fn add_to_history(history: &mut VecDeque<usize>, track_idx: usize) {
    if history.back() == Some(&track_idx) {
        return;
    }
    if history.len() >= MAX_HISTORY_SIZE {
        history.pop_front();
    }
    history.push_back(track_idx);
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::*;

    #[kithara::test]
    fn defaults() {
        let nav = NavigationState::new();
        assert_eq!(nav.current_index(), None);
        assert!(!nav.is_shuffle_enabled());
        assert_eq!(nav.repeat_mode(), RepeatMode::Off);
    }

    #[kithara::test]
    fn select_updates_current_and_pushes_history() {
        let mut nav = NavigationState::new();
        nav.select(2);
        assert_eq!(nav.current_index(), Some(2));
        assert_eq!(nav.history.len(), 0);
        nav.select(5);
        assert_eq!(nav.current_index(), Some(5));
        assert_eq!(nav.history.back(), Some(&2));
    }

    #[kithara::test]
    fn select_dedupes_adjacent_history() {
        let mut nav = NavigationState::new();
        nav.select(1);
        nav.select(1); // same
        nav.select(1); // same
        assert!(nav.history.is_empty());
    }

    #[kithara::test]
    fn next_from_empty_queue_is_none() {
        let mut nav = NavigationState::new();
        assert_eq!(nav.next(0), None);
    }

    #[kithara::test]
    fn next_from_unselected_starts_at_zero() {
        let mut nav = NavigationState::new();
        assert_eq!(nav.next(3), Some(0));
    }

    #[kithara::test]
    fn next_wraps_with_repeat_all() {
        let mut nav = NavigationState::new();
        nav.set_repeat(RepeatMode::All);
        assert_eq!(nav.next(3), Some(0));
        assert_eq!(nav.next(3), Some(1));
        assert_eq!(nav.next(3), Some(2));
        assert_eq!(nav.next(3), Some(0)); // wrap
    }

    #[kithara::test]
    fn next_stops_at_end_with_repeat_off() {
        let mut nav = NavigationState::new();
        nav.select(2);
        assert_eq!(nav.next(3), None);
    }

    #[kithara::test]
    fn next_returns_current_with_repeat_one() {
        let mut nav = NavigationState::new();
        nav.select(1);
        nav.set_repeat(RepeatMode::One);
        assert_eq!(nav.next(3), Some(1));
        assert_eq!(nav.next(3), Some(1));
    }

    #[kithara::test]
    fn prev_at_zero_is_none() {
        let mut nav = NavigationState::new();
        nav.select(0);
        assert_eq!(nav.prev(), None);
    }

    #[kithara::test]
    fn prev_at_unselected_is_none() {
        let mut nav = NavigationState::new();
        assert_eq!(nav.prev(), None);
    }

    #[kithara::test]
    fn prev_decrements() {
        let mut nav = NavigationState::new();
        nav.select(2);
        assert_eq!(nav.prev(), Some(1));
        assert_eq!(nav.prev(), Some(0));
        assert_eq!(nav.prev(), None);
    }

    #[kithara::test]
    fn history_caps_at_max() {
        let mut nav = NavigationState::new();
        nav.select(0);
        for i in 1..=(MAX_HISTORY_SIZE + 10) {
            nav.select(i);
        }
        assert_eq!(nav.history.len(), MAX_HISTORY_SIZE);
    }

    #[kithara::test]
    fn shuffle_toggle() {
        let mut nav = NavigationState::new();
        assert!(!nav.is_shuffle_enabled());
        nav.set_shuffle(true);
        assert!(nav.is_shuffle_enabled());
        nav.set_shuffle(false);
        assert!(!nav.is_shuffle_enabled());
    }

    #[kithara::test]
    fn repeat_mode_roundtrip() {
        let mut nav = NavigationState::new();
        nav.set_repeat(RepeatMode::All);
        assert_eq!(nav.repeat_mode(), RepeatMode::All);
        nav.set_repeat(RepeatMode::One);
        assert_eq!(nav.repeat_mode(), RepeatMode::One);
        nav.set_repeat(RepeatMode::Off);
        assert_eq!(nav.repeat_mode(), RepeatMode::Off);
    }
}
