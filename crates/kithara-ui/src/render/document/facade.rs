use num_traits::cast::AsPrimitive;

use super::{
    Band, Ctx, Group, GroupMount, Host, Lit, Measured, Module, PlacedMount, Popover, Snap,
    SplitMount,
};
use crate::{
    compile::{Address, CompiledNode, CompiledUi, SplitCell},
    draw::{Pt, Transform},
    expand::{Binding, ExpandedNode, MagnetSpec, MeasureSpec, SurfaceSpec},
    ids::InternId,
    layout::{Axis, FrameCorners, FrameSides},
    module::{ChromeStyle, MeasureAxis, Motion, PopoverAlign, PopoverAt, Pose, TextAlign},
    render::{InputOwner, ReadValue},
    size::{
        BlockNode, Dim, SizeSpec, Snapshot, branch as adaptive_branch,
        compiled_node_size_with_hidden, effective_size, is_hidden, visible_compiled_children,
    },
    skin::{ColorRole, SkinDoc},
};

const HOSTED_MODULES: [&str; 27] = [
    "app-bar",
    "app-deck",
    "app-library",
    "app-menu",
    "app-menu-module-cell",
    "app-menu-window-row",
    "app-select-row",
    "app-strip",
    "app-strip-eq-3-band",
    "app-strip-eq-4-band",
    "app-mixer",
    "app-mixer-single",
    "app-overview",
    "app-overview-single",
    "deck-overview-row",
    "gallery-knobs",
    "gallery-meters",
    "gallery-toggles",
    "gallery-chips",
    "gallery-buttons-tab",
    "gallery-cells-tab",
    "gallery-faders-tab",
    "gallery-library2-tab",
    "gallery-table-tab",
    "gallery-tree-tab",
    "gallery-module-tabs",
    "gallery-nav",
];

/// Produces a complete host output from a compiled document.
///
/// The traversal, conditional visibility, retained-owner selection, and root
/// composition are toolkit-neutral. `host` mounts each already-traversed node
/// into its local layout, paint, and interaction vocabulary.
#[must_use]
pub fn render<H>(node: &CompiledNode, ctx: Ctx<'_, '_>, mut host: H) -> H::Output
where
    H: Host,
{
    let content = compiled(node, ctx, &mut host);
    host.window(content, ctx.ui.dragged.as_ref(), ctx.ui.resize_edges)
}

#[cfg(test)]
pub(crate) fn render_engine_subtree<H>(
    node: &ExpandedNode,
    address: &Address<'_>,
    owner: InternId,
    ctx: Ctx<'_, '_>,
    mut host: H,
) -> H::Output
where
    H: Host,
{
    expanded(
        node,
        address,
        Branch {
            owner,
            input_owner: InputOwner::Engine,
            round: FrameCorners::EMPTY,
            transform: Transform::IDENTITY,
        },
        ctx,
        &mut host,
    )
}

fn compiled<H>(node: &CompiledNode, ctx: Ctx<'_, '_>, host: &mut H) -> H::Output
where
    H: Host,
{
    let snapshot: &dyn Snapshot = &ctx;
    match node {
        CompiledNode::Optional { child, .. } => compiled(child, ctx, host),
        CompiledNode::Adaptive {
            axis,
            size,
            base,
            steps,
        } => {
            let mut branches: Vec<H::Output> = Vec::with_capacity(steps.len() + 1);
            branches.push(compiled(base, ctx, host));
            for (_, node) in steps {
                branches.push(compiled(node, ctx, host));
            }
            host.measured(
                Measured {
                    axis: *axis,
                    steps: steps.iter().map(|(from, _)| *from).collect(),
                    size: *size,
                },
                branches,
            )
        }
        CompiledNode::Split {
            axis,
            measure,
            children,
            ..
        } => {
            let mut mounted: Vec<SplitMount<H::Output>> = Vec::with_capacity(children.len());
            for cell in split_cells::<H>(children, snapshot) {
                let size = compiled_node_size_with_hidden(&cell.node, ctx.skin, snapshot);
                let output = compiled(&cell.node, ctx, host);
                mounted.push(SplitMount {
                    size,
                    output,
                    band: Band::new(cell.from, cell.until),
                    block: block_of(&cell.node),
                    weight: cell.weight,
                });
            }
            host.split(*axis, *measure, mounted)
        }
        CompiledNode::Module {
            instance,
            module,
            title,
            chip,
            assign,
            chrome,
            frame,
            corners,
            round,
            footer,
            drop,
            collapsed,
            root,
            ..
        } => {
            let collapsed = *chrome == ChromeStyle::Full
                && matches!(
                    ctx.get(ctx.ui.resolve(*collapsed)),
                    Some(ReadValue::Bool(true))
                );
            let footer = footer
                .as_ref()
                .and_then(|binding| ctx.read(binding))
                .and_then(|value| match value {
                    ReadValue::Text(text) => Some(text.to_owned()),
                    _ => None,
                });
            let content_hosted = HOSTED_MODULES.contains(&ctx.ui.resolve(*module));
            let chrome_hosted = *chrome == ChromeStyle::Full || drop.is_some();
            let content = (!collapsed).then(|| {
                let child = expanded(
                    root,
                    &Address::Root,
                    Branch {
                        owner: *instance,
                        input_owner: if content_hosted {
                            InputOwner::Engine
                        } else {
                            InputOwner::Leaf
                        },
                        round: if *chrome == ChromeStyle::Plain {
                            *round
                        } else {
                            FrameCorners::EMPTY
                        },
                        transform: Transform::IDENTITY,
                    },
                    ctx,
                    host,
                );
                if content_hosted {
                    host.hosted(root, child)
                } else {
                    child
                }
            });
            host.module(
                Module {
                    assign,
                    footer,
                    collapsed,
                    chrome_hosted,
                    instance: *instance,
                    module: *module,
                    title: *title,
                    chip: *chip,
                    chrome: *chrome,
                    frame: *frame,
                    corners: *corners,
                    round: *round,
                    drop: drop.as_ref(),
                },
                content,
            )
        }
    }
}

#[derive(Clone, Copy)]
struct Branch {
    /// The window corners the node this branch mounts stands at. Only a
    /// module's own root ever carries any: everything under it is inside the
    /// window, not at its edge.
    round: FrameCorners,
    input_owner: InputOwner,
    owner: InternId,
    /// Every enclosing object's pose, composed and resolved for this frame.
    transform: Transform,
}

#[derive(Clone, Copy)]
struct PopoverNode<'a> {
    open: &'a Binding,
    anchor: &'a ExpandedNode,
    content: &'a ExpandedNode,
    path: InternId,
    size: Option<SizeSpec>,
    align: PopoverAlign,
    at: PopoverAt,
}

#[derive(Clone, Copy)]
struct RowNode<'a> {
    active: Option<&'a Binding>,
    active_background: Option<ColorRole>,
    active_frame_color: Option<ColorRole>,
    background: Option<ColorRole>,
    background_alpha: Option<f32>,
    frame: Option<FrameSides>,
    frame_color: Option<ColorRole>,
    gap: Option<f32>,
    measure: Option<MeasureAxis>,
    pad: Option<f32>,
    pad_x: Option<f32>,
    pad_y: Option<f32>,
    size: Option<SizeSpec>,
    surface: Option<&'a SurfaceSpec>,
    align: TextAlign,
}

fn row_group<'a>(node: RowNode<'a>, round: FrameCorners, ctx: Ctx<'_, '_>) -> Group<'a> {
    let padding = node.pad.unwrap_or(ctx.skin.layout.grid_pad);
    let frame_color = node.frame_color.unwrap_or(ctx.skin.divider.color);
    let lit = node.active.map(|flag| Lit {
        flag,
        background: node.active_background.or(node.background),
        frame_color: node.active_frame_color.unwrap_or(frame_color),
    });
    Group {
        round,
        lit,
        frame_color,
        background: node.background,
        axis: Axis::Horizontal,
        measure: node.measure,
        alignment: node.align,
        gap: node.gap.unwrap_or(ctx.skin.layout.grid_gap),
        padding_x: node.pad_x.unwrap_or(padding),
        padding_y: node.pad_y.unwrap_or(padding),
        frame: node.frame,
        background_alpha: node.background_alpha,
        frame_width: ctx.skin.divider.width,
        surface: node.surface,
        size: node.size,
    }
}

/// Whether this node hands its input to a retained engine before it mounts.
///
/// The subtree under an engine is mounted the same way either way, so the
/// question is asked once here and the answer never reaches [`mounted`].
fn expanded<H>(
    node: &ExpandedNode,
    address: &Address<'_>,
    branch: Branch,
    ctx: Ctx<'_, '_>,
    host: &mut H,
) -> H::Output
where
    H: Host,
{
    if branch.input_owner == InputOwner::Leaf && hosts_engine(ctx.ui, branch.owner, address) {
        let child = expanded(
            node,
            address,
            Branch {
                input_owner: InputOwner::Engine,
                ..branch
            },
            ctx,
            host,
        );
        return host.hosted(node, child);
    }
    mounted(node, address, branch, ctx, host)
}

/// An adaptive block: the branches it chooses between and the box it keeps.
struct Adaptive<'a> {
    base: &'a ExpandedNode,
    node: &'a ExpandedNode,
    measure: &'a MeasureSpec,
    steps: &'a [(f32, ExpandedNode)],
    size: Option<SizeSpec>,
}

/// How an adaptive block becomes host output.
///
/// An axis names a room only the layout pass knows, so every branch is mounted
/// and the host chooses. A measured reading is answered here.
fn mount_adaptive<H>(
    adaptive: &Adaptive<'_>,
    address: &Address<'_>,
    branch: Branch,
    ctx: Ctx<'_, '_>,
    host: &mut H,
) -> H::Output
where
    H: Host,
{
    let &Adaptive {
        node,
        measure,
        size,
        base,
        steps,
    } = adaptive;
    let snapshot: &dyn Snapshot = &ctx;
    match measure.axis() {
        Some(axis) => {
            let mut branches: Vec<H::Output> = Vec::with_capacity(steps.len() + 1);
            branches.push(expanded(base, &address.child(0), branch, ctx, host));
            for (index, (_, node)) in steps.iter().enumerate() {
                branches.push(expanded(node, &address.child(index + 1), branch, ctx, host));
            }
            host.measured(
                Measured {
                    axis,
                    steps: steps.iter().map(|(from, _)| *from).collect(),
                    size: size
                        .or_else(|| effective_size(node, ctx.skin, snapshot))
                        .unwrap_or(SizeSpec::FILL),
                },
                branches,
            )
        }
        None => expanded(
            adaptive_branch(measure, base, steps, snapshot),
            &address.child(0),
            branch,
            ctx,
            host,
        ),
    }
}

/// How a row or column becomes host output.
fn mount_flow<H>(
    node: &ExpandedNode,
    address: &Address<'_>,
    branch: Branch,
    ctx: Ctx<'_, '_>,
    host: &mut H,
) -> H::Output
where
    H: Host,
{
    let snapshot: &dyn Snapshot = &ctx;
    let (group, children) = match node {
        ExpandedNode::Row {
            measure,
            children,
            gap,
            align,
            pad,
            pad_x,
            pad_y,
            frame,
            background,
            background_alpha,
            active,
            active_background,
            frame_color,
            active_frame_color,
            surface,
            ..
        } => (
            row_group(
                RowNode {
                    measure: *measure,
                    gap: *gap,
                    align: *align,
                    pad: *pad,
                    pad_x: *pad_x,
                    pad_y: *pad_y,
                    frame: *frame,
                    background: *background,
                    background_alpha: *background_alpha,
                    active: active.as_ref(),
                    active_background: *active_background,
                    frame_color: *frame_color,
                    active_frame_color: *active_frame_color,
                    surface: surface.as_ref(),
                    size: effective_size(node, ctx.skin, snapshot),
                },
                branch.round,
                ctx,
            ),
            children,
        ),
        ExpandedNode::Column {
            measure,
            children,
            gap,
            align,
            pad,
            pad_x,
            pad_y,
            frame,
            frame_color,
            background,
            background_alpha,
            surface,
            ..
        } => (
            Group {
                round: branch.round,
                axis: Axis::Vertical,
                measure: *measure,
                alignment: *align,
                gap: gap.unwrap_or(ctx.skin.layout.grid_gap),
                padding_x: pad_x.unwrap_or(pad.unwrap_or(ctx.skin.layout.grid_pad)),
                padding_y: pad_y.unwrap_or(pad.unwrap_or(ctx.skin.layout.grid_pad)),
                frame: *frame,
                background: *background,
                background_alpha: *background_alpha,
                lit: None,
                frame_color: frame_color.unwrap_or(ctx.skin.divider.color),
                frame_width: ctx.skin.divider.width,
                surface: surface.as_ref(),
                size: effective_size(node, ctx.skin, snapshot),
            },
            children,
        ),
        _ => unreachable!("mount_flow is called only for a row or column"),
    };
    mount_group(group, children, address, branch, snapshot, ctx, host)
}

/// How one node becomes host output, once the engine question is settled.
fn mounted<H>(
    node: &ExpandedNode,
    address: &Address<'_>,
    branch: Branch,
    ctx: Ctx<'_, '_>,
    host: &mut H,
) -> H::Output
where
    H: Host,
{
    let snapshot: &dyn Snapshot = &ctx;
    match node {
        ExpandedNode::Optional { child, .. }
        | ExpandedNode::Placed { child, .. }
        | ExpandedNode::Reveal { child, .. } => {
            expanded(child, &address.child(0), branch, ctx, host)
        }
        ExpandedNode::Adaptive {
            measure,
            size,
            base,
            steps,
        } => mount_adaptive(
            &Adaptive {
                node,
                measure,
                base,
                steps,
                size: *size,
            },
            address,
            branch,
            ctx,
            host,
        ),
        ExpandedNode::Row { .. } | ExpandedNode::Column { .. } => {
            mount_flow(node, address, branch, ctx, host)
        }
        ExpandedNode::Popover {
            path,
            open,
            at,
            align,
            anchor,
            content,
        } => mount_popover(
            PopoverNode {
                open,
                anchor,
                content,
                path: *path,
                at: *at,
                align: *align,
                size: effective_size(node, ctx.skin, snapshot),
            },
            address,
            branch,
            ctx,
            host,
        ),
        ExpandedNode::Pressable { path, child, .. } => {
            let child = expanded(child, &address.child(0), branch, ctx, host);
            host.pressable(*path, child, effective_size(node, ctx.skin, snapshot))
        }
        ExpandedNode::Scroll { id, child, .. } => {
            let child = expanded(child, &address.child(0), branch, ctx, host);
            host.scroll(*id, child, effective_size(node, ctx.skin, snapshot))
        }
        ExpandedNode::Slot { children, .. } => {
            let mounted = expanded_group_children(
                children,
                Axis::Vertical,
                address,
                branch,
                snapshot,
                ctx,
                host,
            );
            host.slot(mounted, effective_size(node, ctx.skin, snapshot))
        }
        ExpandedNode::Stage { children, .. } => {
            let scene = Scene::of(children, ctx);
            let mounted = children
                .iter()
                .enumerate()
                .map(|(index, child)| {
                    mount_staged(child, &address.child(index), branch, &scene, ctx, host)
                })
                .collect();
            host.stage(mounted, effective_size(node, ctx.skin, snapshot))
        }
        ExpandedNode::Object {
            pose,
            to,
            phase,
            motion,
            child,
        } => {
            let track = Track {
                from: *pose,
                to: to.as_ref(),
                phase: phase.as_ref(),
                motion: motion.as_ref(),
            };
            mount_object(track, child, address, branch, ctx, host)
        }
        ExpandedNode::Control {
            path, spec, read, ..
        } => host.control(
            *path,
            spec,
            read.as_ref(),
            branch.input_owner,
            effective_size(node, ctx.skin, snapshot),
            branch.transform,
        ),
    }
}

fn mount_popover<H>(
    node: PopoverNode<'_>,
    address: &Address<'_>,
    branch: Branch,
    ctx: Ctx<'_, '_>,
    host: &mut H,
) -> H::Output
where
    H: Host,
{
    let anchor = expanded(node.anchor, &address.child(0), branch, ctx, host);
    let content = address.child(1);
    host.popover(
        Popover {
            path: node.path,
            at: node.at,
            align: node.align,
            open: ctx.flag(Some(node.open)),
            flag: node.open,
            size: node.size,
        },
        anchor,
        &mut |host| expanded(node.content, &content, branch, ctx, host),
    )
}

fn mount_group<H>(
    group: Group<'_>,
    children: &[ExpandedNode],
    address: &Address<'_>,
    branch: Branch,
    snapshot: &dyn Snapshot,
    ctx: Ctx<'_, '_>,
    host: &mut H,
) -> H::Output
where
    H: Host,
{
    let children =
        expanded_group_children(children, group.axis, address, branch, snapshot, ctx, host);
    host.group(group, children)
}

fn expanded_group_children<H>(
    children: &[ExpandedNode],
    axis: Axis,
    address: &Address<'_>,
    branch: Branch,
    snapshot: &dyn Snapshot,
    ctx: Ctx<'_, '_>,
    host: &mut H,
) -> Vec<GroupMount<H::Output>>
where
    H: Host,
{
    children
        .iter()
        .enumerate()
        .filter(|(_, child)| H::MOUNTS_HIDDEN || !is_hidden(*child, snapshot))
        .map(|(index, child)| GroupMount {
            band: band_of(child),
            block: block_of(child),
            minimum: main_minimum(child, axis, ctx.skin, snapshot),
            output: expanded(
                child,
                &address.child(index),
                Branch {
                    round: FrameCorners::EMPTY,
                    ..branch
                },
                ctx,
                host,
            ),
        })
        .collect()
}

/// The cells of a split, in the order the host mounts them: every cell for a
/// host that hides a block itself, and only the standing ones for a host that
/// leaves a hidden cell out of the tree.
fn split_cells<'a, H>(
    children: &'a [SplitCell],
    snapshot: &'a dyn Snapshot,
) -> Box<dyn Iterator<Item = &'a SplitCell> + 'a>
where
    H: Host,
{
    if H::MOUNTS_HIDDEN {
        Box::new(children.iter())
    } else {
        Box::new(visible_compiled_children(children, snapshot))
    }
}

/// What the document reads to know one child of a flow is hidden, when the
/// child is a block at all.
fn block_of<N: BlockNode>(node: &N) -> Option<Binding> {
    node.block().map(|block| block.hidden.clone())
}

/// The band of room one child of a flow stands in.
fn band_of(node: &ExpandedNode) -> Band {
    match node {
        ExpandedNode::Reveal { from, until, .. } => Band::new(*from, *until),
        _ => Band::ALWAYS,
    }
}

/// Where an object starts, where it ends, and what carries it between them.
#[derive(Clone, Copy)]
struct Track<'a> {
    motion: Option<&'a Motion<Binding>>,
    phase: Option<&'a Binding>,
    to: Option<&'a Pose>,
    from: Pose,
}

impl Track<'_> {
    /// The pose to draw this object at, this frame.
    ///
    /// A phase an endpoint answers moves the object between one frame and the
    /// next; a motion works the same scalar out from the seconds its clock
    /// hands over. An object with no track, or one nobody drives, sits at the
    /// pose the document wrote down and stays there.
    fn resolve(self, ctx: Ctx<'_, '_>) -> Pose {
        let along = match (self.phase, self.motion) {
            (Some(phase), None) => scalar(ctx, phase),
            (None, Some(motion)) => scalar(ctx, &motion.clock).map(|at| motion.phase_at(at)),
            (None, None) | (Some(_), Some(_)) => None,
        };
        match (self.to, along) {
            (Some(to), Some(along)) => self.from.between(to, along),
            _ => self.from,
        }
    }
}

/// One scalar an endpoint answers with, or nothing when it answers otherwise.
fn scalar(ctx: Ctx<'_, '_>, binding: &Binding) -> Option<f32> {
    match ctx.read(binding)? {
        ReadValue::Scalar(value) => Some(value.as_()),
        _ => None,
    }
}

/// Composes an object's pose onto whatever its subtree draws.
///
/// The object mounts nothing of its own: the child goes straight to the host,
/// carrying an offset the host applies to the picture and to nothing else.
fn mount_object<H>(
    track: Track<'_>,
    child: &ExpandedNode,
    address: &Address<'_>,
    branch: Branch,
    ctx: Ctx<'_, '_>,
    host: &mut H,
) -> H::Output
where
    H: Host,
{
    let here = track.resolve(ctx);
    expanded(
        child,
        &address.child(0),
        Branch {
            transform: here.matrix().then(branch.transform),
            ..branch
        },
        ctx,
        host,
    )
}

/// Where every placement of one stage stands this frame.
///
/// A magnet names placements, and what it needs of them is the point each is
/// on now, so the stage answers that once for all its children rather than
/// every carried placement asking the document again.
struct Scene {
    at: Vec<(InternId, Pt)>,
}

impl Scene {
    fn of(children: &[ExpandedNode], ctx: Ctx<'_, '_>) -> Self {
        Self {
            at: children
                .iter()
                .filter_map(|child| match child {
                    ExpandedNode::Placed { id, at, read, .. } => {
                        Some((*id, point(*at, read.as_ref(), ctx)))
                    }
                    _ => None,
                })
                .collect(),
        }
    }

    fn snap(&self, magnet: &MagnetSpec) -> Snap {
        Snap {
            to: magnet
                .to
                .iter()
                .filter_map(|target| {
                    self.at
                        .iter()
                        .find_map(|(id, at)| (id == target).then_some(*at))
                })
                .collect(),
            within: magnet.within,
        }
    }
}

/// Where a placement stands: the point its endpoint answers, or the one the
/// document wrote when nothing answers.
fn point(at: (f32, f32), read: Option<&Binding>, ctx: Ctx<'_, '_>) -> Pt {
    ctx.point(read).unwrap_or(Pt { x: at.0, y: at.1 })
}

fn mount_staged<H>(
    node: &ExpandedNode,
    address: &Address<'_>,
    branch: Branch,
    scene: &Scene,
    ctx: Ctx<'_, '_>,
    host: &mut H,
) -> H::Output
where
    H: Host,
{
    let ExpandedNode::Placed {
        path,
        at,
        read,
        write,
        magnet,
        child,
        ..
    } = node
    else {
        return expanded(node, address, branch, ctx, host);
    };
    let mounted = expanded(child, &address.child(0), branch, ctx, host);
    host.placed(
        PlacedMount {
            path: *path,
            at: point(*at, read.as_ref(), ctx),
            read: read.as_ref(),
            write: write.as_ref(),
            snap: magnet.as_ref().map(|magnet| scene.snap(magnet)),
        },
        mounted,
    )
}

fn main_minimum(
    node: &ExpandedNode,
    axis: Axis,
    skin: &SkinDoc,
    snapshot: &dyn Snapshot,
) -> Option<f32> {
    let size = effective_size(node, skin, snapshot)?;
    let dim = match axis {
        Axis::Horizontal => size.w,
        Axis::Vertical => size.h,
    };
    match dim {
        Dim::Range { min, .. } => Some(min),
        _ => None,
    }
}

fn hosts_engine(ui: &CompiledUi, owner: InternId, address: &Address<'_>) -> bool {
    HOSTED_MODULES
        .iter()
        .any(|module| ui.includes_module(owner, address, module))
}
