use iced::Color;

/// Resolved color palette consumed by renderers.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RenderPalette {
    pub bg: Color,
    pub bg_deep: Color,
    pub bg_inset: Color,
    pub bg_panel: Color,
    pub bg_footer: Color,
    pub bg_panel_2: Color,
    pub bg_select: Color,
    pub line: Color,
    pub line_inner: Color,
    pub line_soft: Color,
    pub text: Color,
    pub text_dim: Color,
    pub muted: Color,
    pub accent: Color,
    pub accent_strong: Color,
    pub accent_soft: Color,
    pub danger: Color,
    pub success: Color,
    pub warning: Color,
    pub wave_low: Color,
    pub wave_mid: Color,
    pub wave_high: Color,
}
