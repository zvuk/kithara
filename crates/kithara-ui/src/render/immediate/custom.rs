use std::cell::RefCell;

use iced::{
    Element, Event, Length, Rectangle, Renderer, Size as IcedSize, Theme,
    advanced::{
        Clipboard, Shell, Widget as IcedWidget,
        layout::{self, Layout, Node},
        renderer,
        widget::{
            Tree,
            tree::{State, Tag},
        },
    },
    mouse::Cursor,
    time::Instant as IcedInstant,
    window,
};

use crate::{
    draw::{DrawList, DrawListBuilder, Rect},
    interact::iced as iced_interact,
    render::{
        Skin, UiEvent,
        controls::{PaintState, Probe, snapped},
        custom::{CustomKinds, MountedCustom, Repaint, Size2, SizeLimits, TextMeasurer},
    },
};

/// One registered extension drawn straight into an iced canvas.
///
/// The widget owns its own state, so nothing outside it can say whether its
/// picture moved. It is built every frame and the tessellated geometry is
/// dropped only when the list that came out differs, which is the same bargain
/// the retained host takes for the same leaf.
pub(crate) struct Custom<'a> {
    skin: &'a Skin,
    kind: &'a str,
    kinds: Option<&'a CustomKinds>,
}

impl<'a> Custom<'a> {
    pub(crate) const fn new(kind: &'a str, kinds: Option<&'a CustomKinds>, skin: &'a Skin) -> Self {
        Self { skin, kind, kinds }
    }
}

/// A picture nothing outside the widget can key.
///
/// The probe never holds, so the list is built each frame; `Marks` then
/// compares the list itself, so a frame that drew the same thing still costs no
/// tessellation.
struct Redrawn;

impl Probe for Redrawn {
    type Key = ();

    fn holds(&self, _key: &Self::Key) -> bool {
        false
    }

    fn keep(self) -> Self::Key {}
}

struct CustomState {
    drawn: Option<IcedInstant>,
    paint: PaintState<()>,
    widget: RefCell<Option<Box<dyn MountedCustom<UiEvent>>>>,
    kind: String,
}

impl CustomState {
    fn new(kind: &str, kinds: Option<&CustomKinds>) -> Self {
        let widget = kinds.and_then(|kinds| kinds.make(kind));
        if widget.is_none() {
            tracing::error!(kind, "no registered widget for this kind");
        }
        Self {
            kind: kind.to_owned(),
            widget: RefCell::new(widget),
            paint: PaintState::default(),
            drawn: None,
        }
    }

    fn repaint(&self) -> Repaint {
        self.widget
            .borrow()
            .as_ref()
            .map_or(Repaint::None, MountedCustom::repaint)
    }
}

impl Custom<'_> {
    fn list(&self, state: &CustomState, bounds: Rect) -> DrawList {
        let mut list = DrawListBuilder::default();
        state.paint.shaped(self.skin.text_resources(), |text| {
            if let Some(widget) = state.widget.borrow_mut().as_mut() {
                widget.paint(
                    &mut list,
                    &mut TextMeasurer::new(text),
                    bounds,
                    self.skin.custom(&state.kind),
                );
            }
        });
        list.finish()
    }

    /// The state for this leaf, rebuilt when the place it stands in changed
    /// hands: every custom leaf shares one state tag, so the kind is the only
    /// thing that says whether the widget behind it is still the right one.
    fn state_for<'s>(&self, tree: &'s mut Tree) -> &'s mut CustomState {
        let state = tree.state.downcast_mut::<CustomState>();
        if state.kind != self.kind {
            *state = CustomState::new(self.kind, self.kinds);
        }
        state
    }
}

impl IcedWidget<UiEvent, Theme, Renderer> for Custom<'_> {
    fn draw(
        &self,
        tree: &Tree,
        renderer: &mut Renderer,
        _theme: &Theme,
        _style: &renderer::Style,
        layout: Layout<'_>,
        _cursor: Cursor,
        _viewport: &Rectangle,
    ) {
        let bounds = layout.bounds();
        if bounds.width < 1.0 || bounds.height < 1.0 {
            return;
        }
        let bounds = snapped(bounds);
        let state = tree.state.downcast_ref::<CustomState>();
        let local = Rect {
            h: bounds.height,
            w: bounds.width,
            x: 0.0,
            y: 0.0,
        };
        state.paint.mark(Redrawn, || self.list(state, local));
        state.paint.replay(
            renderer,
            bounds,
            |_| Rectangle::with_size(bounds.size()),
            self.skin.text_resources(),
        );
    }

    fn layout(&mut self, tree: &mut Tree, _renderer: &Renderer, limits: &layout::Limits) -> Node {
        let resources = self.skin.text_resources();
        let state = self.state_for(tree);
        let asked = SizeLimits::new(
            Size2::new(limits.min().width, limits.min().height),
            Size2::new(limits.max().width, limits.max().height),
        );
        let intrinsic = state.paint.shaped(resources, |text| {
            state
                .widget
                .borrow_mut()
                .as_mut()
                .map_or_else(Size2::default, |widget| {
                    widget.measure(&mut TextMeasurer::new(text), asked)
                })
        });
        Node::new(limits.resolve(
            Length::Fill,
            Length::Fill,
            IcedSize::new(intrinsic.w, intrinsic.h),
        ))
    }

    fn size(&self) -> IcedSize<Length> {
        IcedSize::new(Length::Fill, Length::Fill)
    }

    fn state(&self) -> State {
        State::new(CustomState::new(self.kind, self.kinds))
    }

    fn tag(&self) -> Tag {
        Tag::of::<CustomState>()
    }

    fn update(
        &mut self,
        tree: &mut Tree,
        event: &Event,
        layout: Layout<'_>,
        cursor: Cursor,
        _renderer: &Renderer,
        _clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, UiEvent>,
        _viewport: &Rectangle,
    ) {
        let state = self.state_for(tree);
        if let Event::Window(window::Event::RedrawRequested(now)) = event {
            let elapsed = state
                .drawn
                .replace(*now)
                .map(|last| now.duration_since(last));
            let asked = state.repaint();
            if let Some(elapsed) = elapsed
                && let Some(published) = state
                    .widget
                    .borrow_mut()
                    .as_mut()
                    .and_then(|widget| widget.frame(elapsed))
            {
                shell.publish(published);
            }
            if asked != Repaint::None || state.repaint() == Repaint::Continuous {
                shell.request_redraw();
            }
            return;
        }
        let Some(input) = iced_interact::input(event) else {
            return;
        };
        let hit = iced_interact::hit(layout.bounds(), cursor);
        let Some(outcome) = state
            .widget
            .borrow_mut()
            .as_mut()
            .map(|widget| widget.input(input, hit))
        else {
            return;
        };
        let captured = outcome.is_captured();
        if let Some(published) = outcome.value() {
            shell.publish(published);
        }
        if captured {
            shell.capture_event();
        }
        if state.repaint() != Repaint::None {
            shell.request_redraw();
        }
    }
}

impl<'a> From<Custom<'a>> for Element<'a, UiEvent> {
    fn from(custom: Custom<'a>) -> Self {
        Self::new(custom)
    }
}

#[cfg(test)]
mod tests {
    use iced::{
        Pixels, Point, Size,
        advanced::{
            clipboard,
            layout::{Layout, Limits},
            widget::Tree,
        },
        mouse::Button,
    };
    use iced_renderer::fallback::Renderer as FallbackRenderer;
    use iced_tiny_skia::Renderer as TinySkiaRenderer;
    use kithara_platform::time::Duration;
    use kithara_test_utils::kithara;

    use super::*;
    use crate::{
        builtin,
        draw::Rgba,
        ids::SourceUri,
        interact::{Hit, Input, Outcome, PointerOwnership, PointerPhase},
        render::{CustomSkin, custom::CustomWidget, fonts::SANS},
        skin::parse_skin_over,
    };

    mod consts {
        use super::Size;

        pub(super) const KIND: &str = "press-extension";
        pub(super) const VIEWPORT: Size = Size {
            width: 200.0,
            height: 120.0,
        };
    }

    /// An extension that answers a press with its own action and asks for the
    /// frame schedule the test names.
    struct PressExtension(Repaint);

    impl CustomWidget for PressExtension {
        type Action = ();

        fn input(&mut self, input: Input<'_>, hit: Hit) -> Outcome<Self::Action> {
            let Input::Pointer(pointer) = input else {
                return Outcome::IGNORED;
            };
            if pointer.phase == PointerPhase::Down && hit.over() {
                return Outcome::set(()).with_ownership(PointerOwnership::Claim);
            }
            Outcome::IGNORED
        }

        fn measure(&mut self, _text: &mut TextMeasurer<'_>, _limits: SizeLimits) -> Size2 {
            Size2::new(40.0, 40.0)
        }

        fn paint(
            &mut self,
            list: &mut DrawListBuilder,
            _text: &mut TextMeasurer<'_>,
            bounds: Rect,
            skin: &CustomSkin,
        ) {
            list.fill_rect(
                bounds,
                skin.color("ink").unwrap_or(Rgba {
                    a: 1.0,
                    b: 1.0,
                    g: 1.0,
                    r: 1.0,
                }),
            );
        }

        fn repaint(&self) -> Repaint {
            self.0
        }
    }

    fn kinds(repaint: Repaint) -> CustomKinds {
        CustomKinds::default().with(
            consts::KIND,
            move || PressExtension(repaint),
            |()| UiEvent::OpenSettings,
        )
    }

    fn renderer() -> Renderer {
        FallbackRenderer::Secondary(TinySkiaRenderer::new(SANS, Pixels(14.0)))
    }

    /// A skin dressing the extension this host mounts in one named colour.
    fn dressed(ink: &str) -> Skin {
        let origin = SourceUri("skins/dressed.kskin.ron".to_owned());
        let text = format!(
            r#"(schema: "kithara.skin", version: 1, id: "dressed",
                custom: {{ "{kind}": {{ "ink": Color("{ink}") }} }})"#,
            kind = consts::KIND,
        );
        let document =
            parse_skin_over(builtin::skin_doc(), &text, &origin).expect("the patch parses");
        Skin::resolve(document, builtin::text_doc(), &origin, &builtin::resolver())
            .expect("the dressed document resolves")
    }

    /// What the mounted extension draws under one skin.
    fn drawn(skin: &Skin) -> DrawList {
        let kinds = kinds(Repaint::None);
        let custom = Custom::new(consts::KIND, Some(&kinds), skin);
        let state = CustomState::new(consts::KIND, Some(&kinds));
        custom.list(
            &state,
            Rect {
                h: consts::VIEWPORT.height,
                w: consts::VIEWPORT.width,
                x: 0.0,
                y: 0.0,
            },
        )
    }

    /// The skin reaches the extension every frame rather than at its making,
    /// so a host that changed skins draws the extension in the new one.
    #[kithara::test]
    fn the_extension_is_drawn_in_what_the_skin_dresses_its_kind_in() {
        assert_ne!(
            drawn(&dressed("#ff0000")),
            drawn(&dressed("#0000ff")),
            "the two skins dress this kind in two colours, so an extension drawing the same list              under both is reading neither"
        );
    }

    /// One press delivered to a mounted extension, with what the host published
    /// and whether it kept the event to itself.
    fn press(kinds: &CustomKinds) -> (Vec<UiEvent>, bool) {
        let mut element: Element<'_, UiEvent> =
            Custom::new(consts::KIND, Some(kinds), builtin::skin()).into();
        let renderer = renderer();
        let mut tree = Tree::new(element.as_widget());
        let node = element.as_widget_mut().layout(
            &mut tree,
            &renderer,
            &Limits::new(Size::ZERO, consts::VIEWPORT),
        );
        let mut clipboard = clipboard::Null;
        let mut messages = Vec::new();
        let mut shell = Shell::new(&mut messages);
        element.as_widget_mut().update(
            &mut tree,
            &Event::Mouse(iced::mouse::Event::ButtonPressed(Button::Left)),
            Layout::new(&node),
            Cursor::Available(Point::new(10.0, 10.0)),
            &renderer,
            &mut clipboard,
            &mut shell,
            &Rectangle::with_size(consts::VIEWPORT),
        );
        let captured = shell.is_event_captured();
        drop(shell);
        (messages, captured)
    }

    /// The frame schedule the host asked for after one delivered animation
    /// frame.
    fn after_a_frame(kinds: &CustomKinds) -> window::RedrawRequest {
        let mut element: Element<'_, UiEvent> =
            Custom::new(consts::KIND, Some(kinds), builtin::skin()).into();
        let renderer = renderer();
        let mut tree = Tree::new(element.as_widget());
        let node = element.as_widget_mut().layout(
            &mut tree,
            &renderer,
            &Limits::new(Size::ZERO, consts::VIEWPORT),
        );
        let mut clipboard = clipboard::Null;
        let mut messages = Vec::new();
        let mut shell = Shell::new(&mut messages);
        element.as_widget_mut().update(
            &mut tree,
            &Event::Window(window::Event::RedrawRequested(
                IcedInstant::now() + Duration::from_millis(16),
            )),
            Layout::new(&node),
            Cursor::Unavailable,
            &renderer,
            &mut clipboard,
            &mut shell,
            &Rectangle::with_size(consts::VIEWPORT),
        );
        let asked = shell.redraw_request();
        drop(shell);
        asked
    }

    #[kithara::test]
    fn a_press_leaves_as_the_event_the_registry_maps_it_to() {
        let (messages, _) = press(&kinds(Repaint::None));

        assert_eq!(messages, [UiEvent::OpenSettings]);
    }

    #[kithara::test]
    fn a_claimed_press_does_not_reach_the_rest_of_the_tree() {
        let (_, captured) = press(&kinds(Repaint::None));

        assert!(captured);
    }

    #[kithara::test]
    fn a_continuous_extension_keeps_the_loop_awake() {
        assert_eq!(
            after_a_frame(&kinds(Repaint::Continuous)),
            window::RedrawRequest::NextFrame,
        );
    }

    #[kithara::test]
    fn an_extension_that_asks_for_nothing_lets_the_loop_sleep() {
        assert_ne!(
            after_a_frame(&kinds(Repaint::None)),
            window::RedrawRequest::NextFrame,
        );
    }
}
