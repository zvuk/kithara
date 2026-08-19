use masonry::{
    core::{BoxConstraints, LayoutCtx, WidgetPod},
    kurbo::{Point, Rect, Size as MasonrySize},
};
use num_traits::cast::AsPrimitive;

use super::{
    MasonryHost, MasonryNode, Painted,
    controls::{Retained, TableLeaf, TreeLeaf},
    flex::{box_constraints, normalized},
    leaf::{DragProgram, Leaf},
    node::Node,
};
use crate::{
    atoms::{button::declared_width, deck::summary::Summary, tab::TabLarge},
    draw::{DrawListBuilder, Rect as DrawRect},
    expand::{Binding, BindingKind, ControlSpec},
    interact::Input,
    module::TextAlign,
    mount,
    render::{
        ControlsProgram, HostedControlPlan, InputOwner, ReadValue, Skin, TitleProgram,
        controls::{Draws, Reading},
        scroll::{Bar, Window},
    },
    size::{Dim, SizeSpec, control_size},
    solve,
};

/// How one built-in control becomes a leaf of the retained tree.
///
/// The default is an empty box of the right size: this host paints a control
/// only once its painter is neutral, and until then the control still holds its
/// place. Which controls are still waiting is the census in `tests`, not a
/// silent arm in a match.
pub(super) trait NodeControl {
    fn leaf<A>(&self, _host: &MasonryHost<'_, A>, cx: &Cx<'_>) -> MasonryNode<A>
    where
        A: std::fmt::Debug + Send + 'static,
    {
        MasonryNode::empty(cx.declared)
    }

    /// Anything the host must still attach once the leaf exists: a window layer
    /// for the controls that move the window, a settings action for the one
    /// that opens it.
    fn wire<A>(&self, host: &MasonryHost<'_, A>, output: &mut MasonryNode<A>)
    where
        A: std::fmt::Debug + Send + 'static,
    {
        let _ = (host, output);
    }
}

/// What a control is handed when it mounts: the box it was given, the endpoint
/// behind it, and who owns the pointer over it.
pub(super) struct Cx<'a> {
    pub(super) declared: solve::Size<solve::Length>,
    pub(super) owner: InputOwner,
    pub(super) path: &'a str,
    pub(super) plan: Option<&'a HostedControlPlan>,
    pub(super) read: Option<&'a Binding>,
}

impl NodeControl for mount::Summary {
    fn leaf<A>(&self, host: &MasonryHost<'_, A>, cx: &Cx<'_>) -> MasonryNode<A>
    where
        A: std::fmt::Debug + Send + 'static,
    {
        painted(self, host, cx)
    }
}
impl NodeControl for mount::Brand {
    fn leaf<A>(&self, host: &MasonryHost<'_, A>, cx: &Cx<'_>) -> MasonryNode<A>
    where
        A: std::fmt::Debug + Send + 'static,
    {
        painted(self, host, cx)
    }
}
impl NodeControl for mount::Spacer {
    fn leaf<A>(&self, host: &MasonryHost<'_, A>, cx: &Cx<'_>) -> MasonryNode<A>
    where
        A: std::fmt::Debug + Send + 'static,
    {
        painted(self, host, cx)
    }
}
impl NodeControl for mount::Divider {
    fn leaf<A>(&self, host: &MasonryHost<'_, A>, cx: &Cx<'_>) -> MasonryNode<A>
    where
        A: std::fmt::Debug + Send + 'static,
    {
        painted(self, host, cx)
    }
}
impl NodeControl for mount::Preset {
    fn leaf<A>(&self, host: &MasonryHost<'_, A>, cx: &Cx<'_>) -> MasonryNode<A>
    where
        A: std::fmt::Debug + Send + 'static,
    {
        painted(self, host, cx)
    }
}
impl NodeControl for mount::Settings {
    fn leaf<A>(&self, host: &MasonryHost<'_, A>, cx: &Cx<'_>) -> MasonryNode<A>
    where
        A: std::fmt::Debug + Send + 'static,
    {
        painted(self, host, cx)
    }
}

impl NodeControl for mount::Drag {
    fn wire<A>(&self, host: &MasonryHost<'_, A>, output: &mut MasonryNode<A>)
    where
        A: std::fmt::Debug + Send + 'static,
    {
        host.add_window_layer(output, DragProgram);
    }
}

impl NodeControl for mount::TitleBar {
    fn wire<A>(&self, host: &MasonryHost<'_, A>, output: &mut MasonryNode<A>)
    where
        A: std::fmt::Debug + Send + 'static,
    {
        host.add_window_layer(
            output,
            TitleProgram::new(host.ctx.ui.resolve(self.label), host.skin),
        );
    }
}

impl NodeControl for mount::Controls {
    fn wire<A>(&self, host: &MasonryHost<'_, A>, output: &mut MasonryNode<A>)
    where
        A: std::fmt::Debug + Send + 'static,
    {
        host.add_window_layer(output, ControlsProgram::new(self.style, host.skin));
    }
}

impl NodeControl for mount::Glyph<'_> {
    fn leaf<A>(&self, host: &MasonryHost<'_, A>, cx: &Cx<'_>) -> MasonryNode<A>
    where
        A: std::fmt::Debug + Send + 'static,
    {
        painted(self, host, cx)
    }
}
impl NodeControl for mount::Bpm {
    fn leaf<A>(&self, host: &MasonryHost<'_, A>, cx: &Cx<'_>) -> MasonryNode<A>
    where
        A: std::fmt::Debug + Send + 'static,
    {
        painted(self, host, cx)
    }
}
impl NodeControl for mount::Time {
    fn leaf<A>(&self, host: &MasonryHost<'_, A>, cx: &Cx<'_>) -> MasonryNode<A>
    where
        A: std::fmt::Debug + Send + 'static,
    {
        painted(self, host, cx)
    }
}
impl NodeControl for mount::Telemetry {
    fn leaf<A>(&self, host: &MasonryHost<'_, A>, cx: &Cx<'_>) -> MasonryNode<A>
    where
        A: std::fmt::Debug + Send + 'static,
    {
        painted(self, host, cx)
    }
}
impl NodeControl for mount::Wave<'_> {
    fn leaf<A>(&self, host: &MasonryHost<'_, A>, cx: &Cx<'_>) -> MasonryNode<A>
    where
        A: std::fmt::Debug + Send + 'static,
    {
        painted(self, host, cx)
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
impl NodeControl for mount::Shader<'_> {
    fn leaf<A>(&self, host: &MasonryHost<'_, A>, cx: &Cx<'_>) -> MasonryNode<A>
    where
        A: std::fmt::Debug + Send + 'static,
    {
        let mut output = host.shader_leaf(self.spec.clone(), cx.path.to_owned(), cx.declared);
        output.watch_snapshot();
        output
    }
}
impl NodeControl for mount::Lottie {
    fn leaf<A>(&self, host: &MasonryHost<'_, A>, cx: &Cx<'_>) -> MasonryNode<A>
    where
        A: std::fmt::Debug + Send + 'static,
    {
        painted(self, host, cx)
    }
}

impl NodeControl for mount::Sprite {
    fn leaf<A>(&self, host: &MasonryHost<'_, A>, cx: &Cx<'_>) -> MasonryNode<A>
    where
        A: std::fmt::Debug + Send + 'static,
    {
        painted(self, host, cx)
    }
}

impl NodeControl for mount::PortalMap {
    fn leaf<A>(&self, host: &MasonryHost<'_, A>, cx: &Cx<'_>) -> MasonryNode<A>
    where
        A: std::fmt::Debug + Send + 'static,
    {
        painted(self, host, cx)
    }
}
impl NodeControl for mount::Range {
    fn leaf<A>(&self, host: &MasonryHost<'_, A>, cx: &Cx<'_>) -> MasonryNode<A>
    where
        A: std::fmt::Debug + Send + 'static,
    {
        painted(self, host, cx)
    }
}
impl NodeControl for mount::Table<'_> {
    fn leaf<A>(&self, host: &MasonryHost<'_, A>, cx: &Cx<'_>) -> MasonryNode<A>
    where
        A: std::fmt::Debug + Send + 'static,
    {
        let Some(HostedControlPlan::Table(plan)) = cx.plan else {
            tracing::error!(
                control_path = cx.path,
                engine_entry = "hosted plan",
                "Table mount is incomplete"
            );
            return MasonryNode::empty(cx.declared);
        };
        MasonryNode::control_leaf(TableLeaf::new((**plan).clone(), host.skin), cx.declared)
    }
}
impl NodeControl for mount::Tree<'_> {
    fn leaf<A>(&self, host: &MasonryHost<'_, A>, cx: &Cx<'_>) -> MasonryNode<A>
    where
        A: std::fmt::Debug + Send + 'static,
    {
        let Some(HostedControlPlan::Tree(plan)) = cx.plan else {
            tracing::error!(
                control_path = cx.path,
                engine_entry = "hosted plan",
                "Tree mount is incomplete"
            );
            return MasonryNode::empty(cx.declared);
        };
        MasonryNode::control_leaf(TreeLeaf::new((**plan).clone(), host.skin), cx.declared)
    }
}
impl NodeControl for mount::ContextBar<'_> {
    fn leaf<A>(&self, host: &MasonryHost<'_, A>, cx: &Cx<'_>) -> MasonryNode<A>
    where
        A: std::fmt::Debug + Send + 'static,
    {
        painted(self, host, cx)
    }
}
impl NodeControl for mount::Segmented<'_> {
    fn leaf<A>(&self, host: &MasonryHost<'_, A>, cx: &Cx<'_>) -> MasonryNode<A>
    where
        A: std::fmt::Debug + Send + 'static,
    {
        painted(self, host, cx)
    }
}
impl NodeControl for mount::Select {
    fn leaf<A>(&self, host: &MasonryHost<'_, A>, cx: &Cx<'_>) -> MasonryNode<A>
    where
        A: std::fmt::Debug + Send + 'static,
    {
        painted(self, host, cx)
    }
}
impl NodeControl for mount::Readout {
    fn leaf<A>(&self, host: &MasonryHost<'_, A>, cx: &Cx<'_>) -> MasonryNode<A>
    where
        A: std::fmt::Debug + Send + 'static,
    {
        painted(self, host, cx)
    }
}

impl NodeControl for mount::Text<'_> {
    fn leaf<A>(&self, host: &MasonryHost<'_, A>, cx: &Cx<'_>) -> MasonryNode<A>
    where
        A: std::fmt::Debug + Send + 'static,
    {
        let content = cx
            .read
            .and_then(|binding| host.ctx.read(binding))
            .and_then(|value| match value {
                ReadValue::Text(value) => Some(value.to_owned()),
                _ => None,
            })
            .or_else(|| {
                self.label
                    .map(|label| host.ctx.ui.resolve(label).to_owned())
            })
            .unwrap_or_default();
        host.text_leaf(self, content, host.reads_true(self.active), cx.declared)
    }
}

impl NodeControl for mount::Knob {
    fn leaf<A>(&self, host: &MasonryHost<'_, A>, cx: &Cx<'_>) -> MasonryNode<A>
    where
        A: std::fmt::Debug + Send + 'static,
    {
        painted(self, host, cx)
    }
}

impl NodeControl for mount::Chip {
    fn leaf<A>(&self, host: &MasonryHost<'_, A>, cx: &Cx<'_>) -> MasonryNode<A>
    where
        A: std::fmt::Debug + Send + 'static,
    {
        painted(self, host, cx)
    }
}

impl NodeControl for mount::Tab {
    fn leaf<A>(&self, host: &MasonryHost<'_, A>, cx: &Cx<'_>) -> MasonryNode<A>
    where
        A: std::fmt::Debug + Send + 'static,
    {
        painted(self, host, cx)
    }
}

/// An unbound meter is an empty track rather than an empty box: that is what
/// the other host has always drawn for it.
impl NodeControl for mount::Meter {
    fn leaf<A>(&self, host: &MasonryHost<'_, A>, cx: &Cx<'_>) -> MasonryNode<A>
    where
        A: std::fmt::Debug + Send + 'static,
    {
        painted(self, host, cx)
    }
}

impl NodeControl for mount::Cell {
    fn leaf<A>(&self, host: &MasonryHost<'_, A>, cx: &Cx<'_>) -> MasonryNode<A>
    where
        A: std::fmt::Debug + Send + 'static,
    {
        painted(self, host, cx)
    }
}

impl NodeControl for mount::Swatch {
    fn leaf<A>(&self, host: &MasonryHost<'_, A>, cx: &Cx<'_>) -> MasonryNode<A>
    where
        A: std::fmt::Debug + Send + 'static,
    {
        painted(self, host, cx)
    }
}

impl NodeControl for mount::StatusDot<'_> {
    fn leaf<A>(&self, host: &MasonryHost<'_, A>, cx: &Cx<'_>) -> MasonryNode<A>
    where
        A: std::fmt::Debug + Send + 'static,
    {
        painted(self, host, cx)
    }
}

impl NodeControl for mount::Toggle {
    fn leaf<A>(&self, host: &MasonryHost<'_, A>, cx: &Cx<'_>) -> MasonryNode<A>
    where
        A: std::fmt::Debug + Send + 'static,
    {
        painted(self, host, cx)
    }
}

impl NodeControl for mount::Checkbox {
    fn leaf<A>(&self, host: &MasonryHost<'_, A>, cx: &Cx<'_>) -> MasonryNode<A>
    where
        A: std::fmt::Debug + Send + 'static,
    {
        painted(self, host, cx)
    }
}

impl NodeControl for mount::VuVertical {
    fn leaf<A>(&self, host: &MasonryHost<'_, A>, cx: &Cx<'_>) -> MasonryNode<A>
    where
        A: std::fmt::Debug + Send + 'static,
    {
        painted(self, host, cx)
    }
}

impl NodeControl for mount::VuStereo {
    fn leaf<A>(&self, host: &MasonryHost<'_, A>, cx: &Cx<'_>) -> MasonryNode<A>
    where
        A: std::fmt::Debug + Send + 'static,
    {
        painted(self, host, cx)
    }
}

impl NodeControl for mount::Crossfader {
    fn leaf<A>(&self, host: &MasonryHost<'_, A>, cx: &Cx<'_>) -> MasonryNode<A>
    where
        A: std::fmt::Debug + Send + 'static,
    {
        painted(self, host, cx)
    }
}

impl NodeControl for mount::Fader {
    fn leaf<A>(&self, host: &MasonryHost<'_, A>, cx: &Cx<'_>) -> MasonryNode<A>
    where
        A: std::fmt::Debug + Send + 'static,
    {
        painted(self, host, cx)
    }
}

impl NodeControl for mount::NavItem {
    fn leaf<A>(&self, host: &MasonryHost<'_, A>, cx: &Cx<'_>) -> MasonryNode<A>
    where
        A: std::fmt::Debug + Send + 'static,
    {
        painted(self, host, cx)
    }
}

impl NodeControl for mount::Button {
    fn leaf<A>(&self, host: &MasonryHost<'_, A>, cx: &Cx<'_>) -> MasonryNode<A>
    where
        A: std::fmt::Debug + Send + 'static,
    {
        painted(self, host, cx)
    }
}

/// A bounded window over a subtree taller than itself.
///
/// The window itself is the neutral one both hosts keep; what lives here is
/// only how this toolkit measures, places and clips the child under it, plus
/// the indicator style resolved once from the skin so the widget can draw the
/// bar without holding one.
pub(super) struct Viewport {
    bar: Bar,
    view: Window,
}

impl Viewport {
    pub(super) const fn new(bar: Bar) -> Self {
        Self {
            bar,
            view: Window::new(),
        }
    }

    pub(super) fn indicate(&self, bounds: DrawRect, list: &mut DrawListBuilder) {
        self.view.indicate(bounds, self.bar, list);
    }

    pub(super) fn layout(
        &mut self,
        ctx: &mut LayoutCtx<'_>,
        children: &mut [WidgetPod<Node>],
        limits: solve::Limits,
        declared: solve::Size<solve::Length>,
    ) -> solve::Size {
        let size = limits.resolve(declared.width, declared.height, limits.max());
        // The child is measured against the window's width and its own height.
        // The scrolled axis is compressed, which is what makes a `Fill` child
        // report the content it has rather than claim the window: an uncompressed
        // limit would make the content exactly as tall as the window, and there
        // would be nothing to scroll to.
        let inner = normalized(solve::Limits::with_compression(
            solve::Size::ZERO,
            solve::Size::new(size.width, f32::MAX),
            solve::Size::new(false, true),
        ));
        let content = children.first_mut().map_or(0.0, |child| {
            Node::set_child_limits(ctx, child, inner);
            let measured = ctx.run_layout(child, &box_constraints(inner));
            AsPrimitive::<f32>::as_(measured.height)
        });
        let offset = self.view.measured(content, size.height);
        if let Some(child) = children.first_mut() {
            ctx.place_child(child, Point::new(0.0, f64::from(-offset)));
        }
        ctx.set_clip_path(Rect::from_origin_size(
            Point::ORIGIN,
            MasonrySize::new(f64::from(size.width), f64::from(size.height)),
        ));
        size
    }

    pub(super) fn wheel(&mut self, input: Input<'_>) -> bool {
        self.view.wheel(input)
    }
}

pub(super) enum NodeLayout {
    Leaf(Leaf),
    Flex(super::flex::Flex),
    Scroll(Viewport),
    Stack,
    Stage,
}

impl NodeLayout {
    pub(super) fn layout(
        &mut self,
        ctx: &mut LayoutCtx<'_>,
        children: &mut [WidgetPod<Node>],
        limits: solve::Limits,
        declared: solve::Size<solve::Length>,
    ) -> solve::Size {
        match self {
            Self::Leaf(leaf) => {
                let intrinsic = leaf.measure(limits);
                limits.resolve(declared.width, declared.height, intrinsic)
            }
            Self::Flex(flex) => {
                let intrinsic = flex.layout(ctx, children, limits);
                limits.resolve(declared.width, declared.height, intrinsic)
            }
            Self::Scroll(viewport) => viewport.layout(ctx, children, limits, declared),
            Self::Stack => stack(ctx, children, limits, declared),
            Self::Stage => stage(ctx, children, limits, declared),
        }
    }

    pub(super) const fn leaf(&mut self) -> Option<&mut Leaf> {
        match self {
            Self::Leaf(leaf) => Some(leaf),
            Self::Flex(_) | Self::Scroll(_) | Self::Stack | Self::Stage => None,
        }
    }

    /// Draws whatever this node paints over its own children. Only a window
    /// has one: its indicator belongs above the rows it scrolls, not under
    /// them.
    pub(super) fn indicate(&self, bounds: DrawRect, list: &mut DrawListBuilder) {
        match self {
            Self::Scroll(viewport) => viewport.indicate(bounds, list),
            Self::Flex(_) | Self::Leaf(_) | Self::Stack | Self::Stage => {}
        }
    }

    /// Moves a bounded window under the pointer, answering whether it did.
    pub(super) fn wheel(&mut self, input: Input<'_>) -> bool {
        match self {
            Self::Scroll(viewport) => viewport.wheel(input),
            Self::Flex(_) | Self::Leaf(_) | Self::Stack | Self::Stage => false,
        }
    }

    /// A window always answers, because the wheel over it is its own; a leaf
    /// answers only what it says it does.
    pub(super) fn accepts_input(&self) -> bool {
        matches!(self, Self::Scroll(_)) || matches!(self, Self::Leaf(leaf) if leaf.accepts_input())
    }

    pub(super) fn accepts_text_input(&self) -> bool {
        matches!(self, Self::Leaf(leaf) if leaf.accepts_text_input())
    }
}

/// A stage sizes itself off its first child, like a stack, and then offers
/// every child that box **loosely**: a child keeps whatever size it declared
/// and sits at the box's origin, which is where an object then offsets it from.
///
/// This is the whole difference from `stack`, and it is not a detail. A stack
/// hands its children a tight box because its one child is a popover or a
/// viewport that must fill it. Handing a stage's children the same tight box
/// stretches every one of them to the full width and throws away the placement
/// the document asked for — measured on the gallery's motion page, where the
/// immediate host drew three sized children and the retained host drew one
/// stretched chip.
fn stage(
    ctx: &mut LayoutCtx<'_>,
    children: &mut [WidgetPod<Node>],
    limits: solve::Limits,
    declared: solve::Size<solve::Length>,
) -> solve::Size {
    let inner = normalized(limits.width(declared.width).height(declared.height).loose());
    let intrinsic = children.first_mut().map_or(solve::Size::ZERO, |first| {
        Node::set_child_limits(ctx, first, inner);
        let size = ctx.run_layout(first, &box_constraints(inner));
        solve::Size::new(size.width.as_(), size.height.as_())
    });
    let size = limits.resolve(declared.width, declared.height, intrinsic);
    let loose = solve::Limits::new(solve::Size::ZERO, size);
    for child in children {
        Node::set_child_limits(ctx, child, loose);
        ctx.run_layout(child, &box_constraints(loose));
        ctx.place_child(child, Point::ORIGIN);
    }
    size
}

fn stack(
    ctx: &mut LayoutCtx<'_>,
    children: &mut [WidgetPod<Node>],
    limits: solve::Limits,
    declared: solve::Size<solve::Length>,
) -> solve::Size {
    let inner = normalized(limits.width(declared.width).height(declared.height).loose());
    let intrinsic = children.first_mut().map_or(solve::Size::ZERO, |first| {
        Node::set_child_limits(ctx, first, inner);
        let size = ctx.run_layout(first, &box_constraints(inner));
        solve::Size::new(size.width.as_(), size.height.as_())
    });
    let size = limits.resolve(declared.width, declared.height, intrinsic);
    let exact = solve::Limits::new(size, size);
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

pub(crate) const fn main_length(dim: Dim) -> solve::Length {
    match dim {
        Dim::Fixed(value) => solve::Length::Fixed(value),
        Dim::Range { .. } | Dim::Fill | Dim::Shrink => solve::Length::Fill,
    }
}

pub(crate) const fn length(dim: Dim) -> solve::Length {
    match dim {
        Dim::Fixed(value) => solve::Length::Fixed(value),
        Dim::Shrink => solve::Length::Shrink,
        Dim::Range { .. } | Dim::Fill => solve::Length::Fill,
    }
}

pub(crate) const fn declared(size: SizeSpec) -> solve::Size<solve::Length> {
    solve::Size::new(length(size.w), length(size.h))
}

pub(crate) fn control_declared(
    spec: &ControlSpec,
    size: Option<SizeSpec>,
    skin: &Skin,
) -> solve::Size<solve::Length> {
    let intrinsic = match spec {
        ControlSpec::DeckSummary { .. } => Summary::declared_length(skin.deck),
        ControlSpec::Button { style, .. } => {
            solve::Size::new(declared_width(*style, skin), solve::Length::Fill)
        }
        ControlSpec::TabLarge { .. } => TabLarge::declared_length(skin.tab_large.height),
        ControlSpec::Text { .. } => solve::Size::new(solve::Length::Shrink, solve::Length::Fill),
        ControlSpec::Spacer | ControlSpec::WindowDrag | ControlSpec::TitleBar { .. } => {
            solve::Size::new(solve::Length::Fill, solve::Length::Fill)
        }
        _ => declared(control_size(spec, skin.document())),
    };
    size.map_or(intrinsic, |size| {
        solve::Size::new(
            control_length(size.w, intrinsic.width),
            control_length(size.h, intrinsic.height),
        )
    })
}

pub(crate) const fn control_length(dim: Dim, intrinsic: solve::Length) -> solve::Length {
    match dim {
        Dim::Fixed(value) => solve::Length::Fixed(value),
        Dim::Shrink => solve::Length::Shrink,
        Dim::Range { .. } => match intrinsic {
            solve::Length::FillPortion(portion) => solve::Length::FillPortion(portion),
            solve::Length::Fill | solve::Length::Shrink | solve::Length::Fixed(_) => {
                solve::Length::Fill
            }
        },
        Dim::Fill => solve::Length::Fill,
    }
}

pub(crate) const fn alignment(value: TextAlign) -> solve::Alignment {
    match value {
        TextAlign::Start => solve::Alignment::Start,
        TextAlign::Center => solve::Alignment::Center,
        TextAlign::End => solve::Alignment::End,
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
fn painted<Control, A>(control: &Control, host: &MasonryHost<'_, A>, cx: &Cx<'_>) -> MasonryNode<A>
where
    Control: Draws,
    Control::Painter: Retained + 'static,
    A: std::fmt::Debug + Send + 'static,
{
    let value = cx.read.and_then(|binding| host.ctx.read(binding));
    let reading = Reading {
        ctx: host.ctx,
        scope: host.ctx.scope(cx.read),
        value: value.as_ref(),
    };
    let Some(data) = control.data(reading) else {
        return MasonryNode::empty(cx.declared);
    };
    let grip = control.grip(host.skin, &data);
    let index_event = control.index_event();
    let refresh = control.retained_refresh(reading, host.ctx.endpoint(cx.read));
    let refreshes = refresh.is_some();
    let leaf = Painted::pooled(
        control.painter(host.skin),
        data,
        host.skin,
        host.ctx.ui.draw_pools(),
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
