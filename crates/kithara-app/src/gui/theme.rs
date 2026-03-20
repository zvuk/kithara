use iced::Theme;

use crate::theme::gui::GuiPalette;

/// Build the custom dark + gold theme from resolved palette.
#[must_use]
pub(crate) fn kithara_theme(p: &GuiPalette) -> Theme {
    let palette = iced::theme::Palette {
        background: p.bg,
        text: p.text,
        primary: p.accent,
        success: p.success,
        danger: p.danger,
        warning: p.warning,
    };

    Theme::custom("Kithara".to_string(), palette)
}
