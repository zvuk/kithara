use iced::Theme;

/// Dark background color (#1a1a2e).
pub(crate) const BG_DARK: iced::Color = iced::Color {
    r: 0.102,
    g: 0.102,
    b: 0.180,
    a: 1.0,
};

/// Slightly lighter panel background (#222244).
pub(crate) const BG_PANEL: iced::Color = iced::Color {
    r: 0.133,
    g: 0.133,
    b: 0.267,
    a: 1.0,
};

/// Build the custom dark + gold theme.
#[must_use]
pub(crate) fn kithara_theme() -> Theme {
    let palette = iced::theme::Palette {
        background: BG_DARK,
        text: iced::Color::from_rgb(0.9, 0.9, 0.9),
        primary: crate::icons::GOLD,
        success: iced::Color::from_rgb(0.4, 0.8, 0.4),
        danger: iced::Color::from_rgb(0.9, 0.3, 0.3),
        warning: iced::Color::from_rgb(0.9, 0.7, 0.2),
    };

    Theme::custom("Kithara".to_string(), palette)
}
