use std::ops::Range;

use num_traits::cast::AsPrimitive;

use super::{
    bars,
    paint::WavePalette,
    zoom_math::{
        bar_bucket_range, bar_grid, max_bucket, norm_to_x, visible_mark_range, visible_marks,
        window_bounds,
    },
};
use crate::{
    draw::{DrawListBuilder, Pt, Rect, Rgba, Transform},
    render::WaveBucket,
    skin::{ColorRole, FontFamily, TextRoleSkin, WaveSkin},
    text::TextContext,
};

#[derive(Clone, Copy)]
pub(crate) struct HeroPalette {
    pub(crate) cue_badge: Rgba,
    pub(crate) cue_text: Rgba,
    pub(crate) base: WavePalette,
}

#[derive(Clone, Copy)]
pub(crate) struct HeroWave<'a> {
    pub(crate) buckets: &'a [WaveBucket],
    pub(crate) beats: &'a [f32],
    pub(crate) cues: &'a [f32],
    pub(crate) downbeats: &'a [f32],
    pub(crate) loop_region: Option<[f32; 2]>,
    pub(crate) position: f32,
    pub(crate) zoom: f32,
}

pub(crate) fn draw(
    list: &mut DrawListBuilder,
    text: &mut TextContext,
    bounds: Rect,
    data: HeroWave<'_>,
    metrics: WaveSkin,
    palette: HeroPalette,
) {
    let window = window_bounds(data.position, data.zoom);
    draw_bars(
        list,
        bounds,
        data.buckets,
        data.zoom,
        &window,
        metrics,
        palette.base,
    );
    draw_grid(list, bounds, data, &window, metrics, palette.base);
    if let Some(region) = data.loop_region {
        draw_loop(list, bounds, region, &window, metrics, palette.base);
    }
    bars::draw_played(
        list,
        bounds,
        norm_to_x(data.position.clamp(0.0, 1.0), &window, bounds.w),
        metrics.played_alpha,
        palette.base.bg_deep,
    );
    draw_cues(list, text, bounds, data.cues, &window, metrics, palette);
    draw_playhead(list, bounds, data.position, &window, metrics, palette.base);
}

fn draw_bars(
    list: &mut DrawListBuilder,
    bounds: Rect,
    buckets: &[WaveBucket],
    zoom: f32,
    window: &Range<f32>,
    metrics: WaveSkin,
    palette: WavePalette,
) {
    let step = bars::step(metrics);
    let content_width = (bounds.w - metrics.content_inset * 2.0).max(0.0);
    let columns: usize = ((content_width + metrics.bar_gap) / step).floor().as_();
    let Some(grid) = bar_grid(columns, zoom, window) else {
        return;
    };
    let available_height = (bounds.h - metrics.content_inset * 2.0).max(0.0);
    for bar in grid.first..grid.last {
        let range = bar_bucket_range(bar, grid.norm_width, buckets.len());
        let Some(bucket) = max_bucket(buckets, range) else {
            continue;
        };
        let bar_f: f32 = bar.as_();
        let center_x = bounds.x + norm_to_x((bar_f + 0.5) * grid.norm_width, window, bounds.w);
        bars::draw_column(
            list,
            bounds,
            center_x,
            bucket,
            available_height,
            metrics,
            [palette.wave_low, palette.wave_mid, palette.wave_high],
        );
    }
}

fn draw_grid(
    list: &mut DrawListBuilder,
    bounds: Rect,
    data: HeroWave<'_>,
    window: &Range<f32>,
    metrics: WaveSkin,
    palette: WavePalette,
) {
    draw_marks(
        list,
        bounds,
        visible_marks(data.beats, window),
        window,
        with_alpha(palette.line, metrics.grid_alpha),
        metrics.grid_width,
    );
    draw_marks(
        list,
        bounds,
        visible_marks(data.downbeats, window),
        window,
        with_alpha(palette.text_dim, metrics.downbeat_alpha),
        metrics.grid_width,
    );
}

fn draw_marks(
    list: &mut DrawListBuilder,
    bounds: Rect,
    marks: &[f32],
    window: &Range<f32>,
    color: Rgba,
    width: f32,
) {
    for &mark in marks {
        let x = bounds.x + norm_to_x(mark, window, bounds.w);
        list.stroke_line(
            Pt { x, y: bounds.y },
            Pt {
                x,
                y: bounds.y + bounds.h,
            },
            color,
            width,
        );
    }
}

fn draw_loop(
    list: &mut DrawListBuilder,
    bounds: Rect,
    region: [f32; 2],
    window: &Range<f32>,
    metrics: WaveSkin,
    palette: WavePalette,
) {
    let start = region[0].min(region[1]).clamp(0.0, 1.0);
    let end = region[0].max(region[1]).clamp(0.0, 1.0);
    let start_x = norm_to_x(start, window, bounds.w);
    let end_x = norm_to_x(end, window, bounds.w);
    let visible_start = start_x.clamp(0.0, bounds.w);
    let visible_end = end_x.clamp(0.0, bounds.w);
    if visible_end > visible_start {
        list.fill_rect(
            Rect {
                h: bounds.h,
                w: visible_end - visible_start,
                x: bounds.x + visible_start,
                y: bounds.y,
            },
            with_alpha(palette.accent, metrics.loop_fill_alpha),
        );
    }
    for x in [start_x, end_x] {
        if (0.0..=bounds.w).contains(&x) {
            let x = bounds.x + x;
            list.stroke_line(
                Pt { x, y: bounds.y },
                Pt {
                    x,
                    y: bounds.y + bounds.h,
                },
                palette.accent,
                metrics.loop_bound_width,
            );
        }
    }
}

fn draw_cues(
    list: &mut DrawListBuilder,
    text: &mut TextContext,
    bounds: Rect,
    cues: &[f32],
    window: &Range<f32>,
    metrics: WaveSkin,
    palette: HeroPalette,
) {
    for index in visible_mark_range(cues, window) {
        let x = bounds.x + norm_to_x(cues[index], window, bounds.w);
        list.stroke_line(
            Pt { x, y: bounds.y },
            Pt {
                x,
                y: bounds.y + bounds.h,
            },
            palette.base.wave_high,
            metrics.cue_line_width,
        );
        let badge = Rect {
            h: metrics.cue_badge_size,
            w: metrics.cue_badge_size,
            x: x - metrics.cue_badge_size / 2.0,
            y: bounds.y,
        };
        list.fill_rect(badge, palette.cue_badge);
        draw_cue_text(list, text, badge, index + 1, metrics, palette.cue_text);
    }
}

fn draw_cue_text(
    list: &mut DrawListBuilder,
    text: &mut TextContext,
    bounds: Rect,
    index: usize,
    metrics: WaveSkin,
    color: Rgba,
) {
    let content = index.to_string();
    let run = text.shape(
        &content,
        TextRoleSkin {
            color: ColorRole::Text,
            font: FontFamily::Mono,
            size: metrics.cue_badge_text.size,
            spacing: 0.0,
            weight: metrics.cue_badge_text.weight,
        },
        Some(bounds.w),
    );
    list.text(
        &run,
        &content,
        Transform::translate(Pt {
            x: bounds.x + (bounds.w - run.width()) / 2.0,
            y: bounds.y + (bounds.h - run.height()) / 2.0,
        }),
        color,
    );
}

fn draw_playhead(
    list: &mut DrawListBuilder,
    bounds: Rect,
    position: f32,
    window: &Range<f32>,
    metrics: WaveSkin,
    palette: WavePalette,
) {
    let x = bounds.x + norm_to_x(position.clamp(0.0, 1.0), window, bounds.w);
    list.stroke_line(
        Pt { x, y: bounds.y },
        Pt {
            x,
            y: bounds.y + bounds.h,
        },
        palette.accent,
        metrics.playhead_width,
    );
    let marker_x = x - metrics.playhead_marker_width / 2.0;
    for y in [
        bounds.y,
        bounds.y + (bounds.h - metrics.playhead_marker_height).max(0.0),
    ] {
        list.fill_rect(
            Rect {
                h: metrics.playhead_marker_height,
                w: metrics.playhead_marker_width,
                x: marker_x,
                y,
            },
            palette.accent,
        );
    }
}

const fn with_alpha(color: Rgba, alpha: f32) -> Rgba {
    Rgba { a: alpha, ..color }
}
