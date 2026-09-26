use bon::Builder;

use crate::ids::InternId;

/// The deck's tempo, editable in place.
#[derive(Builder, kithara_derive::ViewControl, kithara_derive::Control)]
#[control(size = skin.deck.bpm_size)]
#[derive(kithara_derive::NodeControl)]
pub(crate) struct Bpm {
    pub(crate) placeholder: Option<InternId>,
}

#[cfg(feature = "render")]
mod host {
    use super::Bpm;
    use crate::{
        atoms::deck::tempo::{Reading as Beat, Tempo as Face},
        render::{
            ReadValue, Skin, WaveformView,
            controls::{Draws, Reading},
            model::derived,
        },
    };

    mod consts {
        /// The one placeholder that stands in for a tempo nobody measured.
        pub(super) const ELAPSED: &str = "time";
    }

    impl Draws for Bpm {
        type Painter = Face;

        /// A measured tempo if the analysis found one; otherwise the deck's
        /// position, but only where the document asked for that stand-in. A
        /// deck that asked for neither draws nothing.
        fn data(&self, read: Reading<'_>) -> Option<Beat> {
            if let Some(bpm) = tempo(read.value) {
                return Some(Beat::Bpm(bpm));
            }
            let placeholder = self.placeholder.map(|id| read.ctx.ui.resolve(id));
            (placeholder == Some(consts::ELAPSED)).then(|| Beat::Position(position(read)))
        }

        fn painter(&self, skin: &Skin) -> Face {
            Face::new(skin)
        }
    }

    /// The tempo the analysis reported, when it reported one that means
    /// anything.
    fn tempo(value: Option<&ReadValue<'_>>) -> Option<f64> {
        let Some(ReadValue::Waveform(WaveformView { bpm, .. })) = value else {
            return None;
        };
        bpm.map(f64::from)
            .filter(|bpm| bpm.is_finite() && *bpm > 0.0)
    }

    /// The deck's own scoped position, or the start of the track.
    fn position(read: Reading<'_>) -> f64 {
        match read
            .ctx
            .get(&derived("deck.playback.position_secs", read.scope))
        {
            Some(ReadValue::Scalar(value)) => value,
            _ => 0.0,
        }
    }
}
