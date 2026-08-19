use std::{
    cell::{Cell, RefCell},
    ops::Range,
    rc::Rc,
};

use num_traits::cast::AsPrimitive;

#[cfg(feature = "masonry")]
use super::masonry::{TableSource, TableState, TreeSource, TreeState};
#[cfg(test)]
use crate::interact::Gestures;
use crate::{
    atoms::{
        bar::context::Context,
        table::{
            ColumnLayout, TableRowData, column_layouts, column_resizable, face::TableFace,
            minimum_table_width, table_content_height,
        },
        tree::face::Tree,
        wave::zoom_math::{clamp_zoom, window_bounds, zoom_for_wheel},
    },
    draw::Rect,
    engine::{Descriptor, ScrollConfig},
    expand::{Binding, ControlSpec},
    ids::InternId,
    interact::{CursorShape, Hover, ScrollAxis, recognizers::WheelStep},
    module::{FaderStyle, TableColumn, WaveStyle},
    render::{
        ReadValue, Skin, TableRow, TreeRow, document::Ctx, model::derived, picker_selected_index,
        text_input_layout,
    },
    shaping::TextContext,
};
/// What a control plan is resolved against: the compiled document that names
/// things, the model that answers a reading, and the skin that sizes it.
#[derive(Clone, Copy)]
pub(crate) struct Resolving<'a> {
    pub(crate) ctx: Ctx<'a, 'a>,
    pub(crate) skin: &'a Skin,
}

#[derive(Clone)]
pub(crate) enum HostedControlPlan {
    Activation {
        path: String,
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
    Tree(Box<TreePlan>),
    Table(Box<TablePlan>),
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
    scale: f32,
    progress: f32,
    start: f32,
    end: f32,
    wheel_positive: f32,
    wheel_non_positive: f32,
}

impl HeroWindow {
    /// What the deck at `scope` is showing this frame.
    fn read(scope: &str, zoom: Option<&Binding>, ctx: Ctx<'_, '_>) -> Self {
        let progress = match ctx.get(&derived("deck.playback.position_normalized", scope)) {
            Some(ReadValue::Scalar(value)) => value.as_(),
            _ => 0.0,
        };
        let scale = clamp_zoom(ctx.wave_zoom(zoom));
        let visible = window_bounds(progress, scale);
        Self {
            scale,
            progress,
            start: visible.start,
            end: visible.end,
            wheel_positive: zoom_for_wheel(scale, 1.0),
            wheel_non_positive: zoom_for_wheel(scale, 0.0),
        }
    }

    fn visible(self) -> Range<f32> {
        self.start..self.end
    }

    /// The window a deck at `progress` shows at `zoom`, without a document to
    /// read it from.
    #[cfg(test)]
    fn at(progress: f32, zoom: f32) -> Self {
        let scale = clamp_zoom(zoom);
        let visible = window_bounds(progress, scale);
        Self {
            scale,
            progress,
            start: visible.start,
            end: visible.end,
            wheel_positive: zoom_for_wheel(scale, 1.0),
            wheel_non_positive: zoom_for_wheel(scale, 0.0),
        }
    }
}

#[derive(Clone)]
pub(crate) struct TreePlan {
    pub(crate) path: String,
    pub(super) picture: Rc<RefCell<Tree>>,
    pub(crate) search_path: String,
    #[cfg(feature = "masonry")]
    pub(super) state: TreeState,
}

#[derive(Clone)]
pub(crate) struct TablePlan {
    divider_paths: DividerPaths,
    pub(crate) horizontal_path: String,
    pub(crate) path: String,
    pub(crate) row_target: String,
    min_column_width: f32,
    pub(super) picture: Rc<RefCell<TableFace>>,
    #[cfg(feature = "masonry")]
    pub(super) state: TableState,
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

impl HostedControlPlan {
    pub(in crate::render) fn resolved(
        path: &str,
        spec: &ControlSpec,
        value: Option<ReadValue<'_>>,
        read: Option<&Binding>,
        scope: &str,
        cx: Resolving<'_>,
    ) -> Option<Self> {
        let Resolving { ctx, skin } = cx;
        match (spec, value) {
            (ControlSpec::Button { .. }, _)
            | (
                ControlSpec::Checkbox
                | ControlSpec::Chip { .. }
                | ControlSpec::NavItem { .. }
                | ControlSpec::TabLarge { .. }
                | ControlSpec::Toggle,
                Some(ReadValue::Bool(_)),
            ) => Some(Self::Activation {
                path: path.to_owned(),
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
            (ControlSpec::Tree { query }, Some(ReadValue::Tree(rows))) => Some(Self::Tree(
                Box::new(tree_plan(path, query.as_ref(), read, rows, cx)),
            )),
            (ControlSpec::Tree { query }, _) => Some(Self::Tree(Box::new(tree_plan(
                path,
                query.as_ref(),
                read,
                &[],
                cx,
            )))),
            (
                ControlSpec::Table {
                    columns,
                    columns_state,
                },
                Some(ReadValue::Table(rows)),
            ) => Some(Self::Table(Box::new(TablePlan::resolved(
                path,
                columns,
                columns_state.as_ref(),
                read,
                rows,
                cx,
            )))),
            (
                ControlSpec::Table {
                    columns,
                    columns_state,
                },
                _,
            ) => Some(Self::Table(Box::new(TablePlan::resolved(
                path,
                columns,
                columns_state.as_ref(),
                read,
                &[],
                cx,
            )))),
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
                    path: path.to_owned(),
                    style: *style,
                    labelled: label.is_some(),
                    drag_step,
                    wheel,
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
            // What a hand on a waveform does is what the document says it does.
            // A deck is mounted before anything is loaded into it, and a host
            // that keeps its tree decides this once: a wave that had to be
            // carrying a track to answer would stay deaf for the life of the
            // window, however many tracks were dropped on it afterwards.
            (ControlSpec::Wave { style, zoom, .. }, _) => {
                Some(wave_plan(path, *style, zoom.as_ref(), scope, ctx))
            }
            _ => None,
        }
    }

    /// A hero wave showing a deck at `progress`, for a test that mounts one
    /// without a document behind it.
    #[cfg(test)]
    pub(crate) fn hero_wave_at(path: &str, progress: f32, zoom: f32) -> Self {
        Self::HeroWave {
            path: path.to_owned(),
            scope: String::new(),
            zoom: None,
            window: Cell::new(HeroWindow::at(progress, zoom)),
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

    /// What a module's `drop:` amounts to, wherever it is mounted.
    ///
    /// Both hosts ask here instead of each spelling out the path and the
    /// gesture again, so a document that takes drops means one thing.
    pub(in crate::render) fn crossing(instance: &str) -> Self {
        Self::Crossing {
            path: format!("{instance}/drop"),
        }
    }

    pub(crate) fn descriptors(&self) -> Vec<Descriptor> {
        let mut descriptors = Vec::with_capacity(self.descriptor_count());
        self.append_descriptors(&mut descriptors);
        descriptors
    }

    #[cfg(test)]
    pub(crate) fn gestures(&self) -> Gestures {
        self.descriptors()
            .iter()
            .fold(Gestures::NONE, |gestures, descriptor| {
                gestures.union(descriptor.gestures())
            })
    }

    pub(in crate::render) fn path(&self) -> &str {
        match self {
            Self::Activation { path }
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
            Self::Tree(plan) => &plan.path,
            Self::Table(plan) => &plan.path,
        }
    }

    fn descriptor_count(&self) -> usize {
        if matches!(self, Self::Tree(_)) {
            return TreePlan::DESCRIPTORS;
        }
        if let Self::Table(plan) = self {
            return plan.descriptor_count();
        }
        1
    }

    fn append_descriptors(&self, descriptors: &mut Vec<Descriptor>) {
        match self {
            Self::Activation { path } => descriptors.push(Descriptor::activation(path.clone())),
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
                    window.scale,
                    window.progress,
                    window.visible(),
                    window.wheel_positive,
                    window.wheel_non_positive,
                ));
            }
        }
    }
}

fn tree_plan(
    path: &str,
    query: Option<&Binding>,
    _read: Option<&Binding>,
    rows: &[TreeRow<'_>],
    cx: Resolving<'_>,
) -> TreePlan {
    let Resolving { ctx, skin } = cx;
    let query_text = query
        .and_then(|binding| ctx.read(binding))
        .and_then(|value| match value {
            ReadValue::Text(query) => Some(query),
            _ => None,
        })
        .unwrap_or_default();
    let plan = TreePlan {
        path: path.to_owned(),
        picture: Rc::new(RefCell::new(Tree::new(rows, query_text, skin))),
        search_path: format!("{path}/search"),
        #[cfg(feature = "masonry")]
        state: TreeState::default(),
    };
    #[cfg(feature = "masonry")]
    plan.bind_source(TreeSource::new(
        _read.map(|binding| ctx.ui.resolve(binding.key).to_owned()),
        query.map(|binding| ctx.ui.resolve(binding.key).to_owned()),
    ));
    plan
}

fn context_bar_plan(
    path: &str,
    scope_items: &[InternId],
    scope: Option<&Binding>,
    ctx: Ctx<'_, '_>,
    skin: &Skin,
) -> HostedControlPlan {
    let scope_value = scope.and_then(|binding| ctx.read(binding));
    let selected = picker_selected_index(scope_value.as_ref(), scope_items.len());
    let mut text = TextContext::from(skin.text_resources());
    let items: Vec<String> = scope_items
        .iter()
        .map(|item| ctx.ui.resolve(*item).to_owned())
        .collect();
    let face = Context::new(skin).face_of(&mut text, items.iter().map(String::as_str));
    HostedControlPlan::Picker {
        path: path.to_owned(),
        items,
        item_height: skin.tree.scope_item_height,
        selected,
        face,
    }
}

fn wave_plan(
    path: &str,
    style: WaveStyle,
    zoom: Option<&Binding>,
    scope: &str,
    ctx: Ctx<'_, '_>,
) -> HostedControlPlan {
    if style != WaveStyle::Hero {
        return HostedControlPlan::Wave {
            path: path.to_owned(),
        };
    }
    // The window is not a property of the document, it is what the deck reads
    // this frame, so both hosts arrive at it the same way: the immediate one
    // here, the retained one again whenever the frame moves under its tree.
    let plan = HostedControlPlan::HeroWave {
        path: path.to_owned(),
        scope: scope.to_owned(),
        zoom: zoom.cloned(),
        window: Cell::default(),
    };
    plan.reread(ctx);
    plan
}

impl TreePlan {
    /// A tree registers exactly two: its search field and its scroll.
    const DESCRIPTORS: usize = 2;

    fn append_descriptors(&self, descriptors: &mut Vec<Descriptor>) {
        let picture = self.picture.borrow();
        descriptors.push(Descriptor::text_input(
            self.search_path.clone(),
            picture.query().to_owned(),
            text_input_layout(picture.query(), picture.skin()),
        ));
        let row_count = picture.row_count();
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

impl TablePlan {
    fn resolved(
        path: &str,
        declared_columns: &[TableColumn],
        columns_state: Option<&Binding>,
        _read: Option<&Binding>,
        rows: &[TableRow<'_>],
        cx: Resolving<'_>,
    ) -> Self {
        let Resolving { ctx, skin } = cx;
        let state =
            columns_state.map(|binding| (ctx.ui.resolve(binding.id), ctx.scope(Some(binding))));
        let columns = column_layouts(declared_columns, &ctx, state, skin);
        let rows = rows.iter().map(TableRowData::from).collect();
        let plan = Self::new(path, rows, columns, skin);
        #[cfg(feature = "masonry")]
        plan.bind_source(TableSource::new(
            declared_columns.to_vec(),
            columns_state.map(|binding| {
                (
                    ctx.ui.resolve(binding.id).to_owned(),
                    ctx.scope(Some(binding)).to_owned(),
                )
            }),
            _read.map(|binding| ctx.ui.resolve(binding.key).to_owned()),
        ));
        plan
    }

    pub(super) fn new(
        path: &str,
        rows: Vec<TableRowData>,
        columns: Vec<ColumnLayout>,
        skin: &Skin,
    ) -> Self {
        Self {
            divider_paths: DividerPaths::new(path, &columns),
            horizontal_path: format!("{path}/scroll-x"),
            path: path.to_owned(),
            row_target: format!("{path}/rows"),
            min_column_width: skin.table.min_column_width,
            picture: Rc::new(RefCell::new(TableFace::new(rows, columns, skin))),
            #[cfg(feature = "masonry")]
            state: TableState::default(),
        }
    }

    pub(crate) fn columns(&self) -> Vec<ColumnLayout> {
        self.picture.borrow().columns().to_vec()
    }

    pub(crate) fn row_count(&self) -> usize {
        self.picture.borrow().rows().len()
    }

    fn descriptor_count(&self) -> usize {
        let picture = self.picture.borrow();
        picture
            .columns()
            .iter()
            .enumerate()
            .filter(|(index, _)| column_resizable(picture.columns(), *index))
            .count()
            + 3
    }

    fn append_descriptors(&self, descriptors: &mut Vec<Descriptor>) {
        let picture = self.picture.borrow();
        let columns = picture.columns();
        let row_count = picture.rows().len();
        descriptors.push(Descriptor::scroll(
            self.horizontal_path.clone(),
            ScrollConfig::plain(ScrollAxis::Horizontal, minimum_table_width(columns)),
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
        let resizable = columns
            .iter()
            .enumerate()
            .filter(|(index, _)| column_resizable(columns, *index));
        for (_, column) in resizable {
            let divider_path = self.divider_path(&column.column);
            descriptors.push(Descriptor::column_divider(
                divider_path.to_owned(),
                column.width,
                self.min_column_width,
            ));
        }
    }

    pub(crate) fn divider_path(&self, column: &TableColumn) -> &str {
        self.divider_paths.get(column)
    }
}
