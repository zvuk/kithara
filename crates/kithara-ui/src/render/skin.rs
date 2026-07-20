use iced::{Border, Color};

use super::theme::RenderPalette;
use crate::{
    error::UiDocError,
    ids::SourceUri,
    skin::{
        ButtonSkin, CellSkin, CheckboxSkin, ChipSkin, ChromeSkin, ColorRole, DeckSkin, FaderSkin,
        FrameSkin, GlobalBarSkin, KnobSkin, LayoutPreviewSkin, LayoutSkin, NavSkin, ReadoutSkin,
        SegmentedSkin, SelectSkin, SkinDoc, StatusDotSkin, TabLargeSkin, TelemetrySkin,
        TextInputSkin, TextSkin, ToggleSkin, TrackListSkin, VuStereoSkin, VuVerticalSkin, WaveSkin,
        parse_color,
    },
};

/// Resolved skin consumed by iced renderers.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub struct Skin {
    document: SkinDoc,
    pub palette: RenderPalette,
    pub layout: LayoutSkin,
    pub chrome: ChromeSkin,
    pub text_input: TextInputSkin,
    pub knob: KnobSkin,
    pub vu_stereo: VuStereoSkin,
    pub vu_vertical: VuVerticalSkin,
    pub toggle: ToggleSkin,
    pub checkbox: CheckboxSkin,
    pub readout: ReadoutSkin,
    pub chip: ChipSkin,
    pub button: ButtonSkin,
    pub nav: NavSkin,
    pub tab_large: TabLargeSkin,
    pub text: TextSkin,
    pub segmented: SegmentedSkin,
    pub select: SelectSkin,
    pub status_dot: StatusDotSkin,
    pub cell: CellSkin,
    pub fader: FaderSkin,
    pub wave: WaveSkin,
    pub deck: DeckSkin,
    pub global_bar: GlobalBarSkin,
    pub telemetry: TelemetrySkin,
    pub track_list: TrackListSkin,
    pub layout_preview: LayoutPreviewSkin,
}

impl Skin {
    /// Resolves a parsed document into iced colors and render metrics.
    ///
    /// # Errors
    /// Returns [`UiDocError::BadColor`] when any palette value is malformed.
    pub fn resolve(document: SkinDoc, origin: &SourceUri) -> Result<Self, UiDocError> {
        Ok(Self {
            palette: RenderPalette {
                bg: color(&document.palette.bg, origin)?,
                bg_deep: color(&document.palette.bg_deep, origin)?,
                bg_inset: color(&document.palette.bg_inset, origin)?,
                bg_panel: color(&document.palette.bg_panel, origin)?,
                bg_footer: color(&document.palette.bg_footer, origin)?,
                bg_panel_2: color(&document.palette.bg_panel_2, origin)?,
                bg_select: color(&document.palette.bg_select, origin)?,
                line: color(&document.palette.line, origin)?,
                line_inner: color(&document.palette.line_inner, origin)?,
                line_soft: color(&document.palette.line_soft, origin)?,
                text: color(&document.palette.text, origin)?,
                text_dim: color(&document.palette.text_dim, origin)?,
                muted: color(&document.palette.muted, origin)?,
                accent: color(&document.palette.accent, origin)?,
                accent_strong: color(&document.palette.accent_strong, origin)?,
                accent_soft: color(&document.palette.accent_soft, origin)?,
                danger: color(&document.palette.danger, origin)?,
                success: color(&document.palette.success, origin)?,
                warning: color(&document.palette.warning, origin)?,
                wave_low: color(&document.palette.wave_low, origin)?,
                wave_mid: color(&document.palette.wave_mid, origin)?,
                wave_high: color(&document.palette.wave_high, origin)?,
            },
            layout: document.layout,
            chrome: document.chrome,
            text_input: document.text_input,
            knob: document.knob,
            vu_stereo: document.vu_stereo,
            vu_vertical: document.vu_vertical,
            toggle: document.toggle,
            checkbox: document.checkbox,
            readout: document.readout,
            chip: document.chip,
            button: document.button,
            nav: document.nav,
            tab_large: document.tab_large,
            text: document.text,
            segmented: document.segmented,
            select: document.select,
            status_dot: document.status_dot,
            cell: document.cell,
            fader: document.fader,
            wave: document.wave,
            deck: document.deck,
            global_bar: document.global_bar,
            telemetry: document.telemetry,
            track_list: document.track_list,
            layout_preview: document.layout_preview,
            document,
        })
    }

    pub(crate) fn document(&self) -> &SkinDoc {
        &self.document
    }

    pub(crate) fn color(&self, role: ColorRole) -> Color {
        match role {
            ColorRole::Bg => self.palette.bg,
            ColorRole::BgDeep => self.palette.bg_deep,
            ColorRole::BgInset => self.palette.bg_inset,
            ColorRole::BgPanel => self.palette.bg_panel,
            ColorRole::BgFooter => self.palette.bg_footer,
            ColorRole::BgPanel2 => self.palette.bg_panel_2,
            ColorRole::BgSelect => self.palette.bg_select,
            ColorRole::Line => self.palette.line,
            ColorRole::LineInner => self.palette.line_inner,
            ColorRole::LineSoft => self.palette.line_soft,
            ColorRole::Text => self.palette.text,
            ColorRole::TextDim => self.palette.text_dim,
            ColorRole::Muted => self.palette.muted,
            ColorRole::Accent => self.palette.accent,
            ColorRole::AccentStrong => self.palette.accent_strong,
            ColorRole::AccentSoft => self.palette.accent_soft,
            ColorRole::Danger => self.palette.danger,
            ColorRole::Success => self.palette.success,
            ColorRole::Warning => self.palette.warning,
            ColorRole::WaveLow => self.palette.wave_low,
            ColorRole::WaveMid => self.palette.wave_mid,
            ColorRole::WaveHigh => self.palette.wave_high,
        }
    }

    pub(crate) fn border(&self, frame: FrameSkin) -> Border {
        Border {
            color: self.color(frame.border),
            width: frame.border_width,
            radius: frame.radius.into(),
        }
    }
}

fn color(value: &str, origin: &SourceUri) -> Result<Color, UiDocError> {
    let [red, green, blue, alpha] = parse_color(value, origin)?;
    Ok(Color::from_rgba8(
        red,
        green,
        blue,
        f32::from(alpha) / 255.0,
    ))
}
