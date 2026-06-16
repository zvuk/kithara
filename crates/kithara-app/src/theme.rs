/// RGB color triple.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rgb(pub u8, pub u8, pub u8);

/// Application color palette shared between TUI and GUI frontends.
///
/// Single source of truth — both frontends convert from this
/// to their framework-specific color types via [`From`].
#[derive(Debug, Clone, Copy)]
pub struct Palette {
    /// Accent color (active elements, highlights).
    pub accent: Rgb,
    /// Strong accent stop for gradients.
    pub accent_strong: Rgb,
    /// Main background.
    pub bg: Rgb,
    /// Deep outer background.
    pub bg_deep: Rgb,
    /// Highest elevation background.
    pub bg_elev: Rgb,
    /// Inset surface behind strips and inline controls.
    pub bg_inset: Rgb,
    /// Panel / elevated surface background.
    pub bg_panel: Rgb,
    /// Secondary panel background.
    pub bg_panel_2: Rgb,
    /// Danger indicator.
    pub danger: Rgb,
    /// Border / divider color.
    pub line: Rgb,
    /// Soft border / divider color.
    pub line_soft: Rgb,
    /// Muted / inactive text.
    pub muted: Rgb,
    /// Success indicator.
    pub success: Rgb,
    /// Primary text.
    pub text: Rgb,
    /// Secondary text.
    pub text_dim: Rgb,
    /// Warning indicator.
    pub warning: Rgb,
}

impl Palette {
    const ACCENT_B: u8 = 66;
    const ACCENT_G: u8 = 148;
    const ACCENT_R: u8 = 187;

    const BG_B: u8 = 46;
    const BG_G: u8 = 26;
    const BG_PANEL_B: u8 = 68;

    const BG_PANEL_G: u8 = 34;
    const BG_PANEL_R: u8 = 34;
    const BG_R: u8 = 26;

    const DANGER_B: u8 = 77;
    const DANGER_G: u8 = 77;
    const DANGER_R: u8 = 230;

    const MUTED_B: u8 = 136;
    const MUTED_G: u8 = 136;
    const MUTED_R: u8 = 136;

    const SUCCESS_B: u8 = 102;
    const SUCCESS_G: u8 = 204;
    const SUCCESS_R: u8 = 102;

    const TEXT_B: u8 = 230;
    const TEXT_G: u8 = 230;
    const TEXT_R: u8 = 230;

    const WARNING_B: u8 = 51;
    const WARNING_G: u8 = 179;
    const WARNING_R: u8 = 230;

    /// Studio surface and accent stops layered on the base theme.
    const ACCENT_STRONG: Rgb = Rgb(214, 173, 89);
    const BG_DEEP: Rgb = Rgb(14, 14, 29);
    const BG_ELEV: Rgb = Rgb(47, 47, 94);
    const BG_INSET: Rgb = Rgb(20, 20, 41);
    const BG_PANEL_2: Rgb = Rgb(42, 42, 84);
    const LINE: Rgb = Rgb(59, 59, 103);
    const LINE_SOFT: Rgb = Rgb(44, 44, 82);
    const TEXT_DIM: Rgb = Rgb(176, 179, 200);

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

    /// Resolved iced color palette.
    #[derive(Debug, Clone, Copy)]
    pub(crate) struct GuiPalette {
        pub(crate) accent: Color,
        pub(crate) accent_glow: Color,
        pub(crate) accent_soft: Color,
        pub(crate) accent_strong: Color,
        pub(crate) bg: Color,
        pub(crate) bg_deep: Color,
        pub(crate) bg_elev: Color,
        pub(crate) bg_inset: Color,
        pub(crate) bg_panel: Color,
        pub(crate) bg_panel_2: Color,
        pub(crate) danger: Color,
        pub(crate) line: Color,
        pub(crate) line_soft: Color,
        pub(crate) muted: Color,
        pub(crate) success: Color,
        pub(crate) text: Color,
        pub(crate) text_dim: Color,
        pub(crate) warning: Color,
    }

    impl From<Palette> for GuiPalette {
        fn from(p: Palette) -> Self {
            Self {
                accent: to_iced(p.accent),
                accent_glow: Color::from_rgba8(p.accent.0, p.accent.1, p.accent.2, 0.45),
                accent_soft: Color::from_rgba8(p.accent.0, p.accent.1, p.accent.2, 0.18),
                accent_strong: to_iced(p.accent_strong),
                bg: to_iced(p.bg),
                bg_deep: to_iced(p.bg_deep),
                bg_elev: to_iced(p.bg_elev),
                bg_inset: to_iced(p.bg_inset),
                bg_panel: to_iced(p.bg_panel),
                bg_panel_2: to_iced(p.bg_panel_2),
                danger: to_iced(p.danger),
                line: to_iced(p.line),
                line_soft: to_iced(p.line_soft),
                muted: to_iced(p.muted),
                success: to_iced(p.success),
                text: to_iced(p.text),
                text_dim: to_iced(p.text_dim),
                warning: to_iced(p.warning),
            }
        }
    }

    fn to_iced(rgb: Rgb) -> Color {
        Color::from_rgb8(rgb.0, rgb.1, rgb.2)
    }

    /// Deck-waveform band colors (Serato-style overlay): `low` magenta, `mid`
    /// yellow, `high` cyan. The deck paints the three as concentric mirrored
    /// bars (low behind, high in front), so all bands stay visible. This is the
    /// single seam where waveform band-color policy lives.
    pub(crate) const WAVE_LOW: Color = Color {
        r: 0.92,
        g: 0.16,
        b: 0.55,
        a: 1.0,
    };
    pub(crate) const WAVE_MID: Color = Color {
        r: 0.95,
        g: 0.82,
        b: 0.16,
        a: 1.0,
    };
    pub(crate) const WAVE_HIGH: Color = Color {
        r: 0.18,
        g: 0.78,
        b: 0.92,
        a: 1.0,
    };
}

#[cfg(feature = "tui")]
pub(crate) mod tui {
    use ratatui::style::Color;

    use super::{Palette, Rgb};

    /// Resolved ratatui color palette.
    #[derive(Debug, Clone, Copy)]
    pub struct TuiPalette {
        pub accent: Color,
        pub bg: Color,
        pub bg_panel: Color,
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
