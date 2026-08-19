use num_traits::cast::AsPrimitive;

use crate::{
    atoms::design::crossfader::{TickAxis, TickRail},
    draw::{DrawListBuilder, Rect, Rgba},
    render::{Skin, StereoLevels, theme::RenderPalette},
    skin::VuVerticalSkin,
};

pub(crate) struct VerticalVu {
    metrics: VuVerticalSkin,
    palette: RenderPalette,
    thumb_color: Rgba,
    thumb_notch_color: Rgba,
    ticks: Option<TickRail>,
}

impl VerticalVu {
    pub(crate) fn new(ticks: bool, skin: &Skin) -> Self {
        Self {
            metrics: skin.vu_vertical,
            palette: skin.palette,
            thumb_color: skin.rgba(skin.vu_vertical.thumb_color),
            thumb_notch_color: skin.rgba(skin.vu_vertical.thumb_notch_color),
            ticks: ticks.then(|| TickRail::new(TickAxis::Vertical, skin.vu_vertical.ticks, skin)),
        }
    }

    pub(crate) fn paint(&self, list: &mut DrawListBuilder, levels: StereoLevels, bounds: Rect) {
        let fader = if self.ticks.is_some() {
            fader_bounds(bounds, self.metrics.fader_width)
        } else {
            bounds
        };
        if let Some(rail) = &self.ticks {
            rail.paint(
                list,
                tick_rail_bounds(bounds, fader.x, self.metrics.ticks.gap),
            );
        }
        self.paint_segments(list, fader, levels);
        self.paint_thumb(list, fader, levels.volume);
    }

    fn paint_segments(&self, list: &mut DrawListBuilder, bounds: Rect, levels: StereoLevels) {
        let step = self.metrics.segment_height + self.metrics.segment_gap;
        let count = ((bounds.h + self.metrics.segment_gap) / step).floor();
        if count <= 0.0 {
            return;
        }

        let count_usize: usize = count.as_();
        let level = (levels.l.max(levels.r) * levels.volume).clamp(0.0, 1.0);
        let lit = (level * count).round();
        let width = (bounds.w - self.metrics.segment_inset_x * 2.0).max(0.0);
        for index in 0..count_usize {
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
                    w: width,
                    x: bounds.x + self.metrics.segment_inset_x,
                    y: bounds.y + bounds.h - self.metrics.segment_height - index * step,
                },
                color,
            );
        }
    }

    fn paint_thumb(&self, list: &mut DrawListBuilder, bounds: Rect, volume: f32) {
        let travel = (bounds.h - self.metrics.thumb_height).max(0.0);
        let y = bounds.y + ((1.0 - volume.clamp(0.0, 1.0)) * travel).round();
        list.fill_rect(
            Rect {
                h: self.metrics.thumb_height,
                w: bounds.w,
                x: bounds.x,
                y,
            },
            self.thumb_color,
        );
        if self.metrics.thumb_notch_offset < self.metrics.thumb_height {
            list.fill_rect(
                Rect {
                    h: self.metrics.thumb_notch_height,
                    w: bounds.w,
                    x: bounds.x,
                    y: y + self.metrics.thumb_notch_offset,
                },
                self.thumb_notch_color,
            );
        }
    }
}

fn fader_bounds(bounds: Rect, width: f32) -> Rect {
    let width = width.clamp(0.0, bounds.w);
    Rect {
        w: width,
        x: bounds.x + bounds.w - width,
        ..bounds
    }
}

fn tick_rail_bounds(bounds: Rect, fader_x: f32, gap: f32) -> Rect {
    let right = (fader_x - gap).max(bounds.x);
    Rect {
        w: right - bounds.x,
        ..bounds
    }
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::*;

    #[kithara::test]
    fn the_fader_keeps_its_width_and_yields_the_rest_to_the_ticks() {
        let bare = fader_bounds(
            Rect {
                h: 0.0,
                w: 18.0,
                x: 3.0,
                y: 0.0,
            },
            18.0,
        );
        let with_ticks = fader_bounds(
            Rect {
                h: 0.0,
                w: 38.0,
                x: 3.0,
                y: 0.0,
            },
            18.0,
        );

        assert_eq!(bare.x, 3.0);
        assert_eq!(bare.w, 18.0);
        assert_eq!(with_ticks.x, 23.0);
        assert_eq!(with_ticks.w, 18.0);
    }
}
