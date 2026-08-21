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
    metrics: LayoutPreviewSkin,
    geometry: PreviewGeometry,
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
                    size.width = self.metrics.module_inset.mul_add(-2.0, size.width).max(0.0);
                    size.height = self
                        .metrics
                        .module_inset
                        .mul_add(-2.0, size.height)
                        .max(0.0);
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
    kind: AreaKind,
    bounds: UnitRect,
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum AreaKind {
    Split,
    Module,
}

#[derive(Clone, Copy)]
struct UnitRect {
    height: f32,
    width: f32,
    x: f32,
    y: f32,
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
        CompiledNode::Optional { child, .. } => collect_areas(child, bounds, areas),
        CompiledNode::Adaptive { base, .. } => collect_areas(base, bounds, areas),
        CompiledNode::Split { axis, children, .. } => {
            areas.push(PreviewArea {
                bounds,
                kind: AreaKind::Split,
            });
            let total = children
                .iter()
                .map(|cell| cell.weight.max(0.0))
                .sum::<f32>();
            if total <= f32::EPSILON {
                return;
            }
            let mut cursor = 0.0_f32;
            for (index, cell) in children.iter().enumerate() {
                let fraction = if index + 1 == children.len() {
                    (1.0 - cursor).max(0.0)
                } else {
                    cell.weight.max(0.0) / total
                };
                let child_bounds = match axis {
                    Axis::Horizontal => UnitRect {
                        x: bounds.width.mul_add(cursor, bounds.x),
                        width: bounds.width * fraction,
                        ..bounds
                    },
                    Axis::Vertical => UnitRect {
                        y: bounds.height.mul_add(cursor, bounds.y),
                        height: bounds.height * fraction,
                        ..bounds
                    },
                };
                collect_areas(&cell.node, child_bounds, areas);
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
    use std::collections::BTreeMap;

    use kithara_test_utils::kithara;

    use super::*;
    use crate::{
        builtin,
        compile::compile,
        ids::EndpointId,
        registry::{EndpointCategory, EndpointDesc, EndpointRegistry, ValueKind},
        source::UiConfig,
    };

    /// Answers every endpoint the builtin presets name, keyed the way the
    /// documents bind them.
    #[derive(Default)]
    struct Registry {
        endpoints: BTreeMap<(EndpointCategory, String), EndpointDesc>,
    }

    impl Registry {
        fn preset_surface() -> Self {
            let mut registry = Self::default();
            registry.deck();
            registry.player();
            registry.bar();
            registry.menu();
            registry.clock();
            registry
        }

        fn add(
            &mut self,
            category: EndpointCategory,
            ids: &[&'static str],
            kind: ValueKind,
            scopes: &[&str],
        ) {
            let desc = scopes.iter().fold(EndpointDesc::new(kind), |desc, scope| {
                desc.with_scope(scope)
            });
            for id in ids {
                self.endpoints
                    .insert((category, (*id).to_owned()), desc.clone());
            }
        }

        fn deck(&mut self) {
            use EndpointCategory::{Command, Model, Telemetry};
            self.add(
                Command,
                &[
                    "deck.transport.jump_back",
                    "deck.transport.jump_forward",
                    "deck.transport.set_cue",
                    "deck.transport.toggle_loop",
                    "deck.transport.toggle_play",
                    "deck.transport.toggle_reverse",
                    "deck.transport.toggle_sync",
                    "deck.view.zoom_in",
                    "deck.view.zoom_out",
                    "deck.stream.toggle_quality_menu",
                ],
                ValueKind::Trigger,
                &["deck"],
            );
            self.add(
                Command,
                &["deck.transport.seek_normalized"],
                ValueKind::Scalar,
                &["deck"],
            );
            self.add(
                Telemetry,
                &[
                    "deck.playback.position_normalized",
                    "deck.playback.cached_normalized",
                ],
                ValueKind::Scalar,
                &["deck"],
            );
            self.add(
                Telemetry,
                &[
                    "deck.playback.looping",
                    "deck.playback.playing",
                    "deck.playback.reverse",
                    "deck.playback.synced",
                    "deck.stream.quality_hidden",
                ],
                ValueKind::Bool,
                &["deck"],
            );
            self.add(
                Model,
                &["deck.stream.quality_menu"],
                ValueKind::Bool,
                &["deck"],
            );
            self.add(
                Telemetry,
                &["deck.playback.waveform"],
                ValueKind::Waveform,
                &["deck"],
            );
            self.add(
                Telemetry,
                &["deck.track.title", "deck.playback.tempo"],
                ValueKind::Text,
                &["deck"],
            );
            self.add(Model, &["deck.stream.quality"], ValueKind::Text, &["deck"]);
            self.add(Model, &["deck.view.zoom"], ValueKind::Scalar, &[]);
            self.add(
                Telemetry,
                &["deck.stream.variant_hidden"],
                ValueKind::Bool,
                &["deck", "variant"],
            );
            self.add(
                Model,
                &["deck.stream.variant_active"],
                ValueKind::Bool,
                &["deck", "variant"],
            );
            self.add(
                Telemetry,
                &["deck.stream.variant_label", "deck.stream.variant_sub"],
                ValueKind::Text,
                &["deck", "variant"],
            );
            self.add(
                Command,
                &["deck.stream.select_variant"],
                ValueKind::Trigger,
                &["deck", "variant"],
            );
        }

        fn player(&mut self) {
            use EndpointCategory::{Model, Parameter, Telemetry};
            self.add(Telemetry, &["player.output.levels"], ValueKind::Stereo, &[]);
            self.add(Parameter, &["player.output.volume"], ValueKind::Scalar, &[]);
            self.add(
                Model,
                &["library.visible_tracks"],
                ValueKind::TrackList,
                &[],
            );
        }

        fn bar(&mut self) {
            use EndpointCategory::{Command, Model, Telemetry};
            self.add(Telemetry, &["engine.load"], ValueKind::Scalar, &[]);
            self.add(Telemetry, &["engine.latency"], ValueKind::Text, &[]);
            self.add(Model, &["ui.set.recording"], ValueKind::Bool, &[]);
            self.add(
                Model,
                &["ui.set.record_hint", "ui.set.record_time"],
                ValueKind::Text,
                &[],
            );
            self.add(Command, &["ui.set.toggle_record"], ValueKind::Trigger, &[]);
        }

        fn menu(&mut self) {
            use EndpointCategory::{Command, Model};
            self.add(
                Model,
                &[
                    "ui.menu.open",
                    "ui.window.can_open",
                    "ui.prefs.wave_follow",
                    "ui.prefs.autogain",
                    "ui.prefs.mono",
                    "ui.set.casting",
                ],
                ValueKind::Bool,
                &[],
            );
            self.add(
                Model,
                &[
                    "ui.window.count",
                    "ui.modules.title",
                    "ui.modules.count",
                    "ui.layouts.active",
                    "ui.set.cast_hint",
                ],
                ValueKind::Text,
                &[],
            );
            self.add(
                Model,
                &["ui.menu.group_open", "ui.menu.group_hidden"],
                ValueKind::Bool,
                &["group"],
            );
            self.add(
                Model,
                &[
                    "ui.window.active",
                    "ui.window.hidden",
                    "ui.window.close_hidden",
                ],
                ValueKind::Bool,
                &["window"],
            );
            self.add(
                Model,
                &["ui.window.title", "ui.window.caption"],
                ValueKind::Text,
                &["window"],
            );
            self.add(Model, &["ui.module.on"], ValueKind::Bool, &["module"]);
            self.add(Model, &["ui.layout.selected"], ValueKind::Bool, &["layout"]);
            self.add(
                Command,
                &[
                    "ui.menu.toggle",
                    "ui.menu.close",
                    "ui.window.open",
                    "ui.window.toggle_full_screen",
                    "ui.prefs.toggle_wave_follow",
                    "ui.prefs.toggle_autogain",
                    "ui.prefs.toggle_mono",
                    "ui.set.toggle_cast",
                    "ui.library.add_folder",
                    "ui.settings.open",
                ],
                ValueKind::Trigger,
                &[],
            );
            self.add(
                Command,
                &["ui.menu.toggle_group"],
                ValueKind::Trigger,
                &["group"],
            );
            self.add(
                Command,
                &[
                    "ui.window.focus",
                    "ui.window.cycle_display",
                    "ui.window.close",
                ],
                ValueKind::Trigger,
                &["window"],
            );
            self.add(
                Command,
                &["ui.module.toggle"],
                ValueKind::Trigger,
                &["module"],
            );
            self.add(
                Command,
                &["ui.layout.apply"],
                ValueKind::Trigger,
                &["layout"],
            );
        }

        fn clock(&mut self) {
            use EndpointCategory::{Command, Model, Parameter};
            self.add(
                Model,
                &[
                    "clock.open",
                    "clock.family.step",
                    "clock.family.leap",
                    "clock.grid.quantize",
                    "clock.grid.snap",
                    "clock.grid.click",
                    "clock.link.enabled",
                    "clock.midi.send",
                ],
                ValueKind::Bool,
                &[],
            );
            self.add(
                Model,
                &[
                    "clock.bpm",
                    "clock.source",
                    "clock.warning",
                    "clock.grid.division",
                    "clock.link.peers",
                    "clock.midi.input",
                    "clock.midi.output",
                ],
                ValueKind::Text,
                &[],
            );
            self.add(
                Model,
                &["clock.limit", "clock.tolerance"],
                ValueKind::Scalar,
                &[],
            );
            self.add(Parameter, &["clock.tempo"], ValueKind::Scalar, &[]);
            self.add(
                Model,
                &["clock.source.active", "clock.source.synced"],
                ValueKind::Bool,
                &["source"],
            );
            self.add(
                Model,
                &[
                    "clock.source.name",
                    "clock.source.tempo",
                    "clock.source.mode",
                    "clock.source.pulse",
                    "clock.source.stretch",
                ],
                ValueKind::Text,
                &["source"],
            );
            self.add(
                Command,
                &[
                    "clock.toggle",
                    "clock.close",
                    "clock.nudge_up",
                    "clock.nudge_down",
                    "clock.family.step",
                    "clock.family.leap",
                    "clock.grid.toggle_quantize",
                    "clock.grid.toggle_snap",
                    "clock.grid.toggle_click",
                    "clock.link.toggle",
                    "clock.midi.toggle_send",
                    "clock.tap",
                    "clock.half",
                    "clock.double",
                    "clock.reset",
                ],
                ValueKind::Trigger,
                &[],
            );
            self.add(
                Command,
                &["clock.source.select"],
                ValueKind::Trigger,
                &["source"],
            );
        }
    }

    impl EndpointRegistry for Registry {
        fn endpoint(&self, category: EndpointCategory, id: &EndpointId) -> Option<&EndpointDesc> {
            self.endpoints.get(&(category, id.0.clone()))
        }
    }

    #[kithara::test]
    fn builtin_presets_build_preview_geometry() {
        let registry = Registry::preset_surface();
        let geometry = [builtin::MICRO_PRESET, builtin::PLAYER_PRESET].map(|preset| {
            let ui = compile(
                preset,
                &builtin::resolver(),
                &registry,
                builtin::skin_doc(),
                builtin::text_doc(),
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
