use crate::{mount::Control, size::SizeSpec, skin::SkinDoc};

/// A horizontal pair of level bars with a volume cap.
pub(crate) struct VuStereo;

impl Control for VuStereo {
    fn size(&self, skin: &SkinDoc) -> SizeSpec {
        skin.vu_stereo.size
    }
}

#[cfg(feature = "render")]
mod host {
    use super::VuStereo;
    use crate::{
        atoms::meter::StereoMeter,
        interact::{CursorShape, recognizers::Track},
        render::{
            ReadValue, Skin, StereoLevels,
            controls::{Drag, Draws, Grip, Reading},
        },
    };

    impl Draws for VuStereo {
        type Painter = StereoMeter;

        fn painter(&self, skin: &Skin) -> StereoMeter {
            StereoMeter::new(skin)
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
                    .cursor(CursorShape::ResizeH)
                    .track(Track::AbsoluteHorizontal)
                    .build(),
            )
        }
    }
}
