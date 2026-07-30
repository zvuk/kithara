use serde::{Deserialize, Serialize};

use super::document::{ColorRole, FontFamily, FontWeight};
use crate::module::WindowControlsStyle;

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
pub struct TextRoleSkin {
    pub font: FontFamily,
    pub weight: FontWeight,
    pub size: f32,
    pub spacing: f32,
    pub color: ColorRole,
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
pub struct ShadowSkin {
    pub color: ColorRole,
    pub alpha: f32,
    pub offset_x: f32,
    pub offset_y: f32,
    pub blur: f32,
}

/// Scale beside a fader: hairlines with a longer, brighter one at centre.
/// `thickness` runs along the scale, `length` across it, whatever the axis.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct TickSkin {
    pub count: usize,
    pub thickness: f32,
    pub length: f32,
    pub center_length: f32,
    pub gap: f32,
    pub inset: f32,
    pub color: ColorRole,
    pub center_color: ColorRole,
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
    pub header_frame: FrameSkin,
    pub chip_frame: FrameSkin,
    pub title_frame: FrameSkin,
    pub chevron_frame: FrameSkin,
    pub footer_frame: FrameSkin,
    pub header_height: f32,
    pub chip_pad: f32,
    pub chip_text_size: f32,
    pub title_text_size: f32,
    pub chevron_size: f32,
    pub chevron_icon_size: f32,
    pub chevron_stroke_width: f32,
    pub footer_height: f32,
    pub footer_pad: f32,
    pub footer_text_size: f32,
    pub inner_line_width: f32,
    pub panel_background: ColorRole,
    pub header_background: ColorRole,
    pub title_background: ColorRole,
    pub chip_background: ColorRole,
    pub chip_text: ColorRole,
    pub title_text: ColorRole,
    pub chevron_color: ColorRole,
    pub footer_background: ColorRole,
    pub footer_text: ColorRole,
    pub inner_line: ColorRole,
    pub corner_size: f32,
    pub corner_width: f32,
    pub corner_offset: f32,
    pub corner_color: ColorRole,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct WindowSkin {
    pub titlebar_height: f32,
    /// Thickness of the drag zones framing a window that draws its own chrome.
    pub resize_edge: f32,
    pub titlebar_padding_x: f32,
    pub titlebar_text: TextRoleSkin,
    pub icon_color: ColorRole,
    pub icon_hover_color: ColorRole,
    pub icon_stroke_width: f32,
    pub standard_minus_icon_size: f32,
    pub standard_maximize_icon_size: f32,
    pub standard_close_icon_size: f32,
    pub standard_gap: f32,
    pub standard_padding: f32,
    pub compact_minus_icon_size: f32,
    pub compact_maximize_icon_size: f32,
    pub compact_close_icon_size: f32,
    pub compact_gap: f32,
    pub compact_padding: f32,
    pub close_wide_cell_size: f32,
    pub close_wide_icon_size: f32,
    pub close_wide_divider_width: f32,
    pub close_wide_divider_color: ColorRole,
    pub close_micro_cell_size: f32,
    pub close_micro_icon_size: f32,
    pub close_framed_cell_size: f32,
    pub close_framed_icon_size: f32,
    pub close_framed_frame: FrameSkin,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum WindowControlSkin {
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
            WindowControlsStyle::Standard => WindowControlSkin::Buttons {
                minus_icon_size: self.standard_minus_icon_size,
                maximize_icon_size: self.standard_maximize_icon_size,
                close_icon_size: self.standard_close_icon_size,
                gap: self.standard_gap,
                padding: self.standard_padding,
            },
            WindowControlsStyle::Compact => WindowControlSkin::Buttons {
                minus_icon_size: self.compact_minus_icon_size,
                maximize_icon_size: self.compact_maximize_icon_size,
                close_icon_size: self.compact_close_icon_size,
                gap: self.compact_gap,
                padding: self.compact_padding,
            },
            WindowControlsStyle::CloseWide => WindowControlSkin::Close {
                cell_size: self.close_wide_cell_size,
                icon_size: self.close_wide_icon_size,
                frame: None,
                divider: Some((self.close_wide_divider_width, self.close_wide_divider_color)),
            },
            WindowControlsStyle::CloseMicro => WindowControlSkin::Close {
                cell_size: self.close_micro_cell_size,
                icon_size: self.close_micro_icon_size,
                frame: None,
                divider: None,
            },
            WindowControlsStyle::CloseFramed => WindowControlSkin::Close {
                cell_size: self.close_framed_cell_size,
                icon_size: self.close_framed_icon_size,
                frame: Some(self.close_framed_frame),
                divider: None,
            },
        }
    }
}
