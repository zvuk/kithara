use std::{cell::RefCell, ops::Range};

use iced::{
    Element, Event, Rectangle, Renderer, Theme,
    advanced::widget::Operation,
    mouse::{Cursor, Interaction},
    widget::canvas::{self, Action, Frame, Geometry},
};
use num_traits::ToPrimitive;

use super::{RetainedCanvas, RetainedCanvasState, tree_row::TreeRowPaint};
use crate::{
    backends::replay_ordered,
    draw::{DrawList, DrawListBuilder, Rect},
    engine::{ScrollConfig, ScrollState},
    interact::{ScrollAxis, iced as iced_interact},
    render::{InputOwner, Skin, TreeRow, UiEvent, index},
    text::TextContext,
};

pub(crate) fn tree_rows<'a>(
    path: &str,
    rows: &[TreeRow<'_>],
    skin: &'a Skin,
    owner: InputOwner,
) -> Element<'a, UiEvent> {
    let paint = TreePaint::new(rows, skin);
    let row_count = paint.rows.len();
    let row_height = skin.tree.row_height;
    let row_right_inset = skin.tree.scrollbar_margin + skin.tree.scrollbar_width;
    let config = TreeConfig {
        row_count,
        row_height,
        row_right_inset,
    };
    match owner {
        InputOwner::Leaf => RetainedCanvas::new(
            TreeProgram {
                paint,
                path: path.to_owned(),
            },
            path,
            config,
        )
        .view(),
        InputOwner::Engine => RetainedCanvas::new(paint, path, config).view(),
    }
}

pub(crate) fn sync_tree_scroll(path: &str, offset: f32) -> impl Operation + '_ {
    struct Sync<'a> {
        offset: f32,
        path: &'a str,
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
            if let Some(state) = state.downcast_mut::<TreeState>() {
                state.sync(self.path, self.offset);
            }
        }
    }

    Sync { offset, path }
}

struct TreeProgram<'skin> {
    paint: TreePaint<'skin>,
    path: String,
}

impl canvas::Program<UiEvent> for TreeProgram<'_> {
    type State = TreeState;

    fn update(
        &self,
        state: &mut TreeState,
        event: &Event,
        bounds: Rectangle,
        cursor: Cursor,
    ) -> Option<Action<UiEvent>> {
        state.reconcile_scroll(
            &self.path,
            self.paint.rows.len(),
            self.paint.skin.tree.row_height,
            self.paint.skin.tree.scrollbar_margin + self.paint.skin.tree.scrollbar_width,
        );
        let input = iced_interact::input(event)?;
        let before = state.scroll.offset();
        let outcome = state
            .scroll
            .handle(input, &iced_interact::hit(bounds, cursor));
        if outcome.is_captured() && outcome.value().is_none() && state.scroll.offset() != before {
            Some(Action::request_redraw().and_capture())
        } else {
            index(&self.path, outcome)
        }
    }

    fn draw(
        &self,
        state: &TreeState,
        renderer: &Renderer,
        theme: &Theme,
        bounds: Rectangle,
        cursor: Cursor,
    ) -> Vec<Geometry> {
        self.paint.geometry(state, renderer, theme, bounds, cursor)
    }

    fn mouse_interaction(
        &self,
        state: &TreeState,
        bounds: Rectangle,
        cursor: Cursor,
    ) -> Interaction {
        if hovered_row(
            self.paint.rows.len(),
            self.paint.skin.tree.row_height,
            state.scroll.offset(),
            bounds,
            cursor,
        )
        .is_some()
        {
            Interaction::Pointer
        } else {
            Interaction::None
        }
    }
}

struct TreePaint<'skin> {
    rows: Vec<TreeRowPaint>,
    skin: &'skin Skin,
}

impl<'skin> TreePaint<'skin> {
    fn new(rows: &[TreeRow<'_>], skin: &'skin Skin) -> Self {
        Self {
            rows: rows.iter().copied().map(TreeRowPaint::new).collect(),
            skin,
        }
    }

    fn geometry(
        &self,
        state: &TreeState,
        renderer: &Renderer,
        _theme: &Theme,
        bounds: Rectangle,
        cursor: Cursor,
    ) -> Vec<Geometry> {
        let mut frame = Frame::new(renderer, bounds.size());
        let mut text = state.text.borrow_mut();
        let text = text.get_or_insert_with(|| self.skin.text_resources().into());
        let viewport = Rect {
            h: bounds.height,
            w: bounds.width,
            x: 0.0,
            y: 0.0,
        };
        let hovered = hovered_row(
            self.rows.len(),
            self.skin.tree.row_height,
            state.scroll.offset(),
            bounds,
            cursor,
        );
        let list = self.commands(text, viewport, state.scroll.offset(), hovered);
        replay_ordered(&list, &mut frame, self.skin.text_resources());
        vec![frame.into_geometry()]
    }

    fn commands(
        &self,
        text: &mut TextContext,
        viewport: Rect,
        offset: f32,
        hovered: Option<usize>,
    ) -> DrawList {
        paint_rows(&self.rows, self.skin, text, viewport, offset, hovered)
    }
}

impl canvas::Program<UiEvent> for TreePaint<'_> {
    type State = TreeState;

    fn draw(
        &self,
        state: &TreeState,
        renderer: &Renderer,
        theme: &Theme,
        bounds: Rectangle,
        cursor: Cursor,
    ) -> Vec<Geometry> {
        self.geometry(state, renderer, theme, bounds, cursor)
    }
}

#[derive(Default)]
struct TreeState {
    path: String,
    scroll: ScrollState,
    text: RefCell<Option<TextContext>>,
}

#[derive(Clone, Copy)]
struct TreeConfig {
    row_count: usize,
    row_height: f32,
    row_right_inset: f32,
}

impl TreeState {
    fn sync(&mut self, path: &str, offset: f32) {
        if self.path == path {
            self.scroll.sync_offset(offset);
        }
    }

    fn reconcile_scroll(
        &mut self,
        path: &str,
        row_count: usize,
        row_height: f32,
        row_right_inset: f32,
    ) {
        self.reconcile_canvas(
            path,
            &TreeConfig {
                row_count,
                row_height,
                row_right_inset,
            },
        );
    }
}

impl RetainedCanvasState for TreeState {
    type Config = TreeConfig;

    fn reconcile_canvas(&mut self, path: &str, config: &Self::Config) {
        if self.path == path {
            self.scroll.reconcile(ScrollConfig::items(
                ScrollAxis::Vertical,
                config
                    .row_count
                    .to_f32()
                    .map_or(f32::MAX, |count| count * config.row_height),
                config.row_count,
                config.row_height,
                config.row_height,
                config.row_right_inset,
            ));
        } else {
            self.path = path.to_owned();
            self.scroll = ScrollState::new(ScrollConfig::items(
                ScrollAxis::Vertical,
                config
                    .row_count
                    .to_f32()
                    .map_or(f32::MAX, |count| count * config.row_height),
                config.row_count,
                config.row_height,
                config.row_height,
                config.row_right_inset,
            ));
        }
    }

    fn set_canvas_viewport(&mut self, size: iced::Size, _config: &Self::Config) {
        self.scroll.set_viewport(size.height);
    }
}

fn hovered_row(
    row_count: usize,
    row_height: f32,
    offset: f32,
    bounds: Rectangle,
    cursor: Cursor,
) -> Option<usize> {
    if row_height <= 0.0 {
        return None;
    }
    let point = cursor.position_in(bounds)?;
    ((point.y + offset) / row_height)
        .floor()
        .to_usize()
        .filter(|index| *index < row_count)
}

fn paint_rows(
    rows: &[TreeRowPaint],
    skin: &Skin,
    text: &mut TextContext,
    viewport: Rect,
    offset: f32,
    hovered: Option<usize>,
) -> DrawList {
    let mut contents = DrawListBuilder::default();
    let visible = visible_rows(rows.len(), skin.tree.row_height, viewport.h, offset);
    let first = visible.start;
    for (relative, row) in rows[visible].iter().enumerate() {
        let index = first + relative;
        let y = index.to_f32().map_or(f32::MAX, |index| {
            index.mul_add(skin.tree.row_height, viewport.y) - offset
        });
        row.paint(
            &mut contents,
            text,
            Rect {
                h: skin.tree.row_height,
                w: viewport.w,
                x: viewport.x,
                y,
            },
            hovered == Some(index),
            skin,
        );
    }

    let mut list = DrawListBuilder::default();
    list.clip(viewport, contents.finish());
    paint_scrollbar(&mut list, rows.len(), skin, viewport, offset);
    list.finish()
}

fn visible_rows(
    row_count: usize,
    row_height: f32,
    viewport_height: f32,
    offset: f32,
) -> Range<usize> {
    if row_height <= 0.0 || viewport_height <= 0.0 {
        return 0..0;
    }
    let start = (offset.max(0.0) / row_height)
        .floor()
        .to_usize()
        .map_or(row_count, |index| index.min(row_count));
    let end = ((offset.max(0.0) + viewport_height) / row_height)
        .ceil()
        .to_usize()
        .map_or(row_count, |index| index.min(row_count));
    start..end.max(start)
}

fn paint_scrollbar(
    list: &mut DrawListBuilder,
    row_count: usize,
    skin: &Skin,
    viewport: Rect,
    offset: f32,
) {
    let content_height = row_count
        .to_f32()
        .map_or(f32::MAX, |count| count * skin.tree.row_height);
    let max_offset = (content_height - viewport.h).max(0.0);
    if viewport.h <= 0.0 || max_offset <= 0.0 {
        return;
    }
    let width = skin.tree.scrollbar_width.min(viewport.w.max(0.0));
    let x = (viewport.x + viewport.w - skin.tree.scrollbar_margin - width).max(viewport.x);
    let rail = Rect {
        h: viewport.h,
        w: width,
        x,
        y: viewport.y,
    };
    let thumb_height = (viewport.h * viewport.h / content_height)
        .max(width)
        .min(viewport.h);
    let travel = viewport.h - thumb_height;
    let thumb = Rect {
        h: thumb_height,
        w: width,
        x,
        y: viewport.y + offset.clamp(0.0, max_offset) / max_offset * travel,
    };
    list.fill_rect(rail, skin.rgba(skin.tree.scrollbar_background));
    list.fill_rect(thumb, skin.rgba(skin.tree.scroller_color));
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::*;
    use crate::{
        builtin,
        draw::{DrawCmd, Geom},
        interact::{Input, PointerPhase, mouse as mouse_input},
        render::TreeIcon,
    };

    fn rows() -> [TreeRow<'static>; 3] {
        [
            TreeRow {
                depth: 0,
                label: "First",
                icon: TreeIcon::Folder,
                count: None,
                expanded: Some(true),
                selected: false,
                muted: false,
            },
            TreeRow {
                depth: 1,
                label: "Second",
                icon: TreeIcon::Playlist,
                count: Some(2),
                expanded: None,
                selected: true,
                muted: false,
            },
            TreeRow {
                depth: 1,
                label: "Third",
                icon: TreeIcon::Zvuk,
                count: None,
                expanded: None,
                selected: false,
                muted: true,
            },
        ]
    }

    fn commands(offset: f32, viewport: Rect) -> DrawList {
        let skin = builtin::skin();
        let paint = TreePaint::new(&rows(), skin);
        let mut text = TextContext::from(skin.text_resources());
        paint.commands(&mut text, viewport, offset, None)
    }

    #[kithara::test]
    fn scrolled_rows_are_nested_under_the_viewport_clip() {
        let skin = builtin::skin();
        let viewport = Rect {
            h: 48.0,
            w: 180.0,
            x: 0.0,
            y: 0.0,
        };
        let list = commands(skin.tree.row_height / 2.0, viewport);
        let Some(DrawCmd::Clip { region, list }) = list.commands().first() else {
            panic!("the retained tree must start with its scoped viewport clip");
        };

        assert_eq!(*region, viewport);
        assert!(list.commands().iter().any(|command| {
            matches!(
                command,
                DrawCmd::Fill {
                    geom: Geom::Rect(Rect { y, .. }),
                    ..
                } if *y < viewport.y
            )
        }));
    }

    #[kithara::test]
    fn offset_changes_the_retained_row_positions() {
        let viewport = Rect {
            h: 48.0,
            w: 180.0,
            x: 0.0,
            y: 0.0,
        };

        assert_ne!(
            commands(0.0, viewport),
            commands(builtin::skin().tree.row_height, viewport)
        );
    }

    #[kithara::test]
    fn rows_fully_outside_the_viewport_are_not_retained() {
        let viewport = Rect {
            h: builtin::skin().tree.row_height,
            w: 180.0,
            x: 0.0,
            y: 0.0,
        };
        let list = commands(0.0, viewport);
        let Some(DrawCmd::Clip { list, .. }) = list.commands().first() else {
            panic!("the tree painter must retain a clip");
        };

        assert!(list.commands().iter().all(|command| {
            !matches!(
                command,
                DrawCmd::Text { content, .. } if content == "Second" || content == "Third"
            )
        }));
    }

    #[kithara::test]
    fn the_zvuk_row_stays_on_the_neutral_geometry_seam() {
        let skin = builtin::skin();
        let paint = TreePaint::new(&rows()[2..], skin);
        let mut text = TextContext::from(skin.text_resources());
        let list = paint.commands(
            &mut text,
            Rect {
                h: skin.tree.row_height,
                w: 180.0,
                x: 0.0,
                y: 0.0,
            },
            0.0,
            None,
        );
        let Some(DrawCmd::Clip { list, .. }) = list.commands().first() else {
            panic!("the tree painter must retain a clip");
        };

        assert!(list.commands().iter().any(|command| {
            matches!(
                command,
                DrawCmd::Stroke {
                    geom: Geom::RoundedRect { .. } | Geom::Arc { .. },
                    ..
                }
            )
        }));
    }

    #[kithara::test]
    fn paint_only_program_has_no_input_update() {
        let skin = builtin::skin();
        let paint = TreePaint::new(&rows(), skin);
        let mut state = TreeState::default();
        state.reconcile_scroll(
            "tree/browser",
            paint.rows.len(),
            skin.tree.row_height,
            skin.tree.scrollbar_margin + skin.tree.scrollbar_width,
        );
        let event = Event::Mouse(iced::mouse::Event::ButtonPressed(iced::mouse::Button::Left));

        assert!(
            canvas::Program::update(
                &paint,
                &mut state,
                &event,
                Rectangle {
                    height: 48.0,
                    width: 180.0,
                    x: 0.0,
                    y: 0.0,
                },
                Cursor::Unavailable,
            )
            .is_none()
        );
    }

    #[kithara::test]
    fn leaf_wheel_moves_offset_without_notifying_the_document() {
        let skin = builtin::skin();
        let program = TreeProgram {
            paint: TreePaint::new(&rows(), skin),
            path: "tree/browser".to_owned(),
        };
        let mut state = TreeState::default();
        let bounds = Rectangle {
            height: 48.0,
            width: 180.0,
            x: 0.0,
            y: 0.0,
        };
        let event = Event::Mouse(iced::mouse::Event::WheelScrolled {
            delta: iced::mouse::ScrollDelta::Lines { x: 0.0, y: -1.0 },
        });

        let action = canvas::Program::update(
            &program,
            &mut state,
            &event,
            bounds,
            Cursor::Available(iced::Point::new(90.0, 24.0)),
        )
        .unwrap_or_else(|| panic!("a movable leaf tree must answer its wheel"));
        let (message, redraw, status) = action.into_inner();

        assert_eq!(message, None);
        assert_eq!(redraw, iced::window::RedrawRequest::NextFrame);
        assert_eq!(status, iced::event::Status::Captured);
        assert_eq!(state.scroll.offset(), 24.0);
    }

    #[kithara::test]
    fn non_wheel_input_does_not_move_the_paint_snapshot() {
        let skin = builtin::skin();
        let mut state = TreeState::default();
        state.reconcile_scroll(
            "tree/browser",
            rows().len(),
            skin.tree.row_height,
            skin.tree.scrollbar_margin + skin.tree.scrollbar_width,
        );
        state.scroll.set_viewport(48.0);
        let hit = iced_interact::hit(
            Rectangle {
                height: 48.0,
                width: 180.0,
                x: 0.0,
                y: 0.0,
            },
            Cursor::Unavailable,
        );

        assert_eq!(
            state
                .scroll
                .handle(Input::Pointer(mouse_input(PointerPhase::Up, None)), &hit,),
            crate::interact::Outcome::IGNORED
        );
        assert_eq!(state.scroll.offset(), 0.0);
    }

    #[kithara::test]
    fn scroll_projection_updates_only_the_matching_tree_state() {
        let skin = builtin::skin();
        let mut state = TreeState::default();
        state.reconcile_scroll(
            "tree/browser",
            rows().len(),
            skin.tree.row_height,
            skin.tree.scrollbar_margin + skin.tree.scrollbar_width,
        );
        state.scroll.set_viewport(skin.tree.row_height);

        let mut matching = sync_tree_scroll("tree/browser", skin.tree.row_height);
        matching.custom(None, Rectangle::default(), &mut state);
        assert_eq!(state.scroll.offset(), skin.tree.row_height);

        let mut other = sync_tree_scroll("tree/other", 0.0);
        other.custom(None, Rectangle::default(), &mut state);
        assert_eq!(state.scroll.offset(), skin.tree.row_height);
    }
}
