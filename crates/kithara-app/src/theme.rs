/// RGB color triple.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rgb(pub u8, pub u8, pub u8);

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
    /// Kithara dark + gold theme.
    #[must_use]
    pub const fn kithara() -> Self {
        Self {
            bg: Rgb(BG_R, BG_G, BG_B),
            bg_panel: Rgb(BG_PANEL_R, BG_PANEL_G, BG_PANEL_B),
            accent: Rgb(ACCENT_R, ACCENT_G, ACCENT_B),
            muted: Rgb(MUTED_R, MUTED_G, MUTED_B),
            text: Rgb(TEXT_R, TEXT_G, TEXT_B),
            success: Rgb(SUCCESS_R, SUCCESS_G, SUCCESS_B),
            danger: Rgb(DANGER_R, DANGER_G, DANGER_B),
            warning: Rgb(WARNING_R, WARNING_G, WARNING_B),
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
        pub muted: Color,
        pub text: Color,
    }

    impl From<Palette> for TuiPalette {
        fn from(p: Palette) -> Self {
            Self {
                bg: to_ratatui(p.bg),
                bg_panel: to_ratatui(p.bg_panel),
                accent: to_ratatui(p.accent),
                muted: to_ratatui(p.muted),
                text: to_ratatui(p.text),
            }
        }
    }

    fn to_ratatui(rgb: Rgb) -> Color {
        Color::Rgb(rgb.0, rgb.1, rgb.2)
    }
}
