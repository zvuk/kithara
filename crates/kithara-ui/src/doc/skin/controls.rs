use serde::{Deserialize, Serialize};

use super::{
    document::ColorRole,
    primitives::{FontSkin, FrameSkin, TextRoleSkin, TickSkin},
};
use crate::{layout::FrameSides, size::SizeSpec};

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
    pub body_fill: ColorRole,
    pub body_border: ColorRole,
    pub track_color: ColorRole,
    pub value_color: ColorRole,
    pub indicator_color: ColorRole,
    pub drag_range: f32,
    pub wheel_step: f32,
    pub indicator_width: f32,
    pub outer_inset: f32,
    pub start_angle: f32,
    pub neutral_angle: f32,
    pub sweep_angle: f32,
    pub track_width: f32,
    pub track_alpha: f32,
    pub body_border_width: f32,
    pub label_text: TextRoleSkin,
    pub label_gap: f32,
    pub label_height: f32,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct CrossfaderSkin {
    pub size: SizeSpec,
    pub padding_x: f32,
    pub padding_top: f32,
    pub padding_bottom: f32,
    pub rail_height: f32,
    pub rail_frame: FrameSkin,
    pub rail_background: ColorRole,
    pub thumb_width: f32,
    pub thumb_height: f32,
    pub thumb_color: ColorRole,
    pub thumb_notch_width: f32,
    pub thumb_notch_height: f32,
    pub thumb_notch_color: ColorRole,
    pub ticks: TickSkin,
    pub label_text: FontSkin,
    pub label_color: ColorRole,
    pub label_gap: f32,
    pub letter_text: FontSkin,
    pub letter_color: ColorRole,
    pub arrow_size: f32,
    pub arrow_color: ColorRole,
    pub arrow_gap: f32,
    pub left_label: String,
    pub center_label: String,
    pub right_label: String,
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
    pub fader_width: f32,
    pub segment_gap: f32,
    pub segment_height: f32,
    pub segment_inset_x: f32,
    pub ticks: TickSkin,
    pub thumb_height: f32,
    pub thumb_color: ColorRole,
    pub thumb_notch_offset: f32,
    pub thumb_notch_height: f32,
    pub thumb_notch_color: ColorRole,
    pub warning_threshold: f32,
    pub danger_threshold: f32,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct VisSkin {
    pub size: SizeSpec,
    pub header_height: f32,
    pub footer_height: f32,
    pub nav_cell_size: f32,
    pub nav_padding_x: f32,
    pub nav_padding_y: f32,
    pub name_padding_x: f32,
    pub index_padding_x: f32,
    pub footer_padding_x: f32,
    pub title: TextRoleSkin,
    pub meta: TextRoleSkin,
    pub nav_text: FontSkin,
    pub nav_frame: FrameSkin,
    pub nav_background: ColorRole,
    pub nav_text_color: ColorRole,
    pub icon_size: f32,
    pub icon_color: ColorRole,
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
    pub deck_text: FontSkin,
    pub routing_text: FontSkin,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct ButtonSkin {
    pub size: SizeSpec,
    pub frame: FrameSkin,
    pub primary_frame: FrameSkin,
    /// A transport cell draws no border of its own; these sides say where the
    /// seam between neighbouring cells goes.
    pub transport_sides: FrameSides,
    pub padding_x: f32,
    pub padding_y: f32,
    pub text: FontSkin,
    pub primary_text: FontSkin,
    pub icon_size: f32,
    pub transport_icon_size: f32,
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
    pub brand: TextRoleSkin,
    pub brand_small: TextRoleSkin,
    pub deck_letter: TextRoleSkin,
    pub deck_letter_active: ColorRole,
    pub track_title: TextRoleSkin,
    pub body: TextRoleSkin,
    pub telemetry: TextRoleSkin,
    pub mono: TextRoleSkin,
    pub section: TextRoleSkin,
    pub micro_label: TextRoleSkin,
    pub caption: TextRoleSkin,
}

/// Menu icon sizes. Row geometry lives in the markup and menu typography in [`TextSkin`].
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct MenuSkin {
    pub icon_size: f32,
    pub burger_icon_size: f32,
    pub small_icon_size: f32,
    pub cell_icon_size: f32,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct SegmentedSkin {
    pub size: SizeSpec,
    pub frame: FrameSkin,
    pub background: ColorRole,
    pub active_background: ColorRole,
    pub active_text: ColorRole,
    pub inactive_text: ColorRole,
    pub padding_x: f32,
    pub text: FontSkin,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct SelectSkin {
    pub size: SizeSpec,
    pub frame: FrameSkin,
    pub background: ColorRole,
    pub padding_x: f32,
    pub padding_y: f32,
    pub text: FontSkin,
    pub text_color: ColorRole,
    pub chevron_size: f32,
    pub chevron_color: ColorRole,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct StatusDotSkin {
    pub size: SizeSpec,
    pub dot_size: f32,
    pub gap: f32,
    pub text: FontSkin,
    pub text_color: ColorRole,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct SwatchSkin {
    pub size: SizeSpec,
    pub box_height: f32,
    pub frame: FrameSkin,
    pub box_label_gap: f32,
    pub label_hex_gap: f32,
    pub label: TextRoleSkin,
    pub hex: TextRoleSkin,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct CellSkin {
    pub size: SizeSpec,
    pub frame: FrameSkin,
    pub highlighted_frame: FrameSkin,
    pub background: ColorRole,
    pub label_gap: f32,
    pub label_height: f32,
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
    pub handle_color: ColorRole,
    pub icon_size: f32,
    pub icon_width: f32,
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

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::{
        super::document::{FontFamily, FontWeight},
        *,
    };
    use crate::builtin;

    const fn mono(size: f32, spacing: f32, color: ColorRole) -> TextRoleSkin {
        TextRoleSkin {
            font: FontFamily::Mono,
            weight: FontWeight::Normal,
            size,
            spacing,
            color,
        }
    }

    #[kithara::test]
    fn menu_holds_exactly_the_declared_glyph_sizes() {
        assert_eq!(
            builtin::skin_doc().menu,
            MenuSkin {
                icon_size: 11.0,
                burger_icon_size: 13.0,
                small_icon_size: 10.0,
                cell_icon_size: 9.0,
            }
        );
    }

    #[kithara::test]
    fn the_mono_pair_transcribes_the_design_defaults() {
        let text = builtin::skin_doc().text;

        assert_eq!(text.mono, mono(10.0, 0.0, ColorRole::TextDim));
        assert_eq!(text.caption, mono(7.0, 0.08, ColorRole::Muted));
    }
}
