use serde::{Deserialize, Serialize};

use super::{
    document::ColorRole,
    primitives::{FontSkin, FrameSkin, ShadowSkin, TextRoleSkin},
};
use crate::size::SizeSpec;

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct WaveSkin {
    pub size: SizeSpec,
    pub frame: FrameSkin,
    pub overlay: WaveOverlaySkin,
    pub background: ColorRole,
    pub bar_gap: f32,
    /// Width of every band bar; bands nest by level, not by width.
    pub bar_width: f32,
    pub content_inset: f32,
    pub cue_badge_background: ColorRole,
    pub cue_badge_size: f32,
    pub cue_badge_text: FontSkin,
    pub cue_badge_text_color: ColorRole,
    pub cue_line_width: f32,
    pub downbeat_alpha: f32,
    pub grid_alpha: f32,
    pub grid_width: f32,
    pub loop_bound_width: f32,
    pub loop_fill_alpha: f32,
    pub played_alpha: f32,
    /// Overview strips dim harder than the hero wave.
    pub overview_played_alpha: f32,
    pub playhead_marker_height: f32,
    pub playhead_marker_width: f32,
    pub playhead_width: f32,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct WaveOverlaySkin {
    pub background: ColorRole,
    pub background_alpha: f32,
    pub height: f32,
    pub padding_x: f32,
    pub padding_y: f32,
    pub gap: f32,
    pub art_frame: FrameSkin,
    pub art_background: ColorRole,
    pub art_size: f32,
    pub art_label: FontSkin,
    pub art_label_color: ColorRole,
    pub title: FontSkin,
    pub title_color: ColorRole,
    pub artist: FontSkin,
    pub artist_color: ColorRole,
    pub summary_gap: f32,
    pub readout_frame: FrameSkin,
    pub readout_background: ColorRole,
    pub bpm_width: f32,
    pub key_width: f32,
    pub readout_height: f32,
    pub readout_padding_x: f32,
    pub readout_padding_y: f32,
    pub readout_gap: f32,
    pub readout_label: FontSkin,
    pub readout_label_color: ColorRole,
    pub readout_value: FontSkin,
    pub bpm_color: ColorRole,
    pub key_color: ColorRole,
    pub remain_width: f32,
    pub remain_color: ColorRole,
    pub badge_frame: FrameSkin,
    pub badge_background: ColorRole,
    pub badge_size: f32,
    pub badge_text: FontSkin,
    pub badge_text_color: ColorRole,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct DeckSkin {
    pub summary_size: SizeSpec,
    pub bpm_size: SizeSpec,
    pub time_size: SizeSpec,
    pub artist: FontSkin,
    pub bpm_text: FontSkin,
    pub micro_source: FontSkin,
    pub micro_summary_gap: f32,
    pub micro_title: FontSkin,
    pub readout_gap: f32,
    pub readout_label: FontSkin,
    pub summary_fill: u16,
    pub summary_height: f32,
    pub summary_padding_x: f32,
    pub summary_padding_y: f32,
    pub time_padding_x: f32,
    pub time_padding_y: f32,
    pub time_text: FontSkin,
    pub title: FontSkin,
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

/// Horizontal fill bar reporting one scalar, as the design's CPU cell draws it:
/// an inset track with a hairline frame, filled from the left.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct MeterSkin {
    pub size: SizeSpec,
    pub frame: FrameSkin,
    pub background: ColorRole,
    pub fill: ColorRole,
}

/// Hairline between adjacent cells or control sections.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct DividerSkin {
    pub width: f32,
    pub color: ColorRole,
}

/// The label the pointer carries while it drags an item.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct DragSkin {
    pub frame: FrameSkin,
    pub background: ColorRole,
    pub text: TextRoleSkin,
    pub width: f32,
    pub height: f32,
    pub pad_x: f32,
}

/// Pop-over chrome; the frame and the cap draw outward of the content column.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct PopSkin {
    pub background: ColorRole,
    pub frame: FrameSkin,
    pub cap_height: f32,
    pub cap_color: ColorRole,
    pub shadow: ShadowSkin,
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

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct TreeSkin {
    pub size: SizeSpec,
    pub search_placeholder: String,
    pub search_height: f32,
    pub search_icon_width: f32,
    pub search_icon_size: f32,
    pub search_padding_x: f32,
    pub search_text: FontSkin,
    pub search_background: ColorRole,
    pub search_divider: ColorRole,
    pub panel_background: ColorRole,
    pub panel_padding_top: f32,
    pub panel_padding_bottom: f32,
    pub scrollbar_width: f32,
    pub scrollbar_margin: f32,
    pub scroller_color: ColorRole,
    pub scrollbar_background: ColorRole,
    pub row_height: f32,
    pub row_padding_right: f32,
    pub indent_base: f32,
    pub indent_step: f32,
    pub marker_width: f32,
    pub chevron_width: f32,
    pub chevron_size: f32,
    pub icon_size: f32,
    pub content_gap: f32,
    pub label_text: FontSkin,
    pub count_text: FontSkin,
    pub context_height: f32,
    pub context_padding_x: f32,
    pub context_icon_size: f32,
    pub context_gap: f32,
    pub context_text: FontSkin,
    pub context_background: ColorRole,
    pub context_divider: ColorRole,
    pub context_divider_width: f32,
    pub scope_text: FontSkin,
    pub scope_gap: f32,
    pub scope_padding_x: f32,
    pub scope_frame: FrameSkin,
    pub scope_background: ColorRole,
    pub scope_text_color: ColorRole,
    pub scope_chevron_size: f32,
    pub scope_chevron_color: ColorRole,
    pub scope_menu_background: ColorRole,
    pub scope_menu_frame: FrameSkin,
    pub scope_item_height: f32,
    pub scope_menu_text: ColorRole,
    pub scope_selected_background: ColorRole,
    pub scope_selected_text: ColorRole,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct TrackListSkin {
    pub size: SizeSpec,
    pub labels: TrackListLabels,
    pub row_frame: FrameSkin,
    pub deck_chip_frame: FrameSkin,
    pub bpm_badge_frame: FrameSkin,
    pub bpm_badge_background: ColorRole,
    pub divider_color: ColorRole,
    pub divider_hit_width: f32,
    pub divider_width: f32,
    pub energy_bar_background: ColorRole,
    pub cell_padding_x: f32,
    pub footer_padding_x: f32,
    pub grid_gap: f32,
    pub header_height: f32,
    pub row_height: f32,
    pub footer_height: f32,
    pub min_column_width: f32,
    pub index_width: f32,
    pub deck_width: f32,
    pub artist_width: f32,
    pub bpm_width: f32,
    pub key_width: f32,
    pub time_width: f32,
    pub energy_width: f32,
    pub transition_width: f32,
    pub title_min_width: f32,
    pub scrollbar_background: ColorRole,
    pub scrollbar_margin: f32,
    pub scrollbar_width: f32,
    pub scroller_color: ColorRole,
    pub deck_chip_width: f32,
    pub deck_chip_height: f32,
    pub bpm_badge_height: f32,
    pub bpm_badge_padding_x: f32,
    pub energy_bar_width: f32,
    pub energy_bar_height: f32,
    pub energy_bar_gap: f32,
    pub header_text: FontSkin,
    pub index_text: FontSkin,
    pub deck_text: FontSkin,
    pub title_text: FontSkin,
    pub artist_text: FontSkin,
    pub bpm_text: FontSkin,
    pub key_text: FontSkin,
    pub time_text: FontSkin,
    pub energy_text: FontSkin,
    pub transition_text: FontSkin,
    pub footer_text: FontSkin,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct TrackListLabels {
    pub index: String,
    pub deck: String,
    pub title: String,
    pub artist: String,
    pub bpm: String,
    pub key: String,
    pub time: String,
    pub energy: String,
    pub transition: String,
    pub footer_tracks: String,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct LayoutPreviewSkin {
    pub height: f32,
    pub line_width: f32,
    pub module_inset: f32,
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::*;
    use crate::builtin;

    #[kithara::test]
    fn pop_holds_exactly_the_declared_chrome() {
        assert_eq!(
            builtin::skin_doc().pop,
            PopSkin {
                background: ColorRole::BgFooter,
                frame: FrameSkin {
                    radius: 0.0,
                    border_width: 1.0,
                    border: ColorRole::LineHi,
                },
                cap_height: 2.0,
                cap_color: ColorRole::Accent,
                shadow: ShadowSkin {
                    color: ColorRole::Shadow,
                    alpha: 0.6,
                    offset_x: 0.0,
                    offset_y: 16.0,
                    blur: 40.0,
                },
            }
        );
    }
}
