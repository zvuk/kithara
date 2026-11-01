use std::{
    cell::{Cell, RefCell},
    collections::BTreeMap,
    rc::Rc,
};

use masonry::core::{NewWidget, Widget, WidgetId};

use super::{
    CustomWidget, MasonryNode,
    built::{BlockState, LayerParts},
    controls::Retained,
    custom::{HostAction, MappedCustom},
    flex::{ChildLayout, Flex},
    leaf::{Leaf, TextFace, TextFaces, WindowLeafLayer},
    modal::ModalLayer,
    mount::{
        Cx, NodeControl, NodeLayout, Viewport, activates, alignment, control_declared, declared,
        painted, pointer_owner,
    },
    node::{Detent, Face, Faces, Node},
    popover::{PopoverLayer, PopoverState, SurfaceKind},
    shader::ShaderLeaf,
    spot::{Grip, Spot},
    vis::VisLeaf,
    window_layer::WindowLayer,
};
use crate::{
    draw::{Rgba, Transform},
    expand::{Binding, ControlSpec, ExpandedNode},
    hosts::{
        controls::Draws,
        hosted::HostedControlPlan,
        scroll::{Bar, Window},
        solve::{self, Alignment, Length, Padding, Size},
        surface::entry_value,
        window::DragGhost,
    },
    ids::InternId,
    layout::{Axis, FrameSides},
    masonry::hosted::hosted_control_plan,
    module::{ChromeStyle, MeasureAxis, TextStyle},
    mount,
    render::{
        ControlAction, CustomSkin, InputOwner, Published, ReadValue, Skin,
        document::{
            Ctx, Group, GroupMount, Host, Measured, Modal, Module, PlacedMount, Popover,
            SplitMount, StageMount,
        },
    },
    shaping::TextContext,
    size::SizeSpec,
    skin::ColorRole,
};

type Windows = BTreeMap<String, Rc<RefCell<Window>>>;

/// Mounts the toolkit-neutral document fold into a retained Masonry widget tree.
pub struct MasonryHost<'a, Action = Published> {
    pub(crate) skin: &'a Skin,
    pub(crate) ctx: Ctx<'a, 'a>,
    pub(crate) map_event: Rc<dyn Fn(Published) -> HostAction>,
    custom: BTreeMap<String, Box<dyn CustomWidget<Action = HostAction>>>,
    state: MasonryState,
    action: std::marker::PhantomData<fn() -> Action>,
}

/// Retained host state shared across Masonry document rebuilds.
#[derive(Clone, Default)]
pub struct MasonryState {
    paths: Rc<RefCell<BTreeMap<String, Vec<WidgetId>>>>,
    popovers: Rc<RefCell<BTreeMap<String, Rc<PopoverState>>>>,
    windows: Rc<RefCell<Windows>>,
    pointer: Rc<Cell<Option<crate::draw::Pt>>>,
}

impl MasonryState {
    fn popover(&self, path: &str, open: bool, kind: SurfaceKind) -> Rc<PopoverState> {
        let mut popovers = self.popovers.borrow_mut();
        let state = Rc::clone(
            popovers
                .entry(path.to_owned())
                .or_insert_with(|| Rc::new(PopoverState::new(kind))),
        );
        state.latch(open);
        state
    }

    pub(crate) fn tag_path(&self, path: &str, id: WidgetId) {
        self.paths
            .borrow_mut()
            .entry(path.to_owned())
            .or_default()
            .push(id);
    }

    delegate::delegate! {
        to self.paths.borrow() {
            #[expr($?.last().copied())]
            #[call(get)]
            pub(crate) fn widget_id(&self, path: &str) -> Option<WidgetId>;
            /// Every widget the document tagged with one path, in mount order.
            ///
            /// The branches of an adaptive block repeat the paths of the controls
            /// they share, and only the branch the room reaches is laid out, so one
            /// path can name several widgets at once.
            #[expr($.cloned().unwrap_or_default())]
            #[call(get)]
            pub(crate) fn widget_ids(&self, path: &str) -> Vec<WidgetId>;
        }
    }

    /// The window one scroll of the document looks through.
    ///
    /// Where a window is scrolled to belongs to the host and not to the
    /// document, and this host builds another tree whenever the application
    /// turns to a different document. A window kept only by the tree would
    /// start over at the top every time that happened, which throws the hand
    /// back to the first row whenever it picks one of the last.
    fn window(&self, path: &str) -> Rc<RefCell<Window>> {
        Rc::clone(
            self.windows
                .borrow_mut()
                .entry(path.to_owned())
                .or_insert_with(|| Rc::new(RefCell::new(Window::new()))),
        )
    }
}

impl<Action> MasonryHost<'_, Action> {
    /// Reuses retained state, replacing the previous tree's path registrations.
    #[must_use]
    pub fn with_state(mut self, state: MasonryState) -> Self {
        state.paths.borrow_mut().clear();
        self.state = state;
        self
    }
}

impl<'a> MasonryHost<'a, Published> {
    #[must_use]
    pub fn new(ctx: Ctx<'a, 'a>, skin: &'a Skin) -> Self {
        Self::map_actions(ctx, skin, |event| event)
    }
}

impl<'a, Action> MasonryHost<'a, Action>
where
    Action: std::fmt::Debug + Send + 'static,
{
    /// Creates a host that maps built-in document events into one app action.
    #[must_use]
    pub fn map_actions<Map>(ctx: Ctx<'a, 'a>, skin: &'a Skin, map: Map) -> Self
    where
        Map: Fn(Published) -> Action + 'static,
    {
        Self {
            ctx,
            skin,
            custom: BTreeMap::new(),
            map_event: Rc::new(move |event| HostAction::new(map(event))),
            state: MasonryState::default(),
            action: std::marker::PhantomData,
        }
    }

    /// Installs custom content at one resolved document control path.
    #[must_use]
    pub fn with_custom<Path, Widget, Map>(mut self, path: Path, widget: Widget, map: Map) -> Self
    where
        Path: Into<String>,
        Widget: CustomWidget,
        Map: Fn(Widget::Action) -> Action + 'static,
    {
        self.custom.insert(
            path.into(),
            Box::new(MappedCustom::new(widget, move |action| {
                HostAction::new(map(action))
            })),
        );
        self
    }
}

/// Mounting one document control as a leaf.
impl<Action> MasonryHost<'_, Action>
where
    Action: std::fmt::Debug + Send + 'static,
{
    /// The state one child of a flow is hidden by, read once for the tree it
    /// is mounted into and kept for the root to read again.
    ///
    /// The child is mounted whether the document hides it or not, so the flow
    /// above it can show it again without anything being rebuilt.
    fn block(
        &self,
        binding: Option<Binding>,
        child: &mut MasonryNode<Action>,
    ) -> Option<Rc<BlockState>> {
        let binding = binding?;
        let state = Rc::new(BlockState::default());
        state.latch(self.ctx.flag(Some(&binding)));
        child.hidden_by(binding, &state);
        Some(state)
    }

    /// A leaf drawing content this toolkit does not own.
    ///
    /// The dressing is taken here rather than at paint because the leaf
    /// outlives the skin it was mounted from: this host rebuilds its tree when
    /// the skin changes, which is the same moment every other leaf takes its
    /// colours at. A widget installed at a path has no kind for the skin to
    /// dress and is dressed in nothing.
    pub(crate) fn custom_leaf(
        &self,
        widget: Box<dyn CustomWidget<Action = HostAction>>,
        kind: Option<&str>,
        declared: Size<Length>,
    ) -> MasonryNode<Action> {
        MasonryNode::document(
            NodeLayout::Leaf(Leaf::Custom {
                widget,
                skin: kind.map_or_else(CustomSkin::default, |kind| self.skin.custom(kind).clone()),
                text: Box::new(TextContext::from(self.skin.text_resources.as_ref())),
                pictures: Vec::new(),
            }),
            declared,
            Vec::new(),
            false,
            None,
            None,
        )
    }

    /// One flow's children, split into the layout each asks for and the node
    /// it is, with the state of every block among them collected on the way.
    fn flow(
        &self,
        children: Vec<GroupMount<MasonryNode<Action>>>,
    ) -> (Vec<ChildLayout>, Vec<MasonryNode<Action>>) {
        let mut layouts: Vec<ChildLayout> = Vec::with_capacity(children.len());
        let mut nodes: Vec<MasonryNode<Action>> = Vec::with_capacity(children.len());
        for mut child in children {
            let block = self.block(child.block, &mut child.output);
            layouts.push(
                ChildLayout::natural(child.output.natural(), child.minimum)
                    .within(child.band)
                    .floating(child.floats)
                    .blocked(block),
            );
            nodes.push(child.output);
        }
        (layouts, nodes)
    }

    /// One module's chrome around the content the walk already produced.
    fn mount_module(
        &self,
        module: &mut Module<'_>,
        content: Option<MasonryNode<Action>>,
    ) -> MasonryNode<Action> {
        let chrome = module.chrome();
        let frame = Some((
            module.frame(),
            self.skin.rgba(self.skin.chrome.frame.border),
            self.skin.chrome.frame.border_width,
        ));
        let panel = Some(self.skin.rgba(self.skin.chrome.panel_background));
        match chrome {
            ChromeStyle::Full if module.collapsed() => MasonryNode::chrome(
                NodeLayout::Stack,
                Size::new(Length::Fill, Length::Fixed(self.skin.chrome.header_height)),
                vec![self.module_header(module)],
                panel,
                frame,
            ),
            ChromeStyle::Full => {
                let header = self.module_header(module);
                let first_line = furniture(
                    self.skin.chrome.inner_line_width,
                    Some(self.skin.rgba(self.skin.chrome.inner_line)),
                );
                let content =
                    content.unwrap_or_else(|| MasonryNode::empty(declared(SizeSpec::FILL)));
                let second_line = furniture(
                    self.skin.chrome.inner_line_width,
                    Some(self.skin.rgba(self.skin.chrome.inner_line)),
                );
                let footer = self.module_footer(module.take_footer().unwrap_or_default());
                let children = vec![header, first_line, content, second_line, footer];
                let layouts = children
                    .iter()
                    .map(|child| ChildLayout::natural(child.natural(), None))
                    .collect();
                MasonryNode::document(
                    NodeLayout::Flex(Flex::new(
                        Axis::Vertical,
                        Length::Fill,
                        Length::Fill,
                        Padding::default(),
                        0.0,
                        Alignment::Start,
                        layouts,
                    )),
                    Size::new(Length::Fill, Length::Fill),
                    children,
                    true,
                    panel,
                    frame,
                )
            }
            ChromeStyle::Frame | ChromeStyle::Plain => {
                let child = content.unwrap_or_else(|| MasonryNode::empty(declared(SizeSpec::FILL)));
                MasonryNode::document(
                    NodeLayout::Stack,
                    Size::new(Length::Fill, Length::Fill),
                    vec![child],
                    true,
                    (chrome == ChromeStyle::Frame).then_some(panel).flatten(),
                    (chrome == ChromeStyle::Frame).then_some(frame).flatten(),
                )
            }
        }
    }

    pub(crate) fn shader_leaf(
        &self,
        spec: crate::shader::ShaderSpec,
        path: String,
        declared: Size<Length>,
    ) -> MasonryNode<Action> {
        MasonryNode::document(
            NodeLayout::Leaf(Leaf::Shader(ShaderLeaf::new(spec, path, self.ctx))),
            declared,
            Vec::new(),
            false,
            None,
            None,
        )
    }

    /// The text leaf `spec` describes, carrying `content` and the flag it is
    /// dressed by, in `skin`: the one its path wears, so an override the skin
    /// names for this text dresses it as it dresses any other control. The
    /// content is resolved by the caller, which is the only thing that can
    /// reach the reading; the flag travels so that this host can read it again
    /// into the tree it keeps.
    pub(crate) fn text_leaf(
        &self,
        spec: &mount::label::text::host::Text<'_>,
        content: &str,
        skin: &Skin,
        declared: Size<Length>,
    ) -> MasonryNode<Action> {
        let style = spec.style;
        let face = |active| {
            let role = skin
                .text_role(style, spec.color, spec.active_color, active)
                .faced(spec.font, spec.weight);
            TextFace {
                role,
                color: skin.rgba(role.color),
            }
        };
        let idle = face(false);
        let lit = spec.active.map(|flag| (flag, face(true)));
        let shown = lit
            .as_ref()
            .is_some_and(|(flag, _)| self.ctx.flag(Some(flag)));
        let TextFace { role, color } = lit
            .as_ref()
            .filter(|_| shown)
            .map_or(idle, |(_, face)| *face);
        let padding_x = match style {
            TextStyle::VisFooter => skin.vis.footer_padding_x,
            TextStyle::VisMeta => skin.vis.index_padding_x,
            TextStyle::VisTitle => skin.vis.name_padding_x,
            _ => 0.0,
        };
        let content = role.cased(content).into_owned();
        let mut output = MasonryNode::document(
            NodeLayout::Leaf(Leaf::Text {
                content,
                role,
                padding_x,
                color,
                align: spec.align,
                lit: lit.map(|(_, lit)| TextFaces { idle, lit }),
                text: Box::new(TextContext::from(skin.text_resources.as_ref())),
                room: f32::INFINITY,
            }),
            declared,
            Vec::new(),
            false,
            None,
            None,
        );
        if let Some((flag, _)) = lit {
            output.lights(flag.clone(), None);
        }
        output
    }

    pub(crate) fn vis_leaf(
        &self,
        preset: Option<String>,
        value: Option<ReadValue<'_>>,
        declared: Size<Length>,
    ) -> MasonryNode<Action> {
        MasonryNode::document(
            NodeLayout::Leaf(Leaf::Vis(VisLeaf::new(preset, value, self.ctx))),
            declared,
            Vec::new(),
            false,
            None,
            None,
        )
    }
}

/// Mounting one built-in control, and the pieces its own file reaches for.
///
/// Its own block so the leaf helpers do not drag the rest of the host over
/// the size gate.
impl<Action> MasonryHost<'_, Action>
where
    Action: std::fmt::Debug + Send + 'static,
{
    pub(crate) fn add_window_layer<Program>(
        &self,
        output: &mut MasonryNode<Action>,
        program: Program,
    ) where
        Program: crate::hosts::layer::WindowLayerProgram + 'static,
    {
        let geometry = output.geometry();
        let pointer = Rc::clone(&self.state.pointer);
        let layer = WindowLeafLayer::new(
            program,
            geometry,
            Rc::clone(&pointer),
            Rc::clone(&self.map_event),
        );
        output.add_layer(NewWidget::new(layer).erased());
        output.set_window_pointer(pointer);
    }

    fn control_action(&self, path: String, action: ControlAction) -> Box<dyn Fn() -> HostAction> {
        self.event(move || crate::hosts::event::control_event(&path, action.clone()))
    }

    pub(crate) fn event(
        &self,
        event: impl Fn() -> Published + 'static,
    ) -> Box<dyn Fn() -> HostAction> {
        let map = Rc::clone(&self.map_event);
        Box::new(move || map(event()))
    }

    /// Gives a control its own click gesture only where the document says the
    /// leaf owns input; an engine-owned control is painted and left alone.
    pub(crate) fn owned<Control>(
        &self,
        control: Control,
        owner: InputOwner,
        path: &str,
        interactive: impl FnOnce(Control, String, Rc<dyn Fn(Published) -> HostAction>) -> Control,
    ) -> Control {
        match owner {
            InputOwner::Leaf => interactive(control, path.to_owned(), Rc::clone(&self.map_event)),
            InputOwner::Engine => control,
        }
    }

    fn shared_control_action(
        &self,
        path: String,
        action: ControlAction,
    ) -> Rc<dyn Fn() -> HostAction> {
        let map = Rc::clone(&self.map_event);
        Rc::new(move || map(crate::hosts::event::control_event(&path, action.clone())))
    }

    /// Hangs a surface's layer, built from its content and the state it
    /// stands by, on the node that mounts it, with the layers, registrations,
    /// boxes, native leaves and window tracker the content carries.
    fn hang_surface(
        &self,
        output: &mut MasonryNode<Action>,
        (path, open, kind, flag): (InternId, bool, SurfaceKind, &Binding),
        content: MasonryNode<Action>,
        layer: impl FnOnce(NewWidget<Node>, Size<Length>, Rc<PopoverState>) -> NewWidget<dyn Widget>,
    ) {
        let path = self.ctx.ui.resolve(path).to_owned();
        let state = self.state.popover(&path, open, kind);
        let dismiss = self.shared_control_action(path, ControlAction::Activate);
        let (content, declared, layers, registrations, boxes, native, window) =
            LayerParts::from(content);
        let layer = layer(content, declared, Rc::clone(&state));
        let held = registrations
            .engines
            .iter()
            .map(|engine| engine.item.owner())
            .collect();
        output.add_popover(layer.id(), flag, state, dismiss, held);
        output.append_layers(layers);
        output.append_registrations(registrations);
        output.append_boxes(boxes);
        output.append_native(native);
        if let Some(window) = window {
            output.set_window_tracker(window);
        }
        output.add_layer(layer);
    }
}

impl<Action> Host for MasonryHost<'_, Action>
where
    Action: std::fmt::Debug + Send + 'static,
{
    type Output = MasonryNode<Action>;

    /// A retained tree mounts a hidden block and hides it in the flow above
    /// it, because a block left out of the tree could never come back without
    /// the tree being rebuilt around it.
    const MOUNTS_HIDDEN: bool = true;

    fn control(
        &mut self,
        path: InternId,
        spec: &ControlSpec,
        read: Option<&Binding>,
        owner: InputOwner,
        size: Option<SizeSpec>,
        transform: Transform,
    ) -> Self::Output {
        let declared = control_declared(spec, size, self.skin);
        let plan = hosted_control_plan(path, spec, read, self.ctx, self.skin);
        let path_id = path;
        let path = self.ctx.ui.resolve(path);
        let owns_pointer = pointer_owner(owner, spec);
        let custom = self.custom.remove(path);
        let custom_installed = custom.is_some();
        let cx = Cx {
            declared,
            owner: owns_pointer,
            path,
            plan: plan.as_ref(),
            read,
            skin: self.skin.at(path),
        };
        let mut output = custom.map_or_else(
            || {
                mount::controls!(
                    host: spec,
                    Mount {
                        cx: &cx,
                        host: &*self
                    }
                )
            },
            |widget| self.custom_leaf(widget, None, declared),
        );
        output.place(transform);
        if self.ctx.ui.driven {
            output.watch_placement(path_id);
        }
        self.state.tag_path(path, output.widget_id());
        if custom_installed {
            return output;
        }
        if matches!(
            spec,
            ControlSpec::Vis
                | ControlSpec::Search
                | ControlSpec::Table { .. }
                | ControlSpec::Tree { .. }
        ) {
            output.watch_snapshot();
        } else if let Some(read) = read {
            output.watch(read);
        }
        if owns_pointer == InputOwner::Leaf {
            return output;
        }
        output = mount::controls!(
            host: spec,
            Wire {
                host: &*self,
                cx: &cx,
                output
            }
        );
        if let Some(plan) = plan {
            output.add_engine_control(plan, false);
            if owner == InputOwner::Leaf {
                output.host_engine(Rc::clone(&self.map_event), self.skin);
            }
        } else if activates(spec) {
            output.set_actions(
                Some(self.control_action(path.to_owned(), ControlAction::Activate)),
                None,
            );
        }
        output
    }

    fn group(&mut self, group: Group<'_>, children: Vec<GroupMount<Self::Output>>) -> Self::Output {
        let size = group.size().unwrap_or(SizeSpec::FILL);
        let (layouts, nodes) = self.flow(children);
        let alpha = group.background_alpha().unwrap_or(1.0);
        let face = |background: Option<ColorRole>, frame_color: ColorRole| Face {
            background: background.map(|role| {
                let mut color = self.skin.rgba(role);
                color.a = alpha;
                color
            }),
            frame: group
                .frame()
                .map(|sides| (sides, self.skin.rgba(frame_color), group.frame_width())),
        };
        let idle = face(group.background(), group.frame_color());
        let lit = group
            .lit()
            .map(|lit| (lit.flag(), face(lit.background(), lit.frame_color())));
        let shown = lit
            .as_ref()
            .is_some_and(|(flag, _)| self.ctx.flag(Some(flag)));
        let Face { background, frame } = lit
            .as_ref()
            .filter(|_| shown)
            .map_or(idle, |(_, face)| *face);
        let mut output = MasonryNode::document(
            NodeLayout::Flex(
                Flex::new(
                    group.axis(),
                    solve::length(size.w),
                    solve::length(size.h),
                    Padding {
                        top: group.padding_y(),
                        right: group.padding_x(),
                        bottom: group.padding_y(),
                        left: group.padding_x(),
                    },
                    group.gap(),
                    alignment(group.alignment()),
                    layouts,
                )
                .measure(group.measure()),
            ),
            declared(size),
            nodes,
            true,
            background,
            frame,
        )
        .rounded(group.round(), self.skin.chrome.frame.radius);
        if let Some(surface) = group.surface() {
            output.set_detent(Detent::new(
                self.ctx.ui.resolve(surface.path).to_owned(),
                Rc::clone(&self.map_event),
                surface
                    .entry
                    .as_ref()
                    .map(|entry| (entry_value(self.ctx, entry), self.skin.clone())),
            ));
            if let Some(entry) = &surface.entry {
                output.watch(entry);
            }
        }
        if let Some((flag, lit)) = lit {
            output.lights(flag.clone(), Some(Faces { idle, lit }));
        }
        output
    }

    fn hosted(&mut self, _node: &ExpandedNode, mut child: Self::Output) -> Self::Output {
        child.host_engine(Rc::clone(&self.map_event), self.skin);
        child
    }

    fn measured(&mut self, plan: Measured, branches: Vec<Self::Output>) -> Self::Output {
        let size = declared(plan.size);
        MasonryNode::document(NodeLayout::Measured(plan), size, branches, true, None, None)
    }

    fn module(&mut self, mut module: Module<'_>, content: Option<Self::Output>) -> Self::Output {
        let mut output = self
            .mount_module(&mut module, content)
            .rounded(module.round(), self.skin.chrome.frame.radius);
        if module.takes_drops() {
            let instance = self.ctx.ui.resolve(module.instance());
            let crossing = HostedControlPlan::crossing(instance);
            let zone = crossing.path().to_owned();
            output.takes_drops(
                zone,
                (
                    FrameSides::default(),
                    self.skin.rgba(self.skin.chrome.drop_zone_color),
                    self.skin.chrome.frame.border_width,
                ),
                Rc::clone(&self.state.pointer),
            );
            output.add_engine_control(crossing, true);
            output.host_engine(Rc::clone(&self.map_event), self.skin);
        }
        output
    }

    /// One placement of a stage: the child keeps the box it asked for, and the
    /// stage lays it out at the point the placement carries. A placement with
    /// somewhere to write also carries the grip that moves that point, and the
    /// endpoint it is re-read from between mounts.
    fn placed(&mut self, placement: PlacedMount<'_>, child: Self::Output) -> Self::Output {
        let declared = child.declared();
        let mut output =
            MasonryNode::document(NodeLayout::Stack, declared, vec![child], false, None, None);
        let grip = placement.write.map(|_| {
            Grip::new(
                self.ctx.ui.resolve(placement.path).to_owned(),
                placement.snap.clone(),
                Rc::clone(&self.map_event),
            )
        });
        output.set_spot(Spot::new(placement.at, grip));
        if let Some(read) = placement.read {
            output.watch_spot(read);
        }
        output
    }

    fn popover(
        &mut self,
        popover: Popover<'_>,
        anchor: Self::Output,
        content: &mut dyn FnMut(&mut Self) -> Self::Output,
    ) -> Self::Output {
        let content = content(self);
        let size = popover.size().map_or_else(|| anchor.declared(), declared);
        let mut output =
            MasonryNode::document(NodeLayout::Stack, size, vec![anchor], false, None, None);
        let surface = (
            popover.path(),
            popover.is_open(),
            SurfaceKind::popover(),
            popover.flag(),
        );
        let (at, align, skin) = (popover.at(), popover.align(), self.skin);
        self.hang_surface(&mut output, surface, content, |content, declared, state| {
            NewWidget::new(PopoverLayer::new(content, declared, state, at, align, skin)).erased()
        });
        let path = self.ctx.ui.resolve(popover.path()).to_owned();
        output.set_actions(
            Some(self.control_action(path, ControlAction::Activate)),
            None,
        );
        output
    }

    fn modal(
        &mut self,
        modal: Modal<'_>,
        content: &mut dyn FnMut(&mut Self) -> Self::Output,
    ) -> Self::Output {
        let content = content(self);
        let nothing = Size::new(Length::Fixed(0.0), Length::Fixed(0.0));
        let mut output =
            MasonryNode::document(NodeLayout::Stack, nothing, Vec::new(), false, None, None);
        let surface = (
            modal.path(),
            modal.is_open(),
            SurfaceKind::Modal,
            modal.flag(),
        );
        self.hang_surface(&mut output, surface, content, |content, declared, state| {
            NewWidget::new(ModalLayer::new(content, declared, state, self.skin)).erased()
        });
        output
    }

    fn pressable(
        &mut self,
        path: InternId,
        child: Self::Output,
        size: Option<SizeSpec>,
    ) -> Self::Output {
        let declared = size.map_or_else(|| child.declared(), declared);
        let path = self.ctx.ui.resolve(path);
        let mut output =
            MasonryNode::document(NodeLayout::Stack, declared, vec![child], false, None, None);
        output.add_engine_control(
            HostedControlPlan::Activation {
                path: path.to_owned(),
            },
            true,
        );
        output.host_engine(Rc::clone(&self.map_event), self.skin);
        output.set_actions(
            None,
            Some(self.control_action(path.to_owned(), ControlAction::SecondaryActivate)),
        );
        output
    }

    /// The window is the declared box and the child keeps its own height, so
    /// the retained viewport has something to travel over.
    fn scroll(
        &mut self,
        id: InternId,
        child: Self::Output,
        size: Option<SizeSpec>,
    ) -> Self::Output {
        let declared = size.map_or_else(|| child.declared(), declared);
        let view = self.state.window(self.ctx.ui.resolve(id));
        MasonryNode::document(
            NodeLayout::Scroll(Viewport::new(Bar::new(self.skin), view)),
            declared,
            vec![child],
            false,
            None,
            None,
        )
    }

    fn slot(
        &mut self,
        children: Vec<GroupMount<Self::Output>>,
        size: Option<SizeSpec>,
    ) -> Self::Output {
        let declared = size.map_or(Size::new(Length::Fill, Length::Shrink), declared);
        let (layouts, nodes) = self.flow(children);
        MasonryNode::document(
            NodeLayout::Flex(
                Flex::new(
                    Axis::Vertical,
                    Length::Fill,
                    declared.height,
                    Padding::default(),
                    self.skin.layout.grid_gap,
                    Alignment::Start,
                    layouts,
                )
                .align_main(Alignment::Center),
            ),
            declared,
            nodes,
            true,
            None,
            None,
        )
    }

    fn split(
        &mut self,
        axis: Axis,
        measure: Option<MeasureAxis>,
        children: Vec<SplitMount<Self::Output>>,
    ) -> Self::Output {
        let mut layouts: Vec<ChildLayout> = Vec::with_capacity(children.len());
        let mut nodes: Vec<MasonryNode<Action>> = Vec::with_capacity(children.len());
        for mut cell in children {
            let block = self.block(cell.block, &mut cell.output);
            layouts.push(
                ChildLayout::cell(axis, cell.size, cell.weight)
                    .within(cell.band)
                    .blocked(block),
            );
            nodes.push(cell.output);
        }
        MasonryNode::document(
            NodeLayout::Flex(Flex::split(axis, layouts).measure(measure)),
            Size::new(Length::Fill, Length::Fill),
            nodes,
            true,
            None,
            None,
        )
    }

    fn stage(
        &mut self,
        children: Vec<StageMount<Self::Output>>,
        size: Option<SizeSpec>,
    ) -> Self::Output {
        let children = children
            .into_iter()
            .map(|mut child| {
                let block = self.block(child.block, &mut child.output);
                (block, child.floats, child.output)
            })
            .collect();
        MasonryNode::stage(size, children)
    }

    fn window(&mut self, mut content: Self::Output, resize_edges: bool) -> Self::Output {
        let drops = content.drops();
        if !resize_edges && !drops {
            return content;
        }
        let ghost = drops.then(|| DragGhost::new(None, self.skin));
        let pointer = Rc::clone(&self.state.pointer);
        let layer = WindowLayer::new(
            ghost,
            resize_edges,
            Rc::clone(&pointer),
            Rc::clone(&self.map_event),
            self.skin,
        );
        let layer = NewWidget::new(layer);
        let layer_id = layer.id();
        content.add_layer(layer.erased());
        content.set_window_layer(pointer, layer_id, drops);
        content
    }
}

fn furniture<Action>(height: f32, background: Option<Rgba>) -> MasonryNode<Action> {
    MasonryNode::furniture(
        NodeLayout::Leaf(Leaf::Empty),
        Size::new(Length::Fill, Length::Fixed(height)),
        background,
    )
}

/// Asks whichever control the document named to mount itself as a leaf.
struct Mount<'cx, 'host, 'a, Action> {
    cx: &'cx Cx<'cx>,
    host: &'host MasonryHost<'a, Action>,
}

impl<Action> Mount<'_, '_, '_, Action>
where
    Action: std::fmt::Debug + Send + 'static,
{
    fn apply<C: NodeControl>(self, control: &C) -> MasonryNode<Action> {
        control.leaf(self.host, self.cx)
    }

    fn painted<C>(self, control: &C) -> MasonryNode<Action>
    where
        C: Draws,
        C::Painter: Retained + 'static,
    {
        painted(control, self.host, self.cx)
    }
}

/// Asks whichever control the document named to attach whatever it still needs
/// beyond its own leaf, and hands the mounted node back.
struct Wire<'host, 'cx, 'a, Action> {
    cx: &'cx Cx<'cx>,
    host: &'host MasonryHost<'a, Action>,
    output: MasonryNode<Action>,
}

impl<Action> Wire<'_, '_, '_, Action>
where
    Action: std::fmt::Debug + Send + 'static,
{
    fn apply<C: NodeControl>(mut self, control: &C) -> MasonryNode<Action> {
        control.wire(self.host, self.cx, &mut self.output);
        self.output
    }

    /// A control that draws itself attaches nothing beyond its leaf.
    fn painted<C>(self, _control: &C) -> MasonryNode<Action> {
        self.output
    }
}
