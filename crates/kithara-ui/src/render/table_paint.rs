use std::cell::RefCell;

use iced::{
    Rectangle, Renderer, Theme,
    mouse::Cursor,
    widget::canvas::{self, Frame, Geometry},
};

use super::super::{Skin, UiEvent, controls::RetainedCanvasState};
use crate::{
    atoms::table::{
        ColumnLayout, TableRowData, column_resizable,
        face::{Drawn, TableFace},
        minimum_table_width, table_content_height, table_row_at,
    },
    backends::replay_ordered,
    draw::{Pt, Rect},
    engine::{ScrollConfig, ScrollState},
    interact::{
        ScrollAxis,
        recognizers::{ItemDrag, ScalarState},
    },
    module::TableColumn,
    shaping::TextContext,
};

pub(super) struct TablePaint {
    pub(super) face: TableFace,
    pub(super) path: String,
}

impl TablePaint {
    pub(super) fn new(
        path: &str,
        rows: Vec<TableRowData>,
        columns: Vec<ColumnLayout>,
        skin: &Skin,
    ) -> Self {
        Self {
            face: TableFace::new(rows, columns, skin),
            path: path.to_owned(),
        }
    }

    pub(super) fn geometry(
        &self,
        state: &TableState,
        renderer: &Renderer,
        _theme: &Theme,
        bounds: Rectangle,
        cursor: Cursor,
    ) -> Vec<Geometry> {
        let (horizontal, vertical) = state.paint_offsets();
        let mut frame = Frame::new(renderer, bounds.size());
        let mut text = state.text.borrow_mut();
        let text = text.get_or_insert_with(|| self.face.skin().text_resources().into());
        let point = cursor.position_in(bounds).map(Into::into);
        let bounds = local_rect(bounds);
        let hovered = hovered_row(
            point,
            bounds,
            self.face.rows().len(),
            horizontal,
            vertical,
            &self.face,
        );
        let list = self.face.commands(
            text,
            bounds,
            &Drawn {
                columns: self.face.columns().to_vec(),
                horizontal,
                hovered,
                pressed: state.pressed_index,
                vertical,
            },
        );
        replay_ordered(&list, &mut frame, self.face.skin().text_resources());
        vec![frame.into_geometry()]
    }

    pub(super) fn config(&self) -> TableConfig {
        let skin = self.face.skin();
        TableConfig {
            body_inset: skin.table.header_height
                + skin.table.footer_height
                + skin.table.grid_gap * 2.0,
            content_height: table_content_height(self.face.rows().len(), skin),
            content_width: minimum_table_width(self.face.columns()),
            divider_columns: self
                .face
                .columns()
                .iter()
                .enumerate()
                .filter(|(index, _)| column_resizable(self.face.columns(), *index))
                .map(|(_, column)| column.column.clone())
                .collect(),
            row_count: self.face.rows().len(),
        }
    }
}

impl canvas::Program<UiEvent> for TablePaint {
    type State = TableState;

    fn draw(
        &self,
        state: &TableState,
        renderer: &Renderer,
        theme: &Theme,
        bounds: Rectangle,
        cursor: Cursor,
    ) -> Vec<Geometry> {
        self.geometry(state, renderer, theme, bounds, cursor)
    }
}

#[derive(Default)]
pub(super) struct TableState {
    configured: bool,
    pub(super) dividers: Vec<(TableColumn, ScalarState)>,
    pub(super) horizontal: ScrollState,
    path: String,
    pub(super) drag_index: Option<usize>,
    pub(super) pressed_index: Option<usize>,
    pub(super) row_drag: ItemDrag,
    text: RefCell<Option<TextContext>>,
    pub(super) vertical: ScrollState,
}

#[derive(Clone)]
pub(super) struct TableConfig {
    body_inset: f32,
    content_height: f32,
    content_width: f32,
    divider_columns: Vec<TableColumn>,
    row_count: usize,
}

impl TableState {
    pub(super) fn reconcile(&mut self, path: &str, config: &TableConfig) {
        self.rebind(path);
        let horizontal = ScrollConfig::plain(ScrollAxis::Horizontal, config.content_width);
        let vertical = ScrollConfig::plain(ScrollAxis::Vertical, config.content_height);
        if !self.configured {
            self.drag_index = None;
            self.horizontal = ScrollState::new(horizontal);
            self.row_drag = ItemDrag::default();
            self.vertical = ScrollState::new(vertical);
            self.dividers = config
                .divider_columns
                .iter()
                .cloned()
                .map(|column| (column, ScalarState::default()))
                .collect();
            self.configured = true;
        } else {
            self.horizontal.reconcile(horizontal);
            self.vertical.reconcile(vertical);
            let mut retained = std::mem::take(&mut self.dividers);
            self.dividers = config
                .divider_columns
                .iter()
                .map(|column| {
                    retained
                        .iter()
                        .position(|(candidate, _)| candidate == column)
                        .map_or_else(
                            || (column.clone(), ScalarState::default()),
                            |index| retained.remove(index),
                        )
                })
                .collect();
        }
        if self
            .drag_index
            .is_some_and(|index| index >= config.row_count)
        {
            self.drag_index = None;
            self.pressed_index = None;
            self.row_drag = ItemDrag::default();
        }
        if self
            .pressed_index
            .is_some_and(|index| index >= config.row_count)
        {
            self.pressed_index = None;
        }
    }

    pub(super) fn set_viewport(&mut self, size: iced::Size, config: &TableConfig) {
        self.horizontal.set_viewport(size.width);
        self.vertical
            .set_viewport((size.height - config.body_inset).max(0.0));
    }

    pub(super) fn sync(
        &mut self,
        path: &str,
        horizontal: f32,
        pressed: Option<usize>,
        vertical: f32,
    ) {
        if self.path == path {
            self.horizontal.sync_offset(horizontal);
            self.pressed_index = pressed;
            self.vertical.sync_offset(vertical);
        }
    }

    pub(super) fn rebind(&mut self, path: &str) {
        if self.path != path {
            self.path = path.to_owned();
            self.configured = false;
            self.dividers.clear();
            self.drag_index = None;
            self.horizontal = ScrollState::default();
            self.pressed_index = None;
            self.row_drag = ItemDrag::default();
            self.vertical = ScrollState::default();
        }
    }

    fn paint_offsets(&self) -> (f32, f32) {
        (self.horizontal.offset(), self.vertical.offset())
    }
}

impl RetainedCanvasState for TableState {
    type Config = TableConfig;

    delegate::delegate! {
        to self {
            #[call(reconcile)]
            fn reconcile_canvas(&mut self, path: &str, config: &Self::Config);
            #[call(set_viewport)]
            fn set_canvas_viewport(&mut self, size: iced::Size, config: &Self::Config);
        }
    }
}

pub(super) fn local_rect(bounds: Rectangle) -> Rect {
    Rect {
        h: bounds.height,
        w: bounds.width,
        x: 0.0,
        y: 0.0,
    }
}

pub(super) fn hovered_row(
    point: Option<Pt>,
    bounds: Rect,
    row_count: usize,
    horizontal: f32,
    vertical: f32,
    face: &TableFace,
) -> Option<usize> {
    table_row_at(
        point,
        bounds,
        face.columns(),
        row_count,
        horizontal,
        vertical,
        face.skin(),
    )
}
