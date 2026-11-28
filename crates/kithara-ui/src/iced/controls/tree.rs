use std::cell::RefCell;

use iced::{
    Element, Event, Rectangle, Renderer, Theme,
    advanced::widget::{Id, Operation},
    mouse::{Cursor, Interaction},
    widget::canvas::{self, Action, Geometry},
};
use kithara_platform::{sync::Arc, time::Instant};
use num_traits::ToPrimitive;

use super::{Marks, RetainedCanvas, RetainedCanvasState, painted::shaped_with_resources, snapped};
use crate::{
    atoms::tree::Tree,
    backends::replay_ordered,
    draw::Rect,
    engine::{
        ScrollConfig,
        component::{item::ItemComponent, retained::Component, scroll::ScrollState},
    },
    iced::tree::index,
    interact::{Hit, ScrollAxis, iced as iced_interact},
    render::{InputOwner, Published},
    shaping::{TextContext, TextResources},
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
    let resources = &picture.skin().text_resources;
    let list = shaped_with_resources(&state.text, resources, |text| {
        picture.row_commands(text, viewport, state.scroll.offset, hovered)
    });
    state.marks.picture(list);
    state
        .marks
        .drawn(|cache, list| {
            vec![cache.draw(renderer, bounds.size(), |frame| {
                replay_ordered(list, frame, resources.as_ref());
            })]
        })
        .expect("tree picture is retained before geometry replay")
}

#[derive(Default)]
struct TreeState {
    marks: Marks<()>,
    text: RefCell<Option<(Arc<TextResources>, TextContext)>>,
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
    use std::sync::Arc as GeometryArc;

    use iced::mouse;
    use kithara_test_utils::kithara;

    use super::*;
    use crate::{
        builtin,
        iced::controls::painted::picture_tests,
        interact::{Input, Outcome, PointerPhase, mouse as mouse_input},
        module::IconName,
        render::TreeRow,
        shaping::FontId,
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

    fn tree_state(picture: &Tree, bounds: Rectangle, offset: f32) -> TreeState {
        let mut state = TreeState::default();
        let skin = picture.skin();
        state.reconcile_scroll(
            "tree/picture",
            picture.row_count(),
            skin.tree.row_height,
            skin.tree.scrollbar_margin + skin.tree.scrollbar_width,
        );
        state.scroll.set_viewport(bounds.height);
        state.sync("tree/picture", offset);
        assert_eq!(state.scroll.offset, offset);
        state
    }

    fn tree_picture(
        picture: &Tree,
        state: &TreeState,
        renderer: &Renderer,
        bounds: Rectangle,
        cursor: Cursor,
    ) -> iced_tiny_skia::geometry::Cache {
        let mut drawn = geometry(picture, state, renderer, &Theme::Dark, bounds, cursor);
        assert_eq!(drawn.len(), 1);
        let Geometry::<Renderer>::Secondary(iced_tiny_skia::geometry::Geometry::Cache(cache)) =
            drawn.pop().unwrap()
        else {
            panic!("the actual tree path must return prepared cached geometry");
        };
        assert!(!cache.primitives.is_empty());
        assert!(cache.images.is_empty() && cache.text.is_empty());
        cache
    }

    fn assert_tree_picture(
        actual: &iced_tiny_skia::geometry::Cache,
        fresh: &iced_tiny_skia::geometry::Cache,
    ) {
        assert!(!actual.primitives.is_empty() && !fresh.primitives.is_empty());
        assert_eq!(actual.primitives.as_ref(), fresh.primitives.as_ref());
        assert_eq!(actual.clip_bounds, fresh.clip_bounds);
        assert!(actual.images.is_empty() && fresh.images.is_empty());
        assert!(actual.text.is_empty() && fresh.text.is_empty());
    }

    fn assert_tree_changed(
        before: &iced_tiny_skia::geometry::Cache,
        changed: &iced_tiny_skia::geometry::Cache,
        fresh: &iced_tiny_skia::geometry::Cache,
    ) {
        assert!(!GeometryArc::ptr_eq(
            &before.primitives,
            &changed.primitives
        ));
        assert_ne!(before.primitives.as_ref(), changed.primitives.as_ref());
        assert_tree_picture(changed, fresh);
    }

    fn changed_tree_picture(
        before: &iced_tiny_skia::geometry::Cache,
        picture: &Tree,
        state: &TreeState,
        renderer: &Renderer,
        bounds: Rectangle,
        cursor: Cursor,
    ) -> iced_tiny_skia::geometry::Cache {
        let changed = tree_picture(picture, state, renderer, bounds, cursor);
        let fresh = tree_picture(
            picture,
            &tree_state(picture, bounds, state.scroll.offset),
            renderer,
            bounds,
            cursor,
        );
        assert_tree_changed(before, &changed, &fresh);
        changed
    }

    #[kithara::test]
    fn unchanged_tree_draws_reuse_nonempty_backing() {
        let skin = picture_tests::skin(FontId::InterRegular);
        let picture = Tree::new(&rows(), None, &skin);
        let renderer = picture_tests::renderer();
        let bounds = Rectangle::with_size(iced::Size::new(240.0, skin.tree.row_height * 1.5));
        let state = tree_state(&picture, bounds, 0.0);
        let first = tree_picture(&picture, &state, &renderer, bounds, Cursor::Unavailable);
        let second = tree_picture(&picture, &state, &renderer, bounds, Cursor::Unavailable);
        let third = tree_picture(&picture, &state, &renderer, bounds, Cursor::Unavailable);
        assert!(GeometryArc::ptr_eq(&first.primitives, &second.primitives));
        assert!(GeometryArc::ptr_eq(&second.primitives, &third.primitives));
        assert_tree_picture(&first, &second);
        assert_tree_picture(&second, &third);
    }

    #[kithara::test]
    fn actual_unselected_hover_and_overflow_scroll_match_fresh_pictures() {
        let skin = picture_tests::skin(FontId::InterRegular);
        let source = rows();
        assert!(!source[0].selected && !source[0].muted);
        assert_ne!(
            skin.rgba(skin.tree.row_hovered_fill),
            crate::draw::TRANSPARENT
        );
        let picture = Tree::new(&source, None, &skin);
        let renderer = picture_tests::renderer();
        let bounds = Rectangle::with_size(iced::Size::new(240.0, skin.tree.row_height * 1.5));
        assert!(picture.row_count().to_f32().unwrap() * skin.tree.row_height > bounds.height);
        let mut state = tree_state(&picture, bounds, 0.0);
        let before = tree_picture(&picture, &state, &renderer, bounds, Cursor::Unavailable);
        let hovered = Cursor::Available(iced::Point::new(20.0, skin.tree.row_height * 0.5));
        assert_eq!(
            hovered_row(
                picture.row_count(),
                skin.tree.row_height,
                0.0,
                bounds,
                hovered
            ),
            Some(0)
        );
        let hover = changed_tree_picture(&before, &picture, &state, &renderer, bounds, hovered);
        let unhovered = changed_tree_picture(
            &hover,
            &picture,
            &state,
            &renderer,
            bounds,
            Cursor::Unavailable,
        );
        assert_tree_picture(&unhovered, &before);
        let offset = skin.tree.row_height * 0.75;
        state.sync("tree/picture", offset);
        assert!(state.scroll.offset > 0.0);
        assert_eq!(state.scroll.offset, offset);
        changed_tree_picture(
            &unhovered,
            &picture,
            &state,
            &renderer,
            bounds,
            Cursor::Unavailable,
        );
    }

    #[kithara::test]
    fn changed_tree_label_bounds_and_used_colour_match_fresh_pictures() {
        let skin = picture_tests::skin(FontId::InterRegular);
        let source = rows();
        let picture = Tree::new(&source, None, &skin);
        let renderer = picture_tests::renderer();
        let bounds = Rectangle::with_size(iced::Size::new(240.0, skin.tree.row_height * 1.5));
        let state = tree_state(&picture, bounds, 0.0);
        let before = tree_picture(&picture, &state, &renderer, bounds, Cursor::Unavailable);
        let mut changed_rows = source;
        changed_rows[0].label = "Changed visible label";
        let changed_picture = Tree::new(&changed_rows, None, &skin);
        let changed = changed_tree_picture(
            &before,
            &changed_picture,
            &state,
            &renderer,
            bounds,
            Cursor::Unavailable,
        );

        let wider = Rectangle {
            width: bounds.width + 32.0,
            ..bounds
        };
        let resized = changed_tree_picture(
            &changed,
            &changed_picture,
            &state,
            &renderer,
            wider,
            Cursor::Unavailable,
        );

        let mut coloured_skin = skin.clone();
        assert_ne!(
            skin.rgba(skin.tree.row_idle_text_color),
            skin.rgba(skin.tree.row_selected_fill)
        );
        assert!(!changed_rows[0].selected && !changed_rows[0].muted);
        coloured_skin.tree.row_idle_text_color = skin.tree.row_selected_fill;
        let coloured_picture = Tree::new(&changed_rows, None, &coloured_skin);
        changed_tree_picture(
            &resized,
            &coloured_picture,
            &state,
            &renderer,
            wider,
            Cursor::Unavailable,
        );
    }

    #[kithara::test]
    fn tree_font_owner_replacement_matches_fresh_ordered_primitives() {
        let a = picture_tests::skin(FontId::InterRegular);
        let b = picture_tests::skin(FontId::JetBrainsMonoRegular);
        assert_eq!(a.tree, b.tree);
        picture_tests::assert_font(&a, a.tree.label_text, FontId::InterRegular);
        picture_tests::assert_font(&b, b.tree.label_text, FontId::JetBrainsMonoRegular);
        let mut source = rows();
        source[0].label = picture_tests::WORDS;
        let first = Tree::new(&source, None, &a);
        let changed = Tree::new(&source, None, &b);
        let renderer = picture_tests::renderer();
        let bounds = Rectangle::with_size(iced::Size::new(240.0, a.tree.row_height * 1.5));
        let state = tree_state(&first, bounds, 0.0);
        let before = tree_picture(&first, &state, &renderer, bounds, Cursor::Unavailable);
        let retained_b = tree_picture(&changed, &state, &renderer, bounds, Cursor::Unavailable);
        let fresh_b = tree_picture(
            &changed,
            &tree_state(&changed, bounds, 0.0),
            &renderer,
            bounds,
            Cursor::Unavailable,
        );
        assert_tree_changed(&before, &retained_b, &fresh_b);
        let again = tree_picture(&changed, &state, &renderer, bounds, Cursor::Unavailable);
        assert!(GeometryArc::ptr_eq(
            &retained_b.primitives,
            &again.primitives
        ));
        assert_tree_picture(&again, &fresh_b);
    }
}
