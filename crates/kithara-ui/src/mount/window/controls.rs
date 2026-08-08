use bon::Builder;

use crate::{
    module::WindowControlsStyle,
    mount::Control,
    size::{Dim, SizeSpec},
    skin::{SkinDoc, WindowControlSkin},
};

/// The close, minimise and maximise buttons.
#[derive(Builder)]
pub(crate) struct Controls {
    pub(crate) style: WindowControlsStyle,
}

impl Control for Controls {
    fn size(&self, skin: &SkinDoc) -> SizeSpec {
        let width = match skin.window.controls(self.style) {
            WindowControlSkin::Buttons {
                minus_icon_size,
                maximize_icon_size,
                close_icon_size,
                gap,
                padding,
            } => minus_icon_size + maximize_icon_size + close_icon_size + gap * 2.0 + padding * 2.0,
            WindowControlSkin::Close { cell_size, .. } => cell_size,
        };
        SizeSpec::new(Dim::Fixed(width), Dim::Fill)
    }
}
