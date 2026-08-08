use bon::Builder;

use crate::{mount::Control, size::SizeSpec, skin::SkinDoc};

/// A vertical pair of level bars with a volume cap.
#[derive(Builder)]
pub(crate) struct VuVertical {
    pub(crate) ticks: bool,
}

impl Control for VuVertical {
    fn size(&self, skin: &SkinDoc) -> SizeSpec {
        skin.vu_vertical.size
    }
}

#[cfg(feature = "render")]
mod host {
    use super::VuVertical;
    use crate::{
        atoms::vu::VerticalVu,
        interact::{CursorShape, recognizers::Track},
        render::{
            ReadValue, Skin, StereoLevels,
            controls::{Drag, Draws, Grip, Reading},
        },
    };

    impl Draws for VuVertical {
        type Painter = VerticalVu;

        fn painter(&self, skin: &Skin) -> VerticalVu {
            VerticalVu::new(self.ticks, skin)
        }

        fn data(&self, read: Reading<'_>) -> Option<StereoLevels> {
            match read.value {
                Some(ReadValue::Stereo(levels)) => Some(*levels),
                _ => None,
            }
        }

        fn grip(&self, _skin: &Skin, _data: &StereoLevels) -> Grip {
            Grip::Drag(
                Drag::builder()
                    .cursor(CursorShape::ResizeV)
                    .track(Track::AbsoluteVertical)
                    .build(),
            )
        }
    }
}
