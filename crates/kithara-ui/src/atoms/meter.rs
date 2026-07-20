use iced::{
    Element, Event, Length, Point, Rectangle, Renderer, Size, Theme,
    mouse::{self, Cursor},
    widget::{
        Space,
        canvas::{self, Action, Canvas, Frame, Geometry},
    },
};
use num_traits::cast::AsPrimitive;

use crate::{
    render::{ReadValue, Skin, StereoLevels, UiEvent, theme::RenderPalette},
    skin::VuStereoSkin,
    widgets::{
        Widget,
        behavior::{HoverState, ScalarDrag, ScalarDragMode, ScalarDragState},
    },
};

#[derive(bon::Builder)]
pub(crate) struct StereoMeter<'path, 'value, 'data, 'skin> {
    path: &'path str,
    value: Option<&'value ReadValue<'data>>,
    skin: &'skin Skin,
}

impl<'a> Widget<'a> for StereoMeter<'_, '_, '_, '_> {
    fn view(self) -> Element<'a, UiEvent> {
        let Some(ReadValue::Stereo(levels)) = self.value else {
            return Space::new().into();
        };
        Canvas::new(StereoMeterCanvas {
            drag: ScalarDrag::builder()
                .path(self.path.to_owned())
                .mode(ScalarDragMode::Horizontal)
                .hover(HoverState::new(mouse::Interaction::ResizingHorizontally))
                .build(),
            metrics: self.skin.vu_stereo,
            levels: *levels,
            palette: self.skin.palette,
        })
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
    }
}

struct StereoMeterCanvas {
    drag: ScalarDrag,
    metrics: VuStereoSkin,
    levels: StereoLevels,
    palette: RenderPalette,
}

impl canvas::Program<UiEvent> for StereoMeterCanvas {
    type State = ScalarDragState;

    fn draw(
        &self,
        _state: &ScalarDragState,
        renderer: &Renderer,
        _theme: &Theme,
        bounds: Rectangle,
        _cursor: Cursor,
    ) -> Vec<Geometry> {
        let mut frame = Frame::new(renderer, bounds.size());
        frame.fill_rectangle(Point::ORIGIN, bounds.size(), self.palette.bg_deep);

        for (level, y) in [self.levels.l, self.levels.r]
            .into_iter()
            .zip([self.metrics.channel_l_y, self.metrics.channel_r_y])
        {
            draw_channel(&mut frame, y, level, self.metrics, self.palette);
        }

        let x = self.levels.volume.clamp(0.0, 1.0) * bounds.width;
        frame.fill_rectangle(
            Point::new(x, 0.0),
            Size::new(self.metrics.carriage_width, bounds.height),
            self.palette.accent,
        );
        vec![frame.into_geometry()]
    }

    delegate::delegate! {
        to self.drag {
            fn update(
                &self,
                state: &mut ScalarDragState,
                event: &Event,
                bounds: Rectangle,
                cursor: Cursor,
            ) -> Option<Action<UiEvent>>;
            fn mouse_interaction(
                &self,
                state: &ScalarDragState,
                bounds: Rectangle,
                cursor: Cursor,
            ) -> mouse::Interaction;
        }
    }
}

fn draw_channel(
    frame: &mut Frame,
    y: f32,
    level: f32,
    metrics: VuStereoSkin,
    palette: RenderPalette,
) {
    let count: f32 = metrics.segment_count.as_();
    let lit = (level.clamp(0.0, 1.0) * count).round();

    for index in 0..metrics.segment_count {
        let index: f32 = index.as_();
        let x = index * (metrics.segment_width + metrics.segment_gap);
        let ratio = index / count;
        let color = if index >= lit {
            palette.bg_inset
        } else if ratio > metrics.danger_threshold {
            palette.danger
        } else if ratio > metrics.warning_threshold {
            palette.warning
        } else {
            palette.success
        };
        frame.fill_rectangle(
            Point::new(x, y),
            Size::new(metrics.segment_width, metrics.segment_height),
            color,
        );
    }
}
