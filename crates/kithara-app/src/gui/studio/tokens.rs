pub(super) mod studio_space {
    /// Global bar: 36px tall in the design (28px logo + 4px), segments 0 10-13px.
    pub(crate) const TOPBAR: [f32; 2] = [4.0, 12.0];
    /// Padded control bay inside a module, `~8px 10px` in the design decks.
    pub(crate) const CLUSTER: [f32; 2] = [8.0, 10.0];
    /// Footer is `0 12px` on a fixed 22px row in the design.
    pub(crate) const STATUS: [f32; 2] = [0.0, 12.0];
    pub(crate) const BUTTON: [f32; 2] = [4.0, 12.0];
    pub(crate) const LIBRARY_RAIL: [f32; 2] = [0.0, 12.0];
    pub(crate) const LIBRARY_ROW: [f32; 2] = [0.0, 10.0];
}

pub(super) mod studio_size {
    pub(crate) const BRAND_LOGO: f32 = 28.0;
    /// Library sits under the decks and spans the full width; only its
    /// height is a layout decision.
    pub(crate) const LIBRARY_HEIGHT: f32 = 220.0;
    pub(crate) const MIXER_WIDTH: f32 = 224.0;
    pub(crate) const WAVEFORM_HEIGHT: f32 = 160.0;
    pub(crate) const FADER_HEIGHT: f32 = 88.0;
    pub(crate) const LIB_HEAD_HEIGHT: f32 = 26.0;
    /// Column-header row is 22px in the design, vs the 26px module header.
    pub(crate) const LIB_COL_HEAD_HEIGHT: f32 = 22.0;
    pub(crate) const LIB_ROW_HEIGHT: f32 = 28.0;
    pub(crate) const STATUS_DOT: f32 = 6.0;
    pub(crate) const DIVIDER: f32 = 1.0;
    pub(crate) const TRANSPORT_ICON: f32 = 18.0;
    pub(crate) const STATUS_HEIGHT: f32 = 22.0;
}

pub(super) mod studio_radius {
    /// Physically round status dots only; sanctioned exception to the flat style.
    pub(crate) const ROUND: f32 = 999.0;
}

pub(super) mod studio_type {
    pub(crate) const MONO_XS: f32 = 9.0;
    pub(crate) const MONO_SM: f32 = 10.0;
    pub(crate) const BODY_SM: f32 = 11.0;
    pub(crate) const BODY_MD: f32 = 12.0;
    pub(crate) const TRACK: f32 = 12.0;
    pub(crate) const BRAND: f32 = 15.0;
}
