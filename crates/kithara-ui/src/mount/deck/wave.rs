use bon::Builder;

use crate::{
    expand::Binding, ids::InternId, module::WaveStyle, mount::Control, size::SizeSpec,
    skin::SkinDoc,
};

/// The track's waveform, zoomed and scrubbed.
#[derive(Builder)]
pub(crate) struct Wave<'a> {
    pub(crate) badge: Option<InternId>,
    pub(crate) style: WaveStyle,
    pub(crate) zoom: Option<&'a Binding>,
}

impl Control for Wave<'_> {
    fn size(&self, skin: &SkinDoc) -> SizeSpec {
        skin.wave.size
    }
}

#[cfg(feature = "render")]
mod host {
    use super::Wave;
    use crate::{
        atoms::wave::face::{Drawn, Wave as Face},
        render::{
            Skin,
            controls::{Draws, Reading},
            document::read::wave_zoom,
        },
    };

    impl Draws for Wave<'_> {
        type Painter = Face;

        fn painter(&self, skin: &Skin) -> Face {
            Face::new(self.style, skin)
        }

        /// A deck with no track loaded still draws its wave: an empty shape in
        /// the frame, and — on the hero wave — the panel saying so.
        fn data(&self, read: Reading<'_>) -> Option<Drawn> {
            Some(Drawn::read(
                self.style,
                wave_zoom(self.zoom, read.reads, read.ui),
                self.badge.map(|id| read.ui.resolve(id)),
                read.value,
                read.reads,
                read.scope,
            ))
        }
    }
}
