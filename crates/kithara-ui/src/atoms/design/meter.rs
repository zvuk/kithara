use iced::{
    Color, Element, Length, Point, Rectangle, Renderer, Size, Theme,
    mouse::Cursor,
    widget::canvas::{self, Canvas, Frame, Geometry, Path, Stroke},
};

use num_traits::cast::AsPrimitive;

use crate::{
    render::{ReadValue, Skin, UiEvent},
    widgets::Widget,
};

#[derive(bon::Builder)]
pub(crate) struct Meter<'value, 'data, 'skin> {
    value: Option<&'value ReadValue<'data>>,
    skin: &'skin Skin,
}

impl<'a> Widget<'a> for Meter<'_, '_, '_> {
    fn view(self) -> Element<'a, UiEvent> {
        let metrics = self.skin.meter;
        let level = match self.value {
            Some(ReadValue::Scalar(level)) => level.clamp(0.0, 1.0).as_(),
            _ => 0.0,
        };
        Canvas::new(MeterCanvas {
            level,
            border_width: metrics.frame.border_width,
            border_color: self.skin.color(metrics.frame.border),
            background: self.skin.color(metrics.background),
            fill: self.skin.color(metrics.fill),
        })
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
    }
}

struct MeterCanvas {
    level: f32,
    border_width: f32,
    border_color: Color,
    background: Color,
    fill: Color,
}

impl canvas::Program<UiEvent> for MeterCanvas {
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
        frame.fill_rectangle(Point::ORIGIN, bounds.size(), self.background);

        let width = self.border_width;
        let track = Size::new(
            (bounds.width - width * 2.0).max(0.0),
            (bounds.height - width * 2.0).max(0.0),
        );
        frame.fill_rectangle(
            Point::new(width, width),
            Size::new(track.width * self.level, track.height),
            self.fill,
        );

        if width > 0.0 {
            let inset = width / 2.0;
            frame.stroke(
                &Path::rectangle(
                    Point::new(inset, inset),
                    Size::new(bounds.width - width, bounds.height - width),
                ),
                Stroke::default()
                    .with_width(width)
                    .with_color(self.border_color),
            );
        }
        vec![frame.into_geometry()]
    }
}
