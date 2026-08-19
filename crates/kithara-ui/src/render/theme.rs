use crate::draw::Rgba;

/// Resolved color palette consumed by renderers.
#[derive(Clone, Copy, Debug, PartialEq)]
#[non_exhaustive]
pub struct RenderPalette {
    pub accent: Rgba,
    pub accent_soft: Rgba,
    pub accent_strong: Rgba,
    pub bg: Rgba,
    pub bg_deep: Rgba,
    pub bg_footer: Rgba,
    pub bg_inset: Rgba,
    pub bg_panel: Rgba,
    pub bg_panel_2: Rgba,
    pub bg_select: Rgba,
    pub danger: Rgba,
    pub line: Rgba,
    pub line_dim: Rgba,
    pub line_hi: Rgba,
    pub line_inner: Rgba,
    pub line_pop: Rgba,
    pub line_soft: Rgba,
    pub muted: Rgba,
    pub shadow: Rgba,
    pub success: Rgba,
    pub text: Rgba,
    pub text_dim: Rgba,
    pub warning: Rgba,
    pub wave_high: Rgba,
    pub wave_low: Rgba,
    pub wave_mid: Rgba,
}
