use serde::{Deserialize, Serialize};

use super::ron_io;
use crate::{
    envelope::{self, DocKind},
    error::UiDocError,
    ids::{DocId, SourceUri},
    size::SizeSpec,
};

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct SkinDoc {
    pub schema: String,
    pub version: u32,
    pub id: DocId,
    pub palette: PaletteDoc,
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
    pub fader: FaderSkin,
    pub wave: WaveSkin,
    pub deck: DeckSkin,
    pub global_bar: GlobalBarSkin,
    pub telemetry: TelemetrySkin,
    pub track_list: TrackListSkin,
    pub layout_preview: LayoutPreviewSkin,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct PaletteDoc {
    pub bg: String,
    pub bg_deep: String,
    pub bg_inset: String,
    pub bg_panel: String,
    pub bg_panel_2: String,
    pub bg_select: String,
    pub line: String,
    pub line_soft: String,
    pub text: String,
    pub text_dim: String,
    pub muted: String,
    pub accent: String,
    pub accent_strong: String,
    pub accent_soft: String,
    pub danger: String,
    pub success: String,
    pub warning: String,
    pub wave_low: String,
    pub wave_mid: String,
    pub wave_high: String,
}

impl PaletteDoc {
    fn validate(&self, origin: &SourceUri) -> Result<(), UiDocError> {
        for value in [
            &self.bg,
            &self.bg_deep,
            &self.bg_inset,
            &self.bg_panel,
            &self.bg_panel_2,
            &self.bg_select,
            &self.line,
            &self.line_soft,
            &self.text,
            &self.text_dim,
            &self.muted,
            &self.accent,
            &self.accent_strong,
            &self.accent_soft,
            &self.danger,
            &self.success,
            &self.warning,
            &self.wave_low,
            &self.wave_mid,
            &self.wave_high,
        ] {
            parse_color(value, origin)?;
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[non_exhaustive]
pub enum ColorRole {
    Bg,
    BgDeep,
    BgInset,
    BgPanel,
    BgPanel2,
    BgSelect,
    Line,
    LineSoft,
    Text,
    TextDim,
    Muted,
    Accent,
    AccentStrong,
    AccentSoft,
    Danger,
    Success,
    Warning,
    WaveLow,
    WaveMid,
    WaveHigh,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[non_exhaustive]
pub enum FontWeight {
    Normal,
    Medium,
    Semibold,
    Bold,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct FontSkin {
    pub size: f32,
    pub weight: FontWeight,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct FrameSkin {
    pub radius: f32,
    pub border_width: f32,
    pub border: ColorRole,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct LayoutSkin {
    pub grid_gap: f32,
    pub grid_pad: f32,
    pub fill_weight_scale: f32,
    pub fill_weight_min: f32,
    pub size_gap: f32,
    pub size_pad: f32,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct ChromeSkin {
    pub frame: FrameSkin,
    pub secondary_frame: FrameSkin,
    pub corner_size: f32,
    pub corner_width: f32,
    pub corner_offset: f32,
    pub corner_color: ColorRole,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct TextInputSkin {
    pub height: f32,
    pub padding_x: f32,
    pub padding_y: f32,
    pub font: FontSkin,
    pub radius: f32,
    pub border_width: f32,
    pub idle_border_width: f32,
    pub border: ColorRole,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct KnobSkin {
    pub size: SizeSpec,
    pub body_ratio: f32,
    pub body_border: ColorRole,
    pub drag_range: f32,
    pub indicator_width: f32,
    pub outer_inset: f32,
    pub start_angle: f32,
    pub neutral_angle: f32,
    pub sweep_angle: f32,
    pub track_width: f32,
    pub track_alpha: f32,
    pub body_border_width: f32,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct VuStereoSkin {
    pub size: SizeSpec,
    pub carriage_width: f32,
    pub channel_l_y: f32,
    pub channel_r_y: f32,
    pub segment_count: usize,
    pub segment_gap: f32,
    pub segment_height: f32,
    pub segment_width: f32,
    pub warning_threshold: f32,
    pub danger_threshold: f32,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct VuVerticalSkin {
    pub size: SizeSpec,
    pub segment_gap: f32,
    pub segment_height: f32,
    pub segment_inset_x: f32,
    pub thumb_height: f32,
    pub thumb_line_height: f32,
    pub warning_threshold: f32,
    pub danger_threshold: f32,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct ToggleSkin {
    pub size: SizeSpec,
    pub active_frame: FrameSkin,
    pub inactive_frame: FrameSkin,
    pub thumb_size: f32,
    pub thumb_inset: f32,
    pub thumb_radius: f32,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct CheckboxSkin {
    pub size: SizeSpec,
    pub active_frame: FrameSkin,
    pub inactive_frame: FrameSkin,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct ReadoutSkin {
    pub size: SizeSpec,
    pub frame: FrameSkin,
    pub padding_x: f32,
    pub padding_y: f32,
    pub spacing: f32,
    pub label: FontSkin,
    pub value: FontSkin,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct ChipSkin {
    pub size: SizeSpec,
    pub active_frame: FrameSkin,
    pub inactive_frame: FrameSkin,
    pub padding_x: f32,
    pub padding_y: f32,
    pub text: FontSkin,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct ButtonSkin {
    pub size: SizeSpec,
    pub frame: FrameSkin,
    pub primary_frame: FrameSkin,
    pub height: f32,
    pub padding_x: f32,
    pub padding_y: f32,
    pub text: FontSkin,
    pub primary_text: FontSkin,
    pub icon_size: f32,
    pub icon_gap: f32,
    pub micro_size: f32,
    pub micro_icon_size: f32,
    pub transport_fill: u16,
    pub primary_fill: u16,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct NavSkin {
    pub item_height: f32,
    pub marker_width: f32,
    pub icon_size: f32,
    pub text_size: f32,
    pub text_pad_x: f32,
    pub icon_gap: f32,
    pub header_height: f32,
    pub header_text_size: f32,
    pub header_icon_size: f32,
    pub pad_y: f32,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct TabLargeSkin {
    pub height: f32,
    pub pad_x: f32,
    pub pad_y: f32,
    pub underline_width: f32,
    pub text_size: f32,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct TextSkin {
    pub size: SizeSpec,
    pub track_title: FontSkin,
    pub section: FontSkin,
    pub body: FontSkin,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct FaderSkin {
    pub size: SizeSpec,
    pub content_gap: f32,
    pub control_height: f32,
    pub control_padding_x: f32,
    pub control_padding_y: f32,
    pub rail_frame: FrameSkin,
    pub handle_frame: FrameSkin,
    pub strip_frame: FrameSkin,
    pub handle_width: u16,
    pub icon_size: f32,
    pub label: FontSkin,
    pub label_width: f32,
    pub rail_width: f32,
    pub segment_count: usize,
    pub segment_gap: f32,
    pub segment_height: f32,
    pub slider_height: f32,
    pub step: f64,
    pub strip_height: f32,
    pub strip_padding: f32,
    pub ticks_height: f32,
    pub tick_height: f32,
    pub tick_step: f32,
    pub tick_width: f32,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct WaveSkin {
    pub size: SizeSpec,
    pub frame: FrameSkin,
    pub bar_gap: f32,
    pub content_inset: f32,
    pub downbeat_alpha: f32,
    pub grid_alpha: f32,
    pub grid_width: f32,
    pub high_bar_width: f32,
    pub low_bar_width: f32,
    pub mid_bar_width: f32,
    pub playhead_width: f32,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct DeckSkin {
    pub header_size: SizeSpec,
    pub summary_size: SizeSpec,
    pub bpm_size: SizeSpec,
    pub time_size: SizeSpec,
    pub art_frame: FrameSkin,
    pub readout_frame: FrameSkin,
    pub badge_frame: FrameSkin,
    pub art_size: f32,
    pub art_label: FontSkin,
    pub artist: FontSkin,
    pub badge_size: f32,
    pub badge_text: FontSkin,
    pub bpm_cell_width: f32,
    pub bpm_text: FontSkin,
    pub header_gap: f32,
    pub header_height: f32,
    pub header_padding_x: f32,
    pub header_padding_y: f32,
    pub micro_source: FontSkin,
    pub micro_summary_gap: f32,
    pub micro_title: FontSkin,
    pub readout_gap: f32,
    pub readout_height: f32,
    pub readout_label: FontSkin,
    pub readout_value: FontSkin,
    pub remain_cell_width: f32,
    pub summary_fill: u16,
    pub summary_height: f32,
    pub summary_padding_x: f32,
    pub summary_padding_y: f32,
    pub telemetry_padding_x: f32,
    pub telemetry_padding_y: f32,
    pub time_padding_x: f32,
    pub time_padding_y: f32,
    pub time_text: FontSkin,
    pub title: FontSkin,
    pub transport_height: f32,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct GlobalBarSkin {
    pub brand_size: SizeSpec,
    pub spacer_size: SizeSpec,
    pub preset_size: SizeSpec,
    pub settings_size: SizeSpec,
    pub selector_frame: FrameSkin,
    pub chip_frame: FrameSkin,
    pub settings_frame: FrameSkin,
    pub brand_gap: f32,
    pub brand_padding_x: f32,
    pub brand_padding_y: f32,
    pub brand_text: FontSkin,
    pub chip_gap: f32,
    pub chip_padding_x: f32,
    pub chip_padding_y: f32,
    pub chip_text: FontSkin,
    pub gear_size: f32,
    pub height: f32,
    pub settings_padding: f32,
    pub selector_padding_x: f32,
    pub selector_padding_y: f32,
    pub brand_width: f32,
    pub selector_width: f32,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct TelemetrySkin {
    pub size: SizeSpec,
    pub frame: FrameSkin,
    pub padding_x: f32,
    pub padding_y: f32,
    pub percent_precision: usize,
    pub percent_scale: f64,
    pub percent_width: usize,
    pub scalar_precision: usize,
    pub text: FontSkin,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct TrackListSkin {
    pub size: SizeSpec,
    pub row_frame: FrameSkin,
    pub chip_frame: FrameSkin,
    pub artist_width: f32,
    pub chip_size: f32,
    pub chip_text: FontSkin,
    pub count_padding_x: f32,
    pub count_padding_y: f32,
    pub count_text: FontSkin,
    pub grid_gap: f32,
    pub header_height: f32,
    pub header_text: FontSkin,
    pub number_text: FontSkin,
    pub number_width: f32,
    pub row_height: f32,
    pub row_text: FontSkin,
    pub time_padding_x: f32,
    pub time_padding_y: f32,
    pub time_text: FontSkin,
    pub time_width: f32,
    pub title_gap: f32,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct LayoutPreviewSkin {
    pub height: f32,
    pub line_width: f32,
    pub module_inset: f32,
}

/// Parses and validates a complete skin document.
///
/// # Errors
/// Returns [`UiDocError`] when the envelope, body, or palette is invalid.
pub fn parse_skin(text: &str, origin: &SourceUri) -> Result<SkinDoc, UiDocError> {
    let envelope = envelope::probe(text, origin)?;
    if envelope.kind != DocKind::Skin {
        return Err(UiDocError::WrongDocKind {
            origin: origin.clone(),
            expected: DocKind::Skin.name(),
            found: envelope.kind.name(),
        });
    }
    let document: SkinDoc =
        ron_io::options()
            .from_str(text)
            .map_err(|source| UiDocError::Syntax {
                origin: origin.clone(),
                source: Box::new(source),
            })?;
    document.palette.validate(origin)?;
    Ok(document)
}

pub(crate) fn parse_color(value: &str, origin: &SourceUri) -> Result<[u8; 4], UiDocError> {
    let digits = value
        .strip_prefix('#')
        .ok_or_else(|| bad_color(origin, value))?;
    if digits.len() != 6 && digits.len() != 8 {
        return Err(bad_color(origin, value));
    }
    let component = |start| {
        let pair = digits
            .get(start..start + 2)
            .ok_or_else(|| bad_color(origin, value))?;
        u8::from_str_radix(pair, 16).map_err(|_| bad_color(origin, value))
    };
    Ok([
        component(0)?,
        component(2)?,
        component(4)?,
        if digits.len() == 8 {
            component(6)?
        } else {
            255
        },
    ])
}

fn bad_color(origin: &SourceUri, value: &str) -> UiDocError {
    UiDocError::BadColor {
        origin: origin.clone(),
        value: value.to_owned(),
    }
}
