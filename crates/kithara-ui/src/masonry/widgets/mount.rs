use std::{cell::RefCell, rc::Rc};

use masonry::{
    core::{BoxConstraints, LayoutCtx, WidgetPod},
    kurbo::{Point, Rect, Size as MasonrySize},
};
use num_traits::cast::AsPrimitive;

use super::built::StageSize;
use crate::{
    atoms::{painter::ControlPainter, tab::TabLarge},
    draw::{DrawListBuilder, Rect as DrawRect},
    expand::{Binding, BindingKind, ControlSpec},
    hosts::{
        controls::{Draws, Reading},
        hosted::HostedControlPlan,
        scroll::{Bar, Window},
        solve::{self, Alignment, Length, Limits, Size},
        window::{controls::ControlsProgram, title::TitleProgram},
    },
    interact::Input,
    masonry::{
        custom::Respoken,
        hosted::MasonryHostedState,
        paint::button::declared_width,
        refresh::{DataRefresh, Refresh},
        retained::{
            MasonryHost, MasonryNode, Painted,
            controls::{Retained, SearchLeaf, TableLeaf, TreeLeaf},
            flex::{Flex, box_constraints, normalized},
            leaf::{DragProgram, Leaf},
            node::Node,
        },
    },
    module::{MeasureAxis, TextAlign},
    mount,
    render::{InputOwner, ReadValue, Skin, document::Measured},
    size::{Dim, SizeSpec, control_size},
};

/// How one built-in control becomes a leaf of the retained tree.
///
/// The default is an empty box of the right size: this host paints a control
/// only once its painter is neutral, and until then the control still holds its
/// place. Which controls are still waiting is the census in `tests`, not a
/// silent arm in a match.
pub(crate) trait NodeControl {
    fn leaf<A>(&self, _host: &MasonryHost<'_, A>, cx: &Cx<'_>) -> MasonryNode<A>
    where
        A: std::fmt::Debug + Send + 'static,
    {
        MasonryNode::empty(cx.declared)
    }

    /// Anything the host must still attach once the leaf exists: a window layer
    /// for the controls that move the window, a settings action for the one
    /// that opens it.
    fn wire<A>(&self, host: &MasonryHost<'_, A>, cx: &Cx<'_>, output: &mut MasonryNode<A>)
    where
        A: std::fmt::Debug + Send + 'static,
    {
        let _ = (host, cx, output);
    }
}

/// What a control is handed when it mounts: the box it was given, the endpoint
/// behind it, and who owns the pointer over it.
pub(crate) struct Cx<'a> {
    /// The skin this instance wears, which is the host's own unless the skin
    /// names this path.
    pub(crate) skin: &'a Skin,
    pub(crate) path: &'a str,
    pub(crate) owner: InputOwner,
    pub(crate) plan: Option<&'a HostedControlPlan<MasonryHostedState>>,
    pub(crate) read: Option<&'a Binding>,
    pub(crate) declared: Size<Length>,
}

impl NodeControl for mount::Drag {
    fn wire<A>(&self, host: &MasonryHost<'_, A>, _cx: &Cx<'_>, output: &mut MasonryNode<A>)
    where
        A: std::fmt::Debug + Send + 'static,
    {
        host.add_window_layer(output, DragProgram);
    }
}

impl NodeControl for mount::window::title_bar::host::TitleBar {
    fn wire<A>(&self, host: &MasonryHost<'_, A>, cx: &Cx<'_>, output: &mut MasonryNode<A>)
    where
        A: std::fmt::Debug + Send + 'static,
    {
        host.add_window_layer(
            output,
            TitleProgram::new(host.ctx.ui.resolve(self.label), cx.skin),
        );
    }
}

impl NodeControl for mount::Controls {
    fn wire<A>(&self, host: &MasonryHost<'_, A>, cx: &Cx<'_>, output: &mut MasonryNode<A>)
    where
        A: std::fmt::Debug + Send + 'static,
    {
        host.add_window_layer(output, ControlsProgram::new(self.style, cx.skin));
    }
}

impl NodeControl for mount::Vis {
    fn leaf<A>(&self, host: &MasonryHost<'_, A>, cx: &Cx<'_>) -> MasonryNode<A>
    where
        A: std::fmt::Debug + Send + 'static,
    {
        let value = cx.read.and_then(|binding| host.ctx.read(binding));
        let preset = cx
            .read
            .filter(|binding| binding.kind != BindingKind::Command)
            .map(|binding| host.ctx.ui.resolve(binding.key).to_owned());
        host.vis_leaf(preset, value, cx.declared)
    }
}
impl NodeControl for mount::panel::shader::host::Shader<'_> {
    fn leaf<A>(&self, host: &MasonryHost<'_, A>, cx: &Cx<'_>) -> MasonryNode<A>
    where
        A: std::fmt::Debug + Send + 'static,
    {
        let mut output = host.shader_leaf(self.spec.clone(), cx.path.to_owned(), cx.declared);
        output.watch_snapshot();
        output
    }
}
impl NodeControl for mount::panel::custom::host::Custom {
    fn leaf<A>(&self, host: &MasonryHost<'_, A>, cx: &Cx<'_>) -> MasonryNode<A>
    where
        A: std::fmt::Debug + Send + 'static,
    {
        let kind = host.ctx.ui.resolve(self.kind);
        let Some(widget) = host.ctx.kinds.and_then(|kinds| kinds.make(kind)) else {
            tracing::error!(kind, path = cx.path, "no registered widget for this kind");
            return MasonryNode::empty(cx.declared);
        };
        let map = Rc::clone(&host.map_event);
        host.custom_leaf(
            Box::new(Respoken::new(widget, move |event| map(event))),
            Some(kind),
            cx.declared,
        )
    }
}

macro_rules! hosted_controls {
    ($($control:ty => $variant:ident, $leaf:ident, $message:literal);+ $(;)?) => {
        $(
            impl NodeControl for $control {
                fn leaf<A>(&self, _host: &MasonryHost<'_, A>, cx: &Cx<'_>) -> MasonryNode<A>
                where
                    A: std::fmt::Debug + Send + 'static,
                {
                    let Some(HostedControlPlan::$variant(plan)) = cx.plan else {
                        tracing::error!(
                            control_path = cx.path,
                            engine_entry = "hosted plan",
                            $message
                        );
                        return MasonryNode::empty(cx.declared);
                    };
                    MasonryNode::control_leaf($leaf::new((**plan).clone(), cx.skin), cx.declared)
                }
            }
        )+
    };
}

hosted_controls!(
    mount::panel::table::host::Table<'_> => Table, TableLeaf, "Table mount is incomplete";
    mount::Search => Search, SearchLeaf, "Search mount is incomplete";
    mount::panel::tree::host::Tree<'_> => Tree, TreeLeaf, "Tree mount is incomplete";
);
impl NodeControl for mount::label::text::host::Text<'_> {
    fn leaf<A>(&self, host: &MasonryHost<'_, A>, cx: &Cx<'_>) -> MasonryNode<A>
    where
        A: std::fmt::Debug + Send + 'static,
    {
        let content = cx
            .read
            .and_then(|binding| host.ctx.read(binding))
            .and_then(|value| match value {
                ReadValue::Text(value) => Some(value),
                _ => None,
            })
            .or_else(|| self.label.map(|label| host.ctx.ui.resolve(label)))
            .unwrap_or_default();
        host.text_leaf(self, content, cx.skin, cx.declared)
    }
}

/// An unbound meter is an empty track rather than an empty box: that is what
/// the other host has always drawn for it.
/// A bounded window over a subtree taller than itself.
///
/// The window itself is the neutral one both hosts keep; what lives here is
/// only how this toolkit measures, places and clips the child under it, plus
/// the indicator style resolved once from the skin so the widget can draw the
/// bar without holding one.
pub(crate) struct Viewport {
    bar: Bar,
    view: Rc<RefCell<Window>>,
}

impl Viewport {
    pub(crate) const fn new(bar: Bar, view: Rc<RefCell<Window>>) -> Self {
        Self { bar, view }
    }

    pub(crate) fn indicate(&self, bounds: DrawRect, list: &mut DrawListBuilder) {
        self.view.borrow().indicate(bounds, self.bar, list);
    }

    pub(crate) fn layout(
        &mut self,
        ctx: &mut LayoutCtx<'_>,
        children: &mut [WidgetPod<Node>],
        limits: Limits,
        declared: Size<Length>,
    ) -> Size {
        let size = limits.resolve(declared.width, declared.height, limits.max());
        let inner = normalized(Limits::with_compression(
            Size::ZERO,
            Size::new(size.width, f32::MAX),
            Size::new(false, true),
        ));
        let content = children.first_mut().map_or(0.0, |child| {
            Node::set_child_limits(ctx, child, inner);
            let measured = ctx.run_layout(child, &box_constraints(inner));
            AsPrimitive::<f32>::as_(measured.height)
        });
        let offset = self.view.borrow_mut().measured(content, size.height);
        if let Some(child) = children.first_mut() {
            ctx.place_child(child, Point::new(0.0, f64::from(-offset)));
        }
        ctx.set_clip_path(Rect::from_origin_size(
            Point::ORIGIN,
            MasonrySize::new(f64::from(size.width), f64::from(size.height)),
        ));
        size
    }

    pub(crate) fn wheel(&mut self, input: Input<'_>) -> bool {
        self.view.borrow_mut().wheel(input)
    }
}

pub(crate) enum NodeLayout {
    Leaf(Leaf),
    Flex(Flex),
    /// Branches of which the room picks one.
    Measured(Measured),
    Scroll(Viewport),
    Stack,
    Stage(Rc<StageSize>),
}

impl NodeLayout {
    /// A window always answers, because the wheel over it is its own; a leaf
    /// answers only what it says it does.
    pub(crate) fn accepts_input(&self) -> bool {
        matches!(self, Self::Scroll(_)) || matches!(self, Self::Leaf(leaf) if leaf.accepts_input())
    }

    pub(crate) fn accepts_text_input(&self) -> bool {
        matches!(self, Self::Leaf(leaf) if leaf.accepts_text_input())
    }

    /// Draws whatever this node paints over its own children. Only a window
    /// has one: its indicator belongs above the rows it scrolls, not under
    /// them.
    pub(crate) fn indicate(&self, bounds: DrawRect, list: &mut DrawListBuilder) {
        match self {
            Self::Scroll(viewport) => viewport.indicate(bounds, list),
            Self::Flex(_) | Self::Leaf(_) | Self::Measured(_) | Self::Stack | Self::Stage(_) => {}
        }
    }

    pub(crate) fn layout(
        &mut self,
        ctx: &mut LayoutCtx<'_>,
        children: &mut [WidgetPod<Node>],
        limits: Limits,
        declared: Size<Length>,
    ) -> Size {
        match self {
            Self::Leaf(leaf) => {
                let intrinsic = leaf.measure(limits);
                limits.resolve(declared.width, declared.height, intrinsic)
            }
            Self::Flex(flex) => {
                let intrinsic = flex.layout(ctx, children, limits);
                limits.resolve(declared.width, declared.height, intrinsic)
            }
            Self::Measured(plan) => measured(plan, ctx, children, limits, declared),
            Self::Scroll(viewport) => viewport.layout(ctx, children, limits, declared),
            Self::Stack => stack(ctx, children, limits, declared),
            Self::Stage(size) => stage(ctx, children, size, limits),
        }
    }

    pub(crate) const fn leaf(&mut self) -> Option<&mut Leaf> {
        match self {
            Self::Leaf(leaf) => Some(leaf),
            Self::Flex(_) | Self::Measured(_) | Self::Scroll(_) | Self::Stack | Self::Stage(_) => {
                None
            }
        }
    }

    /// Whether the leaf this node holds draws differently under the pointer.
    pub(crate) fn reads_pointer(&self) -> bool {
        matches!(self, Self::Leaf(leaf) if leaf.reads_pointer())
    }

    /// Moves a bounded window under the pointer, answering whether it did.
    pub(crate) fn wheel(&mut self, input: Input<'_>) -> bool {
        match self {
            Self::Scroll(viewport) => viewport.wheel(input),
            Self::Flex(_) | Self::Leaf(_) | Self::Measured(_) | Self::Stack | Self::Stage(_) => {
                false
            }
        }
    }
}

fn stage(
    ctx: &mut LayoutCtx<'_>,
    children: &mut [WidgetPod<Node>],
    stage: &StageSize,
    limits: Limits,
) -> Size {
    let declared = stage.now();
    for (child, shown) in children.iter_mut().zip(stage.shown()) {
        ctx.set_stashed(child, !shown);
    }
    let inner = normalized(limits.width(declared.width).height(declared.height).loose());
    let intrinsic = children
        .iter_mut()
        .zip(stage.in_flow())
        .find_map(|(child, on)| on.then_some(child))
        .map_or(Size::ZERO, |first| {
            Node::set_child_limits(ctx, first, inner);
            let size = ctx.run_layout(first, &box_constraints(inner));
            Size::new(size.width.as_(), size.height.as_())
        });
    let size = limits.resolve(declared.width, declared.height, intrinsic);
    let loose = Limits::new(Size::ZERO, size);
    let shown = children.iter_mut().zip(stage.shown());
    for child in shown.filter_map(|(child, shown)| shown.then_some(child)) {
        Node::set_child_limits(ctx, child, loose);
        ctx.run_layout(child, &box_constraints(loose));
        let at = Node::child_spot(ctx, child).map_or(Point::ORIGIN, |at| {
            Point::new(f64::from(at.x), f64::from(at.y))
        });
        ctx.place_child(child, at);
    }
    size
}

/// Lays out the one branch the room reaches and stashes every other: a branch
/// the room does not reach takes neither a box nor a hand, even where it holds
/// a control with the same path as one the standing branch holds.
fn measured(
    plan: &Measured,
    ctx: &mut LayoutCtx<'_>,
    children: &mut [WidgetPod<Node>],
    limits: Limits,
    declared: Size<Length>,
) -> Size {
    let inner = normalized(limits.width(declared.width).height(declared.height));
    let room = match plan.axis {
        MeasureAxis::Width => inner.max().width,
        MeasureAxis::Height => inner.max().height,
    };
    let drawn = plan.branch(room).min(children.len().saturating_sub(1));
    let loose = inner.loose();
    let mut intrinsic = Size::ZERO;
    for (index, child) in children.iter_mut().enumerate() {
        ctx.set_stashed(child, index != drawn);
        if index == drawn {
            Node::set_child_limits(ctx, child, loose);
            let size = ctx.run_layout(child, &box_constraints(loose));
            intrinsic = Size::new(size.width.as_(), size.height.as_());
            ctx.place_child(child, Point::ORIGIN);
        }
    }
    limits.resolve(declared.width, declared.height, intrinsic)
}

fn stack(
    ctx: &mut LayoutCtx<'_>,
    children: &mut [WidgetPod<Node>],
    limits: Limits,
    declared: Size<Length>,
) -> Size {
    let inner = normalized(limits.width(declared.width).height(declared.height).loose());
    let intrinsic = children.first_mut().map_or(Size::ZERO, |first| {
        Node::set_child_limits(ctx, first, inner);
        let size = ctx.run_layout(first, &box_constraints(inner));
        Size::new(size.width.as_(), size.height.as_())
    });
    let size = limits.resolve(declared.width, declared.height, intrinsic);
    let exact = Limits::new(size, size);
    for child in children {
        Node::set_child_limits(ctx, child, exact);
        ctx.run_layout(
            child,
            &BoxConstraints::tight(MasonrySize::new(
                f64::from(size.width),
                f64::from(size.height),
            )),
        );
        ctx.place_child(child, Point::ORIGIN);
    }
    size
}

pub(crate) const fn main_length(dim: Dim) -> Length {
    match dim {
        Dim::Fixed(value) => Length::Fixed(value),
        Dim::Range { .. } | Dim::Fill | Dim::Shrink => Length::Fill,
    }
}

pub(crate) const fn declared(size: SizeSpec) -> Size<Length> {
    Size::new(solve::length(size.w), solve::length(size.h))
}

pub(crate) fn control_declared(
    spec: &ControlSpec,
    size: Option<SizeSpec>,
    skin: &Skin,
) -> Size<Length> {
    let intrinsic = match spec {
        ControlSpec::Button { style, .. } => Size::new(declared_width(*style, skin), Length::Fill),
        ControlSpec::TabLarge { .. } => TabLarge::declared_length(skin.tab_large.height),
        ControlSpec::Text { .. } => Size::new(Length::Shrink, Length::Shrink),
        ControlSpec::Spacer | ControlSpec::WindowDrag | ControlSpec::TitleBar { .. } => {
            Size::new(Length::Fill, Length::Fill)
        }
        _ => declared(control_size(spec, skin.document())),
    };
    size.map_or(intrinsic, |size| {
        Size::new(
            control_length(size.w, intrinsic.width),
            control_length(size.h, intrinsic.height),
        )
    })
}

pub(crate) const fn control_length(dim: Dim, intrinsic: Length) -> Length {
    match dim {
        Dim::Fixed(value) => Length::Fixed(value),
        Dim::Shrink => Length::Shrink,
        Dim::Range { .. } => match intrinsic {
            Length::FillPortion(portion) => Length::FillPortion(portion),
            Length::Fill | Length::Shrink | Length::Fixed(_) => Length::Fill,
        },
        Dim::Fill => Length::Fill,
    }
}

pub(crate) const fn alignment(value: TextAlign) -> Alignment {
    match value {
        TextAlign::Start => Alignment::Start,
        TextAlign::Center => Alignment::Center,
        TextAlign::End => Alignment::End,
    }
}

/// Who answers the pointer over this control.
///
/// The document says whether the leaf may own it at all; this host narrows that
/// to the controls it actually paints. One it still mounts as an empty box is
/// driven by the engine plan, and a leaf gesture beside that plan would be two
/// recognizers on one pointer.
pub(crate) fn pointer_owner(owner: InputOwner, spec: &ControlSpec) -> InputOwner {
    if owner == InputOwner::Leaf && leaf_paints(spec) {
        InputOwner::Leaf
    } else {
        InputOwner::Engine
    }
}

/// Whether this control reaches Vello as a painted leaf that can own the
/// pointer itself, rather than as an empty box the engine drives.
const fn leaf_paints(spec: &ControlSpec) -> bool {
    matches!(
        spec,
        ControlSpec::Button { .. }
            | ControlSpec::Chip { .. }
            | ControlSpec::Knob { .. }
            | ControlSpec::NavItem { .. }
            | ControlSpec::PresetSelector
            | ControlSpec::Range
            | ControlSpec::SettingsButton
            | ControlSpec::TabLarge { .. }
    )
}

pub(crate) const fn activates(spec: &ControlSpec) -> bool {
    matches!(
        spec,
        ControlSpec::NavItem { .. }
            | ControlSpec::TabLarge { .. }
            | ControlSpec::Button { .. }
            | ControlSpec::Toggle
            | ControlSpec::Checkbox
            | ControlSpec::Chip { .. }
    )
}

/// Mounts a control that draws itself, adding nothing to the picture.
pub(crate) fn painted<Control, A>(
    control: &Control,
    host: &MasonryHost<'_, A>,
    cx: &Cx<'_>,
) -> MasonryNode<A>
where
    Control: Draws,
    Control::Painter: Retained + 'static,
    A: std::fmt::Debug + Send + 'static,
{
    drawn(control, host, cx, |_| None)
}

/// Mounts a control that draws itself and, once mounted, steps its own data
/// from what the host reads.
pub(crate) fn refreshing<Control, A>(
    control: &Control,
    host: &MasonryHost<'_, A>,
    cx: &Cx<'_>,
) -> MasonryNode<A>
where
    Control: Refresh,
    Control::Painter: Retained + 'static,
    A: std::fmt::Debug + Send + 'static,
{
    drawn(control, host, cx, |reading| {
        control.refresh(reading, host.ctx.endpoint(cx.read))
    })
}

fn drawn<Control, A>(
    control: &Control,
    host: &MasonryHost<'_, A>,
    cx: &Cx<'_>,
    refresh: impl FnOnce(Reading<'_>) -> Option<DataRefresh<<Control::Painter as ControlPainter>::Data>>,
) -> MasonryNode<A>
where
    Control: Draws,
    Control::Painter: Retained + 'static,
    A: std::fmt::Debug + Send + 'static,
{
    let value = cx.read.and_then(|binding| host.ctx.read(binding));
    let reading = Reading {
        ctx: host.ctx,
        scope: host.ctx.scope(cx.read),
        skin: cx.skin,
        value: value.as_ref(),
    };
    let Some(data) = control.data(reading) else {
        return MasonryNode::empty(cx.declared);
    };
    let grip = control.grip(cx.skin, &data);
    let index_event = control.index_event();
    let refresh = refresh(reading);
    let refreshes = refresh.is_some();
    let leaf = Painted::pooled(
        control.painter(cx.skin),
        data,
        cx.skin,
        &host.ctx.ui.draw_buffers,
    );
    let leaf = if let Some(refresh) = refresh {
        leaf.refreshing(refresh)
    } else {
        leaf
    };
    let leaf = host.owned(leaf, cx.owner, cx.path, |leaf, path, map_event| {
        leaf.interactive(grip, path, map_event, index_event)
    });
    let mut output = MasonryNode::control_leaf(leaf, cx.declared);
    if refreshes {
        output.watch_snapshot();
    }
    output
}

impl NodeControl for mount::panel::context_bar::host::ContextBar<'_> {
    fn leaf<A>(&self, host: &MasonryHost<'_, A>, cx: &Cx<'_>) -> MasonryNode<A>
    where
        A: std::fmt::Debug + Send + 'static,
    {
        painted(self, host, cx)
    }
}

/// Which controls this host lets answer the pointer themselves.
#[cfg(test)]
mod owns {
    use kithara_test_utils::kithara;

    use super::{ControlSpec, InputOwner, pointer_owner};

    /// A control this host still mounts as an empty box has an engine plan
    /// behind it. Handing its leaf a gesture as well would put two recognizers
    /// on one pointer, and the document cannot see the difference to say so.
    #[kithara::test]
    fn a_control_this_host_does_not_paint_is_left_to_the_engine() {
        assert_eq!(
            pointer_owner(InputOwner::Leaf, &ControlSpec::VuVertical { ticks: false }),
            InputOwner::Engine
        );
    }

    #[kithara::test]
    fn a_control_this_host_paints_keeps_the_leaf_the_document_gave_it() {
        assert_eq!(
            pointer_owner(InputOwner::Leaf, &ControlSpec::Knob { label: None }),
            InputOwner::Leaf
        );
        assert_eq!(
            pointer_owner(InputOwner::Leaf, &ControlSpec::PresetSelector),
            InputOwner::Leaf
        );
    }

    /// And a document that kept the pointer for the engine keeps it, whatever
    /// this host can paint.
    #[kithara::test]
    fn an_engine_owned_control_stays_engine_owned() {
        assert_eq!(
            pointer_owner(InputOwner::Engine, &ControlSpec::Knob { label: None }),
            InputOwner::Engine
        );
    }
}
