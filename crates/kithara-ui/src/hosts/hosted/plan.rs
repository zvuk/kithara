use std::{
    cell::{Cell, Ref, RefCell},
    ops::Range,
    rc::Rc,
};

use num_traits::cast::AsPrimitive;

use crate::{
    atoms::{
        bar::context::Context,
        table::{
            ColumnLayout, TableCell, TableRowData, column_layouts, column_resizable,
            column_resize_track, empty_bounds, face::TableFace, table_content_height,
        },
        text_input::text_input_layout,
        tree::Tree,
        wave::zoom_math::{window_bounds, zoom_for_wheel},
    },
    draw::{Pt, Rect},
    engine::{Descriptor, Presses, ScrollConfig, Target},
    expand::{Binding, ControlSpec, drop_path},
    hosts::{hosted::SearchPlan, model::derived, picker::picker_selected_index},
    ids::InternId,
    interact::{CursorShape, Hit, Hover, ScrollAxis, recognizers::WheelStep},
    module::{FaderStyle, TableColumn, WaveStyle},
    mount::panel::{table::host::Table as TableControl, tree::host::Tree as TreeControl},
    render::{ReadValue, Skin, TableRow, TreeRow, Zoom, document::Ctx},
    shaping::TextContext,
};
/// What a control plan is resolved against: the compiled document that names
/// things, the model that answers a reading, and the skin that sizes it.
#[derive(Clone, Copy)]
pub(crate) struct Resolving<'a> {
    pub(crate) skin: &'a Skin,
    pub(crate) ctx: Ctx<'a, 'a>,
}

pub(crate) trait HostedState: Clone {
    type Tree: Clone + Default;
    type Table: Clone + Default;
    type Search: Clone + Default;

    fn bind_tree(
        state: &Self::Tree,
        tree: &TreeControl<'_>,
        read: Option<&Binding>,
        cx: Resolving<'_>,
    );

    fn bind_table(
        state: &Self::Table,
        table: &TableControl<'_>,
        read: Option<&Binding>,
        cx: Resolving<'_>,
    );

    fn bind_search(state: &Self::Search, read: Option<&Binding>, cx: Resolving<'_>);
}

#[derive(Clone)]
pub(crate) enum HostedControlPlan<S: HostedState> {
    Activation {
        path: String,
        presses: Presses,
    },
    /// A box that reports the pointer crossing into and out of it.
    ///
    /// This is what a document's `drop:` amounts to: the module never takes the
    /// pointer, it only says when a hand carrying something is over it.
    Crossing {
        path: String,
    },
    Segmented {
        path: String,
        item_count: usize,
    },
    Picker {
        path: String,
        /// The words the menu offers, in the order it offers them. The count
        /// alone answers hit-testing; the words are what the open menu draws,
        /// and the host that raises it has no other source for them.
        items: Vec<String>,
        item_height: f32,
        selected: Option<usize>,
        /// Where the strip put its closed face, as an offset from the strip's
        /// own corner. Both hosts hit-test and anchor the menu against the box
        /// the painter drew, rather than measuring the same parts again.
        face: Rect,
    },
    Search(Box<SearchPlan<S>>),
    Tree(Box<TreePlan<S>>),
    Table(Box<TablePlan<S>>),
    Fader {
        path: String,
        style: FaderStyle,
        labelled: bool,
        drag_step: Option<f64>,
        wheel: Option<WheelStep>,
        metrics: crate::skin::FaderSkin,
    },
    Crossfader {
        path: String,
    },
    Knob {
        path: String,
        current: f32,
        drag_range: f32,
        wheel_step: f32,
    },
    StereoMeter {
        path: String,
    },
    VerticalVu {
        path: String,
    },
    Wave {
        path: String,
    },
    HeroWave {
        path: String,
        /// Where the deck this wave belongs to answers from, kept so the window
        /// below can be re-read once the tree is standing.
        scope: String,
        zoom: Option<Binding>,
        window: Cell<HeroWindow>,
    },
}

/// The stretch of a track a hero wave is showing, which is what a hand on it is
/// measured against.
///
/// It moves with the playhead and with the zoom, so it is not a property of the
/// document: it is what the deck reads right now. A host that rebuilds its tree
/// every frame gets a new one for free; one that keeps a tree re-reads this in
/// place, or every gesture is measured against the window that happened to be
/// on screen when the deck was mounted.
#[derive(Clone, Copy, Default)]
pub(crate) struct HeroWindow {
    scale: Zoom,
    end: f32,
    progress: f32,
    start: f32,
    wheel_non_positive: f32,
    wheel_positive: f32,
}

impl HeroWindow {
    /// What the deck at `scope` is showing this frame.
    fn read(scope: &str, zoom: Option<&Binding>, ctx: Ctx<'_, '_>) -> Self {
        let progress = match ctx.get(&derived("deck.playback.position_normalized", scope)) {
            Some(ReadValue::Scalar(value)) => value.as_(),
            _ => 0.0,
        };
        let scale = ctx.wave_zoom(zoom);
        let visible = window_bounds(progress, scale);
        Self {
            scale,
            progress,
            start: visible.start,
            end: visible.end,
            wheel_positive: zoom_for_wheel(scale, 1.0).into(),
            wheel_non_positive: zoom_for_wheel(scale, 0.0).into(),
        }
    }

    fn visible(self) -> Range<f32> {
        self.start..self.end
    }
}

#[derive(Clone)]
pub(crate) struct TreePlan<S: HostedState> {
    pub(crate) path: String,
    pub(crate) picture: Rc<RefCell<Tree>>,
    pub(crate) search_path: Option<String>,
    pub(crate) toggle_path: Option<String>,
    pub(crate) state: S::Tree,
}

#[derive(Clone)]
pub(crate) struct TablePlan<S: HostedState> {
    divider_paths: DividerPaths,
    action_paths: Vec<(String, String)>,
    pub(crate) horizontal_path: String,
    pub(crate) path: String,
    pub(crate) row_target: String,
    pub(crate) viewport_width: Rc<Cell<f32>>,
    pub(crate) picture: Rc<RefCell<TableFace>>,
    pub(crate) state: S::Table,
}

#[derive(Clone)]
struct DividerPaths(Vec<(String, String)>);

impl DividerPaths {
    fn new(path: &str, columns: &[ColumnLayout]) -> Self {
        Self(
            columns
                .iter()
                .map(|column| {
                    (
                        column.column.id().to_owned(),
                        format!("{path}/width/{}", column.column.id()),
                    )
                })
                .collect(),
        )
    }

    fn get(&self, column: &TableColumn) -> &str {
        self.0
            .iter()
            .find(|(id, _)| id == column.id())
            .map(|(_, path)| path.as_str())
            .expect("BUG: every laid-out table column owns a divider path")
    }
}

impl<S: HostedState> HostedControlPlan<S> {
    fn append_descriptors(&self, descriptors: &mut Vec<Descriptor>) {
        match self {
            Self::Activation { path, presses } => {
                descriptors.push(Descriptor::pressable(path.clone(), *presses));
            }
            Self::Crossing { path } => descriptors.push(Descriptor::crossing(path.clone())),
            Self::Segmented { path, item_count } => {
                descriptors.push(Descriptor::segmented(path.clone(), *item_count));
            }
            Self::Picker {
                path,
                items,
                selected,
                ..
            } => descriptors.push(Descriptor::picker(path.clone(), items.len(), *selected)),
            Self::Search(plan) => descriptors.push(plan.descriptor()),
            Self::Tree(plan) => plan.append_descriptors(descriptors),
            Self::Table(plan) => plan.append_descriptors(descriptors),
            Self::Fader {
                path,
                style,
                drag_step,
                wheel,
                ..
            } => descriptors.push(Descriptor::fader(
                path.clone(),
                Hover::new(match style {
                    FaderStyle::Default => CursorShape::Grab,
                    FaderStyle::Volume => CursorShape::ResizeH,
                }),
                *drag_step,
                *wheel,
            )),
            Self::Crossfader { path } => {
                descriptors.push(Descriptor::crossfader(path.clone()));
            }
            Self::Knob {
                path,
                current,
                drag_range,
                wheel_step,
            } => descriptors.push(Descriptor::knob(
                path.clone(),
                *current,
                *drag_range,
                *wheel_step,
            )),
            Self::StereoMeter { path } => {
                descriptors.push(Descriptor::stereo_meter(path.clone()));
            }
            Self::VerticalVu { path } => {
                descriptors.push(Descriptor::vertical_vu(path.clone()));
            }
            Self::Wave { path } => descriptors.push(Descriptor::wave(path.clone())),
            Self::HeroWave { path, window, .. } => {
                let window = window.get();
                descriptors.push(Descriptor::hero_wave(
                    path.clone(),
                    window.scale.into(),
                    window.progress,
                    window.visible(),
                    window.wheel_positive,
                    window.wheel_non_positive,
                ));
            }
        }
    }

    /// What a module's `drop:` amounts to, wherever it is mounted.
    ///
    /// Both hosts ask here instead of each spelling out the path and the
    /// gesture again, so a document that takes drops means one thing.
    pub(crate) fn crossing(instance: &str) -> Self {
        Self::Crossing {
            path: drop_path(instance),
        }
    }

    fn descriptor_count(&self) -> usize {
        if let Self::Tree(plan) = self {
            return plan.descriptor_count();
        }
        if let Self::Table(plan) = self {
            return plan.descriptor_count();
        }
        1
    }

    pub(crate) fn descriptors(&self) -> Vec<Descriptor> {
        let mut descriptors = Vec::with_capacity(self.descriptor_count());
        self.append_descriptors(&mut descriptors);
        descriptors
    }

    pub(crate) fn path(&self) -> &str {
        match self {
            Self::Activation { path, .. }
            | Self::Crossing { path }
            | Self::Segmented { path, .. }
            | Self::Picker { path, .. }
            | Self::Fader { path, .. }
            | Self::Crossfader { path }
            | Self::Knob { path, .. }
            | Self::StereoMeter { path }
            | Self::VerticalVu { path }
            | Self::Wave { path }
            | Self::HeroWave { path, .. } => path,
            Self::Search(plan) => &plan.path,
            Self::Tree(plan) => &plan.path,
            Self::Table(plan) => &plan.path,
        }
    }

    /// Re-reads whatever this plan measures a gesture against.
    ///
    /// A host that rebuilds its tree every frame resolves the whole plan afresh
    /// and never calls this. One that keeps a tree calls it instead, so a
    /// standing control answers a hand the same way a newly mounted one would.
    pub(crate) fn reread(&self, ctx: Ctx<'_, '_>) {
        if let Self::HeroWave {
            scope,
            zoom,
            window,
            ..
        } = self
        {
            window.set(HeroWindow::read(scope, zoom.as_ref(), ctx));
        }
    }

    pub(crate) fn resolved(
        path: &str,
        spec: &ControlSpec,
        value: Option<ReadValue<'_>>,
        read: Option<&Binding>,
        scope: &str,
        cx: Resolving<'_>,
    ) -> Option<Self> {
        let Resolving { ctx, skin } = cx;
        let skin = skin.at(path);
        match (spec, value) {
            (ControlSpec::Search, value) => {
                let query = match value {
                    Some(ReadValue::Text(query)) => query,
                    _ => "",
                };
                let plan = SearchPlan::new(path, query, read, Resolving { skin, ctx });
                Some(Self::Search(Box::new(plan)))
            }
            (ControlSpec::Button { .. } | ControlSpec::SettingsButton, _)
            | (
                ControlSpec::Checkbox
                | ControlSpec::Chip { .. }
                | ControlSpec::NavItem { .. }
                | ControlSpec::TabLarge { .. }
                | ControlSpec::Toggle,
                Some(ReadValue::Bool(_)),
            ) => Some(Self::Activation {
                path: path.to_owned(),
                presses: Presses::default(),
            }),
            (ControlSpec::Segmented { items }, Some(ReadValue::Scalar(_))) if !items.is_empty() => {
                Some(Self::Segmented {
                    path: path.to_owned(),
                    item_count: items.len(),
                })
            }
            (ControlSpec::ContextBar { scope_items, scope }, Some(ReadValue::Text(_)))
                if !scope_items.is_empty() =>
            {
                Some(context_bar_plan(
                    path,
                    scope_items,
                    scope.as_ref(),
                    ctx,
                    skin,
                ))
            }
            (
                ControlSpec::Tree {
                    query,
                    search,
                    toggle,
                },
                value,
            ) => {
                let rows = match value {
                    Some(ReadValue::Tree(rows)) => rows,
                    _ => &[],
                };
                let tree = TreeControl {
                    query: query.as_ref(),
                    search: *search,
                    toggle: *toggle,
                };
                Some(Self::Tree(Box::new(tree_plan(path, &tree, read, rows, cx))))
            }
            (
                ControlSpec::Table {
                    columns,
                    columns_state,
                    status,
                    frame,
                    width,
                },
                value,
            ) => {
                let table = TableControl {
                    columns,
                    columns_state: columns_state.as_ref(),
                    status: status.as_ref(),
                    frame: *frame,
                    width: width.as_ref(),
                };
                let rows = match value {
                    Some(ReadValue::Table(rows)) => rows,
                    _ => &[],
                };
                Some(Self::Table(Box::new(TablePlan::resolved(
                    path, &table, read, rows, cx,
                ))))
            }
            (ControlSpec::Fader { style, label }, Some(ReadValue::Scalar(value))) => {
                let (drag_step, wheel) = match style {
                    FaderStyle::Default => (Some(skin.fader.step), None),
                    FaderStyle::Volume => (
                        None,
                        Some(WheelStep {
                            value: value.clamp(0.0, 1.0).as_(),
                            step: skin.fader.step.as_(),
                        }),
                    ),
                };
                Some(Self::Fader {
                    drag_step,
                    wheel,
                    path: path.to_owned(),
                    style: *style,
                    labelled: label.is_some(),
                    metrics: skin.fader,
                })
            }
            (ControlSpec::Crossfader { .. }, Some(ReadValue::Scalar(_))) => {
                Some(Self::Crossfader {
                    path: path.to_owned(),
                })
            }
            (ControlSpec::Knob { .. }, Some(ReadValue::Scalar(value))) => Some(Self::Knob {
                path: path.to_owned(),
                current: value.clamp(0.0, 1.0).as_(),
                drag_range: skin.knob.drag_range,
                wheel_step: skin.knob.wheel_step,
            }),
            (ControlSpec::VuStereo, Some(ReadValue::Stereo(_))) => Some(Self::StereoMeter {
                path: path.to_owned(),
            }),
            (ControlSpec::VuVertical { .. }, Some(ReadValue::Stereo(_))) => {
                Some(Self::VerticalVu {
                    path: path.to_owned(),
                })
            }
            (ControlSpec::Wave { style, zoom, .. }, _) => {
                Some(wave_plan(path, *style, zoom.as_ref(), scope, ctx))
            }
            _ => None,
        }
    }
}

fn tree_plan<S: HostedState>(
    path: &str,
    tree: &TreeControl<'_>,
    read: Option<&Binding>,
    rows: &[TreeRow<'_>],
    cx: Resolving<'_>,
) -> TreePlan<S> {
    let Resolving { ctx, skin } = cx;
    let query_text = tree.search.then(|| {
        tree.query
            .and_then(|binding| ctx.read(binding))
            .and_then(|value| match value {
                ReadValue::Text(query) => Some(query),
                _ => None,
            })
            .unwrap_or_default()
    });
    let plan = TreePlan {
        path: path.to_owned(),
        picture: Rc::new(RefCell::new(Tree::new(rows, query_text, skin))),
        search_path: tree.search.then(|| format!("{path}/search")),
        toggle_path: tree.toggle.then(|| format!("{path}/toggle")),
        state: S::Tree::default(),
    };
    S::bind_tree(&plan.state, tree, read, cx);
    plan
}

fn context_bar_plan<S: HostedState>(
    path: &str,
    scope_items: &[InternId],
    scope: Option<&Binding>,
    ctx: Ctx<'_, '_>,
    skin: &Skin,
) -> HostedControlPlan<S> {
    let scope_value = scope.and_then(|binding| ctx.read(binding));
    let selected = picker_selected_index(scope_value.as_ref(), scope_items.len());
    let mut text = TextContext::from(skin.text_resources.as_ref());
    let items: Vec<String> = scope_items
        .iter()
        .map(|item| ctx.ui.resolve(*item).to_owned())
        .collect();
    let face = Context::new(skin).face_of(&mut text, items.iter().map(String::as_str));
    HostedControlPlan::Picker {
        items,
        selected,
        face,
        path: path.to_owned(),
        item_height: skin.tree.scope_item_height,
    }
}

fn wave_plan<S: HostedState>(
    path: &str,
    style: WaveStyle,
    zoom: Option<&Binding>,
    scope: &str,
    ctx: Ctx<'_, '_>,
) -> HostedControlPlan<S> {
    if style != WaveStyle::Hero {
        return HostedControlPlan::Wave {
            path: path.to_owned(),
        };
    }
    let plan = HostedControlPlan::HeroWave {
        path: path.to_owned(),
        scope: scope.to_owned(),
        zoom: zoom.cloned(),
        window: Cell::default(),
    };
    plan.reread(ctx);
    plan
}

impl<S: HostedState> TreePlan<S> {
    pub(crate) fn picture(&self) -> Ref<'_, Tree> {
        self.picture.borrow()
    }

    fn descriptor_count(&self) -> usize {
        1 + usize::from(self.search_path.is_some()) + usize::from(self.toggle_path.is_some())
    }

    pub(crate) fn append_toggle_targets<'a>(
        &'a self,
        rows: Rect,
        point: Option<Pt>,
        offset: f32,
        targets: &mut Vec<Target<'a>>,
    ) {
        let Some(path) = &self.toggle_path else {
            return;
        };
        let under = self
            .picture
            .borrow()
            .toggle_regions(rows, offset)
            .into_iter()
            .find(|(_, chevron)| Hit::new(point, *chevron).over());
        targets.push(match under {
            Some((index, chevron)) => Target::item(path, Hit::new(point, chevron), index),
            None => Target::new(path, Hit::new(point, empty_bounds(rows))),
        });
    }

    fn append_descriptors(&self, descriptors: &mut Vec<Descriptor>) {
        let picture = self.picture.borrow();
        if let (Some(path), Some(query)) = (&self.search_path, picture.query()) {
            descriptors.push(Descriptor::text_input(
                path.clone(),
                query.to_owned(),
                text_input_layout(query, picture.skin()),
            ));
        }
        let row_count = picture.row_count();
        if let Some(path) = &self.toggle_path {
            descriptors.push(Descriptor::item(path.clone(), path.clone(), row_count));
        }
        descriptors.push(Descriptor::scroll(
            self.path.clone(),
            ScrollConfig::items(
                ScrollAxis::Vertical,
                AsPrimitive::<f32>::as_(row_count) * picture.skin().tree.row_height,
                row_count,
                picture.skin().tree.row_height,
                picture.skin().tree.row_height,
                picture.skin().tree.scrollbar_margin + picture.skin().tree.scrollbar_width,
            ),
        ));
    }
}

impl<S: HostedState> TablePlan<S> {
    pub(crate) fn new(path: &str, picture: TableFace) -> Self {
        Self {
            action_paths: picture
                .columns()
                .iter()
                .filter(|column| column.column.write().is_some())
                .map(|column| {
                    (
                        column.column.id().to_owned(),
                        format!("{path}/{}", column.column.action_slot()),
                    )
                })
                .collect(),
            divider_paths: DividerPaths::new(path, picture.columns()),
            horizontal_path: format!("{path}/scroll-x"),
            path: path.to_owned(),
            row_target: format!("{path}/rows"),
            viewport_width: Rc::new(Cell::new(0.0)),
            picture: Rc::new(RefCell::new(picture)),
            state: S::Table::default(),
        }
    }

    fn append_descriptors(&self, descriptors: &mut Vec<Descriptor>) {
        let picture = self.picture.borrow();
        let columns = picture.columns();
        let row_count = picture.rows().len();
        descriptors.push(Descriptor::scroll(
            self.horizontal_path.clone(),
            ScrollConfig::plain(ScrollAxis::Horizontal, picture.metrics().width(columns)),
        ));
        descriptors.push(Descriptor::scroll(
            self.path.clone(),
            ScrollConfig::plain(
                ScrollAxis::Vertical,
                table_content_height(row_count, picture.skin()),
            ),
        ));
        descriptors.push(Descriptor::item(
            self.row_target.clone(),
            self.path.clone(),
            row_count,
        ));
        for (id, path) in &self.action_paths {
            if let Some(column) = columns.iter().position(|column| column.column.id() == id) {
                let texts = picture
                    .rows()
                    .iter()
                    .map(|row| {
                        row.cell(column)
                            .and_then(TableCell::action)
                            .map(ToOwned::to_owned)
                    })
                    .collect();
                descriptors.push(Descriptor::text_actions(path.clone(), texts));
            }
        }
        let resizable = columns
            .iter()
            .enumerate()
            .filter(|(index, _)| column_resizable(columns, *index));
        for (index, column) in resizable {
            let divider_path = self.divider_path(&column.column);
            descriptors.push(Descriptor::column_divider(
                divider_path.to_owned(),
                column.width,
                column_resize_track(columns, index, self.viewport_width.get(), picture.metrics()),
            ));
        }
    }

    pub(crate) fn append_action_targets<'a>(
        &'a self,
        bounds: Rect,
        point: Option<Pt>,
        columns: &[ColumnLayout],
        offsets: (f32, f32),
        targets: &mut Vec<Target<'a>>,
    ) {
        let picture = self.picture.borrow();
        for (index, column, cell, _) in picture.actions_under(point, bounds, offsets, columns) {
            let id = columns[column].column.id();
            if let Some((_, path)) = self.action_paths.iter().find(|(action, _)| action == id) {
                targets.push(Target::item(path, Hit::new(point, cell), index));
            }
        }
    }

    fn descriptor_count(&self) -> usize {
        let picture = self.picture.borrow();
        picture
            .columns()
            .iter()
            .enumerate()
            .filter(|(index, _)| column_resizable(picture.columns(), *index))
            .count()
            + picture
                .columns()
                .iter()
                .filter(|column| column.column.write().is_some())
                .count()
            + 3
    }

    pub(crate) fn divider_path(&self, column: &TableColumn) -> &str {
        self.divider_paths.get(column)
    }

    fn resolved(
        path: &str,
        table: &TableControl<'_>,
        read: Option<&Binding>,
        rows: &[TableRow<'_>],
        cx: Resolving<'_>,
    ) -> Self {
        let Resolving { ctx, skin } = cx;
        let state = table
            .columns_state
            .map(|binding| (ctx.ui.resolve(binding.id), ctx.scope(Some(binding))));
        let columns = column_layouts(
            (table.columns, ctx.endpoint(table.width)),
            &ctx,
            state,
            skin,
        );
        let rows = rows.iter().map(TableRowData::from).collect();
        let picture = TableFace::new(rows, columns, skin, table.frame)
            .with_status(table.status.and_then(|binding| ctx.read(binding)));
        let plan = Self::new(path, picture);
        S::bind_table(&plan.state, table, read, cx);

        plan
    }
}
