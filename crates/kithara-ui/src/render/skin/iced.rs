use iced::{Border, Color};

use super::Skin;
use crate::skin::{ColorRole, FrameSkin};

/// Iced-typed view over a neutral [`Skin`].
pub(crate) trait IcedSkin {
    fn border(&self, frame: FrameSkin) -> Border;

    fn color(&self, role: ColorRole) -> Color;
}

impl IcedSkin for Skin {
    fn border(&self, frame: FrameSkin) -> Border {
        Border {
            color: self.color(frame.border),
            width: frame.border_width,
            radius: frame.radius.into(),
        }
    }

    fn color(&self, role: ColorRole) -> Color {
        self.rgba(role).into()
    }
}
