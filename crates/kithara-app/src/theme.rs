/// RGB color triple.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rgb(pub u8, pub u8, pub u8);

/// Application color palette shared between TUI and GUI frontends.
///
/// Single source of truth — both frontends convert from this
/// to their framework-specific color types via [`From`].
#[derive(Debug, Clone, Copy)]
pub struct Palette {
    /// Main background.
    pub bg: Rgb,
    /// Panel / elevated surface background.
    pub bg_panel: Rgb,
    /// Accent color (active elements, highlights).
    pub accent: Rgb,
    /// Muted / inactive text.
    pub muted: Rgb,
    /// Primary text.
    pub text: Rgb,
    /// Success indicator.
    pub success: Rgb,
    /// Danger indicator.
    pub danger: Rgb,
    /// Warning indicator.
    pub warning: Rgb,
}

impl Palette {
    // Kithara dark + gold palette RGB values

    const BG_R: u8 = 26;
    const BG_G: u8 = 26;
    const BG_B: u8 = 46;

    const BG_PANEL_R: u8 = 34;
    const BG_PANEL_G: u8 = 34;
    const BG_PANEL_B: u8 = 68;

    const ACCENT_R: u8 = 187;
    const ACCENT_G: u8 = 148;
    const ACCENT_B: u8 = 66;

    const MUTED_R: u8 = 136;
    const MUTED_G: u8 = 136;
    const MUTED_B: u8 = 136;

    const TEXT_R: u8 = 230;
    const TEXT_G: u8 = 230;
    const TEXT_B: u8 = 230;

    const SUCCESS_R: u8 = 102;
    const SUCCESS_G: u8 = 204;
    const SUCCESS_B: u8 = 102;

    const DANGER_R: u8 = 230;
    const DANGER_G: u8 = 77;
    const DANGER_B: u8 = 77;

    const WARNING_R: u8 = 230;
    const WARNING_G: u8 = 179;
    const WARNING_B: u8 = 51;

    /// Kithara dark + gold theme.
    #[must_use]
    pub const fn kithara() -> Self {
        Self {
            bg: Rgb(Self::BG_R, Self::BG_G, Self::BG_B),
            bg_panel: Rgb(Self::BG_PANEL_R, Self::BG_PANEL_G, Self::BG_PANEL_B),
            accent: Rgb(Self::ACCENT_R, Self::ACCENT_G, Self::ACCENT_B),
            muted: Rgb(Self::MUTED_R, Self::MUTED_G, Self::MUTED_B),
            text: Rgb(Self::TEXT_R, Self::TEXT_G, Self::TEXT_B),
            success: Rgb(Self::SUCCESS_R, Self::SUCCESS_G, Self::SUCCESS_B),
            danger: Rgb(Self::DANGER_R, Self::DANGER_G, Self::DANGER_B),
            warning: Rgb(Self::WARNING_R, Self::WARNING_G, Self::WARNING_B),
        }
    }
}

impl Default for Palette {
    fn default() -> Self {
        Self::kithara()
    }
}

// GUI (iced) palette

#[cfg(feature = "gui")]
pub(crate) mod gui {
    use iced::Color;

    use super::{Palette, Rgb};

    /// Resolved iced color palette.
    #[derive(Debug, Clone, Copy)]
    pub(crate) struct GuiPalette {
        pub bg: Color,
        pub bg_panel: Color,
        pub accent: Color,
        pub muted: Color,
        pub text: Color,
        pub success: Color,
        pub danger: Color,
        pub warning: Color,
    }

    impl From<Palette> for GuiPalette {
        fn from(p: Palette) -> Self {
            Self {
                bg: to_iced(p.bg),
                bg_panel: to_iced(p.bg_panel),
                accent: to_iced(p.accent),
                muted: to_iced(p.muted),
                text: to_iced(p.text),
                success: to_iced(p.success),
                danger: to_iced(p.danger),
                warning: to_iced(p.warning),
            }
        }
    }

    fn to_iced(rgb: Rgb) -> Color {
        Color::from_rgb8(rgb.0, rgb.1, rgb.2)
    }
}

// TUI (ratatui) palette

#[cfg(feature = "tui")]
pub(crate) mod tui {
    use ratatui::style::Color;

    use super::{Palette, Rgb};

    /// Resolved ratatui color palette.
    #[derive(Debug, Clone, Copy)]
    pub struct TuiPalette {
        pub bg: Color,
        pub bg_panel: Color,
        pub accent: Color,
        pub danger: Color,
        pub muted: Color,
        pub text: Color,
        pub warning: Color,
    }

    impl From<Palette> for TuiPalette {
        fn from(p: Palette) -> Self {
            Self {
                bg: to_ratatui(p.bg),
                bg_panel: to_ratatui(p.bg_panel),
                accent: to_ratatui(p.accent),
                danger: to_ratatui(p.danger),
                muted: to_ratatui(p.muted),
                text: to_ratatui(p.text),
                warning: to_ratatui(p.warning),
            }
        }
    }

    fn to_ratatui(rgb: Rgb) -> Color {
        Color::Rgb(rgb.0, rgb.1, rgb.2)
    }
}
