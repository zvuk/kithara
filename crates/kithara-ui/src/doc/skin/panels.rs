use serde::{Deserialize, Serialize};

use super::{
    palette::ColorRole,
    primitives::{FrameSkin, ShadowSkin, StateColors, TextRoleSkin},
};
use crate::size::SizeSpec;

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Serialize, kithara_derive::SkinWalk)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct WaveSkin {
    pub background: ColorRole,
    pub band_high_color: ColorRole,
    /// The three levels a band nests by, the ground under them, the grid
    /// over them, and the part already played.
    pub band_low_color: ColorRole,
    pub band_mid_color: ColorRole,
    /// Extent cached ahead of the playhead; the played part takes the accent.
    pub cache_strip_color: ColorRole,
    /// Rails and boundaries of a range the analysis has not covered.
    pub coverage_edge_color: ColorRole,
    /// The covered baseline and the stubs standing in for missing columns.
    pub coverage_mark_color: ColorRole,
    pub cue_badge_background: ColorRole,
    pub grid_color: ColorRole,
    pub label_color: ColorRole,
    pub played_color: ColorRole,
    pub trough_color: ColorRole,
    pub frame: FrameSkin,
    /// `WaveStyle::Default`.
    pub default_size: SizeSpec,
    /// `WaveStyle::Micro`.
    pub micro_size: SizeSpec,
    /// `WaveStyle::Hero`.
    pub size: SizeSpec,
    pub cue_badge_text: TextRoleSkin,
    pub overlay: WaveOverlaySkin,
    pub bar_gap: f32,
    /// Width of every band bar; bands nest by level, not by width.
    pub bar_width: f32,
    pub cache_strip_alpha: f32,
    pub cache_strip_height: f32,
    pub content_inset: f32,
    pub coverage_baseline_alpha: f32,
    /// Height of the covered baseline and width of a region boundary.
    pub coverage_hairline: f32,
    pub coverage_rail_height: f32,
    pub coverage_stub_alpha: f32,
    pub coverage_stub_height: f32,
    pub cue_badge_size: f32,
    pub cue_line_width: f32,
    pub downbeat_alpha: f32,
    pub grid_alpha: f32,
    pub grid_width: f32,
    pub loop_bound_width: f32,
    pub loop_fill_alpha: f32,
    /// Overview strips dim harder than the hero wave.
    pub overview_played_alpha: f32,
    pub played_alpha: f32,
    pub playhead_marker_height: f32,
    pub playhead_marker_width: f32,
    pub playhead_width: f32,
}

/// What a skin may restate of [`WaveSkin`].
#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Serialize)]
#[serde(default, deny_unknown_fields)]
#[non_exhaustive]
pub struct WavePatch {
    pub background: Option<ColorRole>,
    pub band_high_color: Option<ColorRole>,
    pub band_low_color: Option<ColorRole>,
    pub band_mid_color: Option<ColorRole>,
    pub bar_gap: Option<f32>,
    pub bar_width: Option<f32>,
    pub cache_strip_alpha: Option<f32>,
    pub cache_strip_color: Option<ColorRole>,
    pub cache_strip_height: Option<f32>,
    pub content_inset: Option<f32>,
    pub coverage_baseline_alpha: Option<f32>,
    pub coverage_edge_color: Option<ColorRole>,
    pub coverage_hairline: Option<f32>,
    pub coverage_mark_color: Option<ColorRole>,
    pub coverage_rail_height: Option<f32>,
    pub coverage_stub_alpha: Option<f32>,
    pub coverage_stub_height: Option<f32>,
    pub cue_badge_background: Option<ColorRole>,
    pub cue_badge_size: Option<f32>,
    pub cue_badge_text: Option<TextRoleSkin>,
    pub cue_line_width: Option<f32>,
    pub default_size: Option<SizeSpec>,
    pub downbeat_alpha: Option<f32>,
    pub frame: Option<FrameSkin>,
    pub grid_alpha: Option<f32>,
    pub grid_color: Option<ColorRole>,
    pub grid_width: Option<f32>,
    pub label_color: Option<ColorRole>,
    pub loop_bound_width: Option<f32>,
    pub loop_fill_alpha: Option<f32>,
    pub micro_size: Option<SizeSpec>,
    pub overlay: Option<WaveOverlaySkin>,
    pub overview_played_alpha: Option<f32>,
    pub played_alpha: Option<f32>,
    pub played_color: Option<ColorRole>,
    pub playhead_marker_height: Option<f32>,
    pub playhead_marker_width: Option<f32>,
    pub playhead_width: Option<f32>,
    pub size: Option<SizeSpec>,
    pub trough_color: Option<ColorRole>,
}

impl WaveSkin {
    /// Takes every field the patch restates, keeping the rest.
    pub(crate) fn patch(&mut self, patch: WavePatch) {
        super::patch::patch_field(&mut self.background, patch.background);
        super::patch::patch_field(&mut self.band_low_color, patch.band_low_color);
        super::patch::patch_field(&mut self.band_mid_color, patch.band_mid_color);
        super::patch::patch_field(&mut self.band_high_color, patch.band_high_color);
        super::patch::patch_field(&mut self.trough_color, patch.trough_color);
        super::patch::patch_field(&mut self.grid_color, patch.grid_color);
        super::patch::patch_field(&mut self.label_color, patch.label_color);
        super::patch::patch_field(&mut self.played_color, patch.played_color);
        super::patch::patch_field(&mut self.cache_strip_color, patch.cache_strip_color);
        super::patch::patch_field(&mut self.coverage_edge_color, patch.coverage_edge_color);
        super::patch::patch_field(&mut self.coverage_mark_color, patch.coverage_mark_color);
        super::patch::patch_field(&mut self.cue_badge_background, patch.cue_badge_background);
        super::patch::patch_field(&mut self.cue_badge_text, patch.cue_badge_text);
        super::patch::patch_field(&mut self.frame, patch.frame);
        super::patch::patch_field(&mut self.default_size, patch.default_size);
        super::patch::patch_field(&mut self.micro_size, patch.micro_size);
        super::patch::patch_field(&mut self.size, patch.size);
        super::patch::patch_field(&mut self.overlay, patch.overlay);
        super::patch::patch_field(&mut self.bar_gap, patch.bar_gap);
        super::patch::patch_field(&mut self.bar_width, patch.bar_width);
        super::patch::patch_field(&mut self.cache_strip_alpha, patch.cache_strip_alpha);
        super::patch::patch_field(&mut self.cache_strip_height, patch.cache_strip_height);
        super::patch::patch_field(&mut self.content_inset, patch.content_inset);
        super::patch::patch_field(
            &mut self.coverage_baseline_alpha,
            patch.coverage_baseline_alpha,
        );
        super::patch::patch_field(&mut self.coverage_hairline, patch.coverage_hairline);
        super::patch::patch_field(&mut self.coverage_rail_height, patch.coverage_rail_height);
        super::patch::patch_field(&mut self.coverage_stub_alpha, patch.coverage_stub_alpha);
        super::patch::patch_field(&mut self.coverage_stub_height, patch.coverage_stub_height);
        super::patch::patch_field(&mut self.cue_badge_size, patch.cue_badge_size);
        super::patch::patch_field(&mut self.cue_line_width, patch.cue_line_width);
        super::patch::patch_field(&mut self.downbeat_alpha, patch.downbeat_alpha);
        super::patch::patch_field(&mut self.grid_alpha, patch.grid_alpha);
        super::patch::patch_field(&mut self.grid_width, patch.grid_width);
        super::patch::patch_field(&mut self.loop_bound_width, patch.loop_bound_width);
        super::patch::patch_field(&mut self.loop_fill_alpha, patch.loop_fill_alpha);
        super::patch::patch_field(&mut self.overview_played_alpha, patch.overview_played_alpha);
        super::patch::patch_field(&mut self.played_alpha, patch.played_alpha);
        super::patch::patch_field(
            &mut self.playhead_marker_height,
            patch.playhead_marker_height,
        );
        super::patch::patch_field(&mut self.playhead_marker_width, patch.playhead_marker_width);
        super::patch::patch_field(&mut self.playhead_width, patch.playhead_width);
    }
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Serialize, kithara_derive::SkinWalk)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct WaveOverlaySkin {
    pub art_background: ColorRole,
    pub background: ColorRole,
    pub badge_background: ColorRole,
    pub bpm_color: ColorRole,
    pub key_color: ColorRole,
    pub readout_background: ColorRole,
    pub remain_color: ColorRole,
    pub art_frame: FrameSkin,
    pub badge_frame: FrameSkin,
    pub readout_frame: FrameSkin,
    pub art_label: TextRoleSkin,
    pub artist: TextRoleSkin,
    pub badge_text: TextRoleSkin,
    pub readout_label: TextRoleSkin,
    pub readout_value: TextRoleSkin,
    pub title: TextRoleSkin,
    pub art_size: f32,
    pub background_alpha: f32,
    pub badge_size: f32,
    pub bpm_width: f32,
    pub gap: f32,
    pub height: f32,
    pub key_width: f32,
    pub padding_x: f32,
    pub padding_y: f32,
    pub readout_gap: f32,
    pub readout_height: f32,
    pub readout_padding_x: f32,
    pub readout_padding_y: f32,
    pub remain_width: f32,
    pub summary_gap: f32,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Serialize, kithara_derive::SkinWalk)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct DeckSkin {
    pub clock_background: ColorRole,
    pub panel_color: ColorRole,
    pub bpm_size: SizeSpec,
    pub summary_size: SizeSpec,
    pub time_size: SizeSpec,
    pub artist: TextRoleSkin,
    pub bpm_text: TextRoleSkin,
    pub micro_source: TextRoleSkin,
    pub micro_title: TextRoleSkin,
    pub readout_label: TextRoleSkin,
    pub time_text: TextRoleSkin,
    pub title: TextRoleSkin,
    pub micro_summary_gap: f32,
    pub readout_gap: f32,
    pub summary_padding_x: f32,
    pub summary_padding_y: f32,
    pub time_padding_x: f32,
    pub time_padding_y: f32,
}

/// What a skin may restate of [`DeckSkin`].
#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Serialize)]
#[serde(default, deny_unknown_fields)]
#[non_exhaustive]
pub struct DeckPatch {
    pub artist: Option<TextRoleSkin>,
    pub bpm_size: Option<SizeSpec>,
    pub bpm_text: Option<TextRoleSkin>,
    pub clock_background: Option<ColorRole>,
    pub micro_source: Option<TextRoleSkin>,
    pub micro_summary_gap: Option<f32>,
    pub micro_title: Option<TextRoleSkin>,
    pub panel_color: Option<ColorRole>,
    pub readout_gap: Option<f32>,
    pub readout_label: Option<TextRoleSkin>,
    pub summary_padding_x: Option<f32>,
    pub summary_padding_y: Option<f32>,
    pub summary_size: Option<SizeSpec>,
    pub time_padding_x: Option<f32>,
    pub time_padding_y: Option<f32>,
    pub time_size: Option<SizeSpec>,
    pub time_text: Option<TextRoleSkin>,
    pub title: Option<TextRoleSkin>,
}

impl DeckSkin {
    /// Takes every field the patch restates, keeping the rest.
    pub(crate) fn patch(&mut self, patch: DeckPatch) {
        super::patch::patch_field(&mut self.clock_background, patch.clock_background);
        super::patch::patch_field(&mut self.panel_color, patch.panel_color);
        super::patch::patch_field(&mut self.artist, patch.artist);
        super::patch::patch_field(&mut self.bpm_text, patch.bpm_text);
        super::patch::patch_field(&mut self.micro_source, patch.micro_source);
        super::patch::patch_field(&mut self.micro_title, patch.micro_title);
        super::patch::patch_field(&mut self.readout_label, patch.readout_label);
        super::patch::patch_field(&mut self.time_text, patch.time_text);
        super::patch::patch_field(&mut self.title, patch.title);
        super::patch::patch_field(&mut self.bpm_size, patch.bpm_size);
        super::patch::patch_field(&mut self.summary_size, patch.summary_size);
        super::patch::patch_field(&mut self.time_size, patch.time_size);
        super::patch::patch_field(&mut self.micro_summary_gap, patch.micro_summary_gap);
        super::patch::patch_field(&mut self.readout_gap, patch.readout_gap);
        super::patch::patch_field(&mut self.summary_padding_x, patch.summary_padding_x);
        super::patch::patch_field(&mut self.summary_padding_y, patch.summary_padding_y);
        super::patch::patch_field(&mut self.time_padding_x, patch.time_padding_x);
        super::patch::patch_field(&mut self.time_padding_y, patch.time_padding_y);
    }
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Serialize, kithara_derive::SkinWalk)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct GlobalBarSkin {
    pub chip_active_text_color: ColorRole,
    pub panel_fill: ColorRole,
    pub selector_fill: ColorRole,
    pub settings_icon_color: ColorRole,
    pub chip_frame: FrameSkin,
    pub selector_frame: FrameSkin,
    pub settings_frame: FrameSkin,
    pub brand_size: SizeSpec,
    pub preset_size: SizeSpec,
    pub settings_size: SizeSpec,
    pub spacer_size: SizeSpec,
    pub chip_active_fill: StateColors,
    /// What a preset chip fills with when it is not the one in use, and
    /// when it is.
    pub chip_fill: StateColors,
    pub settings_fill: StateColors,
    pub brand_text: TextRoleSkin,
    pub chip_text: TextRoleSkin,
    pub brand_gap: f32,
    pub brand_padding_x: f32,
    pub brand_padding_y: f32,
    pub brand_width: f32,
    pub chip_gap: f32,
    pub chip_padding_x: f32,
    pub chip_padding_y: f32,
    pub gear_size: f32,
    pub height: f32,
    pub selector_padding_x: f32,
    pub selector_padding_y: f32,
    pub selector_width: f32,
    pub settings_padding: f32,
}

/// What a skin may restate of [`GlobalBarSkin`].
#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Serialize)]
#[serde(default, deny_unknown_fields)]
#[non_exhaustive]
pub struct GlobalBarPatch {
    pub brand_gap: Option<f32>,
    pub brand_padding_x: Option<f32>,
    pub brand_padding_y: Option<f32>,
    pub brand_size: Option<SizeSpec>,
    pub brand_text: Option<TextRoleSkin>,
    pub brand_width: Option<f32>,
    pub chip_active_fill: Option<StateColors>,
    pub chip_active_text_color: Option<ColorRole>,
    pub chip_fill: Option<StateColors>,
    pub chip_frame: Option<FrameSkin>,
    pub chip_gap: Option<f32>,
    pub chip_padding_x: Option<f32>,
    pub chip_padding_y: Option<f32>,
    pub chip_text: Option<TextRoleSkin>,
    pub gear_size: Option<f32>,
    pub height: Option<f32>,
    pub panel_fill: Option<ColorRole>,
    pub preset_size: Option<SizeSpec>,
    pub selector_fill: Option<ColorRole>,
    pub selector_frame: Option<FrameSkin>,
    pub selector_padding_x: Option<f32>,
    pub selector_padding_y: Option<f32>,
    pub selector_width: Option<f32>,
    pub settings_fill: Option<StateColors>,
    pub settings_frame: Option<FrameSkin>,
    pub settings_icon_color: Option<ColorRole>,
    pub settings_padding: Option<f32>,
    pub settings_size: Option<SizeSpec>,
    pub spacer_size: Option<SizeSpec>,
}

impl GlobalBarSkin {
    /// Takes every field the patch restates, keeping the rest.
    pub(crate) fn patch(&mut self, patch: GlobalBarPatch) {
        super::patch::patch_field(&mut self.brand_text, patch.brand_text);
        super::patch::patch_field(&mut self.chip_text, patch.chip_text);
        super::patch::patch_field(&mut self.chip_fill, patch.chip_fill);
        super::patch::patch_field(&mut self.chip_active_fill, patch.chip_active_fill);
        super::patch::patch_field(
            &mut self.chip_active_text_color,
            patch.chip_active_text_color,
        );
        super::patch::patch_field(&mut self.settings_fill, patch.settings_fill);
        super::patch::patch_field(&mut self.settings_icon_color, patch.settings_icon_color);
        super::patch::patch_field(&mut self.panel_fill, patch.panel_fill);
        super::patch::patch_field(&mut self.selector_fill, patch.selector_fill);
        super::patch::patch_field(&mut self.chip_frame, patch.chip_frame);
        super::patch::patch_field(&mut self.selector_frame, patch.selector_frame);
        super::patch::patch_field(&mut self.settings_frame, patch.settings_frame);
        super::patch::patch_field(&mut self.brand_size, patch.brand_size);
        super::patch::patch_field(&mut self.preset_size, patch.preset_size);
        super::patch::patch_field(&mut self.settings_size, patch.settings_size);
        super::patch::patch_field(&mut self.spacer_size, patch.spacer_size);
        super::patch::patch_field(&mut self.brand_gap, patch.brand_gap);
        super::patch::patch_field(&mut self.brand_padding_x, patch.brand_padding_x);
        super::patch::patch_field(&mut self.brand_padding_y, patch.brand_padding_y);
        super::patch::patch_field(&mut self.brand_width, patch.brand_width);
        super::patch::patch_field(&mut self.chip_gap, patch.chip_gap);
        super::patch::patch_field(&mut self.chip_padding_x, patch.chip_padding_x);
        super::patch::patch_field(&mut self.chip_padding_y, patch.chip_padding_y);
        super::patch::patch_field(&mut self.gear_size, patch.gear_size);
        super::patch::patch_field(&mut self.height, patch.height);
        super::patch::patch_field(&mut self.selector_padding_x, patch.selector_padding_x);
        super::patch::patch_field(&mut self.selector_padding_y, patch.selector_padding_y);
        super::patch::patch_field(&mut self.selector_width, patch.selector_width);
        super::patch::patch_field(&mut self.settings_padding, patch.settings_padding);
    }
}

/// Horizontal fill bar reporting one scalar, as the design's CPU cell draws it:
/// an inset track with a hairline frame, filled from the left.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Serialize, kithara_derive::SkinWalk)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct MeterSkin {
    pub background: ColorRole,
    pub fill: ColorRole,
    pub frame: FrameSkin,
    pub size: SizeSpec,
}

/// What a skin may restate of [`MeterSkin`].
#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Serialize)]
#[serde(default, deny_unknown_fields)]
#[non_exhaustive]
pub struct MeterPatch {
    pub background: Option<ColorRole>,
    pub fill: Option<ColorRole>,
    pub frame: Option<FrameSkin>,
    pub size: Option<SizeSpec>,
}

impl MeterSkin {
    /// Takes every field the patch restates, keeping the rest.
    pub(crate) fn patch(&mut self, patch: MeterPatch) {
        super::patch::patch_field(&mut self.background, patch.background);
        super::patch::patch_field(&mut self.fill, patch.fill);
        super::patch::patch_field(&mut self.frame, patch.frame);
        super::patch::patch_field(&mut self.size, patch.size);
    }
}

/// Hairline between adjacent cells or control sections.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Serialize, kithara_derive::SkinWalk)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct DividerSkin {
    pub color: ColorRole,
    pub width: f32,
}

/// What a skin may restate of [`DividerSkin`].
#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Serialize)]
#[serde(default, deny_unknown_fields)]
#[non_exhaustive]
pub struct DividerPatch {
    pub color: Option<ColorRole>,
    pub width: Option<f32>,
}

impl DividerSkin {
    /// Takes every field the patch restates, keeping the rest.
    pub(crate) fn patch(&mut self, patch: DividerPatch) {
        super::patch::patch_field(&mut self.color, patch.color);
        super::patch::patch_field(&mut self.width, patch.width);
    }
}

/// The label the pointer carries while it drags an item.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Serialize, kithara_derive::SkinWalk)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct DragSkin {
    pub background: ColorRole,
    pub frame: FrameSkin,
    pub text: TextRoleSkin,
    pub height: f32,
    pub pad_x: f32,
    pub width: f32,
}

/// What a skin may restate of [`DragSkin`].
#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Serialize)]
#[serde(default, deny_unknown_fields)]
#[non_exhaustive]
pub struct DragPatch {
    pub background: Option<ColorRole>,
    pub frame: Option<FrameSkin>,
    pub height: Option<f32>,
    pub pad_x: Option<f32>,
    pub text: Option<TextRoleSkin>,
    pub width: Option<f32>,
}

impl DragSkin {
    /// Takes every field the patch restates, keeping the rest.
    pub(crate) fn patch(&mut self, patch: DragPatch) {
        super::patch::patch_field(&mut self.background, patch.background);
        super::patch::patch_field(&mut self.frame, patch.frame);
        super::patch::patch_field(&mut self.text, patch.text);
        super::patch::patch_field(&mut self.height, patch.height);
        super::patch::patch_field(&mut self.pad_x, patch.pad_x);
        super::patch::patch_field(&mut self.width, patch.width);
    }
}

/// Pop-over chrome; the frame and the cap draw outward of the content column.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Serialize, kithara_derive::SkinWalk)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct PopSkin {
    pub background: ColorRole,
    pub cap_color: ColorRole,
    pub frame: FrameSkin,
    pub shadow: ShadowSkin,
    pub cap_height: f32,
}

/// What a skin may restate of [`PopSkin`].
#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Serialize)]
#[serde(default, deny_unknown_fields)]
#[non_exhaustive]
pub struct PopPatch {
    pub background: Option<ColorRole>,
    pub cap_color: Option<ColorRole>,
    pub cap_height: Option<f32>,
    pub frame: Option<FrameSkin>,
    pub shadow: Option<ShadowSkin>,
}

impl PopSkin {
    /// Takes every field the patch restates, keeping the rest.
    pub(crate) fn patch(&mut self, patch: PopPatch) {
        super::patch::patch_field(&mut self.background, patch.background);
        super::patch::patch_field(&mut self.cap_color, patch.cap_color);
        super::patch::patch_field(&mut self.frame, patch.frame);
        super::patch::patch_field(&mut self.shadow, patch.shadow);
        super::patch::patch_field(&mut self.cap_height, patch.cap_height);
    }
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Serialize, kithara_derive::SkinWalk)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct TelemetrySkin {
    pub inset_color: ColorRole,
    pub frame: FrameSkin,
    pub size: SizeSpec,
    pub text: TextRoleSkin,
    pub padding_x: f32,
    pub padding_y: f32,
    pub percent_scale: f64,
    pub percent_precision: usize,
    pub percent_width: usize,
    pub scalar_precision: usize,
}

/// What a skin may restate of [`TelemetrySkin`].
#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Serialize)]
#[serde(default, deny_unknown_fields)]
#[non_exhaustive]
pub struct TelemetryPatch {
    pub frame: Option<FrameSkin>,
    pub inset_color: Option<ColorRole>,
    pub padding_x: Option<f32>,
    pub padding_y: Option<f32>,
    pub percent_precision: Option<usize>,
    pub percent_scale: Option<f64>,
    pub percent_width: Option<usize>,
    pub scalar_precision: Option<usize>,
    pub size: Option<SizeSpec>,
    pub text: Option<TextRoleSkin>,
}

impl TelemetrySkin {
    /// Takes every field the patch restates, keeping the rest.
    pub(crate) fn patch(&mut self, patch: TelemetryPatch) {
        super::patch::patch_field(&mut self.inset_color, patch.inset_color);
        super::patch::patch_field(&mut self.text, patch.text);
        super::patch::patch_field(&mut self.frame, patch.frame);
        super::patch::patch_field(&mut self.size, patch.size);
        super::patch::patch_field(&mut self.padding_x, patch.padding_x);
        super::patch::patch_field(&mut self.padding_y, patch.padding_y);
        super::patch::patch_field(&mut self.percent_scale, patch.percent_scale);
        super::patch::patch_field(&mut self.percent_precision, patch.percent_precision);
        super::patch::patch_field(&mut self.percent_width, patch.percent_width);
        super::patch::patch_field(&mut self.scalar_precision, patch.scalar_precision);
    }
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Serialize, kithara_derive::SkinWalk)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct TreeSkin {
    pub chevron_color: ColorRole,
    pub context_background: ColorRole,
    pub context_divider: ColorRole,
    pub context_icon_color: ColorRole,
    pub panel_background: ColorRole,
    pub row_hovered_fill: ColorRole,
    pub row_idle_text_color: ColorRole,
    pub row_marker_color: ColorRole,
    pub row_muted_text_color: ColorRole,
    /// What a row lays down behind itself, and the bar it carries on its
    /// edge while it is the row in use. A row nobody is on lays nothing.
    pub row_selected_fill: ColorRole,
    pub row_text_color: ColorRole,
    pub scope_background: ColorRole,
    pub scope_chevron_color: ColorRole,
    pub scope_menu_background: ColorRole,
    pub scope_menu_text: ColorRole,
    pub scope_selected_background: ColorRole,
    pub scope_selected_text: ColorRole,
    pub scope_text_color: ColorRole,
    pub scrollbar_background: ColorRole,
    pub scroller_color: ColorRole,
    pub search_background: ColorRole,
    pub search_caret_color: ColorRole,
    pub search_divider: ColorRole,
    pub search_icon_color: ColorRole,
    pub search_placeholder_color: ColorRole,
    pub search_selection_fill: ColorRole,
    pub scope_frame: FrameSkin,
    pub scope_menu_frame: FrameSkin,
    pub size: SizeSpec,
    pub context_text: TextRoleSkin,
    pub count_text: TextRoleSkin,
    pub label_text: TextRoleSkin,
    pub scope_text: TextRoleSkin,
    pub search_text: TextRoleSkin,
    pub chevron_size: f32,
    pub chevron_width: f32,
    pub content_gap: f32,
    pub context_divider_width: f32,
    pub context_gap: f32,
    pub context_height: f32,
    pub context_icon_size: f32,
    pub context_padding_x: f32,
    pub icon_size: f32,
    pub indent_base: f32,
    pub indent_step: f32,
    pub marker_width: f32,
    pub panel_padding_bottom: f32,
    pub panel_padding_top: f32,
    pub row_height: f32,
    pub row_padding_right: f32,
    pub scope_chevron_size: f32,
    pub scope_gap: f32,
    pub scope_item_height: f32,
    pub scope_padding_x: f32,
    pub scrollbar_margin: f32,
    pub scrollbar_width: f32,
    pub search_height: f32,
    pub search_icon_size: f32,
    pub search_icon_width: f32,
    pub search_padding_x: f32,
}

/// What a skin may restate of [`TreeSkin`].
#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Serialize)]
#[serde(default, deny_unknown_fields)]
#[non_exhaustive]
pub struct TreePatch {
    pub chevron_color: Option<ColorRole>,
    pub chevron_size: Option<f32>,
    pub chevron_width: Option<f32>,
    pub content_gap: Option<f32>,
    pub context_background: Option<ColorRole>,
    pub context_divider: Option<ColorRole>,
    pub context_divider_width: Option<f32>,
    pub context_gap: Option<f32>,
    pub context_height: Option<f32>,
    pub context_icon_color: Option<ColorRole>,
    pub context_icon_size: Option<f32>,
    pub context_padding_x: Option<f32>,
    pub context_text: Option<TextRoleSkin>,
    pub count_text: Option<TextRoleSkin>,
    pub icon_size: Option<f32>,
    pub indent_base: Option<f32>,
    pub indent_step: Option<f32>,
    pub label_text: Option<TextRoleSkin>,
    pub marker_width: Option<f32>,
    pub panel_background: Option<ColorRole>,
    pub panel_padding_bottom: Option<f32>,
    pub panel_padding_top: Option<f32>,
    pub row_height: Option<f32>,
    pub row_hovered_fill: Option<ColorRole>,
    pub row_idle_text_color: Option<ColorRole>,
    pub row_marker_color: Option<ColorRole>,
    pub row_muted_text_color: Option<ColorRole>,
    pub row_padding_right: Option<f32>,
    pub row_selected_fill: Option<ColorRole>,
    pub row_text_color: Option<ColorRole>,
    pub scope_background: Option<ColorRole>,
    pub scope_chevron_color: Option<ColorRole>,
    pub scope_chevron_size: Option<f32>,
    pub scope_frame: Option<FrameSkin>,
    pub scope_gap: Option<f32>,
    pub scope_item_height: Option<f32>,
    pub scope_menu_background: Option<ColorRole>,
    pub scope_menu_frame: Option<FrameSkin>,
    pub scope_menu_text: Option<ColorRole>,
    pub scope_padding_x: Option<f32>,
    pub scope_selected_background: Option<ColorRole>,
    pub scope_selected_text: Option<ColorRole>,
    pub scope_text: Option<TextRoleSkin>,
    pub scope_text_color: Option<ColorRole>,
    pub scrollbar_background: Option<ColorRole>,
    pub scrollbar_margin: Option<f32>,
    pub scrollbar_width: Option<f32>,
    pub scroller_color: Option<ColorRole>,
    pub search_background: Option<ColorRole>,
    pub search_caret_color: Option<ColorRole>,
    pub search_divider: Option<ColorRole>,
    pub search_height: Option<f32>,
    pub search_icon_color: Option<ColorRole>,
    pub search_icon_size: Option<f32>,
    pub search_icon_width: Option<f32>,
    pub search_padding_x: Option<f32>,
    pub search_placeholder_color: Option<ColorRole>,
    pub search_selection_fill: Option<ColorRole>,
    pub search_text: Option<TextRoleSkin>,
    pub size: Option<SizeSpec>,
}

impl TreeSkin {
    /// Takes every field the patch restates, keeping the rest.
    pub(crate) fn patch(&mut self, patch: TreePatch) {
        super::patch::patch_field(&mut self.context_background, patch.context_background);
        super::patch::patch_field(&mut self.context_icon_color, patch.context_icon_color);
        super::patch::patch_field(&mut self.row_selected_fill, patch.row_selected_fill);
        super::patch::patch_field(&mut self.row_hovered_fill, patch.row_hovered_fill);
        super::patch::patch_field(&mut self.row_marker_color, patch.row_marker_color);
        super::patch::patch_field(&mut self.row_text_color, patch.row_text_color);
        super::patch::patch_field(&mut self.row_muted_text_color, patch.row_muted_text_color);
        super::patch::patch_field(&mut self.row_idle_text_color, patch.row_idle_text_color);
        super::patch::patch_field(&mut self.chevron_color, patch.chevron_color);
        super::patch::patch_field(&mut self.search_caret_color, patch.search_caret_color);
        super::patch::patch_field(&mut self.search_icon_color, patch.search_icon_color);
        super::patch::patch_field(
            &mut self.search_placeholder_color,
            patch.search_placeholder_color,
        );
        super::patch::patch_field(&mut self.search_selection_fill, patch.search_selection_fill);
        super::patch::patch_field(&mut self.context_divider, patch.context_divider);
        super::patch::patch_field(&mut self.panel_background, patch.panel_background);
        super::patch::patch_field(&mut self.scope_background, patch.scope_background);
        super::patch::patch_field(&mut self.scope_chevron_color, patch.scope_chevron_color);
        super::patch::patch_field(&mut self.scope_menu_background, patch.scope_menu_background);
        super::patch::patch_field(&mut self.scope_menu_text, patch.scope_menu_text);
        super::patch::patch_field(
            &mut self.scope_selected_background,
            patch.scope_selected_background,
        );
        super::patch::patch_field(&mut self.scope_selected_text, patch.scope_selected_text);
        super::patch::patch_field(&mut self.scope_text_color, patch.scope_text_color);
        super::patch::patch_field(&mut self.scrollbar_background, patch.scrollbar_background);
        super::patch::patch_field(&mut self.scroller_color, patch.scroller_color);
        super::patch::patch_field(&mut self.search_background, patch.search_background);
        super::patch::patch_field(&mut self.search_divider, patch.search_divider);
        super::patch::patch_field(&mut self.context_text, patch.context_text);
        super::patch::patch_field(&mut self.count_text, patch.count_text);
        super::patch::patch_field(&mut self.label_text, patch.label_text);
        super::patch::patch_field(&mut self.scope_text, patch.scope_text);
        super::patch::patch_field(&mut self.search_text, patch.search_text);
        super::patch::patch_field(&mut self.scope_frame, patch.scope_frame);
        super::patch::patch_field(&mut self.scope_menu_frame, patch.scope_menu_frame);
        super::patch::patch_field(&mut self.size, patch.size);
        super::patch::patch_field(&mut self.chevron_size, patch.chevron_size);
        super::patch::patch_field(&mut self.chevron_width, patch.chevron_width);
        super::patch::patch_field(&mut self.content_gap, patch.content_gap);
        super::patch::patch_field(&mut self.context_divider_width, patch.context_divider_width);
        super::patch::patch_field(&mut self.context_gap, patch.context_gap);
        super::patch::patch_field(&mut self.context_height, patch.context_height);
        super::patch::patch_field(&mut self.context_icon_size, patch.context_icon_size);
        super::patch::patch_field(&mut self.context_padding_x, patch.context_padding_x);
        super::patch::patch_field(&mut self.icon_size, patch.icon_size);
        super::patch::patch_field(&mut self.indent_base, patch.indent_base);
        super::patch::patch_field(&mut self.indent_step, patch.indent_step);
        super::patch::patch_field(&mut self.marker_width, patch.marker_width);
        super::patch::patch_field(&mut self.panel_padding_bottom, patch.panel_padding_bottom);
        super::patch::patch_field(&mut self.panel_padding_top, patch.panel_padding_top);
        super::patch::patch_field(&mut self.row_height, patch.row_height);
        super::patch::patch_field(&mut self.row_padding_right, patch.row_padding_right);
        super::patch::patch_field(&mut self.scope_chevron_size, patch.scope_chevron_size);
        super::patch::patch_field(&mut self.scope_gap, patch.scope_gap);
        super::patch::patch_field(&mut self.scope_item_height, patch.scope_item_height);
        super::patch::patch_field(&mut self.scope_padding_x, patch.scope_padding_x);
        super::patch::patch_field(&mut self.scrollbar_margin, patch.scrollbar_margin);
        super::patch::patch_field(&mut self.scrollbar_width, patch.scrollbar_width);
        super::patch::patch_field(&mut self.search_height, patch.search_height);
        super::patch::patch_field(&mut self.search_icon_size, patch.search_icon_size);
        super::patch::patch_field(&mut self.search_icon_width, patch.search_icon_width);
        super::patch::patch_field(&mut self.search_padding_x, patch.search_padding_x);
    }
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Serialize, kithara_derive::SkinWalk)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct TableSkin {
    pub badge_fill: ColorRole,
    pub divider_color: ColorRole,
    pub footer_fill: ColorRole,
    /// The ground the grid, the header strip and the footer strip lay down
    /// before any row is drawn.
    pub grid_color: ColorRole,
    pub header_fill: ColorRole,
    pub meter_bar_background: ColorRole,
    pub meter_bar_fill: ColorRole,
    pub metric_badge_background: ColorRole,
    pub row_selected_fill: ColorRole,
    pub scrollbar_background: ColorRole,
    pub scroller_color: ColorRole,
    pub badge_frame: FrameSkin,
    pub metric_badge_frame: FrameSkin,
    pub row_frame: FrameSkin,
    pub size: SizeSpec,
    pub row_fill: StateColors,
    pub badge_text: TextRoleSkin,
    pub footer_text: TextRoleSkin,
    pub header_text: TextRoleSkin,
    pub index_text: TextRoleSkin,
    pub meter_text: TextRoleSkin,
    pub metric_text: TextRoleSkin,
    pub mono_text: TextRoleSkin,
    pub primary_text: TextRoleSkin,
    pub secondary_text: TextRoleSkin,
    pub time_text: TextRoleSkin,
    pub transition_text: TextRoleSkin,
    pub badge_height: f32,
    pub badge_width: f32,
    pub cell_padding_x: f32,
    pub divider_hit_width: f32,
    pub divider_width: f32,
    pub footer_height: f32,
    pub footer_padding_x: f32,
    pub grid_gap: f32,
    pub header_height: f32,
    pub meter_bar_gap: f32,
    pub meter_bar_height: f32,
    pub meter_bar_width: f32,
    pub metric_badge_height: f32,
    pub metric_badge_padding_x: f32,
    pub min_column_width: f32,
    pub row_height: f32,
    pub scrollbar_margin: f32,
    pub scrollbar_width: f32,
}

/// What a skin may restate of [`TableSkin`].
#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Serialize)]
#[serde(default, deny_unknown_fields)]
#[non_exhaustive]
pub struct TablePatch {
    pub badge_fill: Option<ColorRole>,
    pub badge_frame: Option<FrameSkin>,
    pub badge_height: Option<f32>,
    pub badge_text: Option<TextRoleSkin>,
    pub badge_width: Option<f32>,
    pub cell_padding_x: Option<f32>,
    pub divider_color: Option<ColorRole>,
    pub divider_hit_width: Option<f32>,
    pub divider_width: Option<f32>,
    pub footer_fill: Option<ColorRole>,
    pub footer_height: Option<f32>,
    pub footer_padding_x: Option<f32>,
    pub footer_text: Option<TextRoleSkin>,
    pub grid_color: Option<ColorRole>,
    pub grid_gap: Option<f32>,
    pub header_fill: Option<ColorRole>,
    pub header_height: Option<f32>,
    pub header_text: Option<TextRoleSkin>,
    pub index_text: Option<TextRoleSkin>,
    pub meter_bar_background: Option<ColorRole>,
    pub meter_bar_fill: Option<ColorRole>,
    pub meter_bar_gap: Option<f32>,
    pub meter_bar_height: Option<f32>,
    pub meter_bar_width: Option<f32>,
    pub meter_text: Option<TextRoleSkin>,
    pub metric_badge_background: Option<ColorRole>,
    pub metric_badge_frame: Option<FrameSkin>,
    pub metric_badge_height: Option<f32>,
    pub metric_badge_padding_x: Option<f32>,
    pub metric_text: Option<TextRoleSkin>,
    pub min_column_width: Option<f32>,
    pub mono_text: Option<TextRoleSkin>,
    pub primary_text: Option<TextRoleSkin>,
    pub row_fill: Option<StateColors>,
    pub row_frame: Option<FrameSkin>,
    pub row_height: Option<f32>,
    pub row_selected_fill: Option<ColorRole>,
    pub scrollbar_background: Option<ColorRole>,
    pub scrollbar_margin: Option<f32>,
    pub scrollbar_width: Option<f32>,
    pub scroller_color: Option<ColorRole>,
    pub secondary_text: Option<TextRoleSkin>,
    pub size: Option<SizeSpec>,
    pub time_text: Option<TextRoleSkin>,
    pub transition_text: Option<TextRoleSkin>,
}

impl TableSkin {
    /// Takes every field the patch restates, keeping the rest.
    pub(crate) fn patch(&mut self, patch: TablePatch) {
        super::patch::patch_field(
            &mut self.metric_badge_background,
            patch.metric_badge_background,
        );
        super::patch::patch_field(&mut self.grid_color, patch.grid_color);
        super::patch::patch_field(&mut self.header_fill, patch.header_fill);
        super::patch::patch_field(&mut self.footer_fill, patch.footer_fill);
        super::patch::patch_field(&mut self.badge_fill, patch.badge_fill);
        super::patch::patch_field(&mut self.meter_bar_fill, patch.meter_bar_fill);
        super::patch::patch_field(&mut self.row_fill, patch.row_fill);
        super::patch::patch_field(&mut self.row_selected_fill, patch.row_selected_fill);
        super::patch::patch_field(&mut self.divider_color, patch.divider_color);
        super::patch::patch_field(&mut self.meter_bar_background, patch.meter_bar_background);
        super::patch::patch_field(&mut self.scrollbar_background, patch.scrollbar_background);
        super::patch::patch_field(&mut self.scroller_color, patch.scroller_color);
        super::patch::patch_field(&mut self.secondary_text, patch.secondary_text);
        super::patch::patch_field(&mut self.metric_text, patch.metric_text);
        super::patch::patch_field(&mut self.badge_text, patch.badge_text);
        super::patch::patch_field(&mut self.meter_text, patch.meter_text);
        super::patch::patch_field(&mut self.footer_text, patch.footer_text);
        super::patch::patch_field(&mut self.header_text, patch.header_text);
        super::patch::patch_field(&mut self.index_text, patch.index_text);
        super::patch::patch_field(&mut self.mono_text, patch.mono_text);
        super::patch::patch_field(&mut self.time_text, patch.time_text);
        super::patch::patch_field(&mut self.primary_text, patch.primary_text);
        super::patch::patch_field(&mut self.transition_text, patch.transition_text);
        super::patch::patch_field(&mut self.metric_badge_frame, patch.metric_badge_frame);
        super::patch::patch_field(&mut self.badge_frame, patch.badge_frame);
        super::patch::patch_field(&mut self.row_frame, patch.row_frame);
        super::patch::patch_field(&mut self.size, patch.size);
        super::patch::patch_field(&mut self.metric_badge_height, patch.metric_badge_height);
        super::patch::patch_field(
            &mut self.metric_badge_padding_x,
            patch.metric_badge_padding_x,
        );
        super::patch::patch_field(&mut self.cell_padding_x, patch.cell_padding_x);
        super::patch::patch_field(&mut self.badge_height, patch.badge_height);
        super::patch::patch_field(&mut self.badge_width, patch.badge_width);
        super::patch::patch_field(&mut self.divider_hit_width, patch.divider_hit_width);
        super::patch::patch_field(&mut self.divider_width, patch.divider_width);
        super::patch::patch_field(&mut self.meter_bar_gap, patch.meter_bar_gap);
        super::patch::patch_field(&mut self.meter_bar_height, patch.meter_bar_height);
        super::patch::patch_field(&mut self.meter_bar_width, patch.meter_bar_width);
        super::patch::patch_field(&mut self.footer_height, patch.footer_height);
        super::patch::patch_field(&mut self.footer_padding_x, patch.footer_padding_x);
        super::patch::patch_field(&mut self.grid_gap, patch.grid_gap);
        super::patch::patch_field(&mut self.header_height, patch.header_height);
        super::patch::patch_field(&mut self.min_column_width, patch.min_column_width);
        super::patch::patch_field(&mut self.row_height, patch.row_height);
        super::patch::patch_field(&mut self.scrollbar_margin, patch.scrollbar_margin);
        super::patch::patch_field(&mut self.scrollbar_width, patch.scrollbar_width);
    }
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Serialize, kithara_derive::SkinWalk)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct LayoutPreviewSkin {
    pub height: f32,
    pub line_width: f32,
    pub module_inset: f32,
}

/// What a skin may restate of [`LayoutPreviewSkin`].
#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Serialize)]
#[serde(default, deny_unknown_fields)]
#[non_exhaustive]
pub struct LayoutPreviewPatch {
    pub height: Option<f32>,
    pub line_width: Option<f32>,
    pub module_inset: Option<f32>,
}

impl LayoutPreviewSkin {
    /// Takes every field the patch restates, keeping the rest.
    pub(crate) fn patch(&mut self, patch: LayoutPreviewPatch) {
        super::patch::patch_field(&mut self.height, patch.height);
        super::patch::patch_field(&mut self.line_width, patch.line_width);
        super::patch::patch_field(&mut self.module_inset, patch.module_inset);
    }
}
