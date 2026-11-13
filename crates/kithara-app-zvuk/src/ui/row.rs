use std::{borrow::Cow, collections::BTreeMap};

use kithara_app_library::{Playable, PlaybackChoice};
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
    album: Option<String>,
    artwork: Option<Url>,
    choices: Vec<PlaybackChoice>,
    drag: Option<BTreeMap<String, String>>,
}

impl Row {
    pub(super) fn new(track: Track, stream: Option<&MediaTrack>, adaptive: bool) -> Self {
        let artists = track
            .artists
            .iter()
            .map(|artist| artist.title.as_str())
            .collect::<Vec<&str>>()
            .join(", ");
        let mut choices: Vec<PlaybackChoice> = Vec::new();
        if let Some(url) = stream
            .and_then(|stream| stream.stream_v3.as_ref())
            .and_then(|stream| stream.hls.as_ref())
        {
            choices.push(PlaybackChoice {
                source: url.clone(),
                label: "AUTO".to_owned(),
                adaptive: true,
            });
        }
        if let Some(progressive) = stream.and_then(|stream| stream.stream.as_ref()) {
            for (label, url) in [
                ("MP3 High", &progressive.high),
                ("MP3 Mid", &progressive.mid),
                ("FLAC", &progressive.flac),
            ] {
                if let Some(url) = url {
                    choices.push(PlaybackChoice {
                        source: url.clone(),
                        label: label.to_owned(),
                        adaptive: false,
                    });
                }
            }
        }
        let album = track.album().map(str::to_owned);
        let artwork = track
            .artwork()
            .and_then(|src| Url::parse(&src.replace("{size}", "150x150")).ok());
        let liked = track
            .collection_item_data
            .is_some_and(|data| data.item_status == Some(ItemStatus::Liked));
        let mut row = Self {
            id: track.id,
            liked,
            title: track.title,
            artists,
            duration: format!("{}:{:02}", track.duration / 60, track.duration % 60),
            album,
            artwork,
            choices,
            drag: None,
        };
        row.set_adaptive(adaptive);
        row
    }

    pub(super) fn source(&self) -> Option<&str> {
        self.drag.as_ref()?.get("source").map(String::as_str)
    }

    pub(super) fn set_adaptive(&mut self, adaptive: bool) {
        self.drag = self
            .choices
            .iter()
            .find(|choice| choice.adaptive == adaptive)
            .map(|choice| {
                let mut playable = Playable::new(choice.source.to_string());
                playable.identity = Some(format!("zvuk:track:{}", self.id.0));
                playable.title = Some(self.title.clone());
                playable.artist = Some(self.artists.clone());
                playable.album = self.album.clone();
                playable.artwork = self.artwork.clone();
                playable.choices = self.choices.clone();
                playable.into()
            });
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

    const HLS: &str = "https://media.example/master.m3u8";
    const HIGH: &str = "https://media.example/high.mp3";
    const MID: &str = "https://media.example/mid.mp3";
    const FLAC: &str = "https://media.example/audio.flac";

    #[kithara::test]
    fn the_source_default_selects_a_real_progressive_or_hls_url() {
        let media: MediaTrack = serde_json::from_value(serde_json::json!({
            "id":"1000",
            "streamV3":{"hls":HLS,"expire":null},
            "stream":{
                "high":HIGH,
                "mid":MID,
                "flac":"https://media.example/lossless.flac"
            }
        }))
        .unwrap();
        for (adaptive, source) in [(false, HIGH), (true, HLS)] {
            let track = serde_json::from_value(serde_json::json!({
                "id":"1000","title":"Title","duration":120,
                "artists":[],"release":null,"collectionItemData":null
            }))
            .unwrap();
            let row = Row::new(track, Some(&media), adaptive);
            let record = row.view(false).drag().cloned().unwrap();
            let playable = Playable::try_from(record).unwrap();

            assert_eq!(playable.source, source);
            assert_eq!(playable.identity.as_deref(), Some("zvuk:track:1000"));
            assert_eq!(playable.choices.len(), 4);
        }
    }

    #[kithara::test]
    fn startup_uses_only_available_sources_of_the_requested_protocol() {
        for (hls, high, mid, flac, adaptive, expected) in [
            (None, None, Some(MID), Some(FLAC), false, Some(MID)),
            (None, None, None, Some(FLAC), false, Some(FLAC)),
            (None, Some(HIGH), None, None, true, None),
            (Some(HLS), None, None, None, false, None),
        ] {
            let media = serde_json::from_value(serde_json::json!({
                "id":"1000", "streamV3":{"hls":hls,"expire":null},
                "stream":{"high":high,"mid":mid,"flac":flac}
            }))
            .unwrap();
            let track = serde_json::from_value(serde_json::json!({
                "id":"1000","title":"Title","duration":120,
                "artists":[],"release":null,"collectionItemData":null
            }))
            .unwrap();
            let row = Row::new(track, Some(&media), adaptive);
            assert_eq!(row.source(), expected);
            assert_eq!(row.view(false).drag().is_some(), expected.is_some());
        }
    }

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
            "streamV3": {"hls": "https://example.com/master.m3u8", "expire": null},
            "stream": null
        }))
        .expect("media response shape");
        let mut expected = Playable::new("https://example.com/master.m3u8".to_owned());
        expected.identity = Some("zvuk:track:track".to_owned());
        expected.choices = vec![PlaybackChoice {
            source: Url::parse(&expected.source).unwrap(),
            label: "AUTO".to_owned(),
            adaptive: true,
        }];
        expected.title = Some("Catalogue title".to_owned());
        expected.artist = Some("First, Second".to_owned());
        expected.album = album.map(str::to_owned);
        expected.artwork = Url::parse("https://covers.example/150x150/art.jpg").ok();
        assert_eq!(
            Row::new(track, Some(&media), true).view(false).drag(),
            Some(&BTreeMap::from(expected))
        );
    }
}
