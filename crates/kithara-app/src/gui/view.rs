use std::path::Path;

use iced::Element;
use kithara::prelude::{ResourceConfig, ResourceSrc};

use super::{app::Kithara, message::Message};
use crate::state::UiState;

pub(crate) fn view(state: &Kithara, _window: iced::window::Id) -> Element<'_, Message> {
    super::ui::view(state)
}

/// The playhead the UI shows: while the user drags, that is the seek target.
pub(crate) const fn playhead(ui: &UiState) -> f64 {
    if ui.is_seeking {
        ui.seek_position
    } else {
        ui.position
    }
}

/// Folder-derived artist/album only makes sense for local files; a remote
/// URL has no meaningful parent directories.
pub(crate) fn track_subtitle(ui: &UiState) -> String {
    let Some(index) = ui.current_track_index else {
        return "Artist / Album unavailable".to_string();
    };
    let Some(entry) = ui.tracks.get(index) else {
        return "Artist / Album unavailable".to_string();
    };
    let Some(url) = entry.url.as_deref() else {
        return "Artist / Album unavailable".to_string();
    };
    if !matches!(ResourceConfig::parse_src(url), Ok(ResourceSrc::Path(_))) {
        return "Artist / Album unavailable".to_string();
    }

    let path = Path::new(url);
    let album = path
        .parent()
        .and_then(|p| p.file_name())
        .and_then(|p| p.to_str());
    let artist = path
        .parent()
        .and_then(|p| p.parent())
        .and_then(|p| p.file_name())
        .and_then(|p| p.to_str());

    match (artist, album) {
        (Some(artist), Some(album)) if !artist.is_empty() && !album.is_empty() => {
            format!("{artist} / {album}")
        }
        (None, Some(album)) if !album.is_empty() => album.to_string(),
        _ => "Artist / Album unavailable".to_string(),
    }
}
