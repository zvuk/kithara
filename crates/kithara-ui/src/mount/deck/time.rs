use crate::{mount::Control, size::SizeSpec, skin::SkinDoc};

/// The deck's position and what is left of the track.
pub(crate) struct Time;

impl Control for Time {
    fn size(&self, skin: &SkinDoc) -> SizeSpec {
        skin.deck.time_size
    }
}

#[cfg(feature = "render")]
mod host {
    use super::Time;
    use crate::{
        atoms::deck::clock::{Clock as Face, Elapsed},
        render::{
            ReadValue, Skin,
            controls::{Draws, Reading},
            model::derived,
        },
    };

    impl Draws for Time {
        type Painter = Face;

        fn painter(&self, skin: &Skin) -> Face {
            Face::new(skin)
        }

        /// A clock with no position to show draws nothing rather than zeros:
        /// a deck with no track is empty, not at the start of one.
        fn data(&self, read: Reading<'_>) -> Option<Elapsed> {
            let Some(ReadValue::Scalar(_)) = read.value else {
                return None;
            };
            Some(Elapsed {
                duration: seconds(read, "deck.playback.duration_secs"),
                position: seconds(read, "deck.playback.position_secs"),
            })
        }
    }

    /// One of the deck's own scoped readings, or the start of the track.
    fn seconds(read: Reading<'_>, endpoint: &str) -> f64 {
        match read.reads.get(&derived(endpoint, read.scope)) {
            Some(ReadValue::Scalar(value)) => value,
            _ => 0.0,
        }
    }
}
