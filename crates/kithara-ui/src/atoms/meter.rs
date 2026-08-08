use num_traits::cast::AsPrimitive;

use crate::{
    draw::{DrawListBuilder, Rect},
    render::{Skin, StereoLevels, theme::RenderPalette},
    skin::VuStereoSkin,
};

pub(crate) struct StereoMeter {
    metrics: VuStereoSkin,
    palette: RenderPalette,
}

impl StereoMeter {
    pub(crate) fn new(skin: &Skin) -> Self {
        Self {
            metrics: skin.vu_stereo,
            palette: skin.palette,
        }
    }

    pub(crate) fn paint(&self, list: &mut DrawListBuilder, levels: StereoLevels, bounds: Rect) {
        list.fill_rect(bounds, self.palette.bg_deep);

        for (level, y) in [levels.l, levels.r]
            .into_iter()
            .zip([self.metrics.channel_l_y, self.metrics.channel_r_y])
        {
            self.paint_channel(list, bounds, y, level);
        }

        list.fill_rect(
            Rect {
                h: bounds.h,
                w: self.metrics.carriage_width,
                x: bounds.x + levels.volume.clamp(0.0, 1.0) * bounds.w,
                y: bounds.y,
            },
            self.palette.accent,
        );
    }

    fn paint_channel(&self, list: &mut DrawListBuilder, bounds: Rect, y: f32, level: f32) {
        let count: f32 = self.metrics.segment_count.as_();
        let lit = (level.clamp(0.0, 1.0) * count).round();

        for index in 0..self.metrics.segment_count {
            let index: f32 = index.as_();
            let ratio = index / count;
            let color = if index >= lit {
                self.palette.bg_inset
            } else if ratio > self.metrics.danger_threshold {
                self.palette.danger
            } else if ratio > self.metrics.warning_threshold {
                self.palette.warning
            } else {
                self.palette.success
            };
            list.fill_rect(
                Rect {
                    h: self.metrics.segment_height,
                    w: self.metrics.segment_width,
                    x: bounds.x + index * (self.metrics.segment_width + self.metrics.segment_gap),
                    y: bounds.y + y,
                },
                color,
            );
        }
    }
}
