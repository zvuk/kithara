use super::plan::{HostedControlPlan, TrackListPlan};
use crate::{
    atoms::{bar::context::Context, design::fader::rail_bounds as fader_bounds},
    compile::CompiledUi,
    draw::{Pt, Rect},
    engine::{Descriptor, Engine, Target},
    expand::{Binding, ControlSpec},
    ids::InternId,
    interact::{Hit, ScrollAxis},
    render::{
        Reads, Skin,
        document::read::{read_scope, resolve},
        picker_hits,
    },
    widgets::track_list::{
        track_list_body, track_list_dividers, track_list_overflows, track_list_row_at,
        track_list_visible_divider_hit, track_list_visible_row_rect,
    },
};

#[derive(Clone)]
pub(crate) struct TreeMetrics {
    pub(super) search_height: f32,
    pub(super) search_icon_width: f32,
    pub(super) panel_padding_top: f32,
    pub(super) panel_padding_bottom: f32,
}

pub(crate) fn hosted_control_plan(
    path: InternId,
    spec: &ControlSpec,
    read: Option<&Binding>,
    ui: &CompiledUi,
    reads: &dyn Reads,
    skin: &Skin,
) -> Option<HostedControlPlan> {
    HostedControlPlan::resolved(
        ui.resolve(path),
        spec,
        read.and_then(|binding| resolve(reads, binding, ui)),
        read_scope(read, ui),
        reads,
        ui,
        skin,
    )
}

impl HostedControlPlan {
    pub(crate) fn active_descriptors(&self, targets: &[Target<'_>]) -> Vec<Descriptor> {
        self.descriptors()
            .into_iter()
            .filter(|descriptor| match descriptor {
                Descriptor::Scroll { path, config } if config.axis() == ScrollAxis::Horizontal => {
                    targets.iter().any(|target| target.path == path)
                }
                _ => true,
            })
            .collect()
    }

    pub(crate) fn append_targets<'a>(
        &'a self,
        bounds: Rect,
        point: Option<Pt>,
        engine: Option<&Engine>,
        targets: &mut Vec<Target<'a>>,
    ) {
        match self {
            Self::Picker {
                path,
                item_count,
                item_height,
                face,
                ..
            } => {
                let anchor = Context::placed(*face, bounds);
                targets.push(Target::new(path, Hit::new(point, anchor)));
                if engine
                    .and_then(|engine| engine.picker_snapshot(path))
                    .is_some_and(|snapshot| snapshot.open)
                {
                    for region in picker_hits(anchor, *item_height, *item_count) {
                        targets.push(Target::item(
                            path,
                            Hit::new(point, region.area()),
                            *region.action(),
                        ));
                    }
                }
            }
            Self::Tree {
                path,
                search_path,
                metrics,
                ..
            } => {
                let divider = 1.0;
                let search = Rect {
                    x: bounds.x + metrics.search_icon_width + divider,
                    y: bounds.y,
                    w: (bounds.w - metrics.search_icon_width - divider).max(0.0),
                    h: metrics.search_height,
                };
                targets.push(Target::new(search_path, Hit::new(point, search)));
                let body_y = bounds.y + metrics.search_height + metrics.panel_padding_top;
                let body = Rect {
                    x: bounds.x,
                    y: body_y,
                    w: bounds.w,
                    h: (bounds.h
                        - metrics.search_height
                        - metrics.panel_padding_top
                        - metrics.panel_padding_bottom)
                        .max(0.0),
                };
                targets.push(Target::new(path, Hit::new(point, body)));
            }
            Self::TrackList(plan) => plan.append_targets(bounds, point, engine, targets),
            Self::Fader {
                path,
                style,
                labelled,
                metrics,
                ..
            } => targets.push(Target::new(
                path,
                Hit::new(point, fader_bounds(bounds, *style, *labelled, *metrics)),
            )),
            Self::Activation { path }
            | Self::Segmented { path, .. }
            | Self::Crossfader { path }
            | Self::Knob { path, .. }
            | Self::StereoMeter { path }
            | Self::VerticalVu { path }
            | Self::Wave { path }
            | Self::HeroWave { path, .. } => {
                targets.push(Target::new(path, Hit::new(point, bounds)));
            }
        }
    }
}

impl TrackListPlan {
    fn append_targets<'a>(
        &'a self,
        bounds: Rect,
        point: Option<Pt>,
        engine: Option<&Engine>,
        targets: &mut Vec<Target<'a>>,
    ) {
        let overflows = track_list_overflows(&self.columns, bounds.w);
        let horizontal = if overflows {
            engine
                .and_then(|engine| engine.scroll_offset(&self.horizontal_path))
                .unwrap_or(0.0)
        } else {
            0.0
        };
        let vertical = engine
            .and_then(|engine| engine.scroll_offset(&self.path))
            .unwrap_or(0.0);
        if overflows {
            targets.push(Target::new(&self.horizontal_path, Hit::new(point, bounds)));
        }
        targets.push(Target::new(
            &self.path,
            Hit::new(point, track_list_body(bounds, &self.skin)),
        ));
        let row_index = track_list_row_at(
            point,
            bounds,
            &self.columns,
            self.row_count,
            horizontal,
            vertical,
            &self.skin,
        );
        let row = row_index.and_then(|index| {
            track_list_visible_row_rect(
                bounds,
                &self.columns,
                self.row_count,
                index,
                horizontal,
                vertical,
                &self.skin,
            )
        });
        match (row_index, row) {
            (Some(index), Some(row)) => {
                targets.push(Target::item(&self.row_target, Hit::new(point, row), index));
            }
            _ => targets.push(Target::new(
                &self.row_target,
                Hit::new(point, empty_bounds(bounds)),
            )),
        }
        for (divider_path, divider) in self.divider_paths.iter().zip(track_list_dividers(
            bounds,
            &self.columns,
            horizontal,
            &self.skin,
        )) {
            let hit = track_list_visible_divider_hit(bounds, divider.hit).or_else(|| {
                engine
                    .filter(|engine| engine.captures(divider_path))
                    .map(|_| empty_bounds(bounds))
            });
            if let Some(hit) = hit {
                targets.push(Target::new(divider_path, Hit::new(point, hit)));
            }
        }
    }
}

const fn empty_bounds(bounds: Rect) -> Rect {
    Rect {
        x: bounds.x,
        y: bounds.y,
        w: 0.0,
        h: 0.0,
    }
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::*;
    use crate::{
        builtin,
        engine::ScrollConfig,
        interact::{Input, PointerPhase, Scroll, ScrollAxis, mouse as mouse_input},
        module::TrackColumn,
        render::text_input_layout,
        widgets::track_list::ColumnLayout,
    };

    #[kithara::test]
    fn tree_targets_keep_search_and_rows_disjoint() {
        let skin = builtin::skin();
        let plan = HostedControlPlan::Tree {
            path: "tree".to_owned(),
            query: String::new(),
            scroll: ScrollConfig::plain(ScrollAxis::Vertical, 400.0),
            search_layout: text_input_layout("", skin),
            search_path: "tree/search".to_owned(),
            metrics: TreeMetrics {
                search_height: skin.tree.search_height,
                search_icon_width: skin.tree.search_icon_width,
                panel_padding_top: skin.tree.panel_padding_top,
                panel_padding_bottom: skin.tree.panel_padding_bottom,
            },
        };
        let bounds = Rect {
            x: 5.0,
            y: 7.0,
            w: 200.0,
            h: 180.0,
        };
        let mut targets = Vec::new();
        plan.append_targets(bounds, None, None, &mut targets);

        assert_eq!(targets.len(), 2);
        assert_eq!(targets[0].path, "tree/search");
        assert_eq!(targets[0].hit.area().y, bounds.y);
        assert_eq!(targets[0].hit.area().h, skin.tree.search_height);
        assert_eq!(targets[1].path, "tree");
        assert_eq!(
            targets[1].hit.area().y,
            bounds.y + skin.tree.search_height + skin.tree.panel_padding_top
        );
        assert!(targets[0].hit.area().y + targets[0].hit.area().h <= targets[1].hit.area().y);
    }

    #[kithara::test]
    fn picker_plan_adds_typed_option_targets_only_while_open() {
        let skin = builtin::skin();
        let plan = HostedControlPlan::Picker {
            path: "scope".to_owned(),
            item_count: 2,
            item_height: 18.0,
            selected: Some(0),
            face: Rect {
                h: 30.0,
                w: 72.0,
                x: 24.0,
                y: 0.0,
            },
        };
        let bounds = Rect {
            x: 10.0,
            y: 20.0,
            w: 180.0,
            h: skin.tree.context_height,
        };
        let anchor_point = Pt { x: 40.0, y: 26.0 };
        let mut engine = Engine::default();
        engine.reconcile(plan.descriptors());
        let mut targets = Vec::new();
        plan.append_targets(bounds, Some(anchor_point), Some(&engine), &mut targets);
        assert_eq!(targets.len(), 1);
        assert!(
            engine
                .handle(
                    Input::Pointer(mouse_input(PointerPhase::Down, Some(anchor_point))),
                    &targets,
                    kithara_platform::time::Instant::now(),
                )
                .is_some()
        );

        let mut open = Vec::new();
        plan.append_targets(bounds, None, Some(&engine), &mut open);
        assert_eq!(open.len(), 3);
        assert_eq!(open[1].index, Some(0));
        assert_eq!(open[2].index, Some(1));
        assert_eq!(
            open[1].hit.area().y,
            open[0].hit.area().y + open[0].hit.area().h,
            "the menu hangs off the bottom of the face the strip drew"
        );
        assert_eq!(open[2].hit.area().y, open[1].hit.area().y + 18.0);
    }

    #[kithara::test]
    fn track_list_plan_keeps_scroll_rows_and_dividers_distinct() {
        let skin = builtin::skin();
        let columns = vec![
            ColumnLayout {
                column: TrackColumn::Index,
                width: 98.0,
            },
            ColumnLayout {
                column: TrackColumn::Title,
                width: 180.0,
            },
        ];
        let plan = TrackListPlan::new("tracks", columns, 8, skin);
        let bounds = Rect {
            x: 0.0,
            y: 0.0,
            w: 140.0,
            h: 180.0,
        };
        let body = track_list_body(bounds, skin);
        let point = Pt {
            x: 20.0,
            y: body.y + skin.track_list.row_height / 2.0,
        };
        let mut targets = Vec::new();
        plan.append_targets(bounds, Some(point), None, &mut targets);

        assert_eq!(targets[0].path, "tracks/scroll-x");
        assert_eq!(targets[1].path, "tracks");
        assert_eq!(targets[1].hit.area(), body);
        assert_eq!(targets[2].path, "tracks/rows");
        assert_eq!(targets[2].index, Some(0));
        assert!(
            targets
                .iter()
                .any(|target| target.path == "tracks/width/index")
        );
    }

    #[kithara::test]
    fn track_list_drops_horizontal_state_when_overflow_ends() {
        let skin = builtin::skin();
        let columns = vec![
            ColumnLayout {
                column: TrackColumn::Index,
                width: 98.0,
            },
            ColumnLayout {
                column: TrackColumn::Title,
                width: 180.0,
            },
        ];
        let plan =
            HostedControlPlan::TrackList(Box::new(TrackListPlan::new("tracks", columns, 8, skin)));
        let narrow = Rect {
            x: 0.0,
            y: 0.0,
            w: 140.0,
            h: 180.0,
        };
        let mut engine = Engine::default();
        engine.reconcile(plan.descriptors());
        engine.set_scroll_viewport("tracks/scroll-x", narrow);
        let wheel_target = Target::new(
            "tracks/scroll-x",
            Hit::new(Some(Pt { x: 20.0, y: 20.0 }), narrow),
        );
        assert!(
            engine
                .handle(
                    Input::Wheel(Scroll::Lines { x: -1.0, y: 0.0 }),
                    &[wheel_target],
                    kithara_platform::time::Instant::now(),
                )
                .is_some()
        );
        assert_eq!(engine.scroll_offset("tracks/scroll-x"), Some(60.0));

        let wide = Rect { w: 400.0, ..narrow };
        let point = Some(Pt { x: 90.0, y: 40.0 });
        let mut retained_targets = Vec::new();
        plan.append_targets(wide, point, Some(&engine), &mut retained_targets);
        let mut fresh_targets = Vec::new();
        plan.append_targets(wide, point, None, &mut fresh_targets);

        assert_eq!(retained_targets.len(), fresh_targets.len());
        for (retained, fresh) in retained_targets.iter().zip(&fresh_targets) {
            assert_eq!(retained.path, fresh.path);
            assert_eq!(retained.index, fresh.index);
            assert_eq!(retained.hit.area(), fresh.hit.area());
        }
        assert!(
            retained_targets
                .iter()
                .all(|target| target.path != "tracks/scroll-x")
        );

        engine.reconcile(plan.active_descriptors(&retained_targets));
        assert_eq!(engine.scroll_offset("tracks/scroll-x"), None);
    }
}
