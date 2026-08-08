use bon::Builder;

use crate::{module::DeckSummaryStyle, mount::Control, size::SizeSpec, skin::SkinDoc};

/// The deck's headline: what is loaded and how it is playing.
#[derive(Builder)]
pub(crate) struct Summary {
    pub(crate) style: DeckSummaryStyle,
}

impl Control for Summary {
    fn size(&self, skin: &SkinDoc) -> SizeSpec {
        skin.deck.summary_size
    }
}

#[cfg(feature = "render")]
mod host {
    use super::Summary;
    use crate::{
        atoms::deck::summary::{Loaded, Summary as Face},
        render::{
            ReadValue, Skin,
            controls::{Draws, Reading},
            model::derived,
        },
    };

    /// What a deck with nothing loaded says.
    const NO_TRACK: &str = "No track loaded";

    /// What stands in for a source nobody reported.
    fn unknown() -> String {
        "\u{2014}".to_owned()
    }

    impl Draws for Summary {
        type Painter = Face;

        fn painter(&self, skin: &Skin) -> Face {
            Face::new(self.style, skin)
        }

        /// A summary always draws: a deck with nothing loaded says so, which
        /// is a headline of its own rather than an empty panel.
        fn data(&self, read: Reading<'_>) -> Option<Loaded> {
            let title = match read.value {
                Some(ReadValue::Text(value)) if !value.is_empty() => (*value).to_owned(),
                _ => word(read, "deck.track.title")
                    .filter(|title| !title.is_empty())
                    .unwrap_or_else(|| NO_TRACK.to_owned()),
            };
            Some(Loaded {
                source: word(read, "deck.track.source_kind").unwrap_or_else(unknown),
                title,
            })
        }
    }

    /// One of the deck's own scoped words.
    fn word(read: Reading<'_>, endpoint: &str) -> Option<String> {
        match read.reads.get(&derived(endpoint, read.scope)) {
            Some(ReadValue::Text(value)) => Some(value.to_owned()),
            _ => None,
        }
    }
}
