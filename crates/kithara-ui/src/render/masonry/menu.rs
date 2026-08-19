use std::rc::Rc;

use masonry::{
    accesskit::{Node as AccessNode, Role},
    core::{
        AccessCtx, BoxConstraints, ChildrenIds, EventCtx, LayoutCtx, PaintCtx, PointerEvent,
        PropertiesMut, PropertiesRef, QueryCtx, RegisterCtx, Widget, WidgetId, WidgetRef,
    },
    kurbo::{Affine, Point, Size},
    vello::Scene,
};
use tracing::{Span, trace_span};

use super::{custom::HostAction, picker::HostedEngine};
use crate::{
    backends::VelloBackend,
    draw::replay,
    render::{PickerMenu, Skin},
    shaping::TextContext,
};

/// The open scope menu, drawn above the tree that raised it.
///
/// The menu is not part of any control's box: it hangs below the closed face
/// and over whatever the document put underneath, so it is a layer rather than
/// a child. Its state lives in the engine — a press on the face opens it, a
/// press on an option closes it — and this layer only draws what is open, which
/// is why it holds the engine rather than a picture.
///
/// It takes no pointer events. The root answers an open menu before the tree
/// sees the press, so a second recognizer here would be a second owner of the
/// same gesture.
pub(crate) struct PickerLayer {
    engine: Rc<HostedEngine>,
    menu: PickerMenu,
    text: TextContext,
}

impl PickerLayer {
    pub(crate) fn new(engine: Rc<HostedEngine>, skin: &Skin) -> Self {
        Self {
            engine,
            menu: PickerMenu::new(skin),
            text: TextContext::from(skin.text_resources()),
        }
    }
}

impl Widget for PickerLayer {
    type Action = HostAction;

    fn on_pointer_event(
        &mut self,
        _ctx: &mut EventCtx<'_>,
        _props: &mut PropertiesMut<'_>,
        _event: &PointerEvent,
    ) {
    }

    fn register_children(&mut self, _ctx: &mut RegisterCtx<'_>) {}

    fn layout(
        &mut self,
        _ctx: &mut LayoutCtx<'_>,
        _props: &mut PropertiesMut<'_>,
        constraints: &BoxConstraints,
    ) -> Size {
        constraints.max()
    }

    fn paint(&mut self, _ctx: &mut PaintCtx<'_>, _props: &PropertiesRef<'_>, scene: &mut Scene) {
        let Some(open) = self.engine.open_picker() else {
            return;
        };
        let layer = self.menu.layer(
            &mut self.text,
            open.anchor,
            open.items.iter().map(String::as_str),
            open.highlighted,
        );
        // A host layer is drawn at its own origin and placed by its bounds, the
        // same contract the immediate host translates by, so the menu is one
        // list on both hosts rather than two that agree until one is edited.
        let bounds = layer.bounds();
        let mut menu = Scene::new();
        replay(layer.draw(), &mut VelloBackend::new(&mut menu));
        scene.append(
            &menu,
            Some(Affine::translate((
                f64::from(bounds.x),
                f64::from(bounds.y),
            ))),
        );
    }

    fn accessibility_role(&self) -> Role {
        Role::GenericContainer
    }

    fn accessibility(
        &mut self,
        _ctx: &mut AccessCtx<'_>,
        _props: &PropertiesRef<'_>,
        _node: &mut AccessNode,
    ) {
    }

    fn children_ids(&self) -> ChildrenIds {
        ChildrenIds::new()
    }

    fn accepts_pointer_interaction(&self) -> bool {
        false
    }

    fn find_widget_under_pointer<'ctx>(
        &'ctx self,
        _ctx: QueryCtx<'ctx>,
        _pos: Point,
    ) -> Option<WidgetRef<'ctx, dyn Widget>> {
        None
    }

    fn make_trace_span(&self, id: WidgetId) -> Span {
        trace_span!("KitharaPickerLayer", id = id.trace())
    }
}
