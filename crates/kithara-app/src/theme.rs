/// RGB color triple.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rgb(pub u8, pub u8, pub u8);

/// Application color palette shared between TUI and GUI frontends.
///
/// Single source of truth — both frontends convert from this
/// to their framework-specific color types via [`From`].
#[derive(Debug, Clone, Copy)]
#[non_exhaustive]
pub struct Palette {
    /// Accent color (active elements, highlights).
    pub accent: Rgb,
    /// Strong accent stop for gradients.
    pub accent_strong: Rgb,
    /// Main background.
    pub bg: Rgb,
    /// Deep outer background.
    pub bg_deep: Rgb,
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
    /// Darker surfaces used by the modular GUI canvas.
    pub canvas: CanvasPalette,
}

/// Dark modular-canvas palette layered on the canonical application colors.
#[derive(Debug, Clone, Copy)]
#[non_exhaustive]
pub struct CanvasPalette {
    /// Window background.
    pub bg: Rgb,
    /// Structural-gap and waveform background.
    pub bg_deep: Rgb,
    /// Module background.
    pub bg_inset: Rgb,
    /// Control background.
    pub bg_panel: Rgb,
    /// Hover and grip background.
    pub bg_panel_2: Rgb,
    /// Structural border.
    pub line: Rgb,
    /// Subtle divider.
    pub line_soft: Rgb,
    /// Inactive label text.
    pub muted: Rgb,
    /// Primary text.
    pub text: Rgb,
    /// Secondary text.
    pub text_dim: Rgb,
    /// High-frequency waveform band.
    pub wave_high: Rgb,
    /// Low-frequency waveform band.
    pub wave_low: Rgb,
    /// Mid-frequency waveform band.
    pub wave_mid: Rgb,
}

impl CanvasPalette {
    const BG: Rgb = Rgb(18, 18, 31);
    const BG_DEEP: Rgb = Rgb(11, 11, 22);
    const BG_INSET: Rgb = Rgb(21, 21, 42);
    const BG_PANEL: Rgb = Rgb(32, 32, 58);
    const BG_PANEL_2: Rgb = Rgb(38, 38, 74);
    const LINE: Rgb = Rgb(59, 59, 103);
    const LINE_SOFT: Rgb = Rgb(42, 42, 76);
    const MUTED: Rgb = Rgb(111, 113, 137);
    const TEXT: Rgb = Rgb(230, 230, 230);
    const TEXT_DIM: Rgb = Rgb(167, 170, 194);
    const WAVE_HIGH: Rgb = Rgb(46, 199, 235);
    const WAVE_LOW: Rgb = Rgb(235, 41, 140);
    const WAVE_MID: Rgb = Rgb(242, 209, 41);

    /// Default palette for the modular canvas.
    #[must_use]
    pub const fn kithara() -> Self {
        Self {
            bg: Self::BG,
            bg_deep: Self::BG_DEEP,
            bg_inset: Self::BG_INSET,
            bg_panel: Self::BG_PANEL,
            bg_panel_2: Self::BG_PANEL_2,
            line: Self::LINE,
            line_soft: Self::LINE_SOFT,
            muted: Self::MUTED,
            text: Self::TEXT,
            text_dim: Self::TEXT_DIM,
            wave_high: Self::WAVE_HIGH,
            wave_low: Self::WAVE_LOW,
            wave_mid: Self::WAVE_MID,
        }
    }
}

impl Default for CanvasPalette {
    fn default() -> Self {
        Self::kithara()
    }
}

impl Palette {
    const ACCENT_B: u8 = 66;
    const ACCENT_G: u8 = 148;
    const ACCENT_R: u8 = 187;

    /// Studio surface and accent stops layered on the base theme.
    const ACCENT_STRONG: Rgb = Rgb(214, 173, 89);
    const BG_B: u8 = 46;
    const BG_DEEP: Rgb = Rgb(14, 14, 29);

    const BG_G: u8 = 26;
    const BG_INSET: Rgb = Rgb(20, 20, 41);

    const BG_PANEL_2: Rgb = Rgb(42, 42, 84);
    const BG_PANEL_B: u8 = 68;
    const BG_PANEL_G: u8 = 34;

    const BG_PANEL_R: u8 = 34;
    const BG_R: u8 = 26;
    const DANGER_B: u8 = 77;

    const DANGER_G: u8 = 77;
    const DANGER_R: u8 = 230;
    const LINE: Rgb = Rgb(59, 59, 103);

    const LINE_SOFT: Rgb = Rgb(44, 44, 82);
    const MUTED_B: u8 = 136;
    const MUTED_G: u8 = 136;

    const MUTED_R: u8 = 136;
    const SUCCESS_B: u8 = 102;
    const SUCCESS_G: u8 = 204;

    const SUCCESS_R: u8 = 102;
    const TEXT_B: u8 = 230;
    const TEXT_DIM: Rgb = Rgb(176, 179, 200);
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
            canvas: CanvasPalette::kithara(),
        }
    }
}

impl Default for Palette {
    fn default() -> Self {
        Self::kithara()
    }
}

#[cfg(feature = "gui")]
impl From<Palette> for kithara_ui::render::RenderPalette {
    fn from(p: Palette) -> Self {
        let canvas = p.canvas;
        Self::builder()
            .bg(to_iced(canvas.bg))
            .bg_deep(to_iced(canvas.bg_deep))
            .bg_inset(to_iced(canvas.bg_inset))
            .bg_panel(to_iced(canvas.bg_panel))
            .bg_panel_2(to_iced(canvas.bg_panel_2))
            .bg_select(iced::Color::from_rgb8(0x26, 0x26, 0x4a))
            .line(to_iced(canvas.line))
            .line_soft(to_iced(canvas.line_soft))
            .text(to_iced(canvas.text))
            .text_dim(to_iced(canvas.text_dim))
            .muted(to_iced(canvas.muted))
            .accent(to_iced(p.accent))
            .accent_strong(to_iced(p.accent_strong))
            .accent_soft(iced::Color::from_rgba8(
                p.accent.0, p.accent.1, p.accent.2, 0.18,
            ))
            .danger(to_iced(p.danger))
            .success(to_iced(p.success))
            .warning(to_iced(p.warning))
            .wave_low(to_iced(canvas.wave_low))
            .wave_mid(to_iced(canvas.wave_mid))
            .wave_high(to_iced(canvas.wave_high))
            .build()
    }
}

#[cfg(feature = "gui")]
fn to_iced(rgb: Rgb) -> iced::Color {
    iced::Color::from_rgb8(rgb.0, rgb.1, rgb.2)
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
