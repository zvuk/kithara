use crate::{mount::Control, size::SizeSpec, skin::SkinDoc};

/// The global bar's own button, which opens the settings surface.
pub(crate) struct Settings;

impl Control for Settings {
    fn size(&self, skin: &SkinDoc) -> SizeSpec {
        skin.global_bar.settings_size
    }
}

#[cfg(feature = "render")]
mod host {
    use super::Settings;
    use crate::{
        atoms::bar::settings::Settings as Face,
        render::{
            Icon, Mark, Skin, UiEvent,
            controls::{Draws, Grip, Reading},
        },
    };

    impl Draws for Settings {
        type Painter = Face;

        fn painter(&self, skin: &Skin) -> Face {
            Face::new(skin)
        }

        /// The gear is the button: art that cannot be read leaves an empty box
        /// rather than a frame with a hole in it.
        fn data(&self, _read: Reading<'_>) -> Option<Mark> {
            Icon::Gear.mark()
        }

        /// Pressing it opens a surface the application owns. There is no
        /// endpoint under this button to activate, so it names the event
        /// instead.
        fn grip(&self, _skin: &Skin, _data: &Mark) -> Grip {
            Grip::Command(|| UiEvent::OpenSettings)
        }
    }
}
