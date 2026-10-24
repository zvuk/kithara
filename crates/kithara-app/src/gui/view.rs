use std::path::Path;

use iced::{Element, window::Id};
use kithara::prelude::ResourceSrc;

use super::{app::Kithara, message::Message};
use crate::engine::DeckSnapshot;

pub(crate) fn view(state: &Kithara, _window: Id) -> Element<'_, Message> {
    super::ui::view(state)
}

/// Display supplied or decoded tags before deriving labels from local folders.
pub(crate) fn track_subtitle(deck: &DeckSnapshot) -> String {
    let Some(index) = deck.current_track_index else {
        return "Artist / Album unavailable".to_string();
    };
    let Some(entry) = deck.tracks.get(index) else {
        return "Artist / Album unavailable".to_string();
    };
    match (
        entry.metadata().artist.as_deref(),
        entry.metadata().album.as_deref(),
    ) {
        (Some(artist), Some(album)) => return format!("{artist} / {album}"),
        (Some(artist), None) => return artist.to_owned(),
        (None, Some(album)) => return album.to_owned(),
        (None, None) => {}
    }
    let Some(url) = entry.url.as_deref() else {
        return "Artist / Album unavailable".to_string();
    };
    if !matches!(ResourceSrc::parse(url), Ok(ResourceSrc::Path(_))) {
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

#[cfg(test)]
mod tests {
    use ::kithara::platform::{sync::Arc, tokio::sync::mpsc};
    use arc_swap::ArcSwap;
    use iced::window::Id;
    use kithara_test_utils::kithara;

    use super::{super::test_fixture, Kithara, Message, view};
    use crate::{engine::EngineSnapshot, gui::update::update};

    #[kithara::test(native, flash(false))]
    fn the_studio_draws_before_the_engine_publishes() {
        let (runtime, config) = test_fixture::runtime_and_config();
        let snapshots = Arc::new(ArcSwap::from_pointee(EngineSnapshot::unpublished()));
        let (commands, _receiver) = mpsc::unbounded_channel();
        let window = Id::unique();
        let mut state = Kithara::mounted(
            test_fixture::boot(runtime.handle(), &config, snapshots, commands, Vec::new()),
            window,
        );

        assert_eq!(update(&mut state, Message::Tick).units(), 0);
        drop(view(&state, window));
    }
}
