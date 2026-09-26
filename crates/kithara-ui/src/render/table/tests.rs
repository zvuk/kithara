use std::borrow::Cow;

use iced::{
    Event, Pixels, Point, Rectangle, Renderer, Size, Theme,
    advanced::{
        Widget as IcedWidget,
        graphics::text::font_system,
        layout::{Layout, Limits, Node},
        widget::Tree,
    },
    event, mouse,
    mouse::Cursor,
    widget::canvas,
    window::RedrawRequest,
};
use iced_renderer::fallback::Renderer as FallbackRenderer;
use iced_tiny_skia::Renderer as TinySkiaRenderer;
use kithara_test_utils::kithara;

use super::{
    super::{Marked, Skin, UiEvent, controls::RetainedCanvas},
    paint::{TablePaint, TableState, local_rect},
    widget::*,
};
use crate::{
    atoms::table::{
        ColumnLayout, TableRowData, face::Drawn, table_body, table_dividers, table_row_rect,
    },
    builtin,
    draw::{DrawCmd, DrawList, Geom, Rect, Rgba},
    ids::SourceUri,
    module::{TableColumn, TableColumnStyle},
    render::{
        ControlAction, DragPhase,
        fonts::{FONT_BYTES, SANS},
    },
    shaping::TextContext,
    skin::parse_skin_over,
};

fn rows() -> Vec<TableRowData> {
    (0..5)
        .map(|index| {
            let (number, title) = ((index + 1).to_string(), format!("Row {index}"));
            TableRowData::from(&crate::render::TableRow::new(
                vec![
                    crate::render::TableCell::text("index", &number),
                    crate::render::TableCell::text("title", &title),
                    crate::render::TableCell::text("artist", "Detail"),
                ],
                index == 1,
            ))
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

/// One frame of the immediate host's table: the program is built afresh,
/// and the canvas state is the one thing that survived the last frame.
/// Reports what that frame cost.
fn marked(state: &TableState, paint: &TablePaint, drawn: &Drawn) -> Marked {
    let mut text = TextContext::from(paint.face.skin().text_resources());
    let bounds = Rect {
        h: 120.0,
        w: 180.0,
        x: 0.0,
        y: 0.0,
    };
    state.mark(bounds, drawn, &paint.face, || {
        paint.face.commands(&mut text, bounds, drawn)
    })
}

/// What a table standing still is drawn from.
fn still(paint: &TablePaint) -> Drawn {
    Drawn {
        columns: paint.face.columns().to_vec(),
        horizontal: 0.0,
        hovered: None,
        pressed: None,
        vertical: 0.0,
    }
}

/// The host rebuilds the whole element tree every frame, so a table nothing
/// touched must not be built a second time. It was the one canvas this host
/// drew from scratch on every single frame.
#[kithara::test]
fn an_unchanged_table_builds_no_marks_again() {
    let paint = paint();
    let state = TableState::default();
    let drawn = still(&paint);
    marked(&state, &paint, &drawn);

    assert_eq!(
        marked(&state, &paint, &drawn),
        Marked::Kept,
        "a table nothing touched must keep what it drew"
    );
}

/// The host builds a fresh program every frame, so the key the second frame
/// offers is a different `Rc` carrying an equal face. A cache that compared
/// the pointer would miss on every frame of a table nothing touched, and the
/// test above cannot say so: it hands the second frame the very program the
/// first one keyed on.
#[kithara::test]
fn a_table_rebuilt_from_scratch_still_holds_its_key() {
    let state = TableState::default();
    let first = paint();
    marked(&state, &first, &still(&first));

    let second = paint();
    assert_eq!(
        marked(&state, &second, &still(&second)),
        Marked::Kept,
        "a face rebuilt with the same rows must answer the key it built"
    );
}

/// A row under the pointer is a different picture.
#[kithara::test]
fn a_table_whose_hovered_row_moved_draws_again() {
    let paint = paint();
    let state = TableState::default();
    let drawn = still(&paint);
    marked(&state, &paint, &drawn);

    assert_eq!(
        marked(
            &state,
            &paint,
            &Drawn {
                hovered: Some(2),
                ..drawn
            }
        ),
        Marked::Changed,
        "a row taken under the pointer must draw again"
    );
}

/// Scrolling carries other rows under the viewport, which is another
/// picture and not the one that was kept.
#[kithara::test]
fn a_scrolled_table_draws_again() {
    let paint = paint();
    let state = TableState::default();
    let drawn = still(&paint);
    marked(&state, &paint, &drawn);

    assert_eq!(
        marked(
            &state,
            &paint,
            &Drawn {
                vertical: 13.0,
                ..drawn
            }
        ),
        Marked::Changed,
        "a table scrolled to other rows must draw again"
    );
}

/// The rows and the scroll offsets are the loud part of the key, and a key
/// that only carried those would hold across a change of theme and freeze
/// the table on the old skin.
#[kithara::test]
fn a_reskinned_table_draws_again() {
    let paint = paint();
    let state = TableState::default();
    let drawn = still(&paint);
    marked(&state, &paint, &drawn);
    let mut skin = builtin::skin().clone();
    skin.table.header_height += 7.0;

    assert_eq!(
        marked(
            &state,
            &TablePaint::new("library/tracks", rows(), columns(), &skin),
            &drawn
        ),
        Marked::Changed,
        "a table given another skin must draw again"
    );
}

/// The colour a cell's word is drawn in, wherever the nested clips put it.
fn word_color(list: &DrawList, wanted: &str) -> Option<Rgba> {
    for command in list.commands() {
        match command {
            DrawCmd::Text { content, color, .. } if &**content == wanted => {
                return Some(*color);
            }
            DrawCmd::Clip { list, .. } => {
                if let Some(color) = word_color(list, wanted) {
                    return Some(color);
                }
            }
            _ => {}
        }
    }
    None
}

fn drawn_word(skin: &Skin, wanted: &str) -> Rgba {
    let paint = TablePaint::new("library/tracks", rows(), columns(), skin);
    let mut text = TextContext::from(skin.text_resources());
    let bounds = Rect {
        h: 240.0,
        w: 900.0,
        x: 0.0,
        y: 0.0,
    };
    let drawn = still(&paint);
    let list = paint.face.commands(&mut text, bounds, &drawn);
    word_color(&list, wanted).unwrap_or_else(|| panic!("the table must draw {wanted}"))
}

#[kithara::test]
fn a_primary_cell_takes_the_colour_its_skin_role_names() {
    let skin = builtin::skin();

    assert_eq!(
        drawn_word(skin, "Row 0"),
        skin.rgba(skin.table.primary_text.color)
    );
}

#[kithara::test]
fn a_primary_cell_follows_a_skin_written_over_the_builtin_one() {
    let origin = SourceUri("loud.kskin.ron".to_owned());
    let text = r##"(
        schema: "kithara.skin",
        version: 1,
        id: "kithara-loud",
        table: (primary_text: (color: Danger, font: Display, size: 12.0, spacing: 0.0, weight: Medium)),
    )"##;
    let document = parse_skin_over(builtin::skin_doc(), text, &origin).expect("the patch parses");
    let skin = Skin::resolve(document, builtin::text_doc(), &origin, &builtin::resolver())
        .expect("the patched document resolves");

    assert_eq!(drawn_word(&skin, "Row 0"), skin.palette.danger);
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
    .expect("pressing the wider divider hit area must capture");
    assert_eq!(action.into_inner().2, event::Status::Captured);

    let moved = Point::new(point.x + 20.0, point.y);
    let action = canvas::Program::update(
        &program,
        &mut state,
        &Event::Mouse(mouse::Event::CursorMoved { position: moved }),
        bounds,
        Cursor::Available(moved),
    )
    .expect("dragging the divider must publish its new width");
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
    .expect("the index divider press must be retained");

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
    .expect("the armed divider must survive a column reorder");
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
        .expect("crossing the threshold must publish");

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
    .expect("a row press must arm and capture the row");

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
    .expect("a plain row release must repaint its pressed state");
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
    .expect("release outside must clear and repaint the armed row");

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
    .expect("the horizontal scroll must consume its matching wheel");

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
