use iced::{Color, Point, Rectangle, Size, widget::canvas::Frame};

use crate::{
    render::{WaveBucket, theme::RenderPalette},
    skin::WaveSkin,
};

/// Column pitch: one bar plus the gap after it.
pub(crate) fn step(metrics: WaveSkin) -> f32 {
    metrics.bar_width + metrics.bar_gap
}

/// Dim everything left of the playhead. `played_x` is the playhead in canvas
/// pixels, so callers own the norm-to-pixel mapping their window implies.
pub(crate) fn draw_played(
    frame: &mut Frame,
    bounds: Rectangle,
    played_x: f32,
    alpha: f32,
    palette: RenderPalette,
) {
    frame.fill_rectangle(
        Point::ORIGIN,
        Size::new(played_x.clamp(0.0, bounds.width), bounds.height),
        Color {
            a: alpha,
            ..palette.bg_deep
        },
    );
}

/// One column of the waveform: the three bands share a width and nest by
/// level, each drawn from the vertical centre over the previous one.
pub(crate) fn draw_column(
    frame: &mut Frame,
    bounds: Rectangle,
    center_x: f32,
    bucket: WaveBucket,
    available_height: f32,
    metrics: WaveSkin,
    palette: RenderPalette,
) {
    for (level, color) in [
        (bucket.low, palette.wave_low),
        (bucket.mid, palette.wave_mid),
        (bucket.high, palette.wave_high),
    ] {
        let height = level.clamp(0.0, 1.0) * available_height;
        if height <= 0.0 {
            continue;
        }
        frame.fill_rectangle(
            Point::new(
                center_x - metrics.bar_width / 2.0,
                (bounds.height - height) / 2.0,
            ),
            Size::new(metrics.bar_width, height),
            color,
        );
    }
}
