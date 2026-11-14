use std::collections::{BTreeMap, HashSet};

use serde::Deserialize;
use serde_json::{Value, json};
use url::Url;

mod consts {
    pub(super) const SOURCE: &str = "source";
    pub(super) const TITLE: &str = "title";
    pub(super) const ARTIST: &str = "artist";
    pub(super) const ALBUM: &str = "album";
    pub(super) const ARTWORK: &str = "artwork";
    pub(super) const IDENTITY: &str = "identity";
    pub(super) const CHOICES: &str = "choices";
}

/// One available source for a catalogue track's quality menu.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct PlaybackChoice {
    pub source: Url,
    pub label: String,
    /// This source supports automatic adaptation.
    pub adaptive: bool,
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
    /// Provider-qualified catalogue identity, independent of expiring media URLs.
    pub identity: Option<String>,
    /// Available sources; `source` names the initial choice when nonempty.
    pub choices: Vec<PlaybackChoice>,
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
            identity: None,
            choices: Vec::new(),
        }
    }

    /// Check the initial source and the catalogue's available choices.
    ///
    /// # Errors
    /// Returns [`InvalidPlayable`] for malformed identities, URLs or choices.
    pub fn validate(&self) -> Result<(), InvalidPlayable> {
        if self.source.is_empty() {
            return Err(InvalidPlayable);
        }
        if self.choices.is_empty() {
            return if self.identity.is_none() {
                Ok(())
            } else {
                Err(InvalidPlayable)
            };
        }
        if !self.identity.as_deref().is_some_and(visible_text) {
            return Err(InvalidPlayable);
        }
        let initial = Url::parse(&self.source).map_err(|_| InvalidPlayable)?;
        let mut sources = HashSet::new();
        let valid = self.choices.iter().all(|choice| {
            matches!(choice.source.scheme(), "http" | "https")
                && visible_text(&choice.label)
                && sources.insert(&choice.source)
        });
        if !valid
            || self.choices.iter().filter(|choice| choice.adaptive).count() > 1
            || !self.choices.iter().any(|choice| choice.source == initial)
        {
            return Err(InvalidPlayable);
        }
        Ok(())
    }
}

fn visible_text(value: &str) -> bool {
    !value.is_empty() && value.trim() == value && !value.chars().any(char::is_control)
}

/// A drag record with no source or invalid catalogue choices.
#[derive(Debug, thiserror::Error)]
#[error("the record has no source or invalid catalogue choices")]
pub struct InvalidPlayable;

impl From<Playable> for BTreeMap<String, String> {
    fn from(track: Playable) -> Self {
        let choices = (!track.choices.is_empty()).then(|| {
            Value::Array(
                track
                    .choices
                    .into_iter()
                    .map(|choice| {
                        json!({
                            "adaptive": choice.adaptive,
                            "label": choice.label,
                            "source": choice.source.as_str(),
                        })
                    })
                    .collect(),
            )
            .to_string()
        });
        [
            (consts::SOURCE, Some(track.source)),
            (consts::TITLE, track.title),
            (consts::ARTIST, track.artist),
            (consts::ALBUM, track.album),
            (consts::ARTWORK, track.artwork.map(String::from)),
            (consts::IDENTITY, track.identity),
            (consts::CHOICES, choices),
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
        let identity = record.remove(consts::IDENTITY);
        let choices = record
            .remove(consts::CHOICES)
            .map(|value| serde_json::from_str(&value).map_err(|_| InvalidPlayable))
            .transpose()?
            .unwrap_or_default();
        let mut take = |key: &str| record.remove(key).filter(|value| !value.is_empty());
        let track = Self {
            source: take(consts::SOURCE).ok_or(InvalidPlayable)?,
            title: take(consts::TITLE),
            artist: take(consts::ARTIST),
            album: take(consts::ALBUM),
            artwork: take(consts::ARTWORK).and_then(|value| Url::parse(&value).ok()),
            identity,
            choices,
        };
        track.validate()?;
        Ok(track)
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
    fn source_choices_and_identity_survive_the_drag_record() {
        let choices = r#"[{"adaptive":true,"label":"AUTO","source":"https://example.com/master.m3u8"},{"adaptive":false,"label":"High MP3","source":"https://example.com/high.mp3"}]"#;
        let fields = record(&[
            ("source", "https://example.com/high.mp3"),
            ("identity", "zvuk:track:1000"),
            ("choices", choices),
        ]);
        let track = Playable::try_from(fields.clone()).expect("a valid quality record");

        assert_eq!(BTreeMap::from(track), fields);
    }

    #[kithara::test]
    fn malformed_source_choices_are_rejected() {
        for choices in [
            "not json",
            "{}",
            r#"[{"adaptive":false,"label":"High MP3","source":"file:///private/media"}]"#,
            r#"[{"adaptive":false,"label":"High MP3","source":"https://example.com/other.mp3"}]"#,
            r#"[{"adaptive":false,"label":"","source":"https://example.com/high.mp3"}]"#,
            r#"[{"adaptive":false,"label":"High MP3","source":"https://example.com/high.mp3"},{"adaptive":false,"label":"Mid MP3","source":"https://example.com/high.mp3"}]"#,
            r#"[{"adaptive":true,"label":"AUTO","source":"https://example.com/high.mp3"},{"adaptive":true,"label":"AUTO","source":"https://example.com/master.m3u8"}]"#,
        ] {
            assert!(
                Playable::try_from(record(&[
                    ("source", "https://example.com/high.mp3"),
                    ("identity", "zvuk:track:1000"),
                    ("choices", choices),
                ]))
                .is_err(),
                "invalid choices were accepted: {choices}"
            );
        }
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
    fn catalogue_choices_require_a_valid_identity() {
        let choices =
            r#"[{"adaptive":false,"label":"High MP3","source":"https://example.com/high.mp3"}]"#;
        for identity in [None, Some(""), Some(" "), Some("zvuk:track:\n1000")] {
            let mut fields = record(&[
                ("source", "https://example.com/high.mp3"),
                ("choices", choices),
            ]);
            if let Some(identity) = identity {
                fields.insert("identity".to_owned(), identity.to_owned());
            }
            assert!(Playable::try_from(fields).is_err());
        }
        assert!(
            Playable::try_from(record(&[
                ("source", "https://example.com/high.mp3"),
                ("identity", "zvuk:track:1000"),
                ("choices", "[]"),
            ]))
            .is_err()
        );
    }

    #[kithara::test]
    #[case(true)]
    #[case(false)]
    fn a_source_choice_survives_the_drag_record(#[case] adaptive: bool) {
        let mut track = Playable::new("https://example.com/master.m3u8".to_owned());
        track.identity = Some("zvuk:track:1000".to_owned());
        track.choices.push(PlaybackChoice {
            source: Url::parse(&track.source).unwrap(),
            label: "Quality".to_owned(),
            adaptive,
        });
        assert_eq!(
            Playable::try_from(BTreeMap::from(track.clone())).ok(),
            Some(track)
        );
    }
}
