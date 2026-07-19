use iced::{
    Color, Element, Event, Length, Point, Rectangle, Renderer, Size, Theme,
    mouse::{self, Button, Cursor},
    widget::canvas::{self, Action, Canvas, Frame, Geometry, Path, Stroke},
};
use num_traits::cast::AsPrimitive;

use super::chrome;
use crate::{
    registry::{ControlKindDesc, PropKind, ValueKind},
    render::{ControlAction, ReadValue, Reads, RenderPalette, UiEvent, WaveBucket},
    size::{Dim, SizeSpec},
};

struct Consts;

impl Consts {
    const BAR_GAP: f32 = 1.0;
    const CONTENT_INSET: f32 = 2.0;
    const DOWNBEAT_ALPHA: f32 = 0.72;
    const GRID_ALPHA: f32 = 0.55;
    const GRID_WIDTH: f32 = 1.0;
    const HERO_HEIGHT: f32 = 120.0;
    const HIGH_BAR_WIDTH: f32 = 1.0;
    const LOW_BAR_WIDTH: f32 = 3.0;
    const MID_BAR_WIDTH: f32 = 2.0;
    const PLAYHEAD_WIDTH: f32 = 2.0;
}

pub(crate) fn desc() -> ControlKindDesc {
    ControlKindDesc::new(Some(ValueKind::Waveform), Some(ValueKind::Scalar))
        .with_prop("style", PropKind::Text)
        .with_size(SizeSpec::new(
            Dim::Fill,
            Dim::Range {
                min: Consts::HERO_HEIGHT,
                max: None,
            },
        ))
}

pub(crate) fn view(
    path: &str,
    style: Option<&str>,
    value: Option<&ReadValue<'_>>,
    reads: &dyn Reads,
    palette: RenderPalette,
) -> Element<'static, UiEvent> {
    let waveform = match value {
        Some(ReadValue::Waveform(waveform)) => Some(*waveform),
        _ => None,
    };
    let progress = match reads.get("deck.playback.position_normalized") {
        Some(ReadValue::Scalar(value)) => value.as_(),
        _ => 0.0,
    };
    let waveform = waveform.map(|view| WaveformData {
        buckets: view.buckets.to_vec().into_boxed_slice(),
        beats: view.beats.to_vec().into_boxed_slice(),
        downbeats: view.downbeats.to_vec().into_boxed_slice(),
    });
    Canvas::new(MiniWave {
        palette,
        waveform,
        path: path.to_owned(),
        progress,
        show_beats: style == Some("hero"),
    })
    .width(Length::Fill)
    .height(Length::Fill)
    .into()
}

struct MiniWave {
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

impl canvas::Program<UiEvent> for MiniWave {
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
                .with_width(Consts::PLAYHEAD_WIDTH),
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

fn draw_bars(frame: &mut Frame, bounds: Rectangle, buckets: &[WaveBucket], palette: RenderPalette) {
    let step = Consts::LOW_BAR_WIDTH + Consts::BAR_GAP;
    let content_width = (bounds.width - Consts::CONTENT_INSET * 2.0).max(0.0);
    let max_columns: usize = ((content_width + Consts::BAR_GAP) / step).floor().as_();
    let columns = max_columns.min(buckets.len());
    if columns == 0 {
        return;
    }

    let available_height = (bounds.height - Consts::CONTENT_INSET * 2.0).max(0.0);
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
        let center_x = Consts::CONTENT_INSET + column_x * step + Consts::LOW_BAR_WIDTH / 2.0;
        draw_band(
            frame,
            bounds,
            center_x,
            low,
            available_height,
            Consts::LOW_BAR_WIDTH,
            palette.wave_low,
        );
        draw_band(
            frame,
            bounds,
            center_x,
            mid,
            available_height,
            Consts::MID_BAR_WIDTH,
            palette.wave_mid,
        );
        draw_band(
            frame,
            bounds,
            center_x,
            high,
            available_height,
            Consts::HIGH_BAR_WIDTH,
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
        with_alpha(palette.line, Consts::GRID_ALPHA),
    );
    draw_marks(
        frame,
        bounds,
        &data.downbeats,
        with_alpha(palette.accent, Consts::DOWNBEAT_ALPHA),
    );
}

fn draw_marks(frame: &mut Frame, bounds: Rectangle, marks: &[f32], color: Color) {
    for &mark in marks {
        let x = mark.clamp(0.0, 1.0) * bounds.width;
        frame.stroke(
            &Path::line(Point::new(x, 0.0), Point::new(x, bounds.height)),
            Stroke::default()
                .with_color(color)
                .with_width(Consts::GRID_WIDTH),
        );
    }
}

fn with_alpha(color: Color, alpha: f32) -> Color {
    Color { a: alpha, ..color }
}

fn draw_border(frame: &mut Frame, bounds: Rectangle, color: Color) {
    let width = chrome::border_width();
    let inset = width / 2.0;
    frame.stroke_rectangle(
        Point::new(inset, inset),
        Size::new(
            (bounds.width - width).max(0.0),
            (bounds.height - width).max(0.0),
        ),
        Stroke::default().with_color(color).with_width(width),
    );
}
