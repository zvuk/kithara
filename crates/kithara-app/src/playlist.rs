use std::{
    collections::VecDeque,
    sync::{
        Mutex, PoisonError,
        atomic::{AtomicU8, Ordering},
    },
};

use thunderdome::{Arena, Index};
use url::Url;

const MAX_HISTORY_SIZE: usize = 100;

/// Track status in the loading lifecycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum TrackStatus {
    Pending = 0,
    Loaded = 1,
    /// Soft timeout fired — still loading but taking longer than expected.
    Slow = 2,
    Failed = 3,
}

impl TrackStatus {
    fn from_u8(val: u8) -> Self {
        match val {
            v if v == Self::Loaded as u8 => Self::Loaded,
            v if v == Self::Slow as u8 => Self::Slow,
            v if v == Self::Failed as u8 => Self::Failed,
            _ => Self::Pending,
        }
    }
}

/// Immutable track data (set once at playlist creation).
pub struct Track {
    pub url: String,
    pub name: String,
    pub needs_drm: bool,
}

/// Manages playlist navigation with shuffle and history support.
///
/// Thread-safe via internal `Mutex` (navigation) and `AtomicU8` (track status).
/// Shared between app core and frontends via `Arc<Playlist>`.
pub struct Playlist {
    tracks: Arena<Track>,
    order: Vec<Index>,
    statuses: Vec<AtomicU8>,
    state: Mutex<PlaylistState>,
}

struct PlaylistState {
    history: VecDeque<usize>,
    shuffle_enabled: bool,
    current_index: Option<usize>,
}

impl Playlist {
    #[must_use]
    pub fn new(urls: Vec<String>, drm_domains: &[String]) -> Self {
        let mut tracks = Arena::new();
        let mut order = Vec::with_capacity(urls.len());
        let mut statuses = Vec::with_capacity(urls.len());
        for url in urls {
            let name = extract_track_name(&url);
            let needs_drm = needs_drm_domain(&url, drm_domains);
            let index = tracks.insert(Track {
                url,
                name,
                needs_drm,
            });
            order.push(index);
            statuses.push(AtomicU8::new(TrackStatus::Pending as u8));
        }
        Self {
            tracks,
            order,
            statuses,
            state: Mutex::new(PlaylistState {
                history: VecDeque::new(),
                shuffle_enabled: false,
                current_index: None,
            }),
        }
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.order.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.order.is_empty()
    }

    /// Access immutable track data by playlist index.
    #[must_use]
    pub fn track(&self, index: usize) -> Option<&Track> {
        self.order
            .get(index)
            .and_then(|arena_idx| self.tracks.get(*arena_idx))
    }

    #[must_use]
    pub fn track_path(&self, index: usize) -> Option<&str> {
        self.track(index).map(|t| t.url.as_str())
    }

    #[must_use]
    pub fn track_name(&self, index: usize) -> String {
        self.track(index)
            .map_or_else(|| "Unknown".to_string(), |t| t.name.clone())
    }

    /// Get the loading status of a track (lock-free atomic read).
    #[must_use]
    pub fn track_status(&self, index: usize) -> TrackStatus {
        self.statuses.get(index).map_or(TrackStatus::Pending, |a| {
            TrackStatus::from_u8(a.load(Ordering::Relaxed))
        })
    }

    #[must_use]
    pub fn track_needs_drm(&self, index: usize) -> bool {
        self.track(index).is_some_and(|t| t.needs_drm)
    }

    /// Update the loading status of a track (lock-free atomic write).
    pub fn set_status(&self, index: usize, status: TrackStatus) {
        if let Some(atomic) = self.statuses.get(index) {
            atomic.store(status as u8, Ordering::Relaxed);
        }
    }

    /// Returns the first track index (by playlist order) that is loaded.
    #[must_use]
    pub fn first_loaded_index(&self) -> Option<usize> {
        self.statuses.iter().enumerate().find_map(|(i, a)| {
            if a.load(Ordering::Relaxed) == TrackStatus::Loaded as u8 {
                Some(i)
            } else {
                None
            }
        })
    }

    /// Returns all track names (for UI initialization).
    #[must_use]
    pub fn track_names(&self) -> Vec<String> {
        self.order
            .iter()
            .filter_map(|idx| self.tracks.get(*idx))
            .map(|t| t.name.clone())
            .collect()
    }

    pub fn toggle_shuffle(&self) -> bool {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        state.shuffle_enabled = !state.shuffle_enabled;
        state.shuffle_enabled
    }

    #[must_use]
    pub fn is_shuffle_enabled(&self) -> bool {
        self.state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .shuffle_enabled
    }

    pub fn on_track_selected(&self, track_idx: usize) {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(current) = state.current_index
            && current != track_idx
            && state.history.back() != Some(&current)
        {
            add_to_history(&mut state.history, current);
        }
        state.current_index = Some(track_idx);
    }

    pub fn get_next_track(&self) -> Option<usize> {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        if self.order.is_empty() {
            return None;
        }
        let next = state.current_index.map_or(0, |idx| {
            add_to_history(&mut state.history, idx);
            if idx + 1 < self.order.len() {
                idx + 1
            } else {
                0
            }
        });
        state.current_index = Some(next);
        drop(state);
        Some(next)
    }

    pub fn get_prev_track(&self) -> Option<usize> {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        if self.order.is_empty() {
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

fn extract_track_name(url: &str) -> String {
    url.rsplit('/').next().unwrap_or("Unknown").to_string()
}

fn needs_drm_domain(url: &str, drm_domains: &[String]) -> bool {
    Url::parse(url)
        .ok()
        .and_then(|u| {
            u.host_str()
                .map(|h| drm_domains.iter().any(|d| h.ends_with(d.as_str())))
        })
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_creates_tracks_with_correct_status() {
        let urls = vec![
            "https://example.com/track1.mp3".to_string(),
            "https://cdn.zvq.me/track2.mp3".to_string(),
        ];
        let drm_domains = vec!["zvq.me".to_string()];
        let playlist = Playlist::new(urls, &drm_domains);

        assert_eq!(playlist.len(), 2);
        assert!(!playlist.is_empty());

        let t0 = playlist.track(0).expect("track 0");
        assert_eq!(t0.url, "https://example.com/track1.mp3");
        assert_eq!(t0.name, "track1.mp3");
        assert!(!t0.needs_drm);
        assert_eq!(playlist.track_status(0), TrackStatus::Pending);

        let t1 = playlist.track(1).expect("track 1");
        assert_eq!(t1.url, "https://cdn.zvq.me/track2.mp3");
        assert!(t1.needs_drm);
    }

    #[test]
    fn set_status_updates_track() {
        let urls = vec!["https://example.com/a.mp3".to_string()];
        let playlist = Playlist::new(urls, &[]);

        assert_eq!(playlist.track_status(0), TrackStatus::Pending);
        playlist.set_status(0, TrackStatus::Loaded);
        assert_eq!(playlist.track_status(0), TrackStatus::Loaded);
        playlist.set_status(0, TrackStatus::Failed);
        assert_eq!(playlist.track_status(0), TrackStatus::Failed);
    }

    #[test]
    fn first_loaded_index_returns_earliest() {
        let urls = vec![
            "https://example.com/a.mp3".to_string(),
            "https://example.com/b.mp3".to_string(),
            "https://example.com/c.mp3".to_string(),
        ];
        let playlist = Playlist::new(urls, &[]);

        assert_eq!(playlist.first_loaded_index(), None);

        playlist.set_status(2, TrackStatus::Loaded);
        assert_eq!(playlist.first_loaded_index(), Some(2));

        playlist.set_status(0, TrackStatus::Loaded);
        assert_eq!(playlist.first_loaded_index(), Some(0));
    }

    #[test]
    fn track_names_returns_all_names() {
        let urls = vec![
            "https://example.com/song1.mp3".to_string(),
            "https://example.com/song2.mp3".to_string(),
        ];
        let playlist = Playlist::new(urls, &[]);
        assert_eq!(playlist.track_names(), vec!["song1.mp3", "song2.mp3"]);
    }

    #[test]
    fn navigation_works_with_arena() {
        let urls = vec![
            "https://example.com/a.mp3".to_string(),
            "https://example.com/b.mp3".to_string(),
            "https://example.com/c.mp3".to_string(),
        ];
        let playlist = Playlist::new(urls, &[]);

        assert_eq!(playlist.get_next_track(), Some(0)); // first call → track 0
        assert_eq!(playlist.get_next_track(), Some(1));
        assert_eq!(playlist.get_next_track(), Some(2));
        assert_eq!(playlist.get_next_track(), Some(0)); // wraps
        assert_eq!(playlist.get_prev_track(), None); // at 0, no prev
    }

    #[test]
    fn needs_drm_domain_matches_correctly() {
        let domains = vec!["zvq.me".to_string()];
        assert!(needs_drm_domain(
            "https://cdn-edge.zvq.me/track/streamhq?id=123",
            &domains
        ));
        assert!(!needs_drm_domain(
            "https://stream.silvercomet.top/drm/master.m3u8",
            &domains
        ));
    }
}
