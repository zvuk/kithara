use iced::{
    Element, Length, Point, Rectangle, Renderer, Size, Theme,
    alignment::Vertical,
    widget::{
        Space,
        canvas::{self, Canvas, Frame, Geometry, Path, Stroke},
    },
};

use crate::{
    render::{PortalMapView, PortalTarget, Skin, UiEvent, fonts},
    skin::PortalMapSkin,
    widgets::Widget,
};

#[derive(bon::Builder)]
pub(crate) struct PortalMap<'skin> {
    skin: &'skin Skin,
    value: Option<PortalMapData>,
}

impl<'a> Widget<'a> for PortalMap<'_> {
    fn view(self) -> Element<'a, UiEvent> {
        let Some(view) = self.value else {
            return Space::new().into();
        };
        Canvas::new(PortalMapCanvas {
            metrics: self.skin.portal_map,
            palette: self.skin.palette,
            view,
        })
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
    }
}

pub(crate) struct PortalMapData {
    master: f32,
    min: f32,
    max: f32,
    targets: Vec<PortalTarget>,
}

impl From<PortalMapView<'_>> for PortalMapData {
    fn from(view: PortalMapView<'_>) -> Self {
        Self {
            master: view.master,
            min: view.min,
            max: view.max,
            targets: view.targets.to_vec(),
        }
    }
}

struct PortalMapCanvas {
    metrics: PortalMapSkin,
    palette: crate::render::theme::RenderPalette,
    view: PortalMapData,
}

impl canvas::Program<UiEvent> for PortalMapCanvas {
    type State = ();

    fn draw(
        &self,
        _state: &(),
        renderer: &Renderer,
        _theme: &Theme,
        bounds: Rectangle,
        _cursor: iced::mouse::Cursor,
    ) -> Vec<Geometry> {
        let mut frame = Frame::new(renderer, bounds.size());
        frame.fill_rectangle(Point::ORIGIN, bounds.size(), self.palette.bg_inset);
        let axis_y = (bounds.height - self.metrics.axis_offset_bottom).max(0.0);
        let axis = Rectangle {
            x: self.metrics.axis_inset_x.min(bounds.width / 2.0),
            y: axis_y,
            width: (bounds.width - self.metrics.axis_inset_x * 2.0).max(0.0),
            height: 1.0,
        };
        frame.fill_rectangle(axis.position(), axis.size(), self.palette.line);
        let Some(scale) = Scale::new(self.view.min, self.view.max, axis) else {
            return vec![frame.into_geometry()];
        };
        draw_ticks(&mut frame, scale, axis_y, self.metrics, self.palette);
        let master_x = scale.x(self.view.master);
        for target in &self.view.targets {
            draw_arc(
                &mut frame,
                master_x,
                scale.x(target.bpm),
                axis_y,
                target.is_selected,
                self.metrics,
                self.palette,
            );
        }
        draw_marker(
            &mut frame,
            master_x,
            axis_y,
            self.metrics.marker_size,
            self.palette.accent,
        );
        if let Some(target) = self.view.targets.iter().find(|target| target.is_selected) {
            draw_marker(
                &mut frame,
                scale.x(target.bpm),
                axis_y,
                self.metrics.marker_size,
                self.palette.wave_high,
            );
        }
        vec![frame.into_geometry()]
    }
}

#[derive(Clone, Copy)]
struct Scale {
    min: f32,
    span: f32,
    axis: Rectangle,
}

impl Scale {
    fn new(min: f32, max: f32, axis: Rectangle) -> Option<Self> {
        let span = max - min;
        (min.is_finite() && max.is_finite() && span > 0.0).then_some(Self { min, span, axis })
    }

    fn x(self, bpm: f32) -> f32 {
        let unit = ((bpm - self.min) / self.span).clamp(0.0, 1.0);
        self.axis.x + unit * self.axis.width
    }
}

fn draw_ticks(
    frame: &mut Frame,
    scale: Scale,
    axis_y: f32,
    metrics: PortalMapSkin,
    palette: crate::render::theme::RenderPalette,
) {
    let mut bpm = (scale.min / metrics.tick_step).ceil() * metrics.tick_step;
    let max = scale.min + scale.span;
    while bpm <= max {
        let x = scale.x(bpm).round();
        frame.fill_rectangle(
            Point::new(x, axis_y - metrics.tick_height),
            Size::new(1.0, metrics.tick_height),
            palette.line,
        );
        frame.fill_text(canvas::Text {
            content: format!("{bpm:.0}"),
            position: Point::new(x - metrics.label_offset_x, axis_y + metrics.label_offset_y),
            color: palette.muted,
            size: metrics.label.size.into(),
            font: fonts::mono(metrics.label.weight),
            align_y: Vertical::Center,
            shaping: iced::widget::text::Shaping::Advanced,
            ..canvas::Text::default()
        });
        bpm += metrics.tick_step;
    }
}

fn draw_arc(
    frame: &mut Frame,
    master_x: f32,
    target_x: f32,
    axis_y: f32,
    selected: bool,
    metrics: PortalMapSkin,
    palette: crate::render::theme::RenderPalette,
) {
    let radius = (target_x - master_x).abs() / 2.0;
    let center = (master_x + target_x) / 2.0;
    let rise = (radius * metrics.arc_height_scale).min((axis_y - metrics.arc_top_inset).max(0.0));
    let path = Path::new(|builder| {
        builder.move_to(Point::new(master_x, axis_y));
        builder.quadratic_curve_to(
            Point::new(center, axis_y - rise),
            Point::new(target_x, axis_y),
        );
    });
    frame.stroke(
        &path,
        Stroke::default()
            .with_color(if selected {
                palette.accent
            } else {
                palette.line_inner
            })
            .with_width(if selected {
                metrics.selected_line_width
            } else {
                metrics.line_width
            }),
    );
}

fn draw_marker(frame: &mut Frame, x: f32, axis_y: f32, size: f32, color: iced::Color) {
    frame.fill_rectangle(
        Point::new(x - size / 2.0, axis_y - size / 2.0),
        Size::new(size, size),
        color,
    );
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::*;

    #[kithara::test]
    fn portal_scale_maps_and_clamps_the_declared_tempo_range() {
        let scale = Scale::new(
            88.0,
            180.0,
            Rectangle {
                x: 12.0,
                width: 276.0,
                ..Rectangle::default()
            },
        )
        .unwrap();

        assert_eq!(scale.x(88.0), 12.0);
        assert_eq!(scale.x(180.0), 288.0);
        assert_eq!(scale.x(60.0), 12.0);
        assert_eq!(scale.x(200.0), 288.0);
    }
}
