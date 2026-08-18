use iced::Size;

use super::{cache::DeckLayout, modules::Modules};

/// Window size in logical pixels the app opens at.
pub(in crate::gui) const WINDOW_SIZE: Size = Size {
    width: 1280.0,
    height: 760.0,
};

/// What the menu says about the window this app runs: the layout it draws and
/// the size and module count it draws them at.
#[derive(fieldwork::Fieldwork)]
#[fieldwork(opt_in, get)]
pub(in crate::gui) struct WindowState {
    size: Size,
    #[field(get, vis = "pub(in crate::gui)")]
    title: String,
    #[field(get, vis = "pub(in crate::gui)")]
    caption: String,
}

impl WindowState {
    pub(in crate::gui) const fn set_size(&mut self, size: Size) {
        self.size = size;
    }

    pub(in crate::gui) fn refresh(&mut self, layout: DeckLayout, modules: &Modules) {
        self.title = format!("WINDOW 1 \u{b7} {}", layout.label());
        self.caption = format!(
            "{} \u{d7} {} \u{b7} {} MOD.",
            self.size.width.round(),
            self.size.height.round(),
            modules.on()
        );
    }
}

impl Default for WindowState {
    fn default() -> Self {
        let mut state = Self {
            size: WINDOW_SIZE,
            title: String::new(),
            caption: String::new(),
        };
        state.refresh(DeckLayout::default(), &Modules::default());
        state
    }
}
