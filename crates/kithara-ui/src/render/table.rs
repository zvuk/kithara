use iced::{
    Element, Event, Rectangle, Renderer, Theme,
    advanced::widget::Operation,
    mouse::{Cursor, Interaction},
    widget::canvas::{self, Action, Geometry},
};
use kithara_platform::time::Instant;

#[path = "table_paint.rs"]
mod paint;

use paint::{TableConfig, TablePaint, TableState, hovered_row, local_rect};

use super::{InputOwner, Skin, UiEvent, controls::RetainedCanvas, drag, index, scalar};
use crate::{
    atoms::table::{
        ColumnLayout, TableRowData, table_body, table_dividers, table_visible_row_rect,
    },
    draw::{Pt, Rect},
    interact::{
        CursorShape, Hit, Hover, Input, Outcome, PointerPhase, iced as iced_interact,
        recognizers::{ItemDrag, Scalar, Track},
    },
};

pub(crate) fn table<'skin>(
    path: &str,
    rows: Vec<TableRowData>,
    columns: Vec<ColumnLayout>,
    skin: &'skin Skin,
    owner: InputOwner,
) -> Element<'skin, UiEvent> {
    let paint = TablePaint::new(path, rows, columns, skin);
    let config = paint.config();
    match owner {
        InputOwner::Leaf => RetainedCanvas::new(
            TableProgram {
                paint,
                config: config.clone(),
            },
            path,
            config,
        )
        .view(),
        InputOwner::Engine => RetainedCanvas::new(paint, path, config).view(),
    }
}

pub(crate) fn sync_table_scroll(
    path: &str,
    horizontal: f32,
    pressed: Option<usize>,
    vertical: f32,
) -> impl Operation + '_ {
    struct Sync<'a> {
        horizontal: f32,
        path: &'a str,
        pressed: Option<usize>,
        vertical: f32,
    }

    impl Operation for Sync<'_> {
        fn traverse(&mut self, operate: &mut dyn FnMut(&mut dyn Operation)) {
            operate(self);
        }

        fn custom(
            &mut self,
            _id: Option<&iced::advanced::widget::Id>,
            _bounds: Rectangle,
            state: &mut dyn std::any::Any,
        ) {
            if let Some(state) = state.downcast_mut::<TableState>() {
                state.sync(self.path, self.horizontal, self.pressed, self.vertical);
            }
        }
    }

    Sync {
        horizontal,
        path,
        pressed,
        vertical,
    }
}

struct TableProgram {
    config: TableConfig,
    paint: TablePaint,
}

impl canvas::Program<UiEvent> for TableProgram {
    type State = TableState;

    fn update(
        &self,
        state: &mut TableState,
        event: &Event,
        bounds: Rectangle,
        cursor: Cursor,
    ) -> Option<Action<UiEvent>> {
        let input = iced_interact::input(event)?;
        let drag_point = cursor.position().map(Into::into);
        let point = cursor.position_in(bounds).map(Into::into);
        let viewport = bounds.size();
        let origin = Pt {
            x: bounds.x,
            y: bounds.y,
        };
        let bounds = local_rect(bounds);
        state.reconcile(&self.paint.path, &self.config);
        state.set_viewport(viewport, &self.config);

        if let Some(action) = self.divider_input(state, input, bounds, drag_point, origin) {
            return Some(action);
        }
        if let Some(action) = self.row_drag_input(state, input, bounds, point) {
            return Some(action);
        }

        let body = table_body(bounds, self.paint.face.skin());
        let vertical_hit = Hit::new(point, body);
        let before = state.vertical.offset();
        let outcome = state.vertical.handle(input, &vertical_hit);
        let after = state.vertical.offset();
        if outcome.is_captured() || outcome.value().is_some() {
            return scroll_action(&self.paint.path, outcome, before, after);
        }

        let horizontal_hit = Hit::new(point, bounds);
        let before = state.horizontal.offset();
        let outcome = state.horizontal.handle(input, &horizontal_hit);
        let after = state.horizontal.offset();
        scroll_action(
            &format!("{}/scroll-x", self.paint.path),
            outcome,
            before,
            after,
        )
    }

    fn draw(
        &self,
        state: &TableState,
        renderer: &Renderer,
        theme: &Theme,
        bounds: Rectangle,
        cursor: Cursor,
    ) -> Vec<Geometry> {
        self.paint.geometry(state, renderer, theme, bounds, cursor)
    }

    fn mouse_interaction(
        &self,
        state: &TableState,
        bounds: Rectangle,
        cursor: Cursor,
    ) -> Interaction {
        let point = cursor.position_in(bounds).map(Into::into);
        let bounds = local_rect(bounds);
        let dividers = table_dividers(
            bounds,
            self.paint.face.columns(),
            state.horizontal.offset(),
            self.paint.face.skin(),
        );
        for divider in &dividers {
            let Some((_, drag_state)) = state
                .dividers
                .iter()
                .find(|(column, _)| *column == divider.column)
            else {
                continue;
            };
            let drag = divider_drag(divider.value, self.paint.face.skin().table.min_column_width);
            let hit = Hit::new(point, divider.hit);
            let cursor = drag.cursor(drag_state, &hit);
            if cursor != CursorShape::None {
                return cursor.into();
            }
        }
        let row_cursor = state.row_drag.cursor();
        if row_cursor != CursorShape::None {
            return row_cursor.into();
        }
        if hovered_row(
            point,
            bounds,
            self.paint.face.rows().len(),
            state.horizontal.offset(),
            state.vertical.offset(),
            &self.paint.face,
        )
        .is_some()
        {
            Interaction::Pointer
        } else {
            Interaction::None
        }
    }
}

impl TableProgram {
    fn divider_input(
        &self,
        state: &mut TableState,
        input: Input<'_>,
        bounds: Rect,
        point: Option<Pt>,
        origin: Pt,
    ) -> Option<Action<UiEvent>> {
        let dividers = table_dividers(
            bounds,
            self.paint.face.columns(),
            state.horizontal.offset(),
            self.paint.face.skin(),
        );
        for divider in &dividers {
            let Some((_, drag_state)) = state
                .dividers
                .iter_mut()
                .find(|(column, _)| *column == divider.column)
            else {
                continue;
            };
            let drag = divider_drag(divider.value, self.paint.face.skin().table.min_column_width);
            let hit = Rect {
                x: divider.hit.x + origin.x,
                y: divider.hit.y + origin.y,
                ..divider.hit
            };
            let outcome = drag.on_input(drag_state, input, &Hit::new(point, hit), Instant::now());
            if outcome.is_captured() || outcome.value().is_some() {
                let path = format!("{}/width/{}", self.paint.path, divider.column.id());
                return scalar(&path, outcome.map(f64::from));
            }
        }
        None
    }

    fn row_drag_input(
        &self,
        state: &mut TableState,
        input: Input<'_>,
        bounds: Rect,
        point: Option<Pt>,
    ) -> Option<Action<UiEvent>> {
        if matches!(
            input,
            Input::Pointer(pointer) if pointer.phase == PointerPhase::Down
        ) {
            state.row_drag = ItemDrag::default();
            state.drag_index = hovered_row(
                point,
                bounds,
                self.paint.face.rows().len(),
                state.horizontal.offset(),
                state.vertical.offset(),
                &self.paint.face,
            );
            state.pressed_index = state.drag_index;
        }
        let row_index = state.drag_index?;
        let visible = table_visible_row_rect(
            bounds,
            self.paint.face.columns(),
            self.paint.face.rows().len(),
            row_index,
            state.horizontal.offset(),
            state.vertical.offset(),
            self.paint.face.skin(),
        );
        let row = visible.unwrap_or(Rect {
            h: 0.0,
            w: 0.0,
            x: bounds.x,
            y: bounds.y,
        });
        let hit = Hit::new(point, row);
        let was_pressed = state.pressed_index == Some(row_index);
        let outcome = state.row_drag.on_input(input, &hit);
        let released = matches!(
            input,
            Input::Pointer(pointer) if pointer.phase == PointerPhase::Up
        );
        if released {
            state.drag_index = None;
            state.pressed_index = None;
        }
        let action = drag(&self.paint.path, row_index, outcome);
        if action.is_some() {
            return action;
        }
        if matches!(
            input,
            Input::Pointer(pointer) if pointer.phase == PointerPhase::Down
        ) {
            return index(&self.paint.path, Outcome::captured());
        }
        if released && was_pressed {
            return if hit.over() {
                index(&self.paint.path, Outcome::set(row_index))
            } else {
                Some(Action::request_redraw().and_capture())
            };
        }
        None
    }
}

fn scroll_action(
    path: &str,
    outcome: Outcome<usize>,
    before: f32,
    after: f32,
) -> Option<Action<UiEvent>> {
    if outcome.is_captured() && outcome.value().is_none() && before != after {
        Some(Action::request_redraw().and_capture())
    } else {
        index(path, outcome)
    }
}

fn divider_drag(value: f32, minimum: f32) -> Scalar {
    Scalar::builder()
        .track(Track::HorizontalPixels { minimum, value })
        .hover(Hover::new(CursorShape::ResizeH))
        .build()
}

#[cfg(test)]
mod tests {
    use std::borrow::Cow;

    use iced::{
        Pixels, Point, Size,
        advanced::{
            Widget as IcedWidget,
            graphics::text::font_system,
            layout::{Layout, Limits, Node},
            widget::Tree,
        },
        event, mouse,
        window::RedrawRequest,
    };
    use iced_renderer::fallback::Renderer as FallbackRenderer;
    use iced_tiny_skia::Renderer as TinySkiaRenderer;
    use kithara_test_utils::kithara;

    use super::*;
    use crate::{
        atoms::table::{TableCell, face::Drawn, table_body, table_row_rect},
        builtin,
        draw::{DrawCmd, Geom},
        module::{TableColumn, TableColumnStyle},
        render::{
            ControlAction, DragPhase,
            fonts::{FONT_BYTES, SANS},
        },
        shaping::TextContext,
    };

    fn rows() -> Vec<TableRowData> {
        (0..5)
            .map(|index| {
                TableRowData::new(
                    vec![
                        ("index".to_owned(), TableCell::Text((index + 1).to_string())),
                        ("title".to_owned(), TableCell::Text(format!("Row {index}"))),
                        ("artist".to_owned(), TableCell::Text("Detail".to_owned())),
                    ],
                    index == 1,
                )
            })
            .collect()
    }

    fn columns() -> Vec<ColumnLayout> {
        [
            TableColumn::new("index", "#", TableColumnStyle::Index, 28.0, false),
            TableColumn::new("title", "NAME", TableColumnStyle::Primary, 180.0, true),
            TableColumn::new(
                "artist",
                "DETAIL",
                TableColumnStyle::Secondary,
                200.0,
                false,
            ),
        ]
        .into_iter()
        .map(|column| ColumnLayout {
            width: column.width(),
            column,
        })
        .collect()
    }

    fn paint() -> TablePaint {
        TablePaint::new("library/tracks", rows(), columns(), builtin::skin())
    }

    fn program() -> TableProgram {
        let paint = paint();
        let config = paint.config();
        TableProgram { config, paint }
    }

    fn headless_renderer() -> Renderer {
        let mut fonts = font_system()
            .write()
            .unwrap_or_else(|error| panic!("iced font system lock must be available: {error}"));
        for bytes in FONT_BYTES {
            fonts.load_font(Cow::Borrowed(bytes));
        }
        drop(fonts);

        FallbackRenderer::Secondary(TinySkiaRenderer::new(SANS, Pixels(14.0)))
    }

    #[kithara::test]
    fn body_rows_are_scoped_under_a_vertical_clip() {
        let paint = paint();
        let mut text = TextContext::from(paint.face.skin().text_resources());
        let bounds = Rect {
            h: 120.0,
            w: 180.0,
            x: 0.0,
            y: 0.0,
        };
        let list = paint.face.commands(
            &mut text,
            bounds,
            &Drawn {
                columns: paint.face.columns().to_vec(),
                horizontal: 20.0,
                hovered: None,
                pressed: None,
                vertical: 13.0,
            },
        );
        let Some(DrawCmd::Clip { region, list }) = list.commands().first() else {
            panic!("an overflowing track list must start with its outer viewport clip");
        };
        assert_eq!(*region, bounds);
        assert!(list.commands().iter().any(|command| {
            matches!(command, DrawCmd::Clip { region, .. } if *region == table_body(bounds, paint.face.skin()))
        }));
    }

    #[kithara::test]
    fn outer_horizontal_clip_exists_only_while_columns_overflow() {
        for (width, clipped) in [(180.0, true), (900.0, false)] {
            let paint = paint();
            let mut text = TextContext::from(paint.face.skin().text_resources());
            let list = paint.face.commands(
                &mut text,
                Rect {
                    h: 120.0,
                    w: width,
                    x: 0.0,
                    y: 0.0,
                },
                &Drawn {
                    columns: paint.face.columns().to_vec(),
                    horizontal: 0.0,
                    hovered: None,
                    pressed: None,
                    vertical: 0.0,
                },
            );
            assert_eq!(
                matches!(list.commands().first(), Some(DrawCmd::Clip { .. })),
                clipped
            );
        }
    }

    #[kithara::test]
    fn divider_drag_at_nonzero_origin_uses_the_full_hit_width_and_exact_travel() {
        let program = program();
        let bounds = Rectangle::new(Point::new(37.0, 23.0), Size::new(180.0, 120.0));
        let dividers = table_dividers(
            local_rect(bounds),
            program.paint.face.columns(),
            0.0,
            program.paint.face.skin(),
        );
        let divider = &dividers[0];
        assert!(divider.hit.w > divider.paint.w);
        let point = Point::new(
            bounds.x + divider.hit.x + 0.5,
            bounds.y + divider.hit.y + divider.hit.h / 2.0,
        );
        let mut state = TableState::default();
        let press = Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left));
        let action = canvas::Program::update(
            &program,
            &mut state,
            &press,
            bounds,
            Cursor::Available(point),
        )
        .unwrap_or_else(|| panic!("pressing the wider divider hit area must capture"));
        assert_eq!(action.into_inner().2, event::Status::Captured);

        let moved = Point::new(point.x + 20.0, point.y);
        let action = canvas::Program::update(
            &program,
            &mut state,
            &Event::Mouse(mouse::Event::CursorMoved { position: moved }),
            bounds,
            Cursor::Available(moved),
        )
        .unwrap_or_else(|| panic!("dragging the divider must publish its new width"));
        assert!(matches!(
            action.into_inner().0,
            Some(UiEvent::Control {
                action: ControlAction::SetScalar(value),
                ..
            }) if value == f64::from(divider.value + 20.0)
        ));
    }

    #[kithara::test]
    fn leaf_divider_state_follows_its_column_across_reorder_and_removal() {
        let program = program();
        let bounds = Rectangle::new(Point::ORIGIN, Size::new(180.0, 120.0));
        let dividers = table_dividers(
            bounds.into(),
            program.paint.face.columns(),
            0.0,
            program.paint.face.skin(),
        );
        let divider = &dividers[0];
        assert_eq!(divider.column.id(), "index");
        let point = Point::new(divider.hit.x + 0.5, divider.hit.y + divider.hit.h / 2.0);
        let mut state = TableState::default();
        canvas::Program::update(
            &program,
            &mut state,
            &Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)),
            bounds,
            Cursor::Available(point),
        )
        .unwrap_or_else(|| panic!("the index divider press must be retained"));

        let mut reordered = columns();
        reordered.push(ColumnLayout {
            column: TableColumn::new(
                "transition",
                "ACTION",
                TableColumnStyle::Transition,
                130.0,
                false,
            ),
            width: 130.0,
        });
        reordered.swap(0, 2);
        let paint = TablePaint::new("library/tracks", rows(), reordered, builtin::skin());
        let config = paint.config();
        let reordered = TableProgram { config, paint };
        let moved = Point::new(point.x + 20.0, point.y);
        let action = canvas::Program::update(
            &reordered,
            &mut state,
            &Event::Mouse(mouse::Event::CursorMoved { position: moved }),
            bounds,
            Cursor::Available(moved),
        )
        .unwrap_or_else(|| panic!("the armed divider must survive a column reorder"));
        assert!(matches!(
            action.into_inner().0,
            Some(UiEvent::Control { path, .. }) if path == "library/tracks/width/index"
        ));

        let paint = TablePaint::new(
            "library/tracks",
            rows(),
            columns()
                .into_iter()
                .filter(|column| column.column.id() != "index")
                .collect(),
            builtin::skin(),
        );
        state.reconcile("library/tracks", &paint.config());
        assert!(
            state
                .dividers
                .iter()
                .all(|(column, _)| column.id() != "index")
        );
    }

    #[kithara::test]
    fn leaf_row_drag_keeps_the_start_index_binder() {
        let program = program();
        let bounds = Rectangle::new(Point::ORIGIN, Size::new(900.0, 220.0));
        let row = table_row_rect(
            bounds.into(),
            program.paint.face.columns(),
            3,
            0.0,
            0.0,
            program.paint.face.skin(),
        );
        let at = |x: f32| Cursor::Available(Point::new(x, row.y + row.h / 2.0));
        let moved = |x: f32| {
            Event::Mouse(mouse::Event::CursorMoved {
                position: Point::new(x, row.y + row.h / 2.0),
            })
        };
        let mut state = TableState::default();

        canvas::Program::update(
            &program,
            &mut state,
            &Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)),
            bounds,
            at(10.0),
        );
        canvas::Program::update(&program, &mut state, &moved(11.0), bounds, at(11.0));
        let started = canvas::Program::update(&program, &mut state, &moved(40.0), bounds, at(40.0))
            .unwrap_or_else(|| panic!("crossing the threshold must publish"));

        assert_eq!(
            started.into_inner(),
            (
                Some(UiEvent::Control {
                    path: "library/tracks".to_owned(),
                    action: ControlAction::Drag(DragPhase::Start(3)),
                }),
                RedrawRequest::Wait,
                event::Status::Ignored,
            )
        );
    }

    #[kithara::test]
    fn leaf_plain_release_selects_the_armed_row_index() {
        let program = program();
        let bounds = Rectangle::new(Point::ORIGIN, Size::new(900.0, 220.0));
        let row = table_row_rect(
            bounds.into(),
            program.paint.face.columns(),
            2,
            0.0,
            0.0,
            program.paint.face.skin(),
        );
        let cursor = Cursor::Available(Point::new(20.0, row.y + row.h / 2.0));
        let mut state = TableState::default();
        let pressed = canvas::Program::update(
            &program,
            &mut state,
            &Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)),
            bounds,
            cursor,
        )
        .unwrap_or_else(|| panic!("a row press must arm and capture the row"));

        assert_eq!(
            pressed.into_inner(),
            (None, RedrawRequest::Wait, event::Status::Captured)
        );
        assert_eq!(state.pressed_index, Some(2));

        let released = canvas::Program::update(
            &program,
            &mut state,
            &Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)),
            bounds,
            cursor,
        )
        .unwrap_or_else(|| panic!("a plain row release must repaint its pressed state"));
        assert_eq!(state.pressed_index, None);
        assert_eq!(
            released.into_inner(),
            (
                Some(UiEvent::Control {
                    path: "library/tracks".to_owned(),
                    action: ControlAction::SelectIndex(2),
                }),
                RedrawRequest::Wait,
                event::Status::Captured,
            )
        );
    }

    #[kithara::test]
    fn leaf_row_release_outside_only_clears_and_repaints_the_press() {
        let program = program();
        let bounds = Rectangle::new(Point::ORIGIN, Size::new(900.0, 220.0));
        let row = table_row_rect(
            bounds.into(),
            program.paint.face.columns(),
            2,
            0.0,
            0.0,
            program.paint.face.skin(),
        );
        let cursor = Cursor::Available(Point::new(20.0, row.y + row.h / 2.0));
        let mut state = TableState::default();
        let _ = canvas::Program::update(
            &program,
            &mut state,
            &Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)),
            bounds,
            cursor,
        );
        let released = canvas::Program::update(
            &program,
            &mut state,
            &Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)),
            bounds,
            Cursor::Unavailable,
        )
        .unwrap_or_else(|| panic!("release outside must clear and repaint the armed row"));

        assert_eq!(state.pressed_index, None);
        assert_eq!(
            released.into_inner(),
            (None, RedrawRequest::NextFrame, event::Status::Captured)
        );
    }

    #[kithara::test]
    fn horizontal_wheel_passes_the_movable_vertical_state() {
        let program = program();
        let bounds = Rectangle::new(Point::ORIGIN, Size::new(180.0, 120.0));
        let cursor = Cursor::Available(Point::new(90.0, 60.0));
        let mut state = TableState::default();
        let action = canvas::Program::update(
            &program,
            &mut state,
            &Event::Mouse(mouse::Event::WheelScrolled {
                delta: mouse::ScrollDelta::Lines { x: -1.0, y: 0.0 },
            }),
            bounds,
            cursor,
        )
        .unwrap_or_else(|| panic!("the horizontal scroll must consume its matching wheel"));

        assert_eq!(state.vertical.offset(), 0.0);
        assert!(state.horizontal.offset() > 0.0);
        assert_eq!(action.into_inner().2, event::Status::Captured);
    }

    #[kithara::test]
    fn divider_lines_are_retained_as_solid_rectangles() {
        let paint = paint();
        let mut text = TextContext::from(paint.face.skin().text_resources());
        let list = paint.face.commands(
            &mut text,
            Rect {
                h: 120.0,
                w: 900.0,
                x: 0.0,
                y: 0.0,
            },
            &Drawn {
                columns: paint.face.columns().to_vec(),
                horizontal: 0.0,
                hovered: None,
                pressed: None,
                vertical: 0.0,
            },
        );
        assert!(list.commands().iter().any(|command| {
            matches!(
                command,
                DrawCmd::Fill {
                    geom: Geom::Rect(Rect { w, .. }),
                    ..
                } if *w == paint.face.skin().table.divider_width
            )
        }));
    }

    #[kithara::test]
    fn hosted_canvas_forwards_projection_and_rebinds_before_paint() {
        let paint = paint();
        let config = paint.config();
        let mut widget = RetainedCanvas::new(paint, "library/tracks", config);
        let mut tree = Tree::new(&widget as &dyn IcedWidget<UiEvent, Theme, Renderer>);
        let node = Node::new(Size::new(180.0, 120.0));
        let renderer = headless_renderer();
        let mut other = sync_table_scroll("library/history", 40.0, None, 60.0);
        IcedWidget::operate(
            &mut widget,
            &mut tree,
            Layout::new(&node),
            &renderer,
            &mut other,
        );
        let state = tree.state.downcast_ref::<TableState>();
        assert_eq!(
            (state.horizontal.offset(), state.vertical.offset()),
            (0.0, 0.0)
        );

        let mut matching = sync_table_scroll("library/tracks", 14.0, Some(2), 26.0);
        IcedWidget::operate(
            &mut widget,
            &mut tree,
            Layout::new(&node),
            &renderer,
            &mut matching,
        );
        let state = tree.state.downcast_ref::<TableState>();
        assert_eq!(
            (state.horizontal.offset(), state.vertical.offset()),
            (14.0, 26.0)
        );
        assert_eq!(state.pressed_index, Some(2));

        let next_paint = TablePaint::new("library/history", rows(), columns(), builtin::skin());
        let next_config = next_paint.config();
        let next = RetainedCanvas::new(next_paint, "library/history", next_config);
        IcedWidget::diff(&next, &mut tree);
        let state = tree.state.downcast_ref::<TableState>();
        assert_eq!(
            (state.horizontal.offset(), state.vertical.offset()),
            (0.0, 0.0)
        );
    }

    #[kithara::test]
    fn leaf_layout_clamps_offsets_after_rows_shrink_and_viewport_widens() {
        let paint = paint();
        let config = paint.config();
        let mut widget = RetainedCanvas::new(
            TableProgram {
                paint,
                config: config.clone(),
            },
            "library/tracks",
            config,
        );
        let mut tree = Tree::new(&widget as &dyn IcedWidget<UiEvent, Theme, Renderer>);
        let renderer = headless_renderer();
        let narrow = Size::new(180.0, 120.0);
        IcedWidget::layout(
            &mut widget,
            &mut tree,
            &renderer,
            &Limits::new(narrow, narrow),
        );
        {
            let state = tree.state.downcast_mut::<TableState>();
            state.horizontal.sync_offset(500.0);
            state.vertical.sync_offset(500.0);
            assert!(state.horizontal.offset() > 0.0);
            assert!(state.vertical.offset() > 0.0);
        }

        let next_paint = TablePaint::new(
            "library/tracks",
            rows().into_iter().take(1).collect(),
            columns(),
            builtin::skin(),
        );
        let next_config = next_paint.config();
        let next = RetainedCanvas::new(
            TableProgram {
                paint: next_paint,
                config: next_config.clone(),
            },
            "library/tracks",
            next_config,
        );
        IcedWidget::diff(&next, &mut tree);
        widget = next;
        let wide = Size::new(900.0, 300.0);
        IcedWidget::layout(&mut widget, &mut tree, &renderer, &Limits::new(wide, wide));

        let state = tree.state.downcast_ref::<TableState>();
        assert_eq!(
            (state.horizontal.offset(), state.vertical.offset()),
            (0.0, 0.0)
        );
    }
}
