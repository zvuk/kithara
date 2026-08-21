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
    pub border: ColorRole,
    pub font: FontSkin,
    pub border_width: f32,
    pub height: f32,
    pub idle_border_width: f32,
    pub padding_x: f32,
    pub padding_y: f32,
    pub radius: f32,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct KnobSkin {
    pub body_border: ColorRole,
    pub body_fill: ColorRole,
    pub indicator_color: ColorRole,
    pub track_color: ColorRole,
    pub value_color: ColorRole,
    pub size: SizeSpec,
    pub label_text: TextRoleSkin,
    pub body_border_width: f32,
    pub body_ratio: f32,
    pub drag_range: f32,
    pub indicator_width: f32,
    pub label_gap: f32,
    pub label_height: f32,
    pub neutral_angle: f32,
    pub outer_inset: f32,
    pub start_angle: f32,
    pub sweep_angle: f32,
    pub track_alpha: f32,
    pub track_width: f32,
    pub wheel_step: f32,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct CrossfaderSkin {
    pub arrow_color: ColorRole,
    pub label_color: ColorRole,
    pub letter_color: ColorRole,
    pub rail_background: ColorRole,
    pub thumb_color: ColorRole,
    pub thumb_notch_color: ColorRole,
    pub label_text: FontSkin,
    pub letter_text: FontSkin,
    pub rail_frame: FrameSkin,
    pub size: SizeSpec,
    pub ticks: TickSkin,
    pub arrow_gap: f32,
    pub arrow_size: f32,
    pub label_gap: f32,
    pub padding_bottom: f32,
    pub padding_top: f32,
    pub padding_x: f32,
    pub rail_height: f32,
    pub thumb_height: f32,
    pub thumb_notch_height: f32,
    pub thumb_notch_width: f32,
    pub thumb_width: f32,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct VuStereoSkin {
    pub size: SizeSpec,
    pub carriage_width: f32,
    pub channel_l_y: f32,
    pub channel_r_y: f32,
    pub danger_threshold: f32,
    pub segment_gap: f32,
    pub segment_height: f32,
    pub segment_width: f32,
    pub warning_threshold: f32,
    pub segment_count: usize,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct VuVerticalSkin {
    pub thumb_color: ColorRole,
    pub thumb_notch_color: ColorRole,
    pub size: SizeSpec,
    pub ticks: TickSkin,
    pub danger_threshold: f32,
    pub fader_width: f32,
    pub segment_gap: f32,
    pub segment_height: f32,
    pub segment_inset_x: f32,
    pub thumb_height: f32,
    pub thumb_notch_height: f32,
    pub thumb_notch_offset: f32,
    pub warning_threshold: f32,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct VisSkin {
    pub icon_color: ColorRole,
    pub nav_background: ColorRole,
    pub nav_text_color: ColorRole,
    pub nav_text: FontSkin,
    pub nav_frame: FrameSkin,
    pub size: SizeSpec,
    pub meta: TextRoleSkin,
    pub title: TextRoleSkin,
    pub footer_height: f32,
    pub footer_padding_x: f32,
    pub header_height: f32,
    pub icon_size: f32,
    pub index_padding_x: f32,
    pub name_padding_x: f32,
    pub nav_cell_size: f32,
    pub nav_padding_x: f32,
    pub nav_padding_y: f32,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct PortalMapSkin {
    pub size: SizeSpec,
    pub axis_inset_x: f32,
    pub axis_offset_bottom: f32,
    pub arc_height_scale: f32,
    pub arc_top_inset: f32,
    pub line_width: f32,
    pub selected_line_width: f32,
    pub marker_size: f32,
    pub tick_height: f32,
    pub tick_step: f32,
    pub label_offset_x: f32,
    pub label_offset_y: f32,
    pub label: FontSkin,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct RangeSkin {
    pub rail_background: ColorRole,
    pub selection_color: ColorRole,
    pub size: SizeSpec,
    pub thumb_color: ColorRole,
    pub rail_height: f32,
    pub thumb_height: f32,
    pub thumb_width: f32,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct ToggleSkin {
    pub active_frame: FrameSkin,
    pub inactive_frame: FrameSkin,
    pub size: SizeSpec,
    pub thumb_inset: f32,
    pub thumb_radius: f32,
    pub thumb_size: f32,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct CheckboxSkin {
    pub active_frame: FrameSkin,
    pub inactive_frame: FrameSkin,
    pub size: SizeSpec,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct ReadoutSkin {
    pub label: FontSkin,
    pub value: FontSkin,
    pub frame: FrameSkin,
    pub size: SizeSpec,
    pub padding_x: f32,
    pub padding_y: f32,
    pub spacing: f32,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct ChipSkin {
    pub deck_text: FontSkin,
    pub routing_text: FontSkin,
    pub pivot_family_text: TextRoleSkin,
    pub pivot_multiplier_text: TextRoleSkin,
    pub active_frame: FrameSkin,
    pub inactive_frame: FrameSkin,
    pub pivot_frame: FrameSkin,
    pub size: SizeSpec,
    pub padding_x: f32,
    pub padding_y: f32,
    pub pivot_family_padding_x: f32,
    pub pivot_family_padding_y: f32,
    pub pivot_multiplier_padding_x: f32,
    pub pivot_multiplier_padding_y: f32,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct ButtonSkin {
    pub primary_text: FontSkin,
    pub text: FontSkin,
    /// A transport cell draws no border of its own; these sides say where the
    /// seam between neighbouring cells goes.
    pub transport_sides: FrameSides,
    pub frame: FrameSkin,
    pub primary_frame: FrameSkin,
    pub size: SizeSpec,
    pub icon_gap: f32,
    pub icon_size: f32,
    pub micro_icon_size: f32,
    pub micro_size: f32,
    pub padding_x: f32,
    pub padding_y: f32,
    pub transport_icon_size: f32,
    pub primary_fill: u16,
    pub transport_fill: u16,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct NavSkin {
    pub header_height: f32,
    pub header_icon_size: f32,
    pub header_text_size: f32,
    pub icon_gap: f32,
    pub icon_size: f32,
    pub item_height: f32,
    pub marker_width: f32,
    pub pad_y: f32,
    pub text_pad_x: f32,
    pub text_size: f32,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct TabLargeSkin {
    pub height: f32,
    pub pad_x: f32,
    pub pad_y: f32,
    pub text_size: f32,
    pub underline_width: f32,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct TextSkin {
    pub deck_letter_active: ColorRole,
    pub size: SizeSpec,
    pub body: TextRoleSkin,
    pub brand: TextRoleSkin,
    pub brand_small: TextRoleSkin,
    pub caption: TextRoleSkin,
    pub deck_letter: TextRoleSkin,
    pub micro_label: TextRoleSkin,
    pub mono: TextRoleSkin,
    pub pivot_arrow: TextRoleSkin,
    pub pivot_duration: TextRoleSkin,
    pub pivot_footer: TextRoleSkin,
    pub pivot_label: TextRoleSkin,
    pub pivot_ratio: TextRoleSkin,
    pub pivot_small: TextRoleSkin,
    pub pivot_track_artist: TextRoleSkin,
    pub pivot_track_title: TextRoleSkin,
    pub pivot_title: TextRoleSkin,
    pub pivot_value: TextRoleSkin,
    pub section: TextRoleSkin,
    pub telemetry: TextRoleSkin,
    pub track_title: TextRoleSkin,
}

/// Menu icon sizes. Row geometry lives in the markup and menu typography in [`TextSkin`].
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct MenuSkin {
    pub burger_icon_size: f32,
    pub cell_icon_size: f32,
    pub icon_size: f32,
    pub small_icon_size: f32,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct SegmentedSkin {
    pub active_background: ColorRole,
    pub active_text: ColorRole,
    pub background: ColorRole,
    pub inactive_text: ColorRole,
    pub text: FontSkin,
    pub frame: FrameSkin,
    pub size: SizeSpec,
    pub padding_x: f32,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct SelectSkin {
    pub background: ColorRole,
    pub chevron_color: ColorRole,
    pub text_color: ColorRole,
    pub text: FontSkin,
    pub frame: FrameSkin,
    pub size: SizeSpec,
    pub chevron_size: f32,
    pub padding_x: f32,
    pub padding_y: f32,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct StatusDotSkin {
    pub text_color: ColorRole,
    pub text: FontSkin,
    pub size: SizeSpec,
    pub dot_size: f32,
    pub gap: f32,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct SwatchSkin {
    pub frame: FrameSkin,
    pub size: SizeSpec,
    pub hex: TextRoleSkin,
    pub label: TextRoleSkin,
    pub box_height: f32,
    pub box_label_gap: f32,
    pub label_hex_gap: f32,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct CellSkin {
    pub background: ColorRole,
    pub frame: FrameSkin,
    pub highlighted_frame: FrameSkin,
    pub size: SizeSpec,
    pub label_gap: f32,
    pub label_height: f32,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct FaderSkin {
    pub handle_color: ColorRole,
    pub label: FontSkin,
    pub handle_frame: FrameSkin,
    pub rail_frame: FrameSkin,
    pub strip_frame: FrameSkin,
    pub size: SizeSpec,
    pub content_gap: f32,
    pub control_height: f32,
    pub control_padding_x: f32,
    pub control_padding_y: f32,
    pub icon_size: f32,
    pub icon_width: f32,
    pub label_width: f32,
    pub rail_width: f32,
    pub segment_gap: f32,
    pub segment_height: f32,
    pub slider_height: f32,
    pub strip_height: f32,
    pub strip_padding: f32,
    pub tick_height: f32,
    pub tick_step: f32,
    pub tick_width: f32,
    pub ticks_height: f32,
    pub step: f64,
    pub handle_width: u16,
    pub segment_count: usize,
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
            size,
            spacing,
            color,
            font: FontFamily::Mono,
            weight: FontWeight::Normal,
        }
    }

    #[kithara::test]
    fn menu_holds_exactly_the_declared_glyph_sizes() {
        assert_eq!(
            builtin::skin_doc().menu,
            MenuSkin {
                icon_size: 11.0,
                burger_icon_size: 14.0,
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
