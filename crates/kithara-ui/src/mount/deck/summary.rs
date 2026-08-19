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
    #[cfg(feature = "masonry")]
    use crate::render::controls::DataRefresh;
    use crate::{
        atoms::deck::summary::{Loaded, Summary as Face},
        render::{
            ReadValue, Reads, Skin,
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
            Some(snapshot(read.value, &read.ctx, read.scope))
        }

        #[cfg(feature = "masonry")]
        fn retained_refresh(
            &self,
            read: Reading<'_>,
            _endpoint: Option<&str>,
        ) -> Option<DataRefresh<Loaded>> {
            let scope = read.scope.to_owned();
            Some(Box::new(move |data, ctx| {
                let next = snapshot(None, &ctx, &scope);
                std::mem::replace(data, next) != *data
            }))
        }
    }

    fn snapshot(value: Option<&ReadValue<'_>>, reads: &dyn Reads, scope: &str) -> Loaded {
        let title = match value {
            Some(ReadValue::Text(value)) if !value.is_empty() => (*value).to_owned(),
            _ => word(reads, scope, "deck.track.title")
                .filter(|title| !title.is_empty())
                .unwrap_or_else(|| NO_TRACK.to_owned()),
        };
        Loaded {
            source: word(reads, scope, "deck.track.source_kind").unwrap_or_else(unknown),
            title,
        }
    }

    /// One of the deck's own scoped words.
    fn word(reads: &dyn Reads, scope: &str, endpoint: &str) -> Option<String> {
        match reads.get(&derived(endpoint, scope)) {
            Some(ReadValue::Text(value)) => Some(value.to_owned()),
            _ => None,
        }
    }

    #[cfg(test)]
    mod tests {
        use kithara_test_utils::kithara;

        use super::*;

        struct ScopedReads;

        impl Reads for ScopedReads {
            fn get(&self, endpoint: &str) -> Option<ReadValue<'_>> {
                match endpoint {
                    "deck.track.source_kind@deck=a" => Some(ReadValue::Text("FILE")),
                    "deck.track.title@deck=a" => Some(ReadValue::Text("Track A")),
                    "deck.track.source_kind@deck=b" => Some(ReadValue::Text("STREAM")),
                    "deck.track.title@deck=b" => Some(ReadValue::Text("Track B")),
                    _ => None,
                }
            }
        }

        #[kithara::test]
        fn a_summary_snapshot_keeps_its_deck_scope() {
            let a = snapshot(None, &ScopedReads, "@deck=a");
            let b = snapshot(None, &ScopedReads, "@deck=b");

            assert_eq!(a.title, "Track A");
            assert_eq!(a.source, "FILE");
            assert_eq!(b.title, "Track B");
            assert_eq!(b.source, "STREAM");
        }
    }
}
