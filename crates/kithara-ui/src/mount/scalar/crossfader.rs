use bon::Builder;

use crate::{mount::Control, size::SizeSpec, skin::SkinDoc};

/// A horizontal fader centred on its midpoint.
#[derive(Builder)]
pub(crate) struct Crossfader {
    pub(crate) ticks: bool,
}

impl Control for Crossfader {
    fn size(&self, skin: &SkinDoc) -> SizeSpec {
        skin.crossfader.size
    }
}

#[cfg(feature = "render")]
mod host {
    use num_traits::cast::AsPrimitive;

    use super::Crossfader;
    use crate::{
        atoms::design::crossfader::Crossfader as Face,
        interact::{CursorShape, recognizers::Track},
        render::{
            ReadValue, Skin,
            controls::{Drag, Draws, Grip, Reading},
        },
    };

    impl Draws for Crossfader {
        type Painter = Face;

        fn painter(&self, skin: &Skin) -> Face {
            Face::new(self.ticks, skin)
        }

        /// A crossfader is the fraction it sits at, so one whose endpoint has
        /// not reported a fraction draws nothing rather than a rail centred on
        /// a guess.
        fn data(&self, read: Reading<'_>) -> Option<f32> {
            let Some(ReadValue::Scalar(value)) = read.value else {
                return None;
            };
            Some(value.clamp(0.0, 1.0).as_())
        }

        /// The hand points at a position on the rail rather than walking the
        /// value, so the press seeks straight there.
        fn grip(&self, _skin: &Skin, _data: &f32) -> Grip {
            Grip::Drag(
                Drag::builder()
                    .cursor(CursorShape::ResizeH)
                    .track(Track::AbsoluteHorizontal)
                    .build(),
            )
        }
    }
}
