use std::ops::Range;

use iced::{
    Color, Point, Rectangle, Size,
    alignment::Vertical,
    widget::{
        canvas,
        canvas::{Frame, Path, Stroke},
        text,
    },
};
use num_traits::cast::AsPrimitive;

use super::zoom_math::{
    column_bucket_range, max_bucket, norm_to_x, visible_mark_range, visible_marks, window_bounds,
};
use crate::{
    render::{WaveBucket, fonts, theme::RenderPalette},
    skin::WaveSkin,
};

#[derive(Clone, Copy)]
pub(crate) struct HeroPalette {
    pub(crate) base: RenderPalette,
    pub(crate) cue_badge: Color,
    pub(crate) cue_text: Color,
}

#[derive(Clone, Copy)]
pub(crate) struct HeroWave<'a> {
    pub(crate) beats: &'a [f32],
    pub(crate) buckets: &'a [WaveBucket],
    pub(crate) cues: &'a [f32],
    pub(crate) downbeats: &'a [f32],
    pub(crate) loop_region: Option<[f32; 2]>,
    pub(crate) position: f32,
    pub(crate) zoom: f32,
}

pub(crate) fn draw(
    frame: &mut Frame,
    bounds: Rectangle,
    data: HeroWave<'_>,
    metrics: WaveSkin,
    palette: HeroPalette,
) {
    let window = window_bounds(data.position, data.zoom);
    draw_bars(frame, bounds, data.buckets, &window, metrics, palette.base);
    draw_grid(frame, bounds, data, &window, metrics, palette.base);
    if let Some(region) = data.loop_region {
        draw_loop(frame, bounds, region, &window, metrics, palette.base);
    }
    draw_played(frame, bounds, data.position, &window, metrics, palette.base);
    draw_cues(frame, bounds, data.cues, &window, metrics, palette);
    draw_playhead(frame, bounds, data.position, &window, metrics, palette.base);
}

fn draw_bars(
    frame: &mut Frame,
    bounds: Rectangle,
    buckets: &[WaveBucket],
    window: &Range<f32>,
    metrics: WaveSkin,
    palette: RenderPalette,
) {
    let step = metrics.low_bar_width + metrics.bar_gap;
    let content_width = (bounds.width - metrics.content_inset * 2.0).max(0.0);
    let columns: usize = ((content_width + metrics.bar_gap) / step).floor().as_();
    let available_height = (bounds.height - metrics.content_inset * 2.0).max(0.0);
    for column in 0..columns {
        let range = column_bucket_range(column, columns, buckets.len(), window);
        let Some(bucket) = max_bucket(buckets, range) else {
            continue;
        };
        let column_x: f32 = column.as_();
        let center_x = metrics.content_inset + column_x * step + metrics.low_bar_width / 2.0;
        for (level, width, color) in [
            (bucket.low, metrics.low_bar_width, palette.wave_low),
            (bucket.mid, metrics.mid_bar_width, palette.wave_mid),
            (bucket.high, metrics.high_bar_width, palette.wave_high),
        ] {
            draw_band(
                frame,
                bounds,
                center_x,
                level,
                available_height,
                width,
                color,
            );
        }
    }
}

fn draw_band(
    frame: &mut Frame,
    bounds: Rectangle,
    center_x: f32,
    level: f32,
    available_height: f32,
    width: f32,
    color: Color,
) {
    let height = level.clamp(0.0, 1.0) * available_height;
    if height > 0.0 {
        frame.fill_rectangle(
            Point::new(center_x - width / 2.0, (bounds.height - height) / 2.0),
            Size::new(width, height),
            color,
        );
    }
}

fn draw_grid(
    frame: &mut Frame,
    bounds: Rectangle,
    data: HeroWave<'_>,
    window: &Range<f32>,
    metrics: WaveSkin,
    palette: RenderPalette,
) {
    draw_marks(
        frame,
        bounds,
        visible_marks(data.beats, window),
        window,
        with_alpha(palette.line, metrics.grid_alpha),
        metrics.grid_width,
    );
    draw_marks(
        frame,
        bounds,
        visible_marks(data.downbeats, window),
        window,
        with_alpha(palette.text_dim, metrics.downbeat_alpha),
        metrics.grid_width,
    );
}

fn draw_marks(
    frame: &mut Frame,
    bounds: Rectangle,
    marks: &[f32],
    window: &Range<f32>,
    color: Color,
    width: f32,
) {
    for &mark in marks {
        let x = norm_to_x(mark, window, bounds.width);
        frame.stroke(
            &Path::line(Point::new(x, 0.0), Point::new(x, bounds.height)),
            Stroke::default().with_color(color).with_width(width),
        );
    }
}

fn draw_loop(
    frame: &mut Frame,
    bounds: Rectangle,
    region: [f32; 2],
    window: &Range<f32>,
    metrics: WaveSkin,
    palette: RenderPalette,
) {
    let start = region[0].min(region[1]).clamp(0.0, 1.0);
    let end = region[0].max(region[1]).clamp(0.0, 1.0);
    let start_x = norm_to_x(start, window, bounds.width);
    let end_x = norm_to_x(end, window, bounds.width);
    let visible_start = start_x.clamp(0.0, bounds.width);
    let visible_end = end_x.clamp(0.0, bounds.width);
    if visible_end > visible_start {
        frame.fill_rectangle(
            Point::new(visible_start, 0.0),
            Size::new(visible_end - visible_start, bounds.height),
            with_alpha(palette.accent, metrics.loop_fill_alpha),
        );
    }
    for x in [start_x, end_x] {
        if (0.0..=bounds.width).contains(&x) {
            frame.stroke(
                &Path::line(Point::new(x, 0.0), Point::new(x, bounds.height)),
                Stroke::default()
                    .with_color(palette.accent)
                    .with_width(metrics.loop_bound_width),
            );
        }
    }
}

fn draw_played(
    frame: &mut Frame,
    bounds: Rectangle,
    position: f32,
    window: &Range<f32>,
    metrics: WaveSkin,
    palette: RenderPalette,
) {
    let width = norm_to_x(position.clamp(0.0, 1.0), window, bounds.width).clamp(0.0, bounds.width);
    frame.fill_rectangle(
        Point::ORIGIN,
        Size::new(width, bounds.height),
        with_alpha(palette.bg_deep, metrics.played_alpha),
    );
}

fn draw_cues(
    frame: &mut Frame,
    bounds: Rectangle,
    cues: &[f32],
    window: &Range<f32>,
    metrics: WaveSkin,
    palette: HeroPalette,
) {
    for index in visible_mark_range(cues, window) {
        let x = norm_to_x(cues[index], window, bounds.width);
        frame.stroke(
            &Path::line(Point::new(x, 0.0), Point::new(x, bounds.height)),
            Stroke::default()
                .with_color(palette.base.wave_high)
                .with_width(metrics.cue_line_width),
        );
        let badge = Rectangle::new(
            Point::new(x - metrics.cue_badge_size / 2.0, 0.0),
            Size::new(metrics.cue_badge_size, metrics.cue_badge_size),
        );
        frame.fill_rectangle(badge.position(), badge.size(), palette.cue_badge);
        frame.fill_text(canvas::Text {
            content: (index + 1).to_string(),
            position: badge.center(),
            max_width: badge.width,
            color: palette.cue_text,
            size: metrics.cue_badge_text.size.into(),
            font: fonts::mono(metrics.cue_badge_text.weight),
            align_x: text::Alignment::Center,
            align_y: Vertical::Center,
            shaping: text::Shaping::Advanced,
            ..canvas::Text::default()
        });
    }
}

fn draw_playhead(
    frame: &mut Frame,
    bounds: Rectangle,
    position: f32,
    window: &Range<f32>,
    metrics: WaveSkin,
    palette: RenderPalette,
) {
    let x = norm_to_x(position.clamp(0.0, 1.0), window, bounds.width);
    frame.stroke(
        &Path::line(Point::new(x, 0.0), Point::new(x, bounds.height)),
        Stroke::default()
            .with_color(palette.accent)
            .with_width(metrics.playhead_width),
    );
    let marker_x = x - metrics.playhead_marker_width / 2.0;
    for y in [
        0.0,
        (bounds.height - metrics.playhead_marker_height).max(0.0),
    ] {
        frame.fill_rectangle(
            Point::new(marker_x, y),
            Size::new(
                metrics.playhead_marker_width,
                metrics.playhead_marker_height,
            ),
            palette.accent,
        );
    }
}

fn with_alpha(color: Color, alpha: f32) -> Color {
    Color { a: alpha, ..color }
}
