use std::cell::RefCell;

use iced::{
    Element, Event, Rectangle, Renderer, Theme,
    advanced::widget::{Id, Operation},
    mouse::{Cursor, Interaction},
    widget::canvas::{self, Action, Frame, Geometry},
};
use kithara_platform::time::Instant;
use num_traits::ToPrimitive;

use super::{RetainedCanvas, RetainedCanvasState, snapped};
use crate::{
    atoms::tree::Tree,
    backends::{finish, replay_ordered},
    draw::Rect,
    engine::{
        ScrollConfig,
        component::{item::ItemComponent, retained::Component, scroll::ScrollState},
    },
    iced::tree::index,
    interact::{Hit, ScrollAxis, iced as iced_interact},
    render::{InputOwner, Published},
    shaping::TextContext,
};

pub(crate) fn tree_rows<'a>(
    path: &str,
    toggle: Option<String>,
    picture: Tree,
    owner: InputOwner,
) -> Element<'a, Published> {
    let row_count = picture.row_count();
    let row_height = picture.skin().tree.row_height;
    let row_right_inset =
        picture.skin().tree.scrollbar_margin + picture.skin().tree.scrollbar_width;
    let config = TreeConfig {
        row_height,
        row_right_inset,
        row_count,
    };
    match owner {
        InputOwner::Leaf => RetainedCanvas::new(
            TreeProgram {
                picture,
                path: path.to_owned(),
                toggle,
            },
            path,
            config,
        )
        .view(),
        InputOwner::Engine => RetainedCanvas::new(TreePaint { picture }, path, config).view(),
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

        fn custom(&mut self, _id: Option<&Id>, _bounds: Rectangle, state: &mut dyn std::any::Any) {
            if let Some(state) = state.downcast_mut::<TreeState>() {
                state.sync(self.path, self.offset);
            }
        }
    }

    Sync { offset, path }
}

struct TreeProgram {
    path: String,
    toggle: Option<String>,
    picture: Tree,
}

impl canvas::Program<Published> for TreeProgram {
    type State = TreeState;

    fn draw(
        &self,
        state: &TreeState,
        renderer: &Renderer,
        theme: &Theme,
        bounds: Rectangle,
        cursor: Cursor,
    ) -> Vec<Geometry> {
        geometry(&self.picture, state, renderer, theme, bounds, cursor)
    }

    fn mouse_interaction(
        &self,
        state: &TreeState,
        bounds: Rectangle,
        cursor: Cursor,
    ) -> Interaction {
        if hovered_row(
            self.picture.row_count(),
            self.picture.skin().tree.row_height,
            state.scroll.offset,
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

    fn update(
        &self,
        state: &mut TreeState,
        event: &Event,
        bounds: Rectangle,
        cursor: Cursor,
    ) -> Option<Action<Published>> {
        state.reconcile_scroll(
            &self.path,
            self.picture.row_count(),
            self.picture.skin().tree.row_height,
            self.picture.skin().tree.scrollbar_margin + self.picture.skin().tree.scrollbar_width,
        );
        let input = iced_interact::input(event)?;
        if let Some(toggle) = &self.toggle {
            let next =
                ItemComponent::new(toggle.clone(), self.path.clone(), self.picture.row_count());
            let item = match state.toggle.take() {
                Some(item) => item.reconcile(next),
                None => next,
            };
            state.toggle = Some(item);
            let point = cursor.position_in(bounds).map(Into::into);
            let viewport = Rect {
                h: bounds.height,
                w: bounds.width,
                x: 0.0,
                y: 0.0,
            };
            let under = self
                .picture
                .toggle_regions(viewport, state.scroll.offset)
                .into_iter()
                .find(|(_, rect)| Hit::new(point, *rect).over());
            let (row, hit) = under.map_or_else(
                || (None, crate::atoms::table::empty_bounds(viewport)),
                |(row, hit)| (Some(row), hit),
            );
            if let Some(item) = &mut state.toggle {
                let (outcome, child) =
                    item.handle(input, &Hit::new(point, hit), row, Instant::now());
                if let Some(action) = crate::iced::tree::engine(toggle, child, outcome) {
                    return Some(action);
                }
            }
        }
        let before = state.scroll.offset;
        let outcome = state
            .scroll
            .handle(input, &iced_interact::hit(bounds, cursor));
        if outcome.is_captured() && outcome.value().is_none() && state.scroll.offset != before {
            Some(Action::request_redraw().and_capture())
        } else {
            index(&self.path, outcome)
        }
    }
}

struct TreePaint {
    picture: Tree,
}

impl canvas::Program<Published> for TreePaint {
    type State = TreeState;

    fn draw(
        &self,
        state: &TreeState,
        renderer: &Renderer,
        theme: &Theme,
        bounds: Rectangle,
        cursor: Cursor,
    ) -> Vec<Geometry> {
        geometry(&self.picture, state, renderer, theme, bounds, cursor)
    }
}

fn geometry(
    picture: &Tree,
    state: &TreeState,
    renderer: &Renderer,
    _theme: &Theme,
    bounds: Rectangle,
    cursor: Cursor,
) -> Vec<Geometry> {
    let bounds = snapped(bounds);
    let mut frame = Frame::new(renderer, bounds.size());
    let mut text = state.text.borrow_mut();
    let text = text.get_or_insert_with(|| picture.skin().text_resources.as_ref().into());
    let viewport = Rect {
        h: bounds.height,
        w: bounds.width,
        x: 0.0,
        y: 0.0,
    };
    let hovered = hovered_row(
        picture.row_count(),
        picture.skin().tree.row_height,
        state.scroll.offset,
        bounds,
        cursor,
    );
    let list = picture.row_commands(text, viewport, state.scroll.offset, hovered);
    replay_ordered(&list, &mut frame, picture.skin().text_resources.as_ref());
    vec![finish(frame.into_geometry())]
}

#[derive(Default)]
struct TreeState {
    text: RefCell<Option<TextContext>>,
    scroll: ScrollState,
    toggle: Option<ItemComponent>,
    path: String,
}

#[derive(Clone, Copy)]
struct TreeConfig {
    row_height: f32,
    row_right_inset: f32,
    row_count: usize,
}

impl TreeState {
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
                row_height,
                row_right_inset,
                row_count,
            },
        );
    }

    fn sync(&mut self, path: &str, offset: f32) {
        if self.path == path {
            self.scroll.sync_offset(offset);
        }
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

#[cfg(test)]
mod tests {
    use iced::mouse;
    use kithara_test_utils::kithara;

    use super::*;
    use crate::{
        builtin,
        interact::{Input, Outcome, PointerPhase, mouse as mouse_input},
        module::IconName,
        render::TreeRow,
    };

    fn rows() -> [TreeRow<'static>; 3] {
        [
            TreeRow {
                depth: 0,
                label: "First",
                icon: IconName::Folder,
                count: None,
                expanded: Some(true),
                page: false,
                selected: false,
                muted: false,
            },
            TreeRow {
                depth: 1,
                label: "Second",
                icon: IconName::Playlist,
                count: Some(2),
                expanded: None,
                page: false,
                selected: true,
                muted: false,
            },
            TreeRow {
                depth: 1,
                label: "Third",
                icon: IconName::Zvuk,
                count: None,
                expanded: None,
                page: false,
                selected: false,
                muted: true,
            },
        ]
    }

    #[kithara::test]
    fn paint_only_program_has_no_input_update() {
        let skin = builtin::skin();
        let paint = TreePaint {
            picture: Tree::new(&rows(), None, skin),
        };
        let mut state = TreeState::default();
        state.reconcile_scroll(
            "tree/browser",
            paint.picture.row_count(),
            skin.tree.row_height,
            skin.tree.scrollbar_margin + skin.tree.scrollbar_width,
        );
        let event = Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left));

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
            picture: Tree::new(&rows(), None, skin),
            path: "tree/browser".to_owned(),
            toggle: None,
        };
        let mut state = TreeState::default();
        let bounds = Rectangle {
            height: 48.0,
            width: 180.0,
            x: 0.0,
            y: 0.0,
        };
        let event = Event::Mouse(mouse::Event::WheelScrolled {
            delta: mouse::ScrollDelta::Lines { x: 0.0, y: -1.0 },
        });

        let action = canvas::Program::update(
            &program,
            &mut state,
            &event,
            bounds,
            Cursor::Available(iced::Point::new(90.0, 24.0)),
        )
        .expect("a movable leaf tree must answer its wheel");
        let (message, redraw, status) = action.into_inner();

        assert_eq!(message, None);
        assert_eq!(redraw, iced::window::RedrawRequest::NextFrame);
        assert_eq!(status, iced::event::Status::Captured);
        assert_eq!(state.scroll.offset, 24.0);
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
            Outcome::IGNORED
        );
        assert_eq!(state.scroll.offset, 0.0);
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
        assert_eq!(state.scroll.offset, skin.tree.row_height);

        let mut other = sync_tree_scroll("tree/other", 0.0);
        other.custom(None, Rectangle::default(), &mut state);
        assert_eq!(state.scroll.offset, skin.tree.row_height);
    }
}
