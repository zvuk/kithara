use iced::Theme;
use kithara_ui::render::Skin;

/// Build the custom dark + gold theme from resolved palette.
#[must_use]
pub(crate) fn kithara_theme(skin: &Skin) -> Theme {
    let p = skin.palette;
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
