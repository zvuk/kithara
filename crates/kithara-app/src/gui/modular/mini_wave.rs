use iced::{
    Color, Element, Event, Length, Point, Rectangle, Renderer, Size, Theme,
    mouse::{self, Button, Cursor},
    widget::canvas::{self, Action, Canvas, Frame, Geometry, Path, Stroke},
};
use num_traits::cast::AsPrimitive;

use super::{ControlAction, ModularMsg};
use crate::{
    gui::message::Message,
    theme::gui::{GuiPalette, WAVE_HIGH, WAVE_LOW, WAVE_MID},
    waveform::TrackAnalysis,
};

pub(super) struct MiniWave<'a> {
    palette: GuiPalette,
    analysis: Option<&'a TrackAnalysis>,
    path: String,
    progress: f32,
}

impl canvas::Program<Message> for MiniWave<'_> {
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
        let Some(waveform) = self.analysis.and_then(TrackAnalysis::waveform) else {
            return vec![frame.into_geometry()];
        };
        let buckets = waveform.buckets();
        let columns: usize = bounds.width.ceil().as_();
        if buckets.is_empty() || columns == 0 || bounds.height <= 0.0 {
            return vec![frame.into_geometry()];
        }

        let middle = bounds.height / 2.0;
        let amplitude = (middle - 2.0).max(0.0);
        let head_x = self.progress.clamp(0.0, 1.0) * bounds.width;
        for column in 0..columns {
            let start = column * buckets.len() / columns;
            let end = ((column + 1) * buckets.len() / columns)
                .max(start + 1)
                .min(buckets.len());
            let mut peaks = [0.0_f32; 3];
            for bucket in &buckets[start..end] {
                peaks[0] = peaks[0].max(bucket.low());
                peaks[1] = peaks[1].max(bucket.mid());
                peaks[2] = peaks[2].max(bucket.high());
            }
            let x: f32 = column.as_();
            for (peak, color) in peaks.into_iter().zip([WAVE_LOW, WAVE_MID, WAVE_HIGH]) {
                if peak <= 0.0 {
                    continue;
                }
                let half = peak.clamp(0.0, 1.0) * amplitude;
                frame.fill_rectangle(
                    Point::new(x, middle - half),
                    Size::new(1.0, half * 2.0),
                    if x <= head_x { dim(color) } else { color },
                );
            }
        }

        frame.stroke(
            &Path::line(Point::new(head_x, 0.0), Point::new(head_x, bounds.height)),
            Stroke::default()
                .with_color(self.palette.accent)
                .with_width(1.0),
        );
        vec![frame.into_geometry()]
    }

    fn mouse_interaction(
        &self,
        _state: &(),
        bounds: Rectangle,
        cursor: Cursor,
    ) -> mouse::Interaction {
        if cursor.is_over(bounds) {
            mouse::Interaction::Pointer
        } else {
            mouse::Interaction::default()
        }
    }

    fn update(
        &self,
        _state: &mut (),
        event: &Event,
        bounds: Rectangle,
        cursor: Cursor,
    ) -> Option<Action<Message>> {
        let has_waveform = self
            .analysis
            .and_then(TrackAnalysis::waveform)
            .is_some_and(|waveform| !waveform.is_empty());
        if !has_waveform || bounds.width <= 0.0 {
            return None;
        }
        match event {
            Event::Mouse(mouse::Event::ButtonPressed(Button::Left)) if cursor.is_over(bounds) => {
                cursor.position_over(bounds).map(|position| {
                    let value = (position.x / bounds.width).clamp(0.0, 1.0);
                    Action::publish(Message::Modular(ModularMsg::Control {
                        path: self.path.clone(),
                        action: ControlAction::SetScalar(f64::from(value)),
                    }))
                    .and_capture()
                })
            }
            _ => None,
        }
    }
}

pub(super) fn view<'a>(
    analysis: Option<&'a TrackAnalysis>,
    progress: f32,
    palette: GuiPalette,
    path: String,
) -> Element<'a, Message> {
    const HEIGHT: f32 = 28.0;

    Canvas::new(MiniWave {
        palette,
        analysis,
        path,
        progress,
    })
    .width(Length::Fill)
    .height(Length::Fixed(HEIGHT))
    .into()
}

fn dim(color: Color) -> Color {
    Color {
        r: color.r * 0.42,
        g: color.g * 0.42,
        b: color.b * 0.42,
        a: color.a,
    }
}
