use iced::{
    Color, Element, Event, Length, Point, Rectangle, Renderer, Size, Theme,
    mouse::{self, Button, Cursor},
    widget::canvas::{self, Action, Canvas, Frame, Geometry, Path, Stroke},
};
use num_traits::cast::AsPrimitive;

use crate::{
    module::WaveStyle,
    render::{ControlAction, ReadValue, Reads, Skin, UiEvent, WaveBucket, theme::RenderPalette},
    skin::{FrameSkin, WaveSkin},
    widgets::{Widget, behavior::HoverState},
};

#[derive(bon::Builder)]
pub(crate) struct MiniWave<'path, 'value, 'data, 'reads, 'skin> {
    path: &'path str,
    style: WaveStyle,
    value: Option<&'value ReadValue<'data>>,
    reads: &'reads dyn Reads,
    skin: &'skin Skin,
}

impl<'a> Widget<'a> for MiniWave<'_, '_, '_, '_, '_> {
    fn view(self) -> Element<'a, UiEvent> {
        let waveform = match self.value {
            Some(ReadValue::Waveform(waveform)) => Some(*waveform),
            _ => None,
        };
        let progress = match self.reads.get("deck.playback.position_normalized") {
            Some(ReadValue::Scalar(value)) => value.as_(),
            _ => 0.0,
        };
        let waveform = waveform.map(|view| WaveformData {
            buckets: view.buckets.to_vec().into_boxed_slice(),
            beats: view.beats.to_vec().into_boxed_slice(),
            downbeats: view.downbeats.to_vec().into_boxed_slice(),
        });
        Canvas::new(MiniWaveCanvas {
            metrics: self.skin.wave,
            border_color: self.skin.color(self.skin.wave.frame.border),
            hover: HoverState::new(mouse::Interaction::Pointer),
            palette: self.skin.palette,
            waveform,
            path: self.path.to_owned(),
            progress,
            show_beats: self.style == WaveStyle::Hero,
        })
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
    }
}

struct MiniWaveCanvas {
    metrics: WaveSkin,
    border_color: Color,
    hover: HoverState,
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

impl canvas::Program<UiEvent> for MiniWaveCanvas {
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
            draw_bars(
                &mut frame,
                bounds,
                &waveform.buckets,
                self.metrics,
                self.palette,
            );
        }
        if self.show_beats
            && let Some(waveform) = &self.waveform
        {
            draw_beat_grid(&mut frame, bounds, waveform, self.metrics, self.palette);
        }

        let head_x = self.progress.clamp(0.0, 1.0) * bounds.width;
        frame.stroke(
            &Path::line(Point::new(head_x, 0.0), Point::new(head_x, bounds.height)),
            Stroke::default()
                .with_color(self.palette.accent_strong)
                .with_width(self.metrics.playhead_width),
        );
        draw_border(&mut frame, bounds, self.metrics.frame, self.border_color);
        vec![frame.into_geometry()]
    }

    fn mouse_interaction(
        &self,
        _state: &(),
        bounds: Rectangle,
        cursor: Cursor,
    ) -> mouse::Interaction {
        self.hover.mouse_interaction(bounds, cursor)
    }

    fn update(
        &self,
        _state: &mut (),
        event: &Event,
        bounds: Rectangle,
        cursor: Cursor,
    ) -> Option<Action<UiEvent>> {
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
                    Action::publish(UiEvent::Control {
                        path: self.path.clone(),
                        action: ControlAction::SetScalar(f64::from(value)),
                    })
                    .and_capture()
                })
            }
            _ => None,
        }
    }
}

fn draw_bars(
    frame: &mut Frame,
    bounds: Rectangle,
    buckets: &[WaveBucket],
    metrics: WaveSkin,
    palette: RenderPalette,
) {
    let step = metrics.low_bar_width + metrics.bar_gap;
    let content_width = (bounds.width - metrics.content_inset * 2.0).max(0.0);
    let max_columns: usize = ((content_width + metrics.bar_gap) / step).floor().as_();
    let columns = max_columns.min(buckets.len());
    if columns == 0 {
        return;
    }

    let available_height = (bounds.height - metrics.content_inset * 2.0).max(0.0);
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
        let center_x = metrics.content_inset + column_x * step + metrics.low_bar_width / 2.0;
        draw_band(
            frame,
            bounds,
            center_x,
            low,
            available_height,
            metrics.low_bar_width,
            palette.wave_low,
        );
        draw_band(
            frame,
            bounds,
            center_x,
            mid,
            available_height,
            metrics.mid_bar_width,
            palette.wave_mid,
        );
        draw_band(
            frame,
            bounds,
            center_x,
            high,
            available_height,
            metrics.high_bar_width,
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
    metrics: WaveSkin,
    palette: RenderPalette,
) {
    draw_marks(
        frame,
        bounds,
        &data.beats,
        with_alpha(palette.line, metrics.grid_alpha),
        metrics.grid_width,
    );
    draw_marks(
        frame,
        bounds,
        &data.downbeats,
        with_alpha(palette.accent, metrics.downbeat_alpha),
        metrics.grid_width,
    );
}

fn draw_marks(frame: &mut Frame, bounds: Rectangle, marks: &[f32], color: Color, width: f32) {
    for &mark in marks {
        let x = mark.clamp(0.0, 1.0) * bounds.width;
        frame.stroke(
            &Path::line(Point::new(x, 0.0), Point::new(x, bounds.height)),
            Stroke::default().with_color(color).with_width(width),
        );
    }
}

fn with_alpha(color: Color, alpha: f32) -> Color {
    Color { a: alpha, ..color }
}

fn draw_border(frame: &mut Frame, bounds: Rectangle, skin: FrameSkin, color: Color) {
    if skin.border_width <= 0.0 {
        return;
    }
    let inset = skin.border_width / 2.0;
    let path = Path::rounded_rectangle(
        Point::new(inset, inset),
        Size::new(
            (bounds.width - skin.border_width).max(0.0),
            (bounds.height - skin.border_width).max(0.0),
        ),
        skin.radius.into(),
    );
    frame.stroke(
        &path,
        Stroke::default()
            .with_color(color)
            .with_width(skin.border_width),
    );
}
