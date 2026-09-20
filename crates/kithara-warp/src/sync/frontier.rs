use std::num::NonZeroU32;

use num_traits::cast::ToPrimitive;

use crate::{MapAxis, SessionFrame, WarpMapRevision};

/// An exact source/output boundary consumed by the audio callback.
#[derive(Clone, Copy, Debug, Eq, PartialEq, bon::Builder, fieldwork::Fieldwork)]
#[builder(state_mod(vis = "pub"))]
#[fieldwork(opt_in, get)]
#[non_exhaustive]
pub struct PresentationFrontier {
    /// Exclusive session output-frame boundary actually consumed.
    #[field(get, copy)]
    output: SessionFrame,
    /// Exclusive decoded source-frame boundary actually consumed.
    #[field(get, copy)]
    source: u64,
    /// Exact immutable warp map represented by consumed PCM.
    #[field(get, copy)]
    warp_map: Option<WarpMapRevision>,
}

impl PresentationFrontier {
    /// Restates this frontier, published in the output frames the decoder
    /// emits, on a grid's own frame axis.
    ///
    /// A beat grid measures every marker, segment and cue on its own axis,
    /// so a frontier crossing into grid arithmetic is converted once here.
    #[must_use]
    pub fn on_axis(self, axis: MapAxis, output_rate: NonZeroU32) -> Self {
        Self {
            source: axis
                .native_frame(self.source, output_rate)
                .round()
                .to_u64()
                .unwrap_or_default(),
            ..self
        }
    }
}
