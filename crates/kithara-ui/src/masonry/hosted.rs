use std::{
    cell::{OnceCell, Ref, RefCell},
    rc::{Rc, Weak},
};

use kithara_test_macros as kithara;

use crate::{
    atoms::{
        bar::context::Context,
        design::fader::rail_bounds as fader_bounds,
        table::{
            TableRowData, column_layouts, column_resizable, empty_bounds,
            face::{Drawn, TableFace},
            table_body, table_divider_hit, table_dividers, table_overflows, table_row_at,
            table_visible_row_rect,
        },
        tree::Tree,
    },
    draw::{Pt, Rect},
    engine::{Engine, Target, TextInputSnapshot},
    expand::{Binding, ControlSpec},
    hosts::{
        hosted::{HostedControlPlan, HostedState, Resolving, SearchPlan, TablePlan, TreePlan},
        picker::picker_hits,
    },
    ids::InternId,
    interact::Hit,
    masonry::paint::tree::Drawn as TreeDrawn,
    module::{TableColumn, TableFrame},
    mount,
    render::{ReadValue, Skin, document::Ctx},
};

#[derive(Clone)]
pub(crate) struct MasonryHostedState;

impl HostedState for MasonryHostedState {
    type Tree = TreeState;
    type Table = TableState;
    type Search = SearchState;

    fn bind_tree(
        state: &Self::Tree,
        tree: &mount::panel::tree::host::Tree<'_>,
        read: Option<&Binding>,
        cx: Resolving<'_>,
    ) {
        let Resolving { ctx, .. } = cx;
        let source = TreeSource::new(
            read.map(|binding| ctx.ui.resolve(binding.key).to_owned()),
            tree.search,
            tree.query
                .map(|binding| ctx.ui.resolve(binding.key).to_owned()),
        );
        let _ = state.source.get_or_init(|| source);
    }

    fn bind_table(
        state: &Self::Table,
        table: &mount::panel::table::host::Table<'_>,
        read: Option<&Binding>,
        cx: Resolving<'_>,
    ) {
        let source = TableSource::new(table, cx.ctx, read);
        let _ = state.source.get_or_init(|| source);
    }

    fn bind_search(state: &Self::Search, read: Option<&Binding>, cx: Resolving<'_>) {
        let endpoint = read.map(|binding| cx.ctx.ui.resolve(binding.key).to_owned());
        let _ = state.source.get_or_init(|| endpoint);
    }
}

pub(crate) fn hosted_control_plan(
    path: InternId,
    spec: &ControlSpec,
    read: Option<&Binding>,
    ctx: Ctx<'_, '_>,
    skin: &Skin,
) -> Option<HostedControlPlan<MasonryHostedState>> {
    HostedControlPlan::resolved(
        ctx.ui.resolve(path),
        spec,
        read.and_then(|binding| ctx.read(binding)),
        read,
        ctx.scope(read),
        Resolving { skin, ctx },
    )
}

pub(crate) trait TableProjection {
    fn project(&self, plan: &TablePlan<MasonryHostedState>) -> Option<Drawn>;
    fn reconcile(&self);
}

pub(crate) trait TreeProjection {
    fn project(&self, plan: &TreePlan<MasonryHostedState>) -> Option<TreeDrawn>;
    fn reconcile(&self);
}

pub(crate) trait SearchProjection {
    fn project(&self, plan: &SearchPlan<MasonryHostedState>) -> Option<TextInputSnapshot>;
    fn reconcile(&self);
}

#[derive(Clone, Default)]
pub(crate) struct SearchState {
    projection: Rc<OnceCell<Weak<dyn SearchProjection>>>,
    source: Rc<OnceCell<Option<String>>>,
}

impl SearchPlan<MasonryHostedState> {
    pub(crate) fn bind_projection(&self, projection: Weak<dyn SearchProjection>) {
        let _ = self.state.projection.get_or_init(|| projection);
    }

    pub(crate) fn drawn(&self) -> Option<TextInputSnapshot> {
        self.state
            .projection
            .get()
            .and_then(Weak::upgrade)?
            .project(self)
    }

    pub(crate) fn refresh(&self, ctx: Ctx<'_, '_>) -> bool {
        let query = self
            .state
            .source
            .get()
            .and_then(Option::as_ref)
            .and_then(|endpoint| ctx.get(endpoint))
            .and_then(|value| match value {
                ReadValue::Text(query) => Some(query),
                _ => None,
            })
            .unwrap_or_default();
        if self.picture.borrow().query() == query {
            return false;
        }
        let next = crate::atoms::search::Search::new(query, self.picture.borrow().skin());
        *self.picture.borrow_mut() = next;
        if let Some(projection) = self.state.projection.get().and_then(Weak::upgrade) {
            projection.reconcile();
        }
        true
    }
}

#[derive(Clone, Default)]
pub(crate) struct TableState {
    projection: Rc<OnceCell<Weak<dyn TableProjection>>>,
    reported_missing: Rc<RefCell<Vec<String>>>,
    source: Rc<OnceCell<TableSource>>,
}

pub(crate) struct TableSource {
    columns_state: Option<(String, String)>,
    rows: Option<String>,
    status: Option<String>,
    frame: TableFrame,
    columns: Vec<TableColumn>,
    width: Option<String>,
}

#[derive(Clone, Default)]
pub(crate) struct TreeState {
    projection: Rc<OnceCell<Weak<dyn TreeProjection>>>,
    reported_missing: Rc<RefCell<Vec<String>>>,
    source: Rc<OnceCell<TreeSource>>,
}

pub(crate) struct TreeSource {
    search: bool,
    query: Option<String>,
    rows: Option<String>,
}

impl HostedControlPlan<MasonryHostedState> {
    pub(crate) fn carried(&self, path: &str, index: usize) -> Option<crate::hosts::drag::Carried> {
        match self {
            Self::Table(plan) if plan.path == path => plan.carried(index),
            _ => None,
        }
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
            Self::Search(plan) => targets.push(Target::new(
                &plan.path,
                Hit::new(
                    point,
                    crate::atoms::search::input_bounds(bounds, plan.picture.borrow().skin()),
                ),
            )),
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
            Self::Activation { path, .. }
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

impl TreePlan<MasonryHostedState> {
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
        if let Some(search) = &self.search_path {
            targets.push(Target::new(
                search,
                Hit::new(point, picture.search_input_bounds(bounds)),
            ));
        }
        let rows = picture.rows_bounds(bounds);
        targets.push(Target::new(&self.path, Hit::new(point, rows)));
        self.append_toggle_targets(rows, point, view.offset, targets);
    }

    pub(crate) fn bind_projection(&self, projection: Weak<dyn TreeProjection>) {
        let _ = self.state.projection.get_or_init(|| projection);
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
        let search = match &self.search_path {
            Some(path) => engine
                .text_input_snapshot(path)
                .ok_or(MissingEntry { entry: path })?,
            None => TextInputSnapshot::default(),
        };
        let picture = self.picture.borrow();
        Ok(TreeDrawn {
            hovered: picture.hovered_row(point, bounds, offset),
            offset,
            search,
        })
    }

    pub(crate) fn drawn(&self) -> Option<TreeDrawn> {
        self.projection()
            .and_then(|projection| projection.project(self))
    }

    fn projection(&self) -> Option<Rc<dyn TreeProjection>> {
        let projection = self.state.projection.get().and_then(Weak::upgrade);
        if projection.is_none() {
            self.report_missing("retained projection");
        }
        projection
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
        kithara::probe_event!(
            masonry_tree_refreshed,
            rows = next.row_count(),
            query_chars = next.query().map_or(0, |query| query.chars().count())
        );
        *self.picture.borrow_mut() = next;
        if let Some(projection) = self.projection() {
            projection.reconcile();
        }
        true
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
}

impl TablePlan<MasonryHostedState> {
    fn carried(&self, index: usize) -> Option<crate::hosts::drag::Carried> {
        self.picture.borrow().carried(index)
    }

    fn append_targets<'a>(
        &'a self,
        bounds: Rect,
        point: Option<Pt>,
        engine: &Engine,
        targets: &mut Vec<Target<'a>>,
    ) {
        self.viewport_width.set(bounds.w);
        let Some(view) = self.view(engine, point, bounds) else {
            return;
        };
        let picture = self.picture();
        let overflows = table_overflows(&view.columns, bounds.w, picture.metrics());
        if overflows {
            targets.push(Target::new(&self.horizontal_path, Hit::new(point, bounds)));
        }
        targets.push(Target::new(
            &self.path,
            Hit::new(point, table_body(bounds, picture.metrics())),
        ));
        let row = view.hovered.and_then(|index| {
            table_visible_row_rect(
                bounds,
                &view.columns,
                picture.rows().len(),
                index,
                view.horizontal,
                view.vertical,
                picture.metrics(),
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
        self.append_action_targets(
            bounds,
            point,
            &view.columns,
            (view.horizontal, view.vertical),
            targets,
        );
        let dividers = table_dividers(bounds, &view.columns, view.horizontal, picture.metrics());
        for (index, column) in view.columns.iter().enumerate() {
            if !column_resizable(&view.columns, index) {
                continue;
            }
            let divider_path = self.divider_path(&column.column);
            let hit = table_divider_hit(
                bounds,
                &dividers,
                column.column.id(),
                engine.captures(divider_path),
            );
            if let Some(hit) = hit {
                targets.push(Target::new(divider_path, Hit::new(point, hit)));
            }
        }
    }

    pub(crate) fn bind_projection(&self, projection: Weak<dyn TableProjection>) {
        let _ = self.state.projection.get_or_init(|| projection);
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
            .collect::<Vec<(usize, &str)>>();
        for (index, path) in resizable {
            let width = engine
                .column_divider_value(path)
                .ok_or(MissingEntry { entry: path })?;
            columns[index].width = width;
        }
        let overflows = table_overflows(&columns, bounds.w, picture.metrics());
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
            picture.metrics(),
        );
        Ok(Drawn {
            hovered,
            pressed,
            columns,
            horizontal,
            vertical,
        })
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

    pub(crate) fn refresh(&self, ctx: Ctx<'_, '_>) -> bool {
        if !self.refresh_picture(ctx) {
            return false;
        }
        if let Some(projection) = self.projection() {
            projection.reconcile();
        }
        true
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

    pub(crate) fn report_missing(&self, entry: &str) {
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
}

impl TableSource {
    pub(crate) fn new(
        table: &mount::panel::table::host::Table<'_>,
        ctx: Ctx<'_, '_>,
        read: Option<&Binding>,
    ) -> Self {
        Self {
            columns: table.columns.to_vec(),
            width: ctx.endpoint(table.width).map(str::to_owned),
            frame: table.frame,
            columns_state: table.columns_state.map(|binding| {
                (
                    ctx.ui.resolve(binding.id).to_owned(),
                    ctx.scope(Some(binding)).to_owned(),
                )
            }),
            rows: ctx.endpoint(read).map(str::to_owned),
            status: ctx.endpoint(table.status).map(str::to_owned),
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
        let columns = column_layouts((&self.columns, self.width.as_deref()), &ctx, state, skin);
        TableFace::new(rows, columns, skin, self.frame).with_status(
            self.status
                .as_deref()
                .and_then(|endpoint| ctx.get(endpoint)),
        )
    }
}

impl TreeSource {
    pub(crate) fn new(rows: Option<String>, search: bool, query: Option<String>) -> Self {
        Self {
            search,
            query,
            rows,
        }
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
        let query = self.search.then(|| {
            self.query
                .as_deref()
                .and_then(|endpoint| ctx.get(endpoint))
                .and_then(|value| match value {
                    ReadValue::Text(query) => Some(query),
                    _ => None,
                })
                .unwrap_or_default()
        });
        Tree::new(rows, query, skin)
    }
}

struct MissingEntry<'a> {
    entry: &'a str,
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::*;
    use crate::{
        atoms::table::{ColumnLayout, TableRowData},
        builtin,
        interact::{Input, PointerPhase, Scroll, mouse as mouse_input},
        module::{TableColumn, TableFrame},
    };

    #[kithara::test]
    fn tree_targets_keep_search_and_rows_disjoint() {
        let skin = builtin::skin();
        let plan = HostedControlPlan::<MasonryHostedState>::Tree(Box::new(TreePlan {
            path: "tree".to_owned(),
            picture: Rc::new(RefCell::new(Tree::new(&[], Some(""), skin))),
            search_path: Some("tree/search".to_owned()),
            toggle_path: None,
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
        let plan = HostedControlPlan::<MasonryHostedState>::Picker {
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
                resizable: true,
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
                resizable: true,
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
        let plan = table_plan("tracks", table_rows(8), columns, skin);
        let bounds = Rect {
            x: 0.0,
            y: 0.0,
            w: 140.0,
            h: 180.0,
        };
        let body = table_body(
            bounds,
            crate::atoms::table::TableMetrics {
                skin,
                frame: TableFrame::new(0.0, 0.0, true),
            },
        );
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
    fn library_followup_table_survives_narrow_wide_narrow() {
        let skin = builtin::skin();
        let columns = vec![
            ColumnLayout {
                resizable: true,
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
                resizable: true,
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
        let plan =
            HostedControlPlan::Table(Box::new(table_plan("tracks", table_rows(8), columns, skin)));
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

        engine.reconcile(plan.descriptors());
        assert!(engine.scroll_offset("tracks/scroll-x").is_some());
        let mut narrowed_targets = Vec::new();
        plan.append_targets(narrow, point, Some(&engine), &mut narrowed_targets);
        assert!(
            narrowed_targets
                .iter()
                .any(|target| target.path == "tracks/rows")
        );
    }

    /// A table plan over fixed columns, bound to a source that declares them.
    fn table_plan(
        path: &str,
        rows: Vec<TableRowData>,
        columns: Vec<ColumnLayout>,
        skin: &Skin,
    ) -> TablePlan<MasonryHostedState> {
        let declared: Vec<TableColumn> =
            columns.iter().map(|column| column.column.clone()).collect();
        let plan = TablePlan::<MasonryHostedState>::new(
            path,
            TableFace::new(rows, columns, skin, TableFrame::new(0.0, 0.0, true)),
        );
        let _ = plan.state.source.get_or_init(|| TableSource {
            columns: declared,
            columns_state: None,
            status: None,
            rows: None,
            frame: TableFrame::new(0.0, 0.0, true),
            width: Some("width".to_owned()),
        });
        plan
    }

    fn table_rows(count: usize) -> Vec<TableRowData> {
        (0..count)
            .map(|index| {
                let name = format!("Row {index}");
                TableRowData::from(&crate::render::TableRow::new(
                    vec![crate::render::TableCell::text("name", &name)],
                    false,
                ))
            })
            .collect()
    }
}
