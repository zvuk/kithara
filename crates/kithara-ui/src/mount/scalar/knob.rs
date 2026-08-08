use bon::Builder;

use crate::{ids::InternId, mount::Control, size::SizeSpec, skin::SkinDoc};

/// A rotary control dragged along the vertical axis.
#[derive(Builder)]
pub(crate) struct Knob {
    pub(crate) label: Option<InternId>,
}

impl Control for Knob {
    fn size(&self, skin: &SkinDoc) -> SizeSpec {
        skin.knob.size
    }
}

#[cfg(feature = "render")]
mod host {
    use num_traits::cast::AsPrimitive;

    use super::Knob;
    use crate::{
        atoms::{knob::Knob as Face, painter::Captioned},
        interact::{
            CursorShape,
            recognizers::{Track, WheelStep},
        },
        render::{
            ReadValue, Skin,
            controls::{Drag, Draws, Grip, Reading},
        },
    };

    /// Where a double click puts a knob: the middle of its travel.
    const RESET: f32 = 0.5;

    impl Draws for Knob {
        type Painter = Face;

        fn painter(&self, skin: &Skin) -> Face {
            Face::new(skin)
        }

        /// A knob is the fraction it points at, so one whose endpoint has not
        /// reported a fraction draws nothing rather than a dial at rest.
        fn data(&self, read: Reading<'_>) -> Option<Captioned> {
            let Some(ReadValue::Scalar(value)) = read.value else {
                return None;
            };
            Some(Captioned {
                label: self.label.map(|label| read.ui.resolve(label).to_owned()),
                value: value.clamp(0.0, 1.0).as_(),
            })
        }

        /// The hand walks a knob up and down rather than pointing at a
        /// position, so the drag counts travel from the value it is on.
        fn grip(&self, skin: &Skin, data: &Captioned) -> Grip {
            Grip::Drag(
                Drag::builder()
                    .cursor(CursorShape::ResizeV)
                    .track(Track::RelativeVertical {
                        range: skin.knob.drag_range,
                        value: data.value,
                    })
                    .reset(RESET)
                    .wheel(WheelStep {
                        value: data.value,
                        step: skin.knob.wheel_step,
                    })
                    .build(),
            )
        }
    }
}
