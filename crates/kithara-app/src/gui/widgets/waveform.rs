use iced::{
    Color, Element, Length, Point, Rectangle, Renderer, Theme,
    mouse::Cursor,
    widget::canvas::{self, Canvas, Frame, Geometry, Path, Stroke, gradient},
};
use kithara::audio::Envelope;

use crate::{gui::message::Message, theme::gui::GuiPalette};

/// Deck waveform: mirrored envelopes filled with played / unplayed
/// vertical gradients clipped by the playhead, a faint full-height 1/8
/// column grid, a 16-tick beat grid and a playhead. Display-only.
struct Waveform {
    samples: Envelope,
    progress: f32,
    p: GuiPalette,
}

/// Top stop of the unplayed envelope gradient: a desaturated blue with no
/// palette equivalent, kept local as a deliberate decorative tone. The
/// bottom stop reuses the `line` palette token.
const UNPLAYED_TOP: Color = Color::from_rgb(108.0 / 255.0, 111.0 / 255.0, 154.0 / 255.0);

fn envelope_path(samples: &[f32], w: f32, h: f32) -> Path {
    let mid = h / 2.0;
    let amp = mid - 4.0;
    #[expect(
        clippy::cast_precision_loss,
        reason = "envelope point count tracks display width and never nears the f32 mantissa limit"
    )]
    let step = if samples.len() > 1 {
        w / (samples.len() - 1) as f32
    } else {
        0.0
    };
    Path::new(|b| {
        let mut x = 0.0;
        for (i, &v) in samples.iter().enumerate() {
            let y = mid - v * amp;
            if i == 0 {
                b.move_to(Point::new(x, y));
            } else {
                b.line_to(Point::new(x, y));
            }
            x += step;
        }
        for &v in samples.iter().rev() {
            x -= step;
            let y = mid + v * amp * 0.85;
            b.line_to(Point::new(x, y));
        }
        b.close();
    })
}

impl canvas::Program<Message> for Waveform {
    type State = ();

    fn draw(
        &self,
        _state: &(),
        renderer: &Renderer,
        _theme: &Theme,
        bounds: Rectangle,
        _cursor: Cursor,
    ) -> Vec<Geometry> {
        let mut frame = Frame::new(renderer, bounds.size());
        let w = bounds.width;
        let h = bounds.height;
        if w <= 0.0 || h <= 0.0 {
            return vec![frame.into_geometry()];
        }

        let grid_color = Color {
            a: 0.08,
            ..self.p.accent
        };
        for i in 1u16..=8 {
            let x = (f32::from(i) / 8.0) * w;
            frame.stroke(
                &Path::line(Point::new(x, 0.0), Point::new(x, h)),
                Stroke::default().with_color(grid_color).with_width(1.0),
            );
        }

        let played_w = (self.progress.clamp(0.0, 1.0) * w).clamp(0.0, w);

        if self.samples.len() >= 2 {
            let path = envelope_path(&self.samples[..], w, h);

            let unplayed = gradient::Linear::new(Point::new(0.0, 0.0), Point::new(0.0, h))
                .add_stop(
                    0.0,
                    Color {
                        a: 0.55,
                        ..UNPLAYED_TOP
                    },
                )
                .add_stop(
                    1.0,
                    Color {
                        a: 0.65,
                        ..self.p.line
                    },
                );
            let played = gradient::Linear::new(Point::new(0.0, 0.0), Point::new(0.0, h))
                .add_stop(
                    0.0,
                    Color {
                        a: 0.95,
                        ..self.p.accent_strong
                    },
                )
                .add_stop(
                    1.0,
                    Color {
                        a: 0.70,
                        ..self.p.accent
                    },
                );

            frame.with_clip(
                Rectangle {
                    x: played_w,
                    y: 0.0,
                    width: (w - played_w).max(0.0),
                    height: h,
                },
                |f| f.fill(&path, unplayed),
            );
            frame.with_clip(
                Rectangle {
                    x: 0.0,
                    y: 0.0,
                    width: played_w,
                    height: h,
                },
                |f| f.fill(&path, played),
            );
        }

        let tick_color = Color {
            a: 0.4,
            ..self.p.accent
        };
        for i in 0u16..16 {
            let x = (f32::from(i) / 16.0) * w;
            frame.stroke(
                &Path::line(Point::new(x, h - 6.0), Point::new(x, h)),
                Stroke::default().with_color(tick_color).with_width(1.0),
            );
        }

        let px = self.progress.clamp(0.0, 1.0) * w;
        let head = Path::line(Point::new(px, 0.0), Point::new(px, h));
        frame.stroke(
            &head,
            Stroke::default().with_color(self.p.accent).with_width(2.0),
        );

        vec![frame.into_geometry()]
    }
}

/// Build the deck waveform element from a precomputed peak envelope.
pub(crate) fn waveform<'a>(
    peaks: Envelope,
    progress: f32,
    height: f32,
    p: GuiPalette,
) -> Element<'a, Message> {
    Canvas::new(Waveform {
        samples: peaks,
        progress,
        p,
    })
    .width(Length::Fill)
    .height(Length::Fixed(height))
    .into()
}
