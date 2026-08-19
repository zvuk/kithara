use std::{
    cell::{OnceCell, Ref, RefCell},
    rc::{Rc, Weak},
};

use super::plan::{HostedControlPlan, Resolving, TablePlan, TreePlan};
#[cfg(test)]
use crate::atoms::table::ColumnLayout;
use crate::{
    atoms::{
        bar::context::Context,
        design::fader::rail_bounds as fader_bounds,
        table::{
            TableRowData, column_layouts, column_resizable,
            face::{Drawn, TableFace},
            table_body, table_dividers, table_overflows, table_row_at, table_visible_divider_hit,
            table_visible_row_rect,
        },
        tree::{face::Tree, retained::Drawn as TreeDrawn},
    },
    draw::{Pt, Rect},
    engine::{Descriptor, Engine, Target},
    expand::{Binding, ControlSpec},
    ids::InternId,
    interact::{Hit, ScrollAxis},
    module::TableColumn,
    render::{ReadValue, Skin, document::Ctx, picker_hits},
};

pub(crate) fn hosted_control_plan(
    path: InternId,
    spec: &ControlSpec,
    read: Option<&Binding>,
    ctx: Ctx<'_, '_>,
    skin: &Skin,
) -> Option<HostedControlPlan> {
    HostedControlPlan::resolved(
        ctx.ui.resolve(path),
        spec,
        read.and_then(|binding| ctx.read(binding)),
        read,
        ctx.scope(read),
        Resolving { ctx, skin },
    )
}

pub(crate) trait TableProjection {
    fn project(&self, plan: &TablePlan) -> Option<Drawn>;
    fn reconcile(&self);
}

pub(crate) trait TreeProjection {
    fn project(&self, plan: &TreePlan) -> Option<TreeDrawn>;
    fn reconcile(&self);
}

#[derive(Clone, Default)]
pub(super) struct TableState {
    projection: Rc<OnceCell<Weak<dyn TableProjection>>>,
    reported_missing: Rc<RefCell<Vec<String>>>,
    source: Rc<OnceCell<TableSource>>,
}

pub(super) struct TableSource {
    columns: Vec<TableColumn>,
    columns_state: Option<(String, String)>,
    rows: Option<String>,
}

#[derive(Clone, Default)]
pub(super) struct TreeState {
    projection: Rc<OnceCell<Weak<dyn TreeProjection>>>,
    reported_missing: Rc<RefCell<Vec<String>>>,
    source: Rc<OnceCell<TreeSource>>,
}

pub(super) struct TreeSource {
    query: Option<String>,
    rows: Option<String>,
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
                items,
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
                    for region in picker_hits(anchor, *item_height, items.len()) {
                        targets.push(Target::item(
                            path,
                            Hit::new(point, region.area()),
                            *region.action(),
                        ));
                    }
                }
            }
            Self::Tree(plan) => {
                let Some(engine) = engine else {
                    plan.report_missing("retained engine");
                    return;
                };
                plan.append_targets(bounds, point, engine, targets);
            }
            Self::Table(plan) => {
                let Some(engine) = engine else {
                    plan.report_missing("retained engine");
                    return;
                };
                plan.append_targets(bounds, point, engine, targets);
            }
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
            | Self::Crossing { path }
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

impl TreePlan {
    pub(crate) fn bind_projection(&self, projection: Weak<dyn TreeProjection>) {
        let _ = self.state.projection.get_or_init(|| projection);
    }

    pub(super) fn bind_source(&self, source: TreeSource) {
        let _ = self.state.source.get_or_init(|| source);
    }

    pub(crate) fn refresh(&self, ctx: Ctx<'_, '_>) -> bool {
        let Some(source) = self.state.source.get() else {
            self.report_missing("tree source");
            return false;
        };
        let skin = self.picture.borrow().skin().clone();
        let next = source.picture(ctx, &skin);
        if *self.picture.borrow() == next {
            return false;
        }
        *self.picture.borrow_mut() = next;
        if let Some(projection) = self.projection() {
            projection.reconcile();
        }
        true
    }

    pub(crate) fn drawn(&self) -> Option<TreeDrawn> {
        self.projection()
            .and_then(|projection| projection.project(self))
    }

    pub(crate) fn picture(&self) -> Ref<'_, Tree> {
        self.picture.borrow()
    }

    pub(crate) fn view(
        &self,
        engine: &Engine,
        point: Option<Pt>,
        bounds: Rect,
    ) -> Option<TreeDrawn> {
        match self.complete_view(engine, point, bounds) {
            Ok(view) => Some(view),
            Err(missing) => {
                self.report_missing(missing.entry);
                None
            }
        }
    }

    fn complete_view<'a>(
        &'a self,
        engine: &Engine,
        point: Option<Pt>,
        bounds: Rect,
    ) -> Result<TreeDrawn, MissingEntry<'a>> {
        let offset = engine
            .scroll_offset(&self.path)
            .ok_or(MissingEntry { entry: &self.path })?;
        let search = engine
            .text_input_snapshot(&self.search_path)
            .ok_or(MissingEntry {
                entry: &self.search_path,
            })?;
        let picture = self.picture.borrow();
        Ok(TreeDrawn {
            hovered: picture.hovered_row(point, bounds, offset),
            offset,
            search,
        })
    }

    fn projection(&self) -> Option<Rc<dyn TreeProjection>> {
        let projection = self.state.projection.get().and_then(Weak::upgrade);
        if projection.is_none() {
            self.report_missing("retained projection");
        }
        projection
    }

    fn report_missing(&self, entry: &str) {
        let mut reported = self.state.reported_missing.borrow_mut();
        if reported.iter().any(|candidate| candidate == entry) {
            return;
        }
        reported.push(entry.to_owned());
        tracing::error!(
            control_path = self.path,
            engine_entry = entry,
            "Tree projection is incomplete"
        );
    }

    fn append_targets<'a>(
        &'a self,
        bounds: Rect,
        point: Option<Pt>,
        engine: &Engine,
        targets: &mut Vec<Target<'a>>,
    ) {
        if self.view(engine, point, bounds).is_none() {
            return;
        }
        let picture = self.picture();
        targets.push(Target::new(
            &self.search_path,
            Hit::new(point, picture.search_input_bounds(bounds)),
        ));
        targets.push(Target::new(
            &self.path,
            Hit::new(point, picture.rows_bounds(bounds)),
        ));
    }
}

impl TablePlan {
    #[cfg(test)]
    pub(crate) fn fixture(
        path: &str,
        rows: Vec<TableRowData>,
        columns: Vec<ColumnLayout>,
        skin: &Skin,
    ) -> Self {
        let declared = columns.iter().map(|column| column.column.clone()).collect();
        let plan = Self::new(path, rows, columns, skin);
        plan.bind_source(TableSource::new(declared, None, None));
        plan
    }

    pub(crate) fn refresh(&self, ctx: Ctx<'_, '_>) -> bool {
        if !self.refresh_picture(ctx) {
            return false;
        }
        if let Some(projection) = self.projection() {
            projection.reconcile();
        }
        true
    }

    pub(crate) fn bind_projection(&self, projection: Weak<dyn TableProjection>) {
        let _ = self.state.projection.get_or_init(|| projection);
    }

    pub(super) fn bind_source(&self, source: TableSource) {
        let _ = self.state.source.get_or_init(|| source);
    }

    fn refresh_picture(&self, ctx: Ctx<'_, '_>) -> bool {
        let Some(source) = self.state.source.get() else {
            self.report_missing("track-list source");
            return false;
        };
        let skin = self.picture.borrow().skin().clone();
        let next = source.picture(ctx, &skin);
        if *self.picture.borrow() == next {
            return false;
        }
        *self.picture.borrow_mut() = next;
        true
    }

    pub(crate) fn drawn(&self) -> Option<Drawn> {
        self.projection()
            .and_then(|projection| projection.project(self))
    }

    pub(crate) fn picture(&self) -> Ref<'_, TableFace> {
        self.picture.borrow()
    }

    fn projection(&self) -> Option<Rc<dyn TableProjection>> {
        let projection = self.state.projection.get().and_then(Weak::upgrade);
        if projection.is_none() {
            self.report_missing("retained projection");
        }
        projection
    }

    pub(super) fn report_missing(&self, entry: &str) {
        let mut reported = self.state.reported_missing.borrow_mut();
        if reported.iter().any(|candidate| candidate == entry) {
            return;
        }
        reported.push(entry.to_owned());
        tracing::error!(
            control_path = self.path,
            engine_entry = entry,
            "Table projection is incomplete"
        );
    }

    pub(crate) fn view(&self, engine: &Engine, point: Option<Pt>, bounds: Rect) -> Option<Drawn> {
        match self.complete_view(engine, point, bounds) {
            Ok(view) => Some(view),
            Err(missing) => {
                self.report_missing(missing.entry);
                None
            }
        }
    }

    fn complete_view<'a>(
        &'a self,
        engine: &Engine,
        point: Option<Pt>,
        bounds: Rect,
    ) -> Result<Drawn, MissingEntry<'a>> {
        let picture = self.picture();
        let mut columns = picture.columns().to_vec();
        let resizable = columns
            .iter()
            .enumerate()
            .filter(|(index, _)| column_resizable(&columns, *index))
            .map(|(index, column)| (index, self.divider_path(&column.column)))
            .collect::<Vec<_>>();
        for (index, path) in resizable {
            let width = engine
                .column_divider_value(path)
                .ok_or(MissingEntry { entry: path })?;
            columns[index].width = width;
        }
        let overflows = table_overflows(&columns, bounds.w);
        let horizontal = if overflows {
            engine
                .scroll_offset(&self.horizontal_path)
                .ok_or(MissingEntry {
                    entry: &self.horizontal_path,
                })?
        } else {
            0.0
        };
        let vertical = engine
            .scroll_offset(&self.path)
            .ok_or(MissingEntry { entry: &self.path })?;
        let pressed = engine.item_pressed(&self.path).ok_or(MissingEntry {
            entry: &self.row_target,
        })?;
        let hovered = table_row_at(
            point,
            bounds,
            &columns,
            picture.rows().len(),
            horizontal,
            vertical,
            picture.skin(),
        );
        Ok(Drawn {
            columns,
            horizontal,
            hovered,
            pressed,
            vertical,
        })
    }

    fn append_targets<'a>(
        &'a self,
        bounds: Rect,
        point: Option<Pt>,
        engine: &Engine,
        targets: &mut Vec<Target<'a>>,
    ) {
        let Some(view) = self.view(engine, point, bounds) else {
            return;
        };
        let picture = self.picture();
        let overflows = table_overflows(&view.columns, bounds.w);
        if overflows {
            targets.push(Target::new(&self.horizontal_path, Hit::new(point, bounds)));
        }
        targets.push(Target::new(
            &self.path,
            Hit::new(point, table_body(bounds, picture.skin())),
        ));
        let row = view.hovered.and_then(|index| {
            table_visible_row_rect(
                bounds,
                &view.columns,
                picture.rows().len(),
                index,
                view.horizontal,
                view.vertical,
                picture.skin(),
            )
        });
        match (view.hovered, row) {
            (Some(index), Some(row)) => {
                targets.push(Target::item(&self.row_target, Hit::new(point, row), index));
            }
            _ => targets.push(Target::new(
                &self.row_target,
                Hit::new(point, empty_bounds(bounds)),
            )),
        }
        for divider in table_dividers(bounds, &view.columns, view.horizontal, picture.skin()) {
            let divider_path = self.divider_path(&divider.column);
            let hit = table_visible_divider_hit(bounds, divider.hit)
                .or_else(|| engine.captures(divider_path).then(|| empty_bounds(bounds)));
            if let Some(hit) = hit {
                targets.push(Target::new(divider_path, Hit::new(point, hit)));
            }
        }
    }
}

impl TableSource {
    pub(super) fn new(
        columns: Vec<TableColumn>,
        columns_state: Option<(String, String)>,
        rows: Option<String>,
    ) -> Self {
        Self {
            columns,
            columns_state,
            rows,
        }
    }

    fn picture(&self, ctx: Ctx<'_, '_>, skin: &Skin) -> TableFace {
        let rows = self
            .rows
            .as_deref()
            .and_then(|endpoint| ctx.get(endpoint))
            .and_then(|value| match value {
                ReadValue::Table(rows) => Some(rows),
                _ => None,
            })
            .map_or_else(Vec::new, |rows| {
                rows.iter().map(TableRowData::from).collect()
            });
        let state = self
            .columns_state
            .as_ref()
            .map(|(prefix, scope)| (prefix.as_str(), scope.as_str()));
        let columns = column_layouts(&self.columns, &ctx, state, skin);
        TableFace::new(rows, columns, skin)
    }
}

impl TreeSource {
    pub(super) fn new(rows: Option<String>, query: Option<String>) -> Self {
        Self { query, rows }
    }

    fn picture(&self, ctx: Ctx<'_, '_>, skin: &Skin) -> Tree {
        let rows = self
            .rows
            .as_deref()
            .and_then(|endpoint| ctx.get(endpoint))
            .and_then(|value| match value {
                ReadValue::Tree(rows) => Some(rows),
                _ => None,
            })
            .unwrap_or_default();
        let query = self
            .query
            .as_deref()
            .and_then(|endpoint| ctx.get(endpoint))
            .and_then(|value| match value {
                ReadValue::Text(query) => Some(query),
                _ => None,
            })
            .unwrap_or_default();
        Tree::new(rows, query, skin)
    }
}

struct MissingEntry<'a> {
    entry: &'a str,
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
        atoms::table::{ColumnLayout, TableRowData},
        builtin,
        interact::{Input, PointerPhase, Scroll, mouse as mouse_input},
        module::TableColumn,
    };

    #[kithara::test]
    fn tree_targets_keep_search_and_rows_disjoint() {
        let skin = builtin::skin();
        let plan = HostedControlPlan::Tree(Box::new(TreePlan {
            path: "tree".to_owned(),
            picture: Rc::new(RefCell::new(Tree::new(&[], "", skin))),
            search_path: "tree/search".to_owned(),
            state: TreeState::default(),
        }));
        let bounds = Rect {
            x: 5.0,
            y: 7.0,
            w: 200.0,
            h: 180.0,
        };
        let mut targets = Vec::new();
        let mut engine = Engine::default();
        engine.reconcile(plan.descriptors());
        plan.append_targets(bounds, None, Some(&engine), &mut targets);

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
            items: vec!["ZVUK".to_owned(), "LOCAL".to_owned()],
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
    fn table_plan_keeps_scroll_rows_and_dividers_distinct() {
        let skin = builtin::skin();
        let columns = vec![
            ColumnLayout {
                column: TableColumn::new(
                    "index",
                    "#",
                    crate::module::TableColumnStyle::Index,
                    28.0,
                    false,
                ),
                width: 98.0,
            },
            ColumnLayout {
                column: TableColumn::new(
                    "name",
                    "NAME",
                    crate::module::TableColumnStyle::Primary,
                    180.0,
                    true,
                ),
                width: 180.0,
            },
        ];
        let plan = TablePlan::fixture("tracks", table_rows(8), columns, skin);
        let bounds = Rect {
            x: 0.0,
            y: 0.0,
            w: 140.0,
            h: 180.0,
        };
        let body = table_body(bounds, skin);
        let point = Pt {
            x: 20.0,
            y: body.y + skin.table.row_height / 2.0,
        };
        let mut engine = Engine::default();
        engine.reconcile(HostedControlPlan::Table(Box::new(plan.clone())).descriptors());
        let mut targets = Vec::new();
        plan.append_targets(bounds, Some(point), &engine, &mut targets);

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
    fn table_drops_horizontal_state_when_overflow_ends() {
        let skin = builtin::skin();
        let columns = vec![
            ColumnLayout {
                column: TableColumn::new(
                    "index",
                    "#",
                    crate::module::TableColumnStyle::Index,
                    28.0,
                    false,
                ),
                width: 98.0,
            },
            ColumnLayout {
                column: TableColumn::new(
                    "name",
                    "NAME",
                    crate::module::TableColumnStyle::Primary,
                    180.0,
                    true,
                ),
                width: 180.0,
            },
        ];
        let plan = HostedControlPlan::Table(Box::new(TablePlan::fixture(
            "tracks",
            table_rows(8),
            columns,
            skin,
        )));
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
        let mut fresh_engine = Engine::default();
        fresh_engine.reconcile(plan.descriptors());
        let mut fresh_targets = Vec::new();
        plan.append_targets(wide, point, Some(&fresh_engine), &mut fresh_targets);

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

    fn table_rows(count: usize) -> Vec<TableRowData> {
        (0..count)
            .map(|index| {
                TableRowData::new(
                    vec![(
                        "name".to_owned(),
                        crate::atoms::table::TableCell::Text(format!("Row {index}")),
                    )],
                    false,
                )
            })
            .collect()
    }
}
