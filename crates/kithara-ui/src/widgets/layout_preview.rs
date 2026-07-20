use iced::{
    Element, Length, Point, Rectangle, Renderer, Size, Theme,
    widget::{
        Canvas,
        canvas::{self, Frame, Geometry, Stroke},
    },
};

use crate::{
    compile::{CompiledNode, CompiledUi},
    layout::Axis,
    render::{Skin, theme::RenderPalette},
    skin::LayoutPreviewSkin,
};

/// Small canvas representation of compiled split and module geometry.
#[derive(bon::Builder)]
#[non_exhaustive]
pub struct LayoutPreview<'a> {
    ui: &'a CompiledUi,
    skin: &'a Skin,
}

impl LayoutPreview<'_> {
    #[must_use]
    pub fn view<Message: 'static>(self) -> Element<'static, Message> {
        Canvas::new(Preview {
            geometry: PreviewGeometry::new(&self.ui.root),
            metrics: self.skin.layout_preview,
            palette: self.skin.palette,
        })
        .width(Length::Fill)
        .height(Length::Fixed(self.skin.layout_preview.height))
        .into()
    }
}

struct Preview {
    geometry: PreviewGeometry,
    metrics: LayoutPreviewSkin,
    palette: RenderPalette,
}

impl<Message> canvas::Program<Message> for Preview {
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
        frame.fill_rectangle(Point::ORIGIN, bounds.size(), self.palette.bg_deep);

        for area in self.geometry.iter() {
            let mut point = Point::new(area.bounds.x * bounds.width, area.bounds.y * bounds.height);
            let mut size = Size::new(
                area.bounds.width * bounds.width,
                area.bounds.height * bounds.height,
            );
            let color = match area.kind {
                AreaKind::Split => self.palette.line_soft,
                AreaKind::Module => {
                    point.x += self.metrics.module_inset;
                    point.y += self.metrics.module_inset;
                    size.width = (size.width - self.metrics.module_inset * 2.0).max(0.0);
                    size.height = (size.height - self.metrics.module_inset * 2.0).max(0.0);
                    frame.fill_rectangle(point, size, self.palette.bg_panel);
                    self.palette.line
                }
            };
            frame.stroke_rectangle(
                point,
                size,
                Stroke::default()
                    .with_color(color)
                    .with_width(self.metrics.line_width),
            );
        }

        vec![frame.into_geometry()]
    }
}

#[derive(derive_more::Deref, derive_more::From)]
struct PreviewGeometry(Vec<PreviewArea>);

impl PreviewGeometry {
    fn new(root: &CompiledNode) -> Self {
        let mut areas = Vec::new();
        collect_areas(root, UnitRect::root(), &mut areas);
        areas.into()
    }
}

#[derive(Clone, Copy)]
struct PreviewArea {
    bounds: UnitRect,
    kind: AreaKind,
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum AreaKind {
    Split,
    Module,
}

#[derive(Clone, Copy)]
struct UnitRect {
    x: f32,
    y: f32,
    width: f32,
    height: f32,
}

impl UnitRect {
    const fn root() -> Self {
        Self {
            x: 0.0,
            y: 0.0,
            width: 1.0,
            height: 1.0,
        }
    }
}

fn collect_areas(node: &CompiledNode, bounds: UnitRect, areas: &mut Vec<PreviewArea>) {
    match node {
        CompiledNode::Split { axis, children, .. } => {
            areas.push(PreviewArea {
                bounds,
                kind: AreaKind::Split,
            });
            let total = children
                .iter()
                .map(|(weight, _)| weight.max(0.0))
                .sum::<f32>();
            if total <= f32::EPSILON {
                return;
            }
            let mut cursor = 0.0_f32;
            for (index, (weight, child)) in children.iter().enumerate() {
                let fraction = if index + 1 == children.len() {
                    (1.0 - cursor).max(0.0)
                } else {
                    weight.max(0.0) / total
                };
                let child_bounds = match axis {
                    Axis::Horizontal => UnitRect {
                        x: bounds.x + bounds.width * cursor,
                        width: bounds.width * fraction,
                        ..bounds
                    },
                    Axis::Vertical => UnitRect {
                        y: bounds.y + bounds.height * cursor,
                        height: bounds.height * fraction,
                        ..bounds
                    },
                };
                collect_areas(child, child_bounds, areas);
                cursor += fraction;
            }
        }
        CompiledNode::Module { .. } => areas.push(PreviewArea {
            bounds,
            kind: AreaKind::Module,
        }),
    }
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::*;
    use crate::{
        builtin,
        compile::compile,
        ids::EndpointId,
        registry::{EndpointCategory, EndpointDesc, EndpointRegistry, ValueKind},
        source::UiConfig,
    };

    struct Registry {
        bool_value: EndpointDesc,
        scalar: EndpointDesc,
        stereo: EndpointDesc,
        scoped_scalar: EndpointDesc,
        scoped_text: EndpointDesc,
        scoped_trigger: EndpointDesc,
        scoped_waveform: EndpointDesc,
        track_list: EndpointDesc,
    }

    impl Default for Registry {
        fn default() -> Self {
            Self {
                bool_value: EndpointDesc::new(ValueKind::Bool).with_scope("deck"),
                scalar: EndpointDesc::new(ValueKind::Scalar),
                stereo: EndpointDesc::new(ValueKind::Stereo),
                scoped_scalar: EndpointDesc::new(ValueKind::Scalar).with_scope("deck"),
                scoped_text: EndpointDesc::new(ValueKind::Text).with_scope("deck"),
                scoped_trigger: EndpointDesc::new(ValueKind::Trigger).with_scope("deck"),
                scoped_waveform: EndpointDesc::new(ValueKind::Waveform).with_scope("deck"),
                track_list: EndpointDesc::new(ValueKind::TrackList),
            }
        }
    }

    impl EndpointRegistry for Registry {
        fn endpoint(&self, category: EndpointCategory, id: &EndpointId) -> Option<&EndpointDesc> {
            match (category, id.0.as_str()) {
                (
                    EndpointCategory::Command,
                    "deck.transport.toggle_play" | "deck.transport.prev" | "deck.transport.next",
                ) => Some(&self.scoped_trigger),
                (EndpointCategory::Command, "deck.transport.seek_normalized")
                | (EndpointCategory::Telemetry, "deck.playback.position_normalized") => {
                    Some(&self.scoped_scalar)
                }
                (EndpointCategory::Telemetry, "deck.playback.playing") => Some(&self.bool_value),
                (EndpointCategory::Telemetry, "deck.playback.waveform") => {
                    Some(&self.scoped_waveform)
                }
                (EndpointCategory::Telemetry, "deck.track.title") => Some(&self.scoped_text),
                (EndpointCategory::Parameter, "player.output.volume") => Some(&self.scalar),
                (EndpointCategory::Telemetry, "player.output.levels") => Some(&self.stereo),
                (EndpointCategory::Model, "deck.view.zoom") => Some(&self.scalar),
                (EndpointCategory::Model, "library.visible_tracks") => Some(&self.track_list),
                _ => None,
            }
        }
    }

    #[kithara::test]
    fn builtin_presets_build_preview_geometry() {
        let registry = Registry::default();
        let geometry = [builtin::MICRO_PRESET, builtin::PLAYER_PRESET].map(|preset| {
            let ui = compile(
                preset,
                &builtin::resolver(),
                &registry,
                builtin::skin_doc(),
                &UiConfig::default(),
            )
            .unwrap_or_else(|error| panic!("{preset} must compile: {error}"));
            PreviewGeometry::new(&ui.root)
        });
        let module_counts = geometry.map(|preview| {
            preview
                .iter()
                .filter(|area| area.kind == AreaKind::Module)
                .count()
        });

        assert_eq!(module_counts, [1, 3]);
    }
}
