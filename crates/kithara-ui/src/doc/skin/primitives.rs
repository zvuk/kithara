use kithara_ui_shaping::{FontFamily, FontWeight, TextStyle};
use serde::{Deserialize, Serialize};

use super::palette::ColorRole;
use crate::module::{Tone, WindowControlsStyle};

/// One of the two looks a control switches between: what it paints under
/// itself, and what it draws on top. A face naming no fill paints none, and
/// sits on the surface it is mounted in.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct FaceSkin {
    pub content: ColorRole,
    pub fill: Option<ColorRole>,
}

/// What a control paints under itself in each pointer state. A state naming
/// no colour paints nothing, which is how a control sits on the surface it is
/// mounted in rather than over it.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct StateColors {
    pub hovered: Option<ColorRole>,
    pub idle: Option<ColorRole>,
    pub pressed: Option<ColorRole>,
}

/// The colour each of a control's four tones names. A control the document
/// hands a tone reads its colour here rather than from a palette role named
/// in Rust.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct ToneColors {
    pub accent: ColorRole,
    pub danger: ColorRole,
    pub neutral: ColorRole,
    pub success: ColorRole,
}

/// The role one tone names in a control's own tone set.
pub(crate) const fn tone_color(tone: Tone, tones: ToneColors) -> ColorRole {
    match tone {
        Tone::Accent => tones.accent,
        Tone::Danger => tones.danger,
        Tone::Neutral => tones.neutral,
        Tone::Success => tones.success,
    }
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Serialize, kithara_derive::Mirror)]
#[mirror(into = TextStyle)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct TextRoleSkin {
    #[mirror(skip)]
    pub color: ColorRole,
    pub font: FontFamily,
    pub weight: FontWeight,
    pub size: f32,
    pub spacing: f32,
}

impl TextRoleSkin {
    /// This role set in the face a run names, keeping the skin's where it
    /// names none.
    pub(crate) fn faced(self, font: Option<FontFamily>, weight: Option<FontWeight>) -> Self {
        Self {
            font: font.unwrap_or(self.font),
            weight: weight.unwrap_or(self.weight),
            ..self
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct FrameSkin {
    pub border: ColorRole,
    pub border_width: f32,
    pub radius: f32,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct ShadowSkin {
    pub color: ColorRole,
    pub alpha: f32,
    pub blur: f32,
    pub offset_x: f32,
    pub offset_y: f32,
}

/// Scale beside a fader: hairlines with a longer, brighter one at centre.
/// `thickness` runs along the scale, `length` across it, whatever the axis.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct TickSkin {
    pub center_color: ColorRole,
    pub color: ColorRole,
    pub center_length: f32,
    pub gap: f32,
    pub inset: f32,
    pub length: f32,
    pub thickness: f32,
    pub count: usize,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Serialize, kithara_derive::SkinWalk)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct LayoutSkin {
    /// What a host clears the target to wherever no document reaches.
    pub page_background: ColorRole,
    pub grid_gap: f32,
    pub grid_pad: f32,
}

/// What a skin may restate of [`LayoutSkin`].
#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Serialize)]
#[serde(default, deny_unknown_fields)]
#[non_exhaustive]
pub struct LayoutPatch {
    pub grid_gap: Option<f32>,
    pub grid_pad: Option<f32>,
    pub page_background: Option<ColorRole>,
}

impl LayoutSkin {
    /// Takes every field the patch restates, keeping the rest.
    pub(crate) fn patch(&mut self, patch: LayoutPatch) {
        super::patch::patch_field(&mut self.page_background, patch.page_background);
        super::patch::patch_field(&mut self.grid_gap, patch.grid_gap);
        super::patch::patch_field(&mut self.grid_pad, patch.grid_pad);
    }
}

/// The indicator a viewport draws over its own right edge. `min_length` keeps
/// a window over very long content from showing a thumb too short to see.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Serialize, kithara_derive::SkinWalk)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct ScrollSkin {
    pub thumb: ColorRole,
    pub track: ColorRole,
    pub inset: f32,
    pub min_length: f32,
    pub width: f32,
}

/// What a skin may restate of [`ScrollSkin`].
#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Serialize)]
#[serde(default, deny_unknown_fields)]
#[non_exhaustive]
pub struct ScrollPatch {
    pub inset: Option<f32>,
    pub min_length: Option<f32>,
    pub thumb: Option<ColorRole>,
    pub track: Option<ColorRole>,
    pub width: Option<f32>,
}

impl ScrollSkin {
    /// Takes every field the patch restates, keeping the rest.
    pub(crate) fn patch(&mut self, patch: ScrollPatch) {
        super::patch::patch_field(&mut self.thumb, patch.thumb);
        super::patch::patch_field(&mut self.track, patch.track);
        super::patch::patch_field(&mut self.inset, patch.inset);
        super::patch::patch_field(&mut self.min_length, patch.min_length);
        super::patch::patch_field(&mut self.width, patch.width);
    }
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Serialize, kithara_derive::SkinWalk)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct ChromeSkin {
    pub chevron_color: ColorRole,
    pub chip_background: ColorRole,
    pub corner_color: ColorRole,
    pub drop_zone_color: ColorRole,
    pub footer_background: ColorRole,
    pub header_background: ColorRole,
    pub inner_line: ColorRole,
    pub panel_background: ColorRole,
    pub title_background: ColorRole,
    pub chevron_frame: FrameSkin,
    pub chip_frame: FrameSkin,
    pub footer_frame: FrameSkin,
    pub frame: FrameSkin,
    pub header_frame: FrameSkin,
    pub secondary_frame: FrameSkin,
    pub title_frame: FrameSkin,
    pub chip_text: TextRoleSkin,
    pub footer_text: TextRoleSkin,
    pub title_text: TextRoleSkin,
    pub chevron_icon_size: f32,
    pub chevron_size: f32,
    pub chevron_stroke_width: f32,
    pub chip_pad: f32,
    pub corner_offset: f32,
    pub corner_size: f32,
    pub corner_width: f32,
    pub footer_height: f32,
    pub footer_pad: f32,
    pub header_height: f32,
    pub inner_line_width: f32,
}

/// What a skin may restate of [`ChromeSkin`].
#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Serialize)]
#[serde(default, deny_unknown_fields)]
#[non_exhaustive]
pub struct ChromePatch {
    pub chevron_color: Option<ColorRole>,
    pub chevron_frame: Option<FrameSkin>,
    pub chevron_icon_size: Option<f32>,
    pub chevron_size: Option<f32>,
    pub chevron_stroke_width: Option<f32>,
    pub chip_background: Option<ColorRole>,
    pub chip_frame: Option<FrameSkin>,
    pub chip_pad: Option<f32>,
    pub chip_text: Option<TextRoleSkin>,
    pub corner_color: Option<ColorRole>,
    pub corner_offset: Option<f32>,
    pub corner_size: Option<f32>,
    pub corner_width: Option<f32>,
    pub drop_zone_color: Option<ColorRole>,
    pub footer_background: Option<ColorRole>,
    pub footer_frame: Option<FrameSkin>,
    pub footer_height: Option<f32>,
    pub footer_pad: Option<f32>,
    pub footer_text: Option<TextRoleSkin>,
    pub frame: Option<FrameSkin>,
    pub header_background: Option<ColorRole>,
    pub header_frame: Option<FrameSkin>,
    pub header_height: Option<f32>,
    pub inner_line: Option<ColorRole>,
    pub inner_line_width: Option<f32>,
    pub panel_background: Option<ColorRole>,
    pub secondary_frame: Option<FrameSkin>,
    pub title_background: Option<ColorRole>,
    pub title_frame: Option<FrameSkin>,
    pub title_text: Option<TextRoleSkin>,
}

impl ChromeSkin {
    /// Takes every field the patch restates, keeping the rest.
    pub(crate) fn patch(&mut self, patch: ChromePatch) {
        super::patch::patch_field(&mut self.chevron_color, patch.chevron_color);
        super::patch::patch_field(&mut self.chip_background, patch.chip_background);
        super::patch::patch_field(&mut self.corner_color, patch.corner_color);
        super::patch::patch_field(&mut self.drop_zone_color, patch.drop_zone_color);
        super::patch::patch_field(&mut self.footer_background, patch.footer_background);
        super::patch::patch_field(&mut self.header_background, patch.header_background);
        super::patch::patch_field(&mut self.inner_line, patch.inner_line);
        super::patch::patch_field(&mut self.panel_background, patch.panel_background);
        super::patch::patch_field(&mut self.title_background, patch.title_background);
        super::patch::patch_field(&mut self.chip_text, patch.chip_text);
        super::patch::patch_field(&mut self.footer_text, patch.footer_text);
        super::patch::patch_field(&mut self.title_text, patch.title_text);
        super::patch::patch_field(&mut self.chevron_frame, patch.chevron_frame);
        super::patch::patch_field(&mut self.chip_frame, patch.chip_frame);
        super::patch::patch_field(&mut self.footer_frame, patch.footer_frame);
        super::patch::patch_field(&mut self.frame, patch.frame);
        super::patch::patch_field(&mut self.header_frame, patch.header_frame);
        super::patch::patch_field(&mut self.secondary_frame, patch.secondary_frame);
        super::patch::patch_field(&mut self.title_frame, patch.title_frame);
        super::patch::patch_field(&mut self.chevron_icon_size, patch.chevron_icon_size);
        super::patch::patch_field(&mut self.chevron_size, patch.chevron_size);
        super::patch::patch_field(&mut self.chevron_stroke_width, patch.chevron_stroke_width);
        super::patch::patch_field(&mut self.chip_pad, patch.chip_pad);
        super::patch::patch_field(&mut self.corner_offset, patch.corner_offset);
        super::patch::patch_field(&mut self.corner_size, patch.corner_size);
        super::patch::patch_field(&mut self.corner_width, patch.corner_width);
        super::patch::patch_field(&mut self.footer_height, patch.footer_height);
        super::patch::patch_field(&mut self.footer_pad, patch.footer_pad);
        super::patch::patch_field(&mut self.header_height, patch.header_height);
        super::patch::patch_field(&mut self.inner_line_width, patch.inner_line_width);
    }
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Serialize, kithara_derive::SkinWalk)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct WindowSkin {
    pub icon_color: ColorRole,
    pub icon_hover_color: ColorRole,
    pub titlebar_text: TextRoleSkin,
    pub close_framed: WindowControlSkin,
    pub close_micro: WindowControlSkin,
    pub close_wide: WindowControlSkin,
    pub compact: WindowControlSkin,
    pub standard: WindowControlSkin,
    pub icon_stroke_width: f32,
    /// Thickness of the drag zones framing a window that draws its own chrome.
    pub resize_edge: f32,
    pub titlebar_height: f32,
    pub titlebar_padding_x: f32,
}

/// What a skin may restate of [`WindowSkin`].
#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Serialize)]
#[serde(default, deny_unknown_fields)]
#[non_exhaustive]
pub struct WindowPatch {
    pub close_framed: Option<WindowControlSkin>,
    pub close_micro: Option<WindowControlSkin>,
    pub close_wide: Option<WindowControlSkin>,
    pub compact: Option<WindowControlSkin>,
    pub icon_color: Option<ColorRole>,
    pub icon_hover_color: Option<ColorRole>,
    pub icon_stroke_width: Option<f32>,
    pub resize_edge: Option<f32>,
    pub standard: Option<WindowControlSkin>,
    pub titlebar_height: Option<f32>,
    pub titlebar_padding_x: Option<f32>,
    pub titlebar_text: Option<TextRoleSkin>,
}

impl WindowSkin {
    /// Takes every field the patch restates, keeping the rest.
    pub(crate) fn patch(&mut self, patch: WindowPatch) {
        super::patch::patch_field(&mut self.icon_color, patch.icon_color);
        super::patch::patch_field(&mut self.icon_hover_color, patch.icon_hover_color);
        super::patch::patch_field(&mut self.titlebar_text, patch.titlebar_text);
        super::patch::patch_field(&mut self.standard, patch.standard);
        super::patch::patch_field(&mut self.compact, patch.compact);
        super::patch::patch_field(&mut self.close_wide, patch.close_wide);
        super::patch::patch_field(&mut self.close_micro, patch.close_micro);
        super::patch::patch_field(&mut self.close_framed, patch.close_framed);
        super::patch::patch_field(&mut self.icon_stroke_width, patch.icon_stroke_width);
        super::patch::patch_field(&mut self.resize_edge, patch.resize_edge);
        super::patch::patch_field(&mut self.titlebar_height, patch.titlebar_height);
        super::patch::patch_field(&mut self.titlebar_padding_x, patch.titlebar_padding_x);
    }
}

/// How one window-controls style draws: a row of buttons, or a single close
/// cell.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub enum WindowControlSkin {
    Buttons {
        minus_icon_size: f32,
        maximize_icon_size: f32,
        close_icon_size: f32,
        gap: f32,
        padding: f32,
    },
    Close {
        cell_size: f32,
        icon_size: f32,
        frame: Option<FrameSkin>,
        divider: Option<(f32, ColorRole)>,
    },
}

impl WindowSkin {
    pub(crate) const fn controls(self, style: WindowControlsStyle) -> WindowControlSkin {
        match style {
            WindowControlsStyle::Standard => self.standard,
            WindowControlsStyle::Compact => self.compact,
            WindowControlsStyle::CloseWide => self.close_wide,
            WindowControlsStyle::CloseMicro => self.close_micro,
            WindowControlsStyle::CloseFramed => self.close_framed,
        }
    }
}
