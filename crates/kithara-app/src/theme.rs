#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rgb(pub u8, pub u8, pub u8);

/// Application color palette. Single source of truth: the frontend
/// converts from this to its framework-specific color type via [`From`].
#[derive(Debug, Clone, Copy)]
pub struct Palette {
    pub accent: Rgb,
    pub accent_strong: Rgb,
    pub bg: Rgb,
    pub bg_deep: Rgb,
    pub bg_elev: Rgb,
    pub bg_inset: Rgb,
    pub bg_panel: Rgb,
    pub bg_panel_2: Rgb,
    pub danger: Rgb,
    pub line: Rgb,
    pub line_soft: Rgb,
    pub muted: Rgb,
    pub success: Rgb,
    pub text: Rgb,
    pub text_dim: Rgb,
    pub warning: Rgb,
}

impl Palette {
    const ACCENT_B: u8 = 66;
    const ACCENT_G: u8 = 148;
    const ACCENT_R: u8 = 187;

    const ACCENT_STRONG: Rgb = Rgb(214, 173, 89);
    const BG_B: u8 = 31;
    const BG_DEEP: Rgb = Rgb(11, 11, 22);

    const BG_ELEV: Rgb = Rgb(38, 38, 74);
    const BG_G: u8 = 18;
    const BG_INSET: Rgb = Rgb(21, 21, 42);

    const BG_PANEL_2: Rgb = Rgb(27, 27, 50);
    const BG_PANEL_B: u8 = 58;
    const BG_PANEL_G: u8 = 32;

    const BG_PANEL_R: u8 = 32;
    const BG_R: u8 = 18;
    const DANGER_B: u8 = 77;

    const DANGER_G: u8 = 77;
    const DANGER_R: u8 = 230;
    const LINE: Rgb = Rgb(59, 59, 103);

    const LINE_SOFT: Rgb = Rgb(42, 42, 76);
    const MUTED_B: u8 = 137;
    const MUTED_G: u8 = 113;

    const MUTED_R: u8 = 111;
    const SUCCESS_B: u8 = 102;
    const SUCCESS_G: u8 = 204;

    const SUCCESS_R: u8 = 102;
    const TEXT_B: u8 = 230;
    const TEXT_DIM: Rgb = Rgb(167, 170, 194);
    const TEXT_G: u8 = 230;
    const TEXT_R: u8 = 230;
    const WARNING_B: u8 = 51;
    const WARNING_G: u8 = 179;
    const WARNING_R: u8 = 230;

    /// Kithara dark + gold theme.
    #[must_use]
    pub const fn kithara() -> Self {
        Self {
            bg: Rgb(Self::BG_R, Self::BG_G, Self::BG_B),
            bg_deep: Self::BG_DEEP,
            bg_elev: Self::BG_ELEV,
            bg_inset: Self::BG_INSET,
            bg_panel: Rgb(Self::BG_PANEL_R, Self::BG_PANEL_G, Self::BG_PANEL_B),
            bg_panel_2: Self::BG_PANEL_2,
            accent: Rgb(Self::ACCENT_R, Self::ACCENT_G, Self::ACCENT_B),
            accent_strong: Self::ACCENT_STRONG,
            line: Self::LINE,
            line_soft: Self::LINE_SOFT,
            muted: Rgb(Self::MUTED_R, Self::MUTED_G, Self::MUTED_B),
            text: Rgb(Self::TEXT_R, Self::TEXT_G, Self::TEXT_B),
            text_dim: Self::TEXT_DIM,
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

#[cfg(feature = "gui")]
pub(crate) mod gui {
    use iced::Color;

    use super::{Palette, Rgb};

    /// Resolved iced color palette for the window theme; module chrome and
    /// control colors come from the `kithara-ui` skin.
    #[derive(Debug, Clone, Copy)]
    pub(crate) struct GuiPalette {
        pub(crate) accent: Color,
        pub(crate) bg: Color,
        pub(crate) danger: Color,
        pub(crate) success: Color,
        pub(crate) text: Color,
        pub(crate) warning: Color,
    }

    impl From<Palette> for GuiPalette {
        fn from(p: Palette) -> Self {
            Self {
                accent: to_iced(p.accent),
                bg: to_iced(p.bg),
                danger: to_iced(p.danger),
                success: to_iced(p.success),
                text: to_iced(p.text),
                warning: to_iced(p.warning),
            }
        }
    }

    fn to_iced(rgb: Rgb) -> Color {
        Color::from_rgb8(rgb.0, rgb.1, rgb.2)
    }
}
