use iced::{
    Border, Color, Element, Event, Length, Point, Rectangle, Renderer, Shadow, Size, Theme, Vector,
    advanced::{
        Clipboard, Layout, Renderer as _, Shell, Widget,
        layout::{self, Limits},
        mouse::{self, Cursor},
        overlay,
        renderer::{self, Quad},
        widget::{Operation, Tree, tree},
    },
    keyboard::{self, key::Named},
};

use crate::{
    module::{PopoverAlign, PopoverAt},
    render::Skin,
    skin::PopSkin,
};

const OVERHANG: f32 = 1.0;

/// Where the surface sits: the geometry it opens from and the edge of that
/// geometry its own edge lines up with.
#[derive(Clone, Copy)]
pub(crate) struct Placement {
    pub(crate) at: PopoverAt,
    pub(crate) align: PopoverAlign,
}

pub(crate) struct Anchored<'a, Message> {
    anchor: Element<'a, Message>,
    content: Element<'a, Message>,
    open: bool,
    placement: Placement,
    on_dismiss: Message,
    chrome: PopChrome,
}

impl<'a, Message> Anchored<'a, Message> {
    pub(crate) fn new(
        anchor: Element<'a, Message>,
        content: Element<'a, Message>,
        open: bool,
        placement: Placement,
        on_dismiss: Message,
        skin: &Skin,
    ) -> Self {
        Self {
            anchor,
            content,
            open,
            placement,
            on_dismiss,
            chrome: PopChrome::new(skin),
        }
    }
}

#[derive(Clone, Copy)]
struct PopChrome {
    background: Color,
    border: Border,
    cap_height: f32,
    cap_color: Color,
    shadow: Shadow,
}

impl PopChrome {
    fn new(skin: &Skin) -> Self {
        let PopSkin {
            background,
            frame,
            cap_height,
            cap_color,
            shadow,
            ..
        } = skin.pop;
        Self {
            background: skin.color(background),
            border: skin.border(frame),
            cap_height,
            cap_color: skin.color(cap_color),
            shadow: Shadow {
                color: Color {
                    a: shadow.alpha,
                    ..skin.color(shadow.color)
                },
                offset: Vector::new(shadow.offset_x, shadow.offset_y),
                blur_radius: shadow.blur,
            },
        }
    }

    fn offset(self) -> Vector {
        Vector::new(self.border.width, self.border.width + self.cap_height)
    }

    fn expand(self, content: Size) -> Size {
        Size::new(
            content.width + self.border.width * 2.0,
            content.height + self.border.width * 2.0 + self.cap_height,
        )
    }

    fn cap(self, surface: Rectangle) -> Rectangle {
        Rectangle {
            x: surface.x + self.border.width,
            y: surface.y + self.border.width,
            width: (surface.width - self.border.width * 2.0).max(0.0),
            height: self.cap_height,
        }
    }
}

fn open_point(at: PopoverAt, latched: Option<Point>, translation: Vector) -> Option<Point> {
    match at {
        PopoverAt::Anchor => None,
        PopoverAt::Pointer => latched.map(|point| point + translation),
    }
}

fn place(
    anchor: Rectangle,
    pointer: Option<Point>,
    surface: Size,
    viewport: Size,
    align: PopoverAlign,
) -> Rectangle {
    let from = pointer.map_or(anchor, |point| Rectangle::new(point, Size::ZERO));
    let below = from.y + from.height;
    let y = if surface.height <= viewport.height - below {
        below
    } else {
        from.y - surface.height
    };
    // The frame draws outward of the content column, so the overhang carries
    // the aligned edge of the column onto the aligned edge of the anchor.
    let x = match align {
        PopoverAlign::Start => from.x - OVERHANG,
        PopoverAlign::End => from.x + from.width + OVERHANG - surface.width,
    };
    let position = Point::new(
        inside(x, surface.width, viewport.width),
        inside(y, surface.height, viewport.height),
    );
    Rectangle::new(position, surface)
}

fn inside(start: f32, length: f32, extent: f32) -> f32 {
    start.clamp(0.0, (extent - length).max(0.0))
}

fn claims(content: mouse::Interaction, surface: Rectangle, cursor: Cursor) -> mouse::Interaction {
    if cursor.is_over(surface) {
        content.max(mouse::Interaction::Idle)
    } else {
        mouse::Interaction::None
    }
}

fn dismisses(event: &Event, popover: Rectangle, cursor: Cursor) -> bool {
    match event {
        Event::Mouse(mouse::Event::ButtonPressed(_)) => !cursor.is_over(popover),
        Event::Keyboard(keyboard::Event::KeyPressed {
            key: keyboard::Key::Named(Named::Escape),
            ..
        }) => true,
        _ => false,
    }
}

#[derive(Default)]
struct State {
    open: bool,
    press: Option<Point>,
    pointer: Option<Point>,
}

fn press(state: &mut State, event: &Event, cursor: Cursor) {
    if matches!(event, Event::Mouse(mouse::Event::ButtonPressed(_))) {
        state.press = cursor.position();
    }
}

fn latch(state: &mut State, open: bool) -> bool {
    let changed = state.open != open;
    state.open = open;
    if changed && open {
        state.pointer = state.press.take();
    }
    changed
}

impl<Message> Widget<Message, Theme, Renderer> for Anchored<'_, Message>
where
    Message: Clone,
{
    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<State>()
    }

    fn state(&self) -> tree::State {
        tree::State::new(State::default())
    }

    fn children(&self) -> Vec<Tree> {
        vec![Tree::new(&self.anchor), Tree::new(&self.content)]
    }

    fn diff(&self, tree: &mut Tree) {
        tree.diff_children(&[self.anchor.as_widget(), self.content.as_widget()]);
    }

    fn size(&self) -> Size<Length> {
        self.anchor.as_widget().size()
    }

    fn size_hint(&self) -> Size<Length> {
        self.anchor.as_widget().size_hint()
    }

    fn layout(&mut self, tree: &mut Tree, renderer: &Renderer, limits: &Limits) -> layout::Node {
        self.anchor
            .as_widget_mut()
            .layout(&mut tree.children[0], renderer, limits)
    }

    fn update(
        &mut self,
        tree: &mut Tree,
        event: &Event,
        layout: Layout<'_>,
        cursor: Cursor,
        renderer: &Renderer,
        clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, Message>,
        viewport: &Rectangle,
    ) {
        let state = tree.state.downcast_mut::<State>();
        press(state, event, cursor);
        if latch(state, self.open) {
            shell.invalidate_layout();
        }
        self.anchor.as_widget_mut().update(
            &mut tree.children[0],
            event,
            layout,
            cursor,
            renderer,
            clipboard,
            shell,
            viewport,
        );
    }

    fn mouse_interaction(
        &self,
        tree: &Tree,
        layout: Layout<'_>,
        cursor: Cursor,
        viewport: &Rectangle,
        renderer: &Renderer,
    ) -> mouse::Interaction {
        self.anchor.as_widget().mouse_interaction(
            &tree.children[0],
            layout,
            cursor,
            viewport,
            renderer,
        )
    }

    fn draw(
        &self,
        tree: &Tree,
        renderer: &mut Renderer,
        theme: &Theme,
        style: &renderer::Style,
        layout: Layout<'_>,
        cursor: Cursor,
        viewport: &Rectangle,
    ) {
        self.anchor.as_widget().draw(
            &tree.children[0],
            renderer,
            theme,
            style,
            layout,
            cursor,
            viewport,
        );
    }

    fn operate(
        &mut self,
        tree: &mut Tree,
        layout: Layout<'_>,
        renderer: &Renderer,
        operation: &mut dyn Operation,
    ) {
        operation.container(None, layout.bounds());
        operation.traverse(&mut |operation| {
            self.anchor
                .as_widget_mut()
                .operate(&mut tree.children[0], layout, renderer, operation);
        });
    }

    fn overlay<'b>(
        &'b mut self,
        tree: &'b mut Tree,
        layout: Layout<'b>,
        renderer: &Renderer,
        viewport: &Rectangle,
        translation: Vector,
    ) -> Option<overlay::Element<'b, Message, Theme, Renderer>> {
        let pointer = open_point(
            self.placement.at,
            tree.state.downcast_ref::<State>().pointer,
            translation,
        );
        let [anchor_tree, content_tree] = tree.children.as_mut_slice() else {
            return None;
        };
        let anchor = self.anchor.as_widget_mut().overlay(
            anchor_tree,
            layout,
            renderer,
            viewport,
            translation,
        );
        let popover = if self.open {
            Some(overlay::Element::new(Box::new(Popover {
                content: &mut self.content,
                tree: content_tree,
                anchor: layout.bounds() + translation,
                pointer,
                align: self.placement.align,
                on_dismiss: self.on_dismiss.clone(),
                chrome: self.chrome,
            })))
        } else {
            None
        };
        (anchor.is_some() || popover.is_some()).then(|| {
            overlay::Group::with_children(anchor.into_iter().chain(popover).collect()).overlay()
        })
    }
}

impl<'a, Message> From<Anchored<'a, Message>> for Element<'a, Message>
where
    Message: Clone + 'a,
{
    fn from(anchored: Anchored<'a, Message>) -> Self {
        Self::new(anchored)
    }
}

struct Popover<'a, 'b, Message> {
    content: &'b mut Element<'a, Message>,
    tree: &'b mut Tree,
    anchor: Rectangle,
    pointer: Option<Point>,
    align: PopoverAlign,
    on_dismiss: Message,
    chrome: PopChrome,
}

impl<Message> overlay::Overlay<Message, Theme, Renderer> for Popover<'_, '_, Message>
where
    Message: Clone,
{
    fn layout(&mut self, renderer: &Renderer, bounds: Size) -> layout::Node {
        let chrome = self.chrome;
        let content = self.content.as_widget_mut().layout(
            self.tree,
            renderer,
            &Limits::new(Size::ZERO, bounds).shrink(chrome.expand(Size::ZERO)),
        );
        let surface = place(
            self.anchor,
            self.pointer,
            chrome.expand(content.size()),
            bounds,
            self.align,
        );
        layout::Node::with_children(surface.size(), vec![content.translate(chrome.offset())])
            .move_to(surface.position())
    }

    fn update(
        &mut self,
        event: &Event,
        layout: Layout<'_>,
        cursor: Cursor,
        renderer: &Renderer,
        clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, Message>,
    ) {
        let surface = layout.bounds();
        if let Some(content) = layout.children().next() {
            self.content.as_widget_mut().update(
                self.tree, event, content, cursor, renderer, clipboard, shell, &surface,
            );
        }
        if shell.is_event_captured() || !dismisses(event, surface, cursor) {
            return;
        }
        shell.publish(self.on_dismiss.clone());
        shell.capture_event();
        shell.invalidate_layout();
    }

    fn mouse_interaction(
        &self,
        layout: Layout<'_>,
        cursor: Cursor,
        renderer: &Renderer,
    ) -> mouse::Interaction {
        let surface = layout.bounds();
        let content = layout
            .children()
            .next()
            .map_or(mouse::Interaction::None, |content| {
                self.content
                    .as_widget()
                    .mouse_interaction(self.tree, content, cursor, &surface, renderer)
            });
        claims(content, surface, cursor)
    }

    fn draw(
        &self,
        renderer: &mut Renderer,
        theme: &Theme,
        style: &renderer::Style,
        layout: Layout<'_>,
        cursor: Cursor,
    ) {
        let surface = layout.bounds();
        let chrome = self.chrome;
        renderer.fill_quad(
            Quad {
                bounds: surface,
                border: chrome.border,
                shadow: chrome.shadow,
                ..Quad::default()
            },
            chrome.background,
        );
        renderer.fill_quad(
            Quad {
                bounds: chrome.cap(surface),
                ..Quad::default()
            },
            chrome.cap_color,
        );
        if let Some(content) = layout.children().next() {
            self.content
                .as_widget()
                .draw(self.tree, renderer, theme, style, content, cursor, &surface);
        }
    }
}

#[cfg(test)]
mod tests {
    use iced::{
        keyboard::{Location, Modifiers, key::Physical},
        mouse::{Button, ScrollDelta},
        widget::Space,
    };
    use kithara_test_utils::kithara;

    use super::*;
    use crate::{
        builtin,
        render::{ControlAction, UiEvent},
    };

    struct Consts;

    impl Consts {
        const VIEWPORT: Size = Size {
            width: 1280.0,
            height: 800.0,
        };
        const SURFACE: Size = Size {
            width: 300.0,
            height: 404.0,
        };
        const POPOVER: Rectangle = Rectangle {
            x: 35.0,
            y: 36.0,
            width: 300.0,
            height: 404.0,
        };
    }

    const fn burger(x: f32, y: f32) -> Rectangle {
        Rectangle {
            x,
            y,
            width: 36.0,
            height: 36.0,
        }
    }

    const fn row(x: f32, y: f32) -> Rectangle {
        Rectangle {
            x,
            y,
            width: 1280.0,
            height: 30.0,
        }
    }

    #[kithara::test]
    fn a_popover_with_room_below_hangs_one_pixel_left_of_the_anchor() {
        assert_eq!(
            place(
                burger(36.0, 0.0),
                None,
                Consts::SURFACE,
                Consts::VIEWPORT,
                PopoverAlign::Start
            ),
            Rectangle {
                x: 35.0,
                y: 36.0,
                width: 300.0,
                height: 404.0,
            },
            "the frame overhangs the anchor so the content column starts at its left edge"
        );
    }

    #[kithara::test]
    fn an_end_aligned_popover_lines_its_right_edge_up_with_the_anchor() {
        assert_eq!(
            place(
                burger(600.0, 0.0),
                None,
                Consts::SURFACE,
                Consts::VIEWPORT,
                PopoverAlign::End
            ),
            Rectangle {
                x: 337.0,
                y: 36.0,
                width: 300.0,
                height: 404.0,
            },
            "the frame overhangs the anchor so the content column ends at its right edge"
        );
    }

    #[kithara::test]
    fn a_popover_without_room_below_flips_above_the_anchor() {
        assert_eq!(
            place(
                burger(36.0, 700.0),
                None,
                Consts::SURFACE,
                Consts::VIEWPORT,
                PopoverAlign::Start
            ),
            Rectangle {
                x: 35.0,
                y: 296.0,
                width: 300.0,
                height: 404.0,
            }
        );
    }

    #[kithara::test]
    fn a_popover_at_the_left_edge_clamps_into_the_viewport() {
        assert_eq!(
            place(
                burger(0.0, 0.0),
                None,
                Consts::SURFACE,
                Consts::VIEWPORT,
                PopoverAlign::Start
            )
            .x,
            0.0
        );
    }

    #[kithara::test]
    fn a_popover_at_the_right_edge_clamps_into_the_viewport() {
        assert_eq!(
            place(
                burger(1260.0, 0.0),
                None,
                Consts::SURFACE,
                Consts::VIEWPORT,
                PopoverAlign::Start
            )
            .x,
            980.0
        );
    }

    #[kithara::test]
    fn a_popover_taller_than_the_viewport_starts_at_its_top() {
        let tall = Size::new(300.0, 900.0);
        assert_eq!(
            place(
                burger(36.0, 0.0),
                None,
                tall,
                Consts::VIEWPORT,
                PopoverAlign::Start
            ),
            Rectangle {
                x: 35.0,
                y: 0.0,
                width: 300.0,
                height: 900.0,
            },
            "an unfittable surface keeps its size and overflows downward"
        );
    }

    #[kithara::test]
    fn a_pointer_popover_opens_at_the_pointer_not_at_the_anchor() {
        assert_eq!(
            place(
                row(0.0, 200.0),
                Some(Point::new(420.0, 214.0)),
                Consts::SURFACE,
                Consts::VIEWPORT,
                PopoverAlign::Start,
            ),
            Rectangle {
                x: 419.0,
                y: 214.0,
                width: 300.0,
                height: 404.0,
            },
            "the content column starts at the pointer and the surface hangs one pixel left of it"
        );
    }

    #[kithara::test]
    fn a_pointer_popover_at_the_right_edge_clamps_into_the_viewport() {
        assert_eq!(
            place(
                row(0.0, 200.0),
                Some(Point::new(1270.0, 214.0)),
                Consts::SURFACE,
                Consts::VIEWPORT,
                PopoverAlign::Start,
            )
            .x,
            980.0
        );
    }

    #[kithara::test]
    fn a_pointer_popover_without_room_below_flips_above_the_pointer() {
        assert_eq!(
            place(
                row(0.0, 700.0),
                Some(Point::new(420.0, 714.0)),
                Consts::SURFACE,
                Consts::VIEWPORT,
                PopoverAlign::Start,
            ),
            Rectangle {
                x: 419.0,
                y: 310.0,
                width: 300.0,
                height: 404.0,
            }
        );
    }

    #[kithara::test]
    fn only_a_pointer_popover_places_at_the_latched_point() {
        let point = Point::new(420.0, 214.0);
        let scroll = Vector::new(0.0, -80.0);

        assert_eq!(
            open_point(PopoverAt::Pointer, Some(point), Vector::ZERO),
            Some(point)
        );
        assert_eq!(
            open_point(PopoverAt::Anchor, Some(point), Vector::ZERO),
            None,
            "an anchored popover ignores the latch"
        );
        assert_eq!(
            open_point(PopoverAt::Pointer, None, Vector::ZERO),
            None,
            "an open with no press places at the anchor"
        );
        assert_eq!(
            open_point(PopoverAt::Pointer, Some(point), scroll),
            Some(Point::new(420.0, 134.0)),
            "the pointer travels the same translation as the anchor"
        );
    }

    #[kithara::test]
    fn every_open_latches_the_press_that_opened_it() {
        let mut state = State::default();
        let pressed = Event::Mouse(mouse::Event::ButtonPressed(Button::Right));

        assert_eq!(state.pointer, None);
        press(&mut state, &pressed, at(420.0, 214.0));
        latch(&mut state, true);
        assert_eq!(
            state.pointer,
            Some(Point::new(420.0, 214.0)),
            "the overlay levitates the cursor away on the frame the flip lands on"
        );

        latch(&mut state, false);
        press(&mut state, &pressed, at(120.0, 640.0));
        latch(&mut state, true);
        assert_eq!(
            state.pointer,
            Some(Point::new(120.0, 640.0)),
            "the second open replaces the first point"
        );

        latch(&mut state, false);
        latch(&mut state, true);
        assert_eq!(
            state.pointer, None,
            "an open no press preceded places at the anchor"
        );
    }

    #[kithara::test]
    fn a_cursor_moving_over_an_open_popover_never_moves_it() {
        let mut state = State::default();
        let pressed = Event::Mouse(mouse::Event::ButtonPressed(Button::Right));

        press(&mut state, &pressed, at(420.0, 214.0));
        latch(&mut state, true);
        press(&mut state, &pressed, at(700.0, 300.0));
        latch(&mut state, true);
        assert_eq!(state.pointer, Some(Point::new(420.0, 214.0)));
    }

    #[kithara::test]
    fn only_a_press_is_latched() {
        let mut state = State::default();

        press(
            &mut state,
            &Event::Mouse(mouse::Event::CursorMoved {
                position: Point::new(420.0, 214.0),
            }),
            at(420.0, 214.0),
        );
        assert_eq!(state.press, None, "a move is not what opens a menu");

        state.press = Some(Point::new(420.0, 214.0));
        press(
            &mut state,
            &Event::Mouse(mouse::Event::ButtonPressed(Button::Left)),
            Cursor::Unavailable,
        );
        assert_eq!(state.press, None, "a press with no cursor clears the point");
    }

    const fn at(x: f32, y: f32) -> Cursor {
        Cursor::Available(Point::new(x, y))
    }

    fn escape() -> Event {
        Event::Keyboard(keyboard::Event::KeyPressed {
            key: keyboard::Key::Named(Named::Escape),
            modified_key: keyboard::Key::Named(Named::Escape),
            physical_key: Physical::Code(keyboard::key::Code::Escape),
            location: Location::Standard,
            modifiers: Modifiers::empty(),
            text: None,
            repeat: false,
        })
    }

    #[kithara::test]
    fn the_surface_claims_the_cursor_over_itself_and_nowhere_else() {
        assert_eq!(
            claims(mouse::Interaction::None, Consts::POPOVER, at(100.0, 100.0)),
            mouse::Interaction::Idle,
            "an inert row still covers what the surface hides"
        );
        assert_eq!(
            claims(
                mouse::Interaction::Pointer,
                Consts::POPOVER,
                at(100.0, 100.0)
            ),
            mouse::Interaction::Pointer,
            "the content keeps the stronger claim"
        );
        assert_eq!(
            claims(
                mouse::Interaction::Pointer,
                Consts::POPOVER,
                at(600.0, 500.0)
            ),
            mouse::Interaction::None,
            "outside the surface the base tree keeps its cursor"
        );
    }

    #[kithara::test]
    fn a_press_outside_the_popover_dismisses_it() {
        let press = Event::Mouse(mouse::Event::ButtonPressed(Button::Left));
        assert!(dismisses(&press, Consts::POPOVER, at(600.0, 500.0)));
        assert!(
            !dismisses(&press, Consts::POPOVER, at(100.0, 100.0)),
            "a press on the surface belongs to the menu"
        );
        assert!(
            !dismisses(&press, Consts::POPOVER, at(35.0, 36.0)),
            "the frame is part of the surface"
        );
    }

    #[kithara::test]
    fn a_secondary_press_outside_the_popover_dismisses_it() {
        let press = Event::Mouse(mouse::Event::ButtonPressed(Button::Right));
        assert!(dismisses(&press, Consts::POPOVER, at(600.0, 500.0)));
    }

    #[kithara::test]
    fn escape_dismisses_wherever_the_cursor_is() {
        assert!(dismisses(&escape(), Consts::POPOVER, at(100.0, 100.0)));
        assert!(dismisses(&escape(), Consts::POPOVER, at(600.0, 500.0)));
    }

    #[kithara::test]
    fn a_release_a_move_and_a_scroll_never_dismiss() {
        let away = at(600.0, 500.0);
        for event in [
            Event::Mouse(mouse::Event::ButtonReleased(Button::Left)),
            Event::Mouse(mouse::Event::CursorMoved {
                position: Point::new(600.0, 500.0),
            }),
            Event::Mouse(mouse::Event::WheelScrolled {
                delta: ScrollDelta::Lines { x: 0.0, y: -1.0 },
            }),
        ] {
            assert!(
                !dismisses(&event, Consts::POPOVER, away),
                "the press that opens the menu releases over the fresh overlay: {event:?}"
            );
        }
    }

    #[kithara::test]
    fn the_pop_chrome_draws_outward_of_the_content_column() {
        let chrome = PopChrome::new(builtin::skin());

        assert_eq!(
            chrome.expand(Size::new(298.0, 400.0)),
            Size::new(300.0, 404.0),
            "a 298 content column carries a 1 px frame and a 2 px cap"
        );
        assert_eq!(chrome.offset(), Vector::new(1.0, 3.0));
        assert_eq!(
            chrome.cap(Consts::POPOVER),
            Rectangle {
                x: 36.0,
                y: 37.0,
                width: 298.0,
                height: 2.0,
            }
        );
    }

    fn anchored(open: bool) -> Anchored<'static, UiEvent> {
        let anchor: Element<'static, UiEvent> = Space::new()
            .width(Length::Fixed(36.0))
            .height(Length::Fixed(36.0))
            .into();
        let content: Element<'static, UiEvent> = Space::new().width(Length::Fixed(298.0)).into();
        Anchored::new(
            anchor,
            content,
            open,
            Placement {
                at: PopoverAt::Anchor,
                align: PopoverAlign::Start,
            },
            UiEvent::Control {
                path: "menu".to_owned(),
                action: ControlAction::Activate,
            },
            builtin::skin(),
        )
    }

    #[kithara::test]
    fn the_open_flag_reports_only_its_flips() {
        let mut state = State::default();

        assert!(!latch(&mut state, false), "closed is the starting state");
        assert!(latch(&mut state, true));
        assert!(!latch(&mut state, true), "an open menu stays laid out");
        assert!(latch(&mut state, false));
    }

    #[kithara::test]
    fn the_tree_always_holds_the_anchor_and_the_content() {
        for open in [false, true] {
            assert_eq!(
                anchored(open).children().len(),
                2,
                "a closed popover keeps its content slot so the split stays total"
            );
        }
    }

    #[kithara::test]
    fn the_widget_measures_the_anchor_alone() {
        for open in [false, true] {
            assert_eq!(
                anchored(open).size(),
                Size::new(Length::Fixed(36.0), Length::Fixed(36.0))
            );
        }
    }
}
