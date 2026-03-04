use std::{
    collections::VecDeque,
    sync::{Mutex, PoisonError},
};

const MAX_HISTORY_SIZE: usize = 100;

/// Manages playlist navigation with shuffle and history support.
pub(crate) struct Playlist {
    tracks: Vec<String>,
    state: Mutex<PlaylistState>,
}

struct PlaylistState {
    history: VecDeque<usize>,
    shuffle_enabled: bool,
    current_index: Option<usize>,
}

impl Playlist {
    pub(crate) fn new(tracks: Vec<String>) -> Self {
        Self {
            tracks,
            state: Mutex::new(PlaylistState {
                history: VecDeque::new(),
                shuffle_enabled: false,
                current_index: None,
            }),
        }
    }

    pub(crate) fn len(&self) -> usize {
        self.tracks.len()
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.tracks.is_empty()
    }

    pub(crate) fn track_path(&self, index: usize) -> Option<&str> {
        self.tracks.get(index).map(String::as_str)
    }

    pub(crate) fn track_name(&self, index: usize) -> String {
        self.track_path(index)
            .and_then(|p| p.rsplit('/').next())
            .unwrap_or("Unknown")
            .to_string()
    }

    pub(crate) fn toggle_shuffle(&self) -> bool {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        state.shuffle_enabled = !state.shuffle_enabled;
        state.shuffle_enabled
    }

    pub(crate) fn on_track_selected(&self, track_idx: usize) {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(current) = state.current_index
            && current != track_idx
            && state.history.back() != Some(&current)
        {
            add_to_history(&mut state.history, current);
        }
        state.current_index = Some(track_idx);
    }

    pub(crate) fn get_next_track(&self) -> Option<usize> {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        if self.tracks.is_empty() {
            return None;
        }
        let current_idx = state.current_index.unwrap_or(0);
        let next = if current_idx + 1 < self.tracks.len() {
            current_idx + 1
        } else {
            0
        };
        if let Some(current) = state.current_index {
            add_to_history(&mut state.history, current);
        }
        state.current_index = Some(next);
        drop(state);
        Some(next)
    }

    pub(crate) fn get_prev_track(&self) -> Option<usize> {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        if self.tracks.is_empty() {
            return None;
        }
        let current_idx = state.current_index?;
        if current_idx == 0 {
            return None;
        }
        let prev = current_idx - 1;
        state.current_index = Some(prev);
        drop(state);
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
