pub(super) struct StudioSpace;
impl StudioSpace {
    pub(super) const TOPBAR: [f32; 2] = [10.0, 18.0];
    pub(super) const DECK: [f32; 2] = [14.0, 16.0];
    pub(super) const STATUS: [f32; 2] = [6.0, 14.0];
    pub(super) const BUTTON: [f32; 2] = [6.0, 10.0];
    pub(super) const LIBRARY_RAIL: [f32; 2] = [0.0, 12.0];
    pub(super) const LIBRARY_ROW: [f32; 2] = [0.0, 12.0];
}

pub(super) struct StudioSize;
impl StudioSize {
    pub(super) const BRAND_LOGO: f32 = 28.0;
    pub(super) const BRAND_DIVIDER_HEIGHT: f32 = 16.0;
    pub(super) const LIBRARY_WIDTH: f32 = 280.0;
    pub(super) const WAVEFORM_HEIGHT: f32 = 84.0;
    pub(super) const KNOB_SIZE: f32 = 46.0;
    pub(super) const LIB_HEAD_HEIGHT: f32 = 28.0;
    pub(super) const LIB_ROW_HEIGHT: f32 = 30.0;
    pub(super) const STATUS_DOT: f32 = 6.0;
    pub(super) const DIVIDER: f32 = 1.0;
    pub(super) const TRANSPORT_ICON: f32 = 18.0;
}

pub(super) struct StudioRadius;
impl StudioRadius {
    pub(super) const SURFACE: f32 = 10.0;
    pub(super) const BUTTON: f32 = 8.0;
    pub(super) const ROUND: f32 = 999.0;
}

pub(super) struct StudioType;
impl StudioType {
    pub(super) const MONO_XS: f32 = 9.0;
    pub(super) const MONO_SM: f32 = 10.0;
    pub(super) const BODY_SM: f32 = 11.0;
    pub(super) const BODY_MD: f32 = 12.0;
    pub(super) const TRACK: f32 = 15.0;
    pub(super) const BRAND: f32 = 18.0;
}
