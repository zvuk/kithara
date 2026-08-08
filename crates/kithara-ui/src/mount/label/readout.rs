use bon::Builder;

use crate::{ids::InternId, module::Tone, mount::Control, size::SizeSpec, skin::SkinDoc};

/// A caption with a value beside it, toned by the document.
#[derive(Builder)]
pub(crate) struct Readout {
    pub(crate) framed: bool,
    pub(crate) label: Option<InternId>,
    pub(crate) tone: Tone,
}

impl Control for Readout {
    fn size(&self, skin: &SkinDoc) -> SizeSpec {
        skin.readout.size
    }
}

#[cfg(feature = "render")]
mod host {
    use super::Readout;
    use crate::{
        atoms::readout::{Readout as Face, ReadoutData},
        render::{
            ReadValue, Skin,
            controls::{Draws, Reading},
        },
    };

    impl Draws for Readout {
        type Painter = Face;

        fn painter(&self, skin: &Skin) -> Face {
            Face::new(self.tone, self.framed, skin)
        }

        /// A readout is a caption and the number under it, so one missing
        /// either draws nothing rather than half of itself.
        fn data(&self, read: Reading<'_>) -> Option<ReadoutData> {
            Some(ReadoutData {
                label: read.ui.resolve(self.label?).to_owned(),
                value: match read.value? {
                    ReadValue::Text(value) => (*value).to_owned(),
                    ReadValue::Scalar(value) => format!("{value:.2}"),
                    _ => return None,
                },
            })
        }
    }
}
