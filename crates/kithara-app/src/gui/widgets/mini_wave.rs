use iced::{
    Color, Element, Event, Length, Point, Rectangle, Renderer, Size, Theme,
    mouse::{self, Button, Cursor},
    widget::canvas::{self, Action, Canvas, Frame, Geometry, Path, Stroke},
};
use kithara_ui::render::{ControlAction, RenderPalette, UiEvent, WaveBucket, WaveformView};
use num_traits::cast::AsPrimitive;

use crate::gui::{
    message::Message,
    tokens::{chrome, waveform},
};

pub(crate) struct MiniWave {
    palette: RenderPalette,
    waveform: Option<WaveformData>,
    path: String,
    progress: f32,
    show_beats: bool,
}

struct WaveformData {
    buckets: Box<[WaveBucket]>,
    beats: Box<[f32]>,
    downbeats: Box<[f32]>,
}

impl canvas::Program<Message> for MiniWave {
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
        frame.fill_rectangle(Point::ORIGIN, bounds.size(), self.palette.bg_deep);

        if let Some(waveform) = &self.waveform {
            draw_bars(&mut frame, bounds, &waveform.buckets, self.palette);
        }
        if self.show_beats
            && let Some(waveform) = &self.waveform
        {
            draw_beat_grid(&mut frame, bounds, waveform, self.palette);
        }

        let head_x = self.progress.clamp(0.0, 1.0) * bounds.width;
        frame.stroke(
            &Path::line(Point::new(head_x, 0.0), Point::new(head_x, bounds.height)),
            Stroke::default()
                .with_color(self.palette.accent_strong)
                .with_width(waveform::PLAYHEAD_WIDTH),
        );
        draw_border(&mut frame, bounds, self.palette.line);
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
            .waveform
            .as_ref()
            .is_some_and(|waveform| !waveform.buckets.is_empty());
        if !has_waveform || bounds.width <= 0.0 {
            return None;
        }
        match event {
            Event::Mouse(mouse::Event::ButtonPressed(Button::Left)) if cursor.is_over(bounds) => {
                cursor.position_over(bounds).map(|position| {
                    let value = (position.x / bounds.width).clamp(0.0, 1.0);
                    Action::publish(Message::Modular(UiEvent::Control {
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

pub(crate) fn view(
    waveform: Option<WaveformView<'_>>,
    progress: f32,
    palette: RenderPalette,
    path: String,
    height: Length,
    show_beats: bool,
) -> Element<'static, Message> {
    let waveform = waveform.map(|view| WaveformData {
        buckets: view.buckets.to_vec().into_boxed_slice(),
        beats: view.beats.to_vec().into_boxed_slice(),
        downbeats: view.downbeats.to_vec().into_boxed_slice(),
    });
    Canvas::new(MiniWave {
        palette,
        waveform,
        path,
        progress,
        show_beats,
    })
    .width(Length::Fill)
    .height(height)
    .into()
}

fn draw_bars(frame: &mut Frame, bounds: Rectangle, buckets: &[WaveBucket], palette: RenderPalette) {
    let step = waveform::LOW_BAR_WIDTH + waveform::BAR_GAP;
    let content_width = (bounds.width - waveform::CONTENT_INSET * 2.0).max(0.0);
    let max_columns: usize = ((content_width + waveform::BAR_GAP) / step).floor().as_();
    let columns = max_columns.min(buckets.len());
    if columns == 0 {
        return;
    }

    let available_height = (bounds.height - waveform::CONTENT_INSET * 2.0).max(0.0);
    for column in 0..columns {
        let start = column * buckets.len() / columns;
        let end = ((column + 1) * buckets.len() / columns)
            .max(start + 1)
            .min(buckets.len());
        let (low, mid, high) = buckets[start..end].iter().fold(
            (0.0_f32, 0.0_f32, 0.0_f32),
            |(low, mid, high), bucket| {
                (
                    low.max(bucket.low),
                    mid.max(bucket.mid),
                    high.max(bucket.high),
                )
            },
        );
        let column_x: f32 = column.as_();
        let center_x = waveform::CONTENT_INSET + column_x * step + waveform::LOW_BAR_WIDTH / 2.0;
        draw_band(
            frame,
            bounds,
            center_x,
            low,
            available_height,
            waveform::LOW_BAR_WIDTH,
            palette.wave_low,
        );
        draw_band(
            frame,
            bounds,
            center_x,
            mid,
            available_height,
            waveform::MID_BAR_WIDTH,
            palette.wave_mid,
        );
        draw_band(
            frame,
            bounds,
            center_x,
            high,
            available_height,
            waveform::HIGH_BAR_WIDTH,
            palette.wave_high,
        );
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
    if height <= 0.0 {
        return;
    }
    frame.fill_rectangle(
        Point::new(center_x - width / 2.0, (bounds.height - height) / 2.0),
        Size::new(width, height),
        color,
    );
}

fn draw_beat_grid(
    frame: &mut Frame,
    bounds: Rectangle,
    data: &WaveformData,
    palette: RenderPalette,
) {
    draw_marks(
        frame,
        bounds,
        &data.beats,
        with_alpha(palette.line, waveform::GRID_ALPHA),
    );
    draw_marks(
        frame,
        bounds,
        &data.downbeats,
        with_alpha(palette.accent, waveform::DOWNBEAT_ALPHA),
    );
}

fn draw_marks(frame: &mut Frame, bounds: Rectangle, marks: &[f32], color: Color) {
    for &mark in marks {
        let x = mark.clamp(0.0, 1.0) * bounds.width;
        frame.stroke(
            &Path::line(Point::new(x, 0.0), Point::new(x, bounds.height)),
            Stroke::default()
                .with_color(color)
                .with_width(waveform::GRID_WIDTH),
        );
    }
}

fn with_alpha(color: Color, alpha: f32) -> Color {
    Color { a: alpha, ..color }
}

fn draw_border(frame: &mut Frame, bounds: Rectangle, color: Color) {
    let inset = chrome::BORDER_WIDTH / 2.0;
    frame.stroke_rectangle(
        Point::new(inset, inset),
        Size::new(
            (bounds.width - chrome::BORDER_WIDTH).max(0.0),
            (bounds.height - chrome::BORDER_WIDTH).max(0.0),
        ),
        Stroke::default()
            .with_color(color)
            .with_width(chrome::BORDER_WIDTH),
    );
}
