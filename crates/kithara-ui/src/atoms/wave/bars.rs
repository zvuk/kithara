use crate::{
    draw::{DrawListBuilder, Rect, Rgba},
    render::WaveBucket,
    skin::WaveSkin,
};

/// Column pitch: one bar plus the gap after it.
pub(crate) fn step(metrics: WaveSkin) -> f32 {
    metrics.bar_width + metrics.bar_gap
}

/// Dim everything left of the playhead. `played_width` is measured in logical
/// pixels within `bounds`.
pub(crate) fn draw_played(
    list: &mut DrawListBuilder,
    bounds: Rect,
    played_width: f32,
    alpha: f32,
    color: Rgba,
) {
    list.fill_rect(
        Rect {
            h: bounds.h,
            w: played_width.clamp(0.0, bounds.w),
            x: bounds.x,
            y: bounds.y,
        },
        Rgba { a: alpha, ..color },
    );
}

/// One column of the waveform: the three bands share a width and nest by
/// level, each drawn from the vertical centre over the previous one.
pub(crate) fn draw_column(
    list: &mut DrawListBuilder,
    bounds: Rect,
    center_x: f32,
    bucket: WaveBucket,
    available_height: f32,
    metrics: WaveSkin,
    colors: [Rgba; 3],
) {
    for (level, color) in [bucket.low, bucket.mid, bucket.high]
        .into_iter()
        .zip(colors)
    {
        let height = level.clamp(0.0, 1.0) * available_height;
        if height <= 0.0 {
            continue;
        }
        list.fill_rect(
            Rect {
                h: height,
                w: metrics.bar_width,
                x: center_x - metrics.bar_width / 2.0,
                y: bounds.y + (bounds.h - height) / 2.0,
            },
            color,
        );
    }
}
