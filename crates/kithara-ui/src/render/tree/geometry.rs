use iced::{
    Background, Border, Color, Element, Length, Padding, Rectangle,
    advanced::{layout::Layout, mouse},
    alignment::{Horizontal, Vertical},
    widget::{Container, container, container::Style as ContainerStyle},
};

use super::{
    control::{HostedControl, append_control_descriptors, append_control_targets},
    host::ModuleHost,
};
#[cfg(test)]
use crate::render::skin::active_tone;
use crate::{
    engine::{Descriptor, Engine, PickerSnapshot, Target},
    expand::ExpandedNode,
    interact::iced as iced_interact,
    layout::{FrameCorners, FrameSides},
    module::ChromeStyle,
    render::{
        HostedControlPlan, IcedSkin, Resolving, Skin, UiEvent, corner_radius, document::Ctx,
        frame_overlay, picker_hits,
    },
    size::{Dim, SizeSpec, Snapshot, branch, is_hidden},
    skin::ColorRole,
};

pub(super) struct Rendered<'a> {
    element: Element<'a, UiEvent>,
    align: Horizontal,
}

impl<'a> Rendered<'a> {
    pub(super) const fn new(element: Element<'a, UiEvent>, align: Horizontal) -> Self {
        Self { element, align }
    }

    pub(super) const fn leading(element: Element<'a, UiEvent>) -> Self {
        Self::new(element, Horizontal::Left)
    }
}

pub(super) fn padding(horizontal: f32, vertical: f32) -> Padding {
    Padding::ZERO
        .top(vertical)
        .bottom(vertical)
        .left(horizontal)
        .right(horizontal)
}

pub(super) fn filled<'a>(
    element: Container<'a, UiEvent>,
    background: Option<ColorRole>,
    alpha: Option<f32>,
    round: FrameCorners,
    skin: &Skin,
) -> Element<'a, UiEvent> {
    let Some(role) = background else {
        return element.into();
    };
    let color = Color {
        a: alpha.unwrap_or(1.0),
        ..skin.color(role)
    };
    let radius = corner_radius(round, skin);
    element
        .style(move |_| {
            ContainerStyle::default()
                .background(Background::Color(color))
                .border(Border::default().rounded(radius))
        })
        .into()
}

pub(super) fn bordered<'a>(
    element: Element<'a, UiEvent>,
    frame: Option<FrameSides>,
    tone: (ColorRole, f32),
    size: (Length, Length),
    skin: &Skin,
) -> Element<'a, UiEvent> {
    let (role, width) = tone;
    match frame {
        Some(sides) => frame_overlay(element, sides, size, skin.color(role), width),
        None => element,
    }
}

pub(super) fn effective_size(
    node: &ExpandedNode,
    skin: &Skin,
    snapshot: &dyn Snapshot,
) -> Option<SizeSpec> {
    crate::size::effective_size(node, skin.document(), snapshot)
}

pub(super) fn apply_size<'a>(
    rendered: Rendered<'a>,
    size: Option<SizeSpec>,
) -> Element<'a, UiEvent> {
    let Rendered { element, align } = rendered;
    let Some(size) = size else {
        return element;
    };
    let intrinsic = element.as_widget().size_hint();
    container(element)
        .width(length_for(size.w, intrinsic.width))
        .height(length_for(size.h, intrinsic.height))
        .align_x(align)
        .align_y(Vertical::Center)
        .into()
}

pub(super) const fn length_for(dim: Dim, intrinsic: Length) -> Length {
    match dim {
        Dim::Fixed(value) => Length::Fixed(value),
        Dim::Shrink => Length::Shrink,
        Dim::Range { .. } => match intrinsic {
            Length::FillPortion(_) => intrinsic,
            _ => Length::Fill,
        },
        _ => Length::Fill,
    }
}

pub(super) enum HostedLayout {
    Chrome {
        /// What the module's `drop:` mounts, when it declares one.
        drop: Option<HostedControlPlan>,
        header: Option<(String, String)>,
        collapsed: bool,
    },
    Group {
        sized: bool,
        surfaced: bool,
        framed: bool,
        children: Vec<Self>,
    },
    Slot {
        sized: bool,
        children: Vec<Self>,
    },
    /// A stack takes its declared box directly, with no container around it,
    /// so its children sit exactly one level down whether it was sized or not.
    Stage {
        children: Vec<Self>,
    },
    /// Branches that all keep a layout node, of which one is drawn. The rest
    /// keep an empty one, so a branch holds its place whether it stands or not.
    Measured {
        children: Vec<Self>,
    },
    Wrapper {
        sized: bool,
        child: Box<Self>,
    },
    /// A viewport, which like a stack takes its declared box directly: the
    /// `scrollable` itself is the window, and it keeps a layout node of its own
    /// for the content it offsets, so that content sits exactly one level down.
    Scroll {
        child: Box<Self>,
    },
    Control(Option<HostedControl>),
    SelfMeasuredControl(Option<HostedControl>),
}

impl HostedLayout {
    pub(super) fn new(node: &ExpandedNode, ctx: Ctx<'_, '_>, skin: &Skin) -> Self {
        let snapshot: &dyn Snapshot = &ctx;
        match node {
            ExpandedNode::Row {
                size,
                surface,
                frame,
                children,
                ..
            }
            | ExpandedNode::Column {
                size,
                surface,
                frame,
                children,
                ..
            } => Self::Group {
                sized: size.is_some(),
                surfaced: surface.is_some(),
                framed: frame.is_some(),
                children: children
                    .iter()
                    .filter(|child| !is_hidden(*child, snapshot))
                    .map(|child| Self::new(child, ctx, skin))
                    .collect(),
            },
            ExpandedNode::Object { child, .. }
            | ExpandedNode::Optional { child, .. }
            | ExpandedNode::Reveal { child, .. } => Self::new(child, ctx, skin),
            ExpandedNode::Adaptive {
                measure,
                base,
                steps,
                ..
            } => match measure.axis() {
                Some(_) => Self::Measured {
                    children: std::iter::once(base.as_ref())
                        .chain(steps.iter().map(|(_, node)| node))
                        .map(|node| Self::new(node, ctx, skin))
                        .collect(),
                },
                None => Self::new(branch(measure, base, steps, snapshot), ctx, skin),
            },
            ExpandedNode::Slot { size, children, .. } => Self::Slot {
                sized: size.is_some(),
                children: children
                    .iter()
                    .filter(|child| !is_hidden(*child, snapshot))
                    .map(|child| Self::new(child, ctx, skin))
                    .collect(),
            },
            ExpandedNode::Stage { children, .. } => Self::Stage {
                children: children
                    .iter()
                    .filter(|child| !is_hidden(*child, snapshot))
                    .map(|child| Self::new(child, ctx, skin))
                    .collect(),
            },
            ExpandedNode::Control {
                path, spec, read, ..
            } => {
                let control = HostedControl::new(
                    ctx.ui.resolve(*path),
                    spec,
                    read.as_ref().and_then(|binding| ctx.read(binding)),
                    read.as_ref(),
                    ctx.scope(read.as_ref()),
                    Resolving { skin, ctx },
                );
                if effective_size(node, skin, snapshot).is_none() {
                    Self::SelfMeasuredControl(control)
                } else {
                    Self::Control(control)
                }
            }
            ExpandedNode::Popover { anchor, .. } => Self::Wrapper {
                sized: effective_size(node, skin, snapshot).is_some(),
                child: Box::new(Self::new(anchor, ctx, skin)),
            },
            ExpandedNode::Pressable { child, .. } => Self::Wrapper {
                sized: effective_size(node, skin, snapshot).is_some(),
                child: Box::new(Self::new(child, ctx, skin)),
            },
            ExpandedNode::Placed { child, .. } | ExpandedNode::Scroll { child, .. } => {
                Self::Scroll {
                    child: Box::new(Self::new(child, ctx, skin)),
                }
            }
        }
    }

    fn append_descriptors(&self, descriptors: &mut Vec<Descriptor>) {
        match self {
            Self::Chrome { drop, header, .. } => {
                if let Some(plan) = drop {
                    descriptors.append(&mut plan.descriptors());
                }
                if let Some((path, _)) = header {
                    descriptors.push(Descriptor::activation(path.clone()));
                }
            }
            Self::Group { children, .. }
            | Self::Measured { children }
            | Self::Slot { children, .. }
            | Self::Stage { children, .. } => {
                for child in children {
                    child.append_descriptors(descriptors);
                }
            }
            Self::Scroll { child, .. } | Self::Wrapper { child, .. } => {
                child.append_descriptors(descriptors);
            }
            Self::Control(Some(control)) | Self::SelfMeasuredControl(Some(control)) => {
                append_control_descriptors(control, descriptors);
            }
            Self::Control(None) | Self::SelfMeasuredControl(None) => {}
        }
    }

    fn append_open_picker_targets<'a>(
        &'a self,
        engine: &Engine,
        cursor: mouse::Cursor,
        targets: &mut Vec<Target<'a>>,
    ) {
        for (path, item_count, item_height) in self.pickers() {
            if !engine
                .picker_snapshot(path)
                .is_some_and(|snapshot| snapshot.open)
            {
                continue;
            }
            let Some(position) = targets
                .iter()
                .position(|target| target.path == path && target.index.is_none())
            else {
                continue;
            };
            let anchor = targets.remove(position);
            let area = anchor.hit.area();
            targets.push(anchor);
            for region in picker_hits(area, item_height, item_count) {
                let bounds = region.area();
                targets.push(Target::item(
                    path,
                    iced_interact::hit(
                        Rectangle {
                            x: bounds.x,
                            y: bounds.y,
                            width: bounds.w,
                            height: bounds.h,
                        },
                        cursor,
                    ),
                    *region.action(),
                ));
            }
        }
    }

    fn append_pickers<'a>(&'a self, pickers: &mut Vec<(&'a str, usize, f32)>) {
        match self {
            Self::Group { children, .. }
            | Self::Measured { children }
            | Self::Slot { children, .. }
            | Self::Stage { children, .. } => {
                for child in children {
                    child.append_pickers(pickers);
                }
            }
            Self::Scroll { child, .. } | Self::Wrapper { child, .. } => {
                child.append_pickers(pickers);
            }
            Self::Control(Some(control)) | Self::SelfMeasuredControl(Some(control)) => {
                if let Some(picker) = control.picker() {
                    pickers.push(picker);
                }
            }
            Self::Chrome { .. } | Self::Control(None) | Self::SelfMeasuredControl(None) => {}
        }
    }

    fn append_targets<'a>(
        &'a self,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        engine: Option<&Engine>,
        targets: &mut Vec<Target<'a>>,
    ) {
        match self {
            Self::Chrome {
                drop,
                header,
                collapsed,
            } => {
                let shell = if let Some(plan) = drop {
                    targets.push(Target::new(
                        plan.path(),
                        iced_interact::hit(layout.bounds(), cursor),
                    ));
                    let Some(shell) = first_child(layout) else {
                        return;
                    };
                    shell
                } else {
                    layout
                };
                let Some((path, _)) = header else {
                    return;
                };
                let Some(body) = first_child(shell) else {
                    return;
                };
                let Some(content) = first_child(body) else {
                    return;
                };
                let header = if *collapsed {
                    content
                } else {
                    let Some(header) = first_child(content) else {
                        return;
                    };
                    header
                };
                targets.push(Target::new(
                    path,
                    iced_interact::hit(header.bounds(), cursor),
                ));
            }
            Self::Group {
                sized,
                surfaced,
                framed,
                children,
            } => {
                let Some(layout) = group_children(layout, *sized, *surfaced, *framed) else {
                    return;
                };
                for (child, layout) in children.iter().zip(layout.children()) {
                    child.append_targets(layout, cursor, engine, targets);
                }
            }
            Self::Slot { sized, children } => {
                let Some(layout) = slot_children(layout, *sized) else {
                    return;
                };
                for (child, layout) in children.iter().zip(layout.children()) {
                    child.append_targets(layout, cursor, engine, targets);
                }
            }
            Self::Measured { children } | Self::Stage { children } => {
                for (child, layout) in children.iter().zip(layout.children()) {
                    child.append_targets(layout, cursor, engine, targets);
                }
            }
            Self::Wrapper { sized, child } => {
                let layout = if *sized {
                    let Some(layout) = first_child(layout) else {
                        return;
                    };
                    layout
                } else {
                    layout
                };
                child.append_targets(layout, cursor, engine, targets);
            }
            Self::Scroll { child } => {
                let cursor = if engine.is_some_and(Engine::captures_pointer) {
                    cursor
                } else {
                    clipped(cursor, layout.bounds())
                };
                let Some(layout) = first_child(layout) else {
                    return;
                };
                child.append_targets(layout, cursor, engine, targets);
            }
            Self::Control(Some(control)) => {
                let Some(layout) = first_child(layout) else {
                    return;
                };
                append_control_targets(control, layout, cursor, engine, targets);
            }
            Self::SelfMeasuredControl(Some(control)) => {
                append_control_targets(control, layout, cursor, engine, targets);
            }
            Self::Control(None) | Self::SelfMeasuredControl(None) => {}
        }
    }

    pub(super) fn descriptors(&self) -> Vec<Descriptor> {
        let mut descriptors = Vec::new();
        self.append_descriptors(&mut descriptors);
        descriptors
    }

    pub(super) fn header_module<'a>(&'a self, path: &str) -> Option<&'a str> {
        match self {
            Self::Chrome {
                header: Some((header, module)),
                ..
            } if header == path => Some(module),
            Self::Chrome { .. }
            | Self::Group { .. }
            | Self::Measured { .. }
            | Self::Scroll { .. }
            | Self::Slot { .. }
            | Self::Stage { .. }
            | Self::Wrapper { .. }
            | Self::Control(_)
            | Self::SelfMeasuredControl(_) => None,
        }
    }

    pub(super) fn module(spec: ModuleHost<'_>) -> Self {
        let ModuleHost {
            instance,
            module,
            chrome,
            collapsed,
            drop,
        } = spec;
        Self::Chrome {
            collapsed,
            drop: drop.then(|| HostedControlPlan::crossing(instance)),
            header: (chrome == ChromeStyle::Full)
                .then(|| (format!("{instance}/header"), module.to_owned())),
        }
    }

    pub(super) fn picker_snapshots<'a>(
        &'a self,
        engine: &Engine,
    ) -> Vec<(&'a str, PickerSnapshot)> {
        self.pickers()
            .into_iter()
            .filter_map(|(path, _, _)| {
                engine
                    .picker_snapshot(path)
                    .map(|snapshot| (path, snapshot))
            })
            .collect()
    }

    pub(super) fn pickers(&self) -> Vec<(&str, usize, f32)> {
        let mut pickers = Vec::new();
        self.append_pickers(&mut pickers);
        pickers
    }

    #[cfg(test)]
    pub(super) fn targets<'a>(
        &'a self,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
    ) -> Vec<Target<'a>> {
        self.targets_with_engine(layout, cursor, None)
    }

    pub(super) fn targets_with_engine<'a>(
        &'a self,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        engine: Option<&Engine>,
    ) -> Vec<Target<'a>> {
        let mut targets = Vec::new();
        self.append_targets(layout, cursor, engine, &mut targets);
        if let Some(engine) = engine {
            self.append_open_picker_targets(engine, cursor, &mut targets);
        }
        targets
    }
}
pub(super) fn tree_input_layout(layout: Layout<'_>) -> Option<Layout<'_>> {
    let panel = layout.children().nth(1)?;
    first_child(panel)
}

pub(super) fn tree_search_input_layout(layout: Layout<'_>) -> Option<Layout<'_>> {
    let search = layout.children().next()?;
    let row = first_child(search)?;
    row.children().nth(1)
}

pub(super) fn group_children(
    mut layout: Layout<'_>,
    sized: bool,
    surfaced: bool,
    framed: bool,
) -> Option<Layout<'_>> {
    if sized {
        layout = first_child(layout)?;
    }
    if surfaced {
        layout = first_child(layout)?;
    }
    if framed {
        layout = first_child(first_child(layout)?)?;
    }
    first_child(layout)
}

pub(super) fn slot_children(mut layout: Layout<'_>, sized: bool) -> Option<Layout<'_>> {
    if sized {
        layout = first_child(layout)?;
    }
    first_child(layout)
}

pub(super) fn first_child(layout: Layout<'_>) -> Option<Layout<'_>> {
    layout.children().next()
}

/// The pointer as the children of one box see it: itself while it is inside
/// that box, and nothing at all while it is outside. A child laid out past the
/// edge of a viewport is not under a pointer that never entered it, whatever
/// the child's own layout still says its box is.
pub(super) fn clipped(cursor: mouse::Cursor, bounds: Rectangle) -> mouse::Cursor {
    match cursor.position() {
        Some(point) if bounds.contains(point) => cursor,
        Some(_) | None => mouse::Cursor::Unavailable,
    }
}
#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use iced::{Size, widget::Space};
    use kithara_test_utils::kithara;

    use super::*;
    use crate::{
        builtin,
        expand::{Binding, BindingKind, BlockSpec, ControlSpec, MeasureSpec},
        ids::{InternId, Interner, SourceUri},
        module::{PopoverAlign, PopoverAt},
        size::{DEFAULTS, Snapshot},
    };

    #[kithara::test]
    fn fixed_size_spec_sets_both_element_axes() {
        let element: Element<'static, UiEvent> = Space::new().into();
        let element = apply_size(
            Rendered::leading(element),
            Some(SizeSpec::new(Dim::Fixed(34.0), Dim::Fixed(6.0))),
        );

        assert_eq!(
            element.as_widget().size(),
            Size::new(Length::Fixed(34.0), Length::Fixed(6.0))
        );
    }

    #[kithara::test]
    fn shrink_size_spec_reaches_the_toolkit() {
        let element: Element<'static, UiEvent> =
            Space::new().width(Length::Fill).height(Length::Fill).into();
        let element = apply_size(
            Rendered::leading(element),
            Some(SizeSpec::new(Dim::Shrink, Dim::Fill)),
        );

        assert_eq!(
            element.as_widget().size(),
            Size::new(Length::Shrink, Length::Fill)
        );
    }

    fn timed(size: Option<SizeSpec>) -> (Skin, ExpandedNode) {
        let origin = SourceUri("tree-test.ron".to_owned());
        let skin = Skin::resolve(
            builtin::skin_doc().clone(),
            builtin::text_doc(),
            &origin,
            &builtin::resolver(),
        )
        .unwrap();
        let mut interner = Interner::new(1024);
        let id = interner.intern("cell", &origin).unwrap();
        let node = ExpandedNode::Control {
            path: id,
            id,
            spec: ControlSpec::Time,
            size,
            read: None,
            write: None,
        };
        (skin, node)
    }

    #[kithara::test]
    fn a_declared_box_reaches_both_axes_unchanged() {
        let declared = SizeSpec::new(Dim::Fixed(40.0), Dim::Shrink);
        let (skin, node) = timed(Some(declared));

        assert_eq!(effective_size(&node, &skin, DEFAULTS), Some(declared));
    }

    #[kithara::test]
    fn a_control_declaring_no_box_takes_the_one_its_skin_names() {
        let (skin, node) = timed(None);

        assert_eq!(
            effective_size(&node, &skin, DEFAULTS),
            Some(skin.document().deck.time_size)
        );
    }

    fn control(
        interner: &mut Interner,
        origin: &SourceUri,
        name: &str,
        size: SizeSpec,
    ) -> ExpandedNode {
        let id = interner.intern(name, origin).unwrap();
        ExpandedNode::Control {
            path: id,
            id,
            spec: ControlSpec::Time,
            size: Some(size),
            read: None,
            write: None,
        }
    }

    struct Measured(Option<f32>);

    impl Snapshot for Measured {
        fn hidden(&self, _: &BlockSpec) -> bool {
            false
        }

        fn measure(&self, _: &Binding) -> Option<f32> {
            self.0
        }
    }

    #[kithara::test]
    fn a_wrapper_over_an_adaptive_node_measures_the_selected_branch() {
        let origin = SourceUri("tree-test.ron".to_owned());
        let skin = Skin::resolve(
            builtin::skin_doc().clone(),
            builtin::text_doc(),
            &origin,
            &builtin::resolver(),
        )
        .unwrap();
        let mut interner = Interner::new(1024);
        let narrow = SizeSpec::new(Dim::Fixed(34.0), Dim::Fixed(45.0));
        let wide = SizeSpec::new(Dim::Fixed(68.0), Dim::Fixed(45.0));
        let pressable = ExpandedNode::Pressable {
            path: interner.intern("bank", &origin).unwrap(),
            press: model(interner.intern("deck.eq.menu", &origin).unwrap()),
            child: Box::new(ExpandedNode::Adaptive {
                measure: MeasureSpec::Read(model(
                    interner.intern("deck.eq.bands", &origin).unwrap(),
                )),
                size: None,
                base: Box::new(control(&mut interner, &origin, "three", narrow)),
                steps: vec![(4.0, control(&mut interner, &origin, "four", wide))],
            }),
        };

        assert_eq!(
            effective_size(&pressable, &skin, &Measured(Some(4.0))),
            Some(wide),
            "a press target takes the size of the branch that is drawn"
        );
        assert_eq!(
            effective_size(&pressable, &skin, &Measured(None)),
            Some(narrow),
            "nothing read leaves the base branch"
        );
    }

    fn model(id: InternId) -> Binding {
        Binding {
            kind: BindingKind::Model,
            id,
            key: id,
            with: BTreeMap::new(),
        }
    }

    #[kithara::test]
    fn a_popover_measures_its_anchor_and_a_pressable_its_child() {
        let origin = SourceUri("tree-test.ron".to_owned());
        let skin = Skin::resolve(
            builtin::skin_doc().clone(),
            builtin::text_doc(),
            &origin,
            &builtin::resolver(),
        )
        .unwrap();
        let mut interner = Interner::new(1024);
        let anchor = SizeSpec::new(Dim::Fixed(36.0), Dim::Fixed(36.0));
        let content = SizeSpec::new(Dim::Fixed(298.0), Dim::Fixed(400.0));
        let popover = ExpandedNode::Popover {
            path: interner.intern("menu", &origin).unwrap(),
            open: model(interner.intern("ui.menu.open", &origin).unwrap()),
            at: PopoverAt::Anchor,
            align: PopoverAlign::Start,
            anchor: Box::new(control(&mut interner, &origin, "burger", anchor)),
            content: Box::new(control(&mut interner, &origin, "pop", content)),
        };
        let pressable = ExpandedNode::Pressable {
            path: interner.intern("row", &origin).unwrap(),
            press: model(interner.intern("ui.menu.toggle", &origin).unwrap()),
            child: Box::new(control(&mut interner, &origin, "cell", content)),
        };

        assert_eq!(
            effective_size(&popover, &skin, DEFAULTS),
            Some(anchor),
            "the content is laid out in the overlay and never in flow"
        );
        assert_eq!(effective_size(&pressable, &skin, DEFAULTS), Some(content));
    }

    #[kithara::test]
    fn active_tone_takes_the_active_role_only_while_the_flag_is_set() {
        let pair =
            |active| active_tone(Some(ColorRole::LineInner), Some(ColorRole::Accent), active);

        assert_eq!(pair(true), Some(ColorRole::Accent));
        assert_eq!(pair(false), Some(ColorRole::LineInner));
        assert_eq!(
            active_tone(Some(ColorRole::LineHi), None, true),
            Some(ColorRole::LineHi)
        );
        assert_eq!(active_tone(None, None, true), None);
    }

    /// A corner the layout says is the window's takes the radius the skin gives
    /// the window.
    #[kithara::test]
    fn a_window_corner_takes_the_skin_radius() {
        let skin = builtin::skin();

        let radius = corner_radius(
            FrameCorners {
                top_left: true,
                ..FrameCorners::EMPTY
            },
            skin,
        );

        assert_eq!(radius.top_left, skin.chrome.frame.radius);
    }

    /// Every other corner of the same box stays square, however far the skin
    /// rounds the window.
    #[kithara::test]
    fn a_corner_the_layout_leaves_out_stays_square() {
        let radius = corner_radius(
            FrameCorners {
                top_left: true,
                ..FrameCorners::EMPTY
            },
            builtin::skin(),
        );

        assert_eq!(radius.top_right, 0.0);
    }

    #[kithara::test]
    fn range_preserves_widget_fill_portion() {
        let element: Element<'static, UiEvent> = Space::new()
            .width(Length::FillPortion(2))
            .height(Length::Fill)
            .into();
        let element = apply_size(
            Rendered::leading(element),
            Some(SizeSpec::new(
                Dim::Range {
                    min: 20.0,
                    max: None,
                },
                Dim::Fill,
            )),
        );

        assert_eq!(
            element.as_widget().size(),
            Size::new(Length::FillPortion(2), Length::Fill)
        );
    }
}
