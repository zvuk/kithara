use bon::Builder;

use crate::{
    expand::Binding, ids::InternId, module::Tone, mount::Control, size::SizeSpec, skin::SkinDoc,
};

/// A toned dot beside a word.
#[derive(Builder)]
pub(crate) struct StatusDot<'a> {
    pub(crate) active: Option<&'a Binding>,
    pub(crate) active_tone: Option<Tone>,
    pub(crate) dot_size: Option<f32>,
    pub(crate) label: InternId,
    pub(crate) tone: Tone,
}

impl Control for StatusDot<'_> {
    fn size(&self, skin: &SkinDoc) -> SizeSpec {
        skin.status_dot.size
    }
}

#[cfg(feature = "render")]
mod host {
    use super::StatusDot;
    use crate::{
        atoms::design::status_dot::{StatusDot as Face, StatusDotData},
        render::{
            ReadValue, Skin,
            controls::{Draws, Reading},
        },
    };

    impl Draws for StatusDot<'_> {
        type Painter = Face;

        fn painter(&self, skin: &Skin) -> Face {
            Face::with_active_tone(self.tone, self.active_tone, self.dot_size, skin)
        }

        fn data(&self, read: Reading<'_>) -> Option<StatusDotData> {
            Some(StatusDotData {
                active: self.active.is_some_and(|binding| {
                    matches!(read.ctx.read(binding), Some(ReadValue::Bool(true)))
                }),
                label: read.ctx.ui.resolve(self.label).to_owned(),
            })
        }
    }
}
