use std::{borrow::Cow, collections::BTreeMap};

use kithara_app_library::Playable;
use kithara_ui::{
    module::IconName,
    render::{TableCell, TableRow},
};
use url::Url;

use crate::{MediaTrack, Track, TrackId, model::ItemStatus};

pub(super) struct Row {
    pub(super) id: TrackId,
    pub(super) liked: bool,
    title: String,
    artists: String,
    duration: String,
    source: Option<String>,
    drag: Option<BTreeMap<String, String>>,
}

impl Row {
    pub(super) fn new(track: Track, stream: Option<&MediaTrack>) -> Self {
        let artists = track
            .artists
            .iter()
            .map(|artist| artist.title.as_str())
            .collect::<Vec<&str>>()
            .join(", ");
        let (source, drag) = stream
            .and_then(|stream| stream.stream_v3.as_ref())
            .and_then(|stream| stream.hls.as_ref())
            .map(|url| {
                let mut playable = Playable::new(url.to_string());
                playable.title = Some(track.title.clone());
                playable.artist = Some(artists.clone());
                playable.album = track.album().map(str::to_owned);
                playable.artwork = track
                    .artwork()
                    .and_then(|src| Url::parse(&src.replace("{size}", "150x150")).ok());
                (playable.source.clone(), playable.into())
            })
            .unzip();
        let liked = track
            .collection_item_data
            .is_some_and(|data| data.item_status == Some(ItemStatus::Liked));
        Self {
            id: track.id,
            liked,
            title: track.title,
            artists,
            duration: format!("{}:{:02}", track.duration / 60, track.duration % 60),
            source,
            drag,
        }
    }

    pub(super) fn source(&self) -> Option<&str> {
        self.source.as_deref()
    }

    pub(super) fn set_adaptive(&mut self, adaptive: bool) {
        if let Some(drag) = &mut self.drag {
            drag.insert("adaptive_streaming".to_owned(), adaptive.to_string());
        }
    }

    pub(super) fn matches(&self, query: &str) -> bool {
        self.title.to_lowercase().contains(query) || self.artists.to_lowercase().contains(query)
    }

    pub(super) fn view(&self, selected: bool) -> TableRow<'_> {
        let row = TableRow::new(
            vec![
                TableCell::icon(
                    "reaction",
                    if self.liked {
                        IconName::HeartFilled
                    } else {
                        IconName::Heart
                    },
                    self.liked,
                )
                .with_action(self.id.0.as_str()),
                TableCell::text("title", &self.title),
                TableCell::text("artist", &self.artists),
                TableCell::text("time", &self.duration),
            ],
            selected,
        )
        .with_muted(self.drag.is_none());
        match &self.drag {
            Some(drag) => row.with_drag(Cow::Borrowed(drag)),
            None => row,
        }
    }
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::*;

    #[kithara::test]
    #[case(Some("Catalogue album"))]
    #[case(None)]
    fn a_playable_row_carries_catalogue_tags_with_its_stream(#[case] album: Option<&str>) {
        let track: Track = serde_json::from_value(serde_json::json!({
            "id": "track", "title": "Catalogue title", "duration": 120,
            "artists": [{"title": "First"}, {"title": "Second"}],
            "release": {"title": album, "image": {"src": "https://covers.example/{size}/art.jpg"}},
            "collectionItemData": null
        }))
        .expect("catalogue response shape");
        let media: MediaTrack = serde_json::from_value(serde_json::json!({
            "id": "track",
            "streamV3": {"hls": "https://example.com/master.m3u8", "expire": null}
        }))
        .expect("media response shape");
        let mut expected = Playable::new("https://example.com/master.m3u8".to_owned());
        expected.title = Some("Catalogue title".to_owned());
        expected.artist = Some("First, Second".to_owned());
        expected.album = album.map(str::to_owned);
        expected.artwork = Url::parse("https://covers.example/150x150/art.jpg").ok();
        assert_eq!(
            Row::new(track, Some(&media)).view(false).drag(),
            Some(&BTreeMap::from(expected))
        );
    }
}
