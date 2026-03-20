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
    /// Kithara dark + gold theme.
    #[must_use]
    pub const fn kithara() -> Self {
        Self {
            bg: Rgb(26, 26, 46),
            bg_panel: Rgb(34, 34, 68),
            accent: Rgb(187, 148, 66),
            muted: Rgb(136, 136, 136),
            text: Rgb(230, 230, 230),
            success: Rgb(102, 204, 102),
            danger: Rgb(230, 77, 77),
            warning: Rgb(230, 179, 51),
        }
    }
}

impl Default for Palette {
    fn default() -> Self {
        Self::kithara()
    }
}

// ---------------------------------------------------------------------------
// GUI (iced) palette
// ---------------------------------------------------------------------------

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

// ---------------------------------------------------------------------------
// TUI (ratatui) palette
// ---------------------------------------------------------------------------

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
