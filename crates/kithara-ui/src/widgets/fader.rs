use iced::{
    Alignment, Background, Border, Color, Element, Event, Length, Point, Rectangle, Renderer, Size,
    Theme,
    alignment::Vertical,
    mouse::{self, Button, Cursor},
    widget::{
        Canvas, Space, Stack,
        canvas::{self, Action, Frame, Geometry, Stroke},
        container,
        container::Style as ContainerStyle,
        row, slider,
        slider::{Handle, HandleShape, Rail, Status as SliderStatus, Style as SliderStyle},
    },
};
use num_traits::cast::AsPrimitive;

use super::chrome;
use crate::{
    module::FaderStyle,
    render::{ControlAction, Icon, ReadValue, RenderPalette, UiEvent, fonts, shaped_text},
};

struct Consts;

impl Consts {
    const CONTENT_GAP: f32 = 6.0;
    const CONTROL_HEIGHT: f32 = 34.0;
    const CONTROL_PADDING_X: f32 = 8.0;
    const HANDLE_BORDER_WIDTH: f32 = 1.0;
    const HANDLE_WIDTH: u16 = 6;
    const ICON_SIZE: f32 = 11.0;
    const LABEL_TEXT_SIZE: f32 = 12.0;
    const LABEL_WIDTH: f32 = 18.0;
    const RAIL_WIDTH: f32 = 6.0;
    const SEGMENT_COUNT: usize = 12;
    const SEGMENT_GAP: f32 = 1.5;
    const SEGMENT_HEIGHT: f32 = 10.0;
    const SLIDER_HEIGHT: f32 = 16.0;
    const STEP: f64 = 0.01;
    const STRIP_HEIGHT: f32 = 14.0;
    const STRIP_PADDING: f32 = 2.0;
    const TICKS_HEIGHT: f32 = 22.0;
    const TICK_HEIGHT: f32 = 4.0;
    const TICK_STEP: f32 = 12.0;
}

pub(crate) fn view<'a>(
    path: &str,
    style: FaderStyle,
    value: Option<&ReadValue<'_>>,
    palette: RenderPalette,
) -> Element<'a, UiEvent> {
    let Some(ReadValue::Scalar(value)) = value else {
        return Space::new().into();
    };
    if matches!(style, FaderStyle::Volume | FaderStyle::VolumeCompact) {
        return segmented_volume(palette, path, *value);
    }

    let event_path = path.to_owned();
    let slider = slider(0.0..=1.0, value.clamp(0.0, 1.0), move |value| {
        UiEvent::Control {
            path: event_path.clone(),
            action: ControlAction::SetScalar(value),
        }
    })
    .step(Consts::STEP)
    .height(Consts::SLIDER_HEIGHT)
    .style(slider_style(palette))
    .width(Length::Fill);
    let ticks = Canvas::new(FaderTicks {
        color: palette.line_soft,
    })
    .height(Length::Fixed(Consts::TICKS_HEIGHT))
    .width(Length::Fill);
    let slider = container(slider)
        .height(Length::Fixed(Consts::TICKS_HEIGHT))
        .align_y(Vertical::Top);
    let rail = Stack::with_children([ticks.into(), slider.into()])
        .height(Length::Fixed(Consts::TICKS_HEIGHT))
        .width(Length::Fill);
    let label = container(
        shaped_text("VOL")
            .font(fonts::SANS)
            .size(Consts::LABEL_TEXT_SIZE)
            .color(palette.muted),
    )
    .width(Length::Fixed(Consts::LABEL_WIDTH))
    .height(Length::Fill)
    .center_y(Length::Fill);
    let control = row![label, rail]
        .align_y(Alignment::Center)
        .height(Length::Fixed(Consts::CONTROL_HEIGHT))
        .width(Length::Fill);

    container(control)
        .padding([0.0, Consts::CONTROL_PADDING_X])
        .height(Length::Fixed(Consts::CONTROL_HEIGHT))
        .width(Length::Fill)
        .align_y(Vertical::Center)
        .style(move |_| ContainerStyle::default().background(Background::Color(palette.bg_panel)))
        .into()
}

fn segmented_volume(palette: RenderPalette, path: &str, value: f64) -> Element<'static, UiEvent> {
    let icon = container(Icon::SpeakerHigh.view(Consts::ICON_SIZE, palette.muted))
        .width(Length::Fixed(Consts::LABEL_WIDTH))
        .height(Length::Fill)
        .center_x(Length::Fill)
        .center_y(Length::Fill);
    let control = row![icon, volume_strip(palette, path.to_owned(), value)]
        .spacing(Consts::CONTENT_GAP)
        .align_y(Alignment::Center)
        .height(Length::Fixed(Consts::CONTROL_HEIGHT))
        .width(Length::Fill);

    container(control)
        .padding([0.0, Consts::CONTROL_PADDING_X])
        .height(Length::Fixed(Consts::CONTROL_HEIGHT))
        .width(Length::Fill)
        .align_y(Vertical::Center)
        .style(move |_| ContainerStyle::default().background(Background::Color(palette.bg_panel)))
        .into()
}

fn slider_style(palette: RenderPalette) -> impl Fn(&Theme, SliderStatus) -> SliderStyle {
    move |_theme, _status| SliderStyle {
        rail: Rail {
            backgrounds: (
                Background::Color(palette.accent),
                Background::Color(palette.bg_deep),
            ),
            width: Consts::RAIL_WIDTH,
            border: Border::default().width(1).color(palette.line_soft),
        },
        handle: Handle {
            shape: HandleShape::Rectangle {
                width: Consts::HANDLE_WIDTH,
                border_radius: 0.0.into(),
            },
            background: Background::Color(palette.bg_panel_2),
            border_width: Consts::HANDLE_BORDER_WIDTH,
            border_color: palette.line,
        },
    }
}

struct FaderTicks {
    color: Color,
}

impl canvas::Program<UiEvent> for FaderTicks {
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
        let mut x = 0.0;
        let y = (bounds.height - Consts::TICK_HEIGHT).max(0.0);
        while x <= bounds.width {
            frame.fill_rectangle(
                Point::new(x, y),
                Size::new(chrome::border_width(), Consts::TICK_HEIGHT),
                self.color,
            );
            x += Consts::TICK_STEP;
        }
        vec![frame.into_geometry()]
    }
}

struct SegmentedVolume {
    palette: RenderPalette,
    path: String,
    volume: f32,
}

#[derive(Default)]
struct DragState {
    active: bool,
}

impl canvas::Program<UiEvent> for SegmentedVolume {
    type State = DragState;

    fn update(
        &self,
        state: &mut DragState,
        event: &Event,
        bounds: Rectangle,
        cursor: Cursor,
    ) -> Option<Action<UiEvent>> {
        match event {
            Event::Mouse(mouse::Event::ButtonPressed(Button::Left)) if cursor.is_over(bounds) => {
                state.active = true;
                scalar_action(&self.path, bounds, cursor)
            }
            Event::Mouse(mouse::Event::CursorMoved { .. }) if state.active => {
                scalar_action(&self.path, bounds, cursor)
            }
            Event::Mouse(mouse::Event::ButtonReleased(Button::Left)) if state.active => {
                state.active = false;
                Some(Action::capture())
            }
            _ => None,
        }
    }

    fn draw(
        &self,
        _state: &DragState,
        renderer: &Renderer,
        _theme: &Theme,
        bounds: Rectangle,
        _cursor: Cursor,
    ) -> Vec<Geometry> {
        let mut frame = Frame::new(renderer, bounds.size());
        frame.fill_rectangle(Point::ORIGIN, bounds.size(), self.palette.bg_deep);
        draw_segments(&mut frame, bounds, self.volume, self.palette);
        draw_border(&mut frame, bounds, self.palette.line);
        vec![frame.into_geometry()]
    }

    fn mouse_interaction(
        &self,
        state: &DragState,
        bounds: Rectangle,
        cursor: Cursor,
    ) -> mouse::Interaction {
        if state.active || cursor.is_over(bounds) {
            mouse::Interaction::ResizingHorizontally
        } else {
            mouse::Interaction::default()
        }
    }
}

fn volume_strip(palette: RenderPalette, path: String, value: f64) -> Element<'static, UiEvent> {
    Canvas::new(SegmentedVolume {
        palette,
        path,
        volume: value.clamp(0.0, 1.0).as_(),
    })
    .width(Length::Fill)
    .height(Length::Fixed(Consts::STRIP_HEIGHT))
    .into()
}

fn scalar_action(path: &str, bounds: Rectangle, cursor: Cursor) -> Option<Action<UiEvent>> {
    if bounds.width <= 0.0 {
        return None;
    }
    cursor.position_from(bounds.position()).map(|position| {
        let value = (position.x / bounds.width).clamp(0.0, 1.0);
        Action::publish(UiEvent::Control {
            path: path.to_owned(),
            action: ControlAction::SetScalar(f64::from(value)),
        })
        .and_capture()
    })
}

fn draw_segments(frame: &mut Frame, bounds: Rectangle, level: f32, palette: RenderPalette) {
    let count: f32 = Consts::SEGMENT_COUNT.as_();
    let gap_width = Consts::SEGMENT_GAP * (count - 1.0);
    let content_width = (bounds.width - Consts::STRIP_PADDING * 2.0).max(0.0);
    let segment_width = ((content_width - gap_width) / count).max(0.0);
    if segment_width <= 0.0 {
        return;
    }

    let segment_height =
        Consts::SEGMENT_HEIGHT.min((bounds.height - Consts::STRIP_PADDING * 2.0).max(0.0));
    let y = (bounds.height - segment_height) / 2.0;
    let lit = (level.clamp(0.0, 1.0) * count).round();
    for index in 0..Consts::SEGMENT_COUNT {
        let index: f32 = index.as_();
        let x = Consts::STRIP_PADDING + index * (segment_width + Consts::SEGMENT_GAP);
        frame.fill_rectangle(
            Point::new(x, y),
            Size::new(segment_width, segment_height),
            if index < lit {
                palette.success
            } else {
                palette.bg_panel
            },
        );
    }
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
