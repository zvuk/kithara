use std::collections::BTreeMap;

use url::Url;

mod consts {
    pub(super) const SOURCE: &str = "source";
    pub(super) const TITLE: &str = "title";
    pub(super) const ARTIST: &str = "artist";
    pub(super) const ALBUM: &str = "album";
    pub(super) const ARTWORK: &str = "artwork";
    pub(super) const ADAPTIVE: &str = "adaptive_streaming";
}

/// The track a source's row hands a deck it is dropped on: what the deck
/// plays and the tags the source knows before the decoder reads any. A row
/// carries it as its drag record.
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub struct Playable {
    pub source: String,
    pub title: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
    /// Where the track's cover image is read from.
    pub artwork: Option<Url>,
    /// Startup adaptation policy; absent leaves the application's default.
    pub adaptive_streaming: Option<bool>,
}

impl Playable {
    #[must_use]
    pub const fn new(source: String) -> Self {
        Self {
            source,
            title: None,
            artist: None,
            album: None,
            artwork: None,
            adaptive_streaming: None,
        }
    }
}

/// A drag record with no source or an invalid startup policy.
#[derive(Debug, thiserror::Error)]
#[error("the record has no source or an invalid startup policy")]
pub struct InvalidPlayable;

impl From<Playable> for BTreeMap<String, String> {
    fn from(track: Playable) -> Self {
        [
            (consts::SOURCE, Some(track.source)),
            (consts::TITLE, track.title),
            (consts::ARTIST, track.artist),
            (consts::ALBUM, track.album),
            (consts::ARTWORK, track.artwork.map(String::from)),
            (
                consts::ADAPTIVE,
                track.adaptive_streaming.map(|value| value.to_string()),
            ),
        ]
        .into_iter()
        .filter_map(|(key, value)| Some((key.to_owned(), value?)))
        .collect()
    }
}

/// An empty tag reads as unknown, and a cover that is not a URL as none.
impl TryFrom<BTreeMap<String, String>> for Playable {
    type Error = InvalidPlayable;

    fn try_from(mut record: BTreeMap<String, String>) -> Result<Self, InvalidPlayable> {
        let adaptive_streaming = record
            .remove(consts::ADAPTIVE)
            .map(|value| value.parse().map_err(|_| InvalidPlayable))
            .transpose()?;
        let mut take = |key: &str| record.remove(key).filter(|value| !value.is_empty());
        Ok(Self {
            source: take(consts::SOURCE).ok_or(InvalidPlayable)?,
            title: take(consts::TITLE),
            artist: take(consts::ARTIST),
            album: take(consts::ALBUM),
            artwork: take(consts::ARTWORK).and_then(|value| Url::parse(&value).ok()),
            adaptive_streaming,
        })
    }
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::*;

    fn record(entries: &[(&str, &str)]) -> BTreeMap<String, String> {
        entries
            .iter()
            .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
            .collect()
    }

    #[kithara::test]
    fn a_track_survives_its_drag_record_and_loses_what_it_cannot_carry() {
        let mut track = Playable::new("https://example.com/a.m3u8".to_owned());
        track.title = Some("Title".to_owned());
        track.album = Some("Album".to_owned());
        track.artwork = Url::parse("https://covers.example/a.jpg").ok();

        assert_eq!(
            Playable::try_from(BTreeMap::from(track.clone())).ok(),
            Some(track)
        );
        assert_eq!(
            Playable::try_from(record(&[
                ("source", "/music/a.mp3"),
                ("artist", ""),
                ("artwork", "not a url"),
            ]))
            .ok(),
            Some(Playable::new("/music/a.mp3".to_owned()))
        );
        assert!(Playable::try_from(record(&[("source", ""), ("title", "Title")])).is_err());
    }

    #[kithara::test]
    fn malformed_startup_policy_is_rejected() {
        assert!(
            Playable::try_from(record(&[
                ("source", "https://example.com/a.m3u8"),
                ("adaptive_streaming", "sometimes"),
            ]))
            .is_err()
        );
    }

    #[kithara::test]
    #[case(None)]
    #[case(Some(true))]
    #[case(Some(false))]
    fn startup_policy_survives_the_drag_record(#[case] adaptive: Option<bool>) {
        let mut track = Playable::new("https://example.com/master.m3u8".to_owned());
        track.adaptive_streaming = adaptive;
        assert_eq!(
            Playable::try_from(BTreeMap::from(track.clone())).ok(),
            Some(track)
        );
    }
}
