use iced::{
    Alignment, Background, Color, Element, Event, Length, Point, Rectangle, Renderer, Size, Theme,
    alignment::Vertical,
    mouse::{self, Button, Cursor},
    widget::{
        Canvas, Space, Stack,
        canvas::{self, Action, Frame, Geometry, Path, Stroke},
        container,
        container::Style as ContainerStyle,
        row, slider,
        slider::{Handle, HandleShape, Rail, Status as SliderStatus, Style as SliderStyle},
    },
};
use num_traits::cast::AsPrimitive;

use crate::{
    module::FaderStyle,
    render::{
        ControlAction, Icon, ReadValue, Skin, UiEvent, fonts, shaped_text, theme::RenderPalette,
    },
    skin::{FaderSkin, FrameSkin},
};

pub(crate) fn view<'a>(
    path: &str,
    style: FaderStyle,
    value: Option<&ReadValue<'_>>,
    skin: &Skin,
) -> Element<'a, UiEvent> {
    let Some(ReadValue::Scalar(value)) = value else {
        return Space::new().into();
    };
    if matches!(style, FaderStyle::Volume | FaderStyle::VolumeCompact) {
        return segmented_volume(skin, path, *value);
    }

    let palette = skin.palette;
    let event_path = path.to_owned();
    let slider = slider(0.0..=1.0, value.clamp(0.0, 1.0), move |value| {
        UiEvent::Control {
            path: event_path.clone(),
            action: ControlAction::SetScalar(value),
        }
    })
    .step(skin.fader.step)
    .height(skin.fader.slider_height)
    .style(slider_style(skin))
    .width(Length::Fill);
    let ticks = Canvas::new(FaderTicks {
        metrics: skin.fader,
        color: palette.line_soft,
    })
    .height(Length::Fixed(skin.fader.ticks_height))
    .width(Length::Fill);
    let slider = container(slider)
        .height(Length::Fixed(skin.fader.ticks_height))
        .align_y(Vertical::Top);
    let rail = Stack::with_children([ticks.into(), slider.into()])
        .height(Length::Fixed(skin.fader.ticks_height))
        .width(Length::Fill);
    let label = container(
        shaped_text("VOL")
            .font(fonts::sans(skin.fader.label.weight))
            .size(skin.fader.label.size)
            .color(palette.muted),
    )
    .width(Length::Fixed(skin.fader.label_width))
    .height(Length::Fill)
    .center_y(Length::Fill);
    let control = row![label, rail]
        .align_y(Alignment::Center)
        .height(Length::Fixed(skin.fader.control_height))
        .width(Length::Fill);

    container(control)
        .padding([skin.fader.control_padding_y, skin.fader.control_padding_x])
        .height(Length::Fixed(skin.fader.control_height))
        .width(Length::Fill)
        .align_y(Vertical::Center)
        .style(move |_| ContainerStyle::default().background(Background::Color(palette.bg_panel)))
        .into()
}

fn segmented_volume(skin: &Skin, path: &str, value: f64) -> Element<'static, UiEvent> {
    let palette = skin.palette;
    let icon = container(Icon::SpeakerHigh.view(skin.fader.icon_size, palette.muted))
        .width(Length::Fixed(skin.fader.label_width))
        .height(Length::Fill)
        .center_x(Length::Fill)
        .center_y(Length::Fill);
    let control = row![icon, volume_strip(skin, path.to_owned(), value)]
        .spacing(skin.fader.content_gap)
        .align_y(Alignment::Center)
        .height(Length::Fixed(skin.fader.control_height))
        .width(Length::Fill);

    container(control)
        .padding([skin.fader.control_padding_y, skin.fader.control_padding_x])
        .height(Length::Fixed(skin.fader.control_height))
        .width(Length::Fill)
        .align_y(Vertical::Center)
        .style(move |_| ContainerStyle::default().background(Background::Color(palette.bg_panel)))
        .into()
}

fn slider_style(skin: &Skin) -> impl Fn(&Theme, SliderStatus) -> SliderStyle + 'static {
    let palette = skin.palette;
    let metrics = skin.fader;
    let rail_border = skin.border(metrics.rail_frame);
    let handle_border = skin.color(metrics.handle_frame.border);
    move |_theme, _status| SliderStyle {
        rail: Rail {
            backgrounds: (
                Background::Color(palette.accent),
                Background::Color(palette.bg_deep),
            ),
            width: metrics.rail_width,
            border: rail_border,
        },
        handle: Handle {
            shape: HandleShape::Rectangle {
                width: metrics.handle_width,
                border_radius: metrics.handle_frame.radius.into(),
            },
            background: Background::Color(palette.bg_panel_2),
            border_width: metrics.handle_frame.border_width,
            border_color: handle_border,
        },
    }
}

struct FaderTicks {
    metrics: FaderSkin,
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
        let y = (bounds.height - self.metrics.tick_height).max(0.0);
        while x <= bounds.width {
            frame.fill_rectangle(
                Point::new(x, y),
                Size::new(self.metrics.tick_width, self.metrics.tick_height),
                self.color,
            );
            x += self.metrics.tick_step;
        }
        vec![frame.into_geometry()]
    }
}

struct SegmentedVolume {
    metrics: FaderSkin,
    border_color: Color,
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
        draw_segments(&mut frame, bounds, self.volume, self.metrics, self.palette);
        draw_border(
            &mut frame,
            bounds,
            self.metrics.strip_frame,
            self.border_color,
        );
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

fn volume_strip(skin: &Skin, path: String, value: f64) -> Element<'static, UiEvent> {
    Canvas::new(SegmentedVolume {
        metrics: skin.fader,
        border_color: skin.color(skin.fader.strip_frame.border),
        palette: skin.palette,
        path,
        volume: value.clamp(0.0, 1.0).as_(),
    })
    .width(Length::Fill)
    .height(Length::Fixed(skin.fader.strip_height))
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

fn draw_segments(
    frame: &mut Frame,
    bounds: Rectangle,
    level: f32,
    metrics: FaderSkin,
    palette: RenderPalette,
) {
    let count: f32 = metrics.segment_count.as_();
    let gap_width = metrics.segment_gap * (count - 1.0);
    let content_width = (bounds.width - metrics.strip_padding * 2.0).max(0.0);
    let segment_width = ((content_width - gap_width) / count).max(0.0);
    if segment_width <= 0.0 {
        return;
    }

    let segment_height = metrics
        .segment_height
        .min((bounds.height - metrics.strip_padding * 2.0).max(0.0));
    let y = (bounds.height - segment_height) / 2.0;
    let lit = (level.clamp(0.0, 1.0) * count).round();
    for index in 0..metrics.segment_count {
        let index: f32 = index.as_();
        let x = metrics.strip_padding + index * (segment_width + metrics.segment_gap);
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
