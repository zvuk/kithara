use serde::{Deserialize, Serialize};

use crate::{
    compile::{CompiledNode, SplitCell, compiled_node_size},
    expand::{Binding, BlockSpec, ControlSpec, ExpandedNode, MeasureSpec, adaptive_branch},
    layout::Axis,
    module::{ChromeStyle, MeasureAxis},
    mount,
    skin::SkinDoc,
};

/// One-axis size rule. `Fill` takes available space, `Shrink` takes exactly what
/// the content measures; neither has an intrinsic size the document can compose.
#[derive(Clone, Copy, Debug, PartialEq, Deserialize, Serialize)]
#[non_exhaustive]
pub enum Dim {
    Fixed(f32),
    Range { min: f32, max: Option<f32> },
    Fill,
    Shrink,
}

impl Dim {
    /// Returns the upper bound, or `None` for an open range and for the axes
    /// the toolkit decides ([`Dim::Fill`], [`Dim::Shrink`]).
    #[must_use]
    pub fn max(self) -> Option<f32> {
        Bounds::from(self).max
    }

    /// Returns the lower bound in logical pixels, or zero when the toolkit
    /// decides the axis ([`Dim::Fill`], [`Dim::Shrink`]).
    #[must_use]
    pub fn min(self) -> f32 {
        Bounds::from(self).min
    }
}

/// Intrinsic size of a control or module on both axes.
#[derive(Clone, Copy, Debug, PartialEq, Deserialize, Serialize)]
#[non_exhaustive]
pub struct SizeSpec {
    pub h: Dim,
    pub w: Dim,
}

impl SizeSpec {
    pub const FILL: Self = Self {
        w: Dim::Fill,
        h: Dim::Fill,
    };

    #[must_use]
    pub const fn new(w: Dim, h: Dim) -> Self {
        Self { h, w }
    }
}

#[derive(Clone, Copy)]
pub(super) struct Bounds {
    pub(super) max: Option<f32>,
    pub(super) min: f32,
}

impl Bounds {
    const ZERO: Self = Self {
        min: 0.0,
        max: Some(0.0),
    };

    fn max(self, dim: Dim) -> Self {
        Self {
            min: self.min.max(dim.min()),
            max: self.max.zip(dim.max()).map(|(left, right)| left.max(right)),
        }
    }

    fn sum(self, dim: Dim) -> Self {
        Self {
            min: self.min + dim.min(),
            max: self.max.zip(dim.max()).map(|(left, right)| left + right),
        }
    }
}

impl From<Dim> for Bounds {
    fn from(dim: Dim) -> Self {
        match dim {
            Dim::Fixed(value) => Self {
                min: value,
                max: Some(value),
            },
            Dim::Range { min, max } => Self { max, min },
            Dim::Fill | Dim::Shrink => Self {
                min: 0.0,
                max: None,
            },
        }
    }
}

impl From<Bounds> for Dim {
    fn from(bounds: Bounds) -> Self {
        match bounds.max {
            Some(max) if bounds.min.to_bits() == max.to_bits() => Self::Fixed(bounds.min),
            Some(max) => Self::Range {
                min: bounds.min,
                max: Some(max),
            },
            None if bounds.min.to_bits() == 0.0f32.to_bits() => Self::Fill,
            None => Self::Range {
                min: bounds.min,
                max: None,
            },
        }
    }
}

pub(crate) fn combine_horizontal(sizes: impl IntoIterator<Item = SizeSpec>) -> SizeSpec {
    let (width, height) = sizes
        .into_iter()
        .fold((Bounds::ZERO, Bounds::ZERO), |(width, height), size| {
            (width.sum(size.w), height.max(size.h))
        });
    SizeSpec::new(Dim::from(width), Dim::from(height))
}

pub(crate) fn combine_vertical(sizes: impl IntoIterator<Item = SizeSpec>) -> SizeSpec {
    let (width, height) = sizes
        .into_iter()
        .fold((Bounds::ZERO, Bounds::ZERO), |(width, height), size| {
            (width.max(size.w), height.sum(size.h))
        });
    SizeSpec::new(Dim::from(width), Dim::from(height))
}

/// Returns the intrinsic size for a typed control specification.
#[must_use]
pub fn control_size(spec: &ControlSpec, skin: &SkinDoc) -> SizeSpec {
    mount::controls!(spec, Intrinsic { skin })
}

/// Asks whichever control the document named how big its skin makes it.
struct Intrinsic<'a> {
    skin: &'a SkinDoc,
}

impl Intrinsic<'_> {
    fn apply<C: mount::Control>(self, control: &C) -> SizeSpec {
        control.size(self.skin)
    }
}

pub(crate) trait Snapshot {
    fn hidden(&self, block: &BlockSpec) -> bool;
    fn measure(&self, measure: &Binding) -> Option<f32>;
}

struct Unanswered;

impl Snapshot for Unanswered {
    fn hidden(&self, _: &BlockSpec) -> bool {
        false
    }

    fn measure(&self, _: &Binding) -> Option<f32> {
        None
    }
}

pub(crate) const DEFAULTS: &dyn Snapshot = &Unanswered;

pub(crate) fn has_blocks(node: &ExpandedNode) -> bool {
    match node {
        ExpandedNode::Adaptive { size, .. } => size.is_none(),
        ExpandedNode::Optional { .. } => true,
        ExpandedNode::Row {
            measure, children, ..
        }
        | ExpandedNode::Column {
            measure, children, ..
        } => measure.is_none() && children.iter().any(has_blocks),
        ExpandedNode::Stage { children, .. } | ExpandedNode::Slot { children, .. } => {
            children.iter().any(has_blocks)
        }
        ExpandedNode::Popover { anchor, .. } => has_blocks(anchor),
        ExpandedNode::Object { child, .. }
        | ExpandedNode::Placed { child, .. }
        | ExpandedNode::Pressable { child, .. }
        | ExpandedNode::Reveal { child, .. }
        | ExpandedNode::Scroll { child, .. } => has_blocks(child),
        ExpandedNode::Control { .. } => false,
    }
}

pub(crate) trait BlockNode {
    fn block(&self) -> Option<&BlockSpec>;
}

pub(crate) fn is_hidden<N: BlockNode>(node: &N, snapshot: &dyn Snapshot) -> bool {
    node.block().is_some_and(|block| snapshot.hidden(block))
}

pub(crate) fn visible<'a, N: BlockNode>(
    children: &'a [N],
    snapshot: &'a dyn Snapshot,
) -> impl Iterator<Item = &'a N> {
    children
        .iter()
        .filter(move |child| !is_hidden(*child, snapshot))
}

pub(crate) fn branch<'a>(
    measure: &MeasureSpec,
    base: &'a ExpandedNode,
    steps: &'a [(f32, ExpandedNode)],
    snapshot: &dyn Snapshot,
) -> &'a ExpandedNode {
    let read = measure
        .binding()
        .and_then(|binding| snapshot.measure(binding));
    adaptive_branch(base, steps, read)
}

pub(crate) fn visible_compiled_children<'a>(
    children: &'a [SplitCell],
    snapshot: &'a dyn Snapshot,
) -> impl Iterator<Item = &'a SplitCell> {
    children
        .iter()
        .filter(move |cell| !is_hidden(&cell.node, snapshot))
}

pub(crate) fn compiled_node_size_with_hidden(
    node: &CompiledNode,
    skin: &SkinDoc,
    snapshot: &dyn Snapshot,
) -> SizeSpec {
    match node {
        CompiledNode::Optional { child, .. } => {
            compiled_node_size_with_hidden(child, skin, snapshot)
        }
        CompiledNode::Adaptive { size, .. } => *size,
        node if !node.blocks() => compiled_node_size(node),
        CompiledNode::Split { axis, children, .. } => {
            let sizes = visible_compiled_children(children, snapshot)
                .map(|cell| compiled_node_size_with_hidden(&cell.node, skin, snapshot));
            match axis {
                Axis::Horizontal => combine_horizontal(sizes),
                Axis::Vertical => combine_vertical(sizes),
            }
        }
        CompiledNode::Module { chrome, root, .. } => {
            crate::compile::module_size(root, *chrome, skin, snapshot)
        }
    }
}

pub(crate) fn effective_size(
    node: &ExpandedNode,
    skin: &SkinDoc,
    snapshot: &dyn Snapshot,
) -> Option<SizeSpec> {
    let declared = match node {
        ExpandedNode::Adaptive {
            measure,
            size,
            base,
            steps,
        } => {
            return size.or_else(|| {
                effective_size(branch(measure, base, steps, snapshot), skin, snapshot)
            });
        }
        ExpandedNode::Object { child, .. }
        | ExpandedNode::Optional { child, .. }
        | ExpandedNode::Placed { child, .. }
        | ExpandedNode::Pressable { child, .. }
        | ExpandedNode::Reveal { child, .. } => {
            return effective_size(child, skin, snapshot);
        }
        ExpandedNode::Popover { anchor, .. } => return effective_size(anchor, skin, snapshot),
        ExpandedNode::Row { size, .. }
        | ExpandedNode::Column { size, .. }
        | ExpandedNode::Scroll { size, .. }
        | ExpandedNode::Stage { size, .. }
        | ExpandedNode::Slot { size, .. }
        | ExpandedNode::Control { size, .. } => *size,
    };
    declared.or_else(|| match node {
        ExpandedNode::Control { spec, .. } => mount::controls!(spec, Composed { skin }),
        _ => None,
    })
}

/// Asks whichever control the document named for the size a parent composes
/// with, which some controls leave to the row that holds them.
struct Composed<'a> {
    skin: &'a SkinDoc,
}

impl Composed<'_> {
    fn apply<C: mount::Control>(self, control: &C) -> Option<SizeSpec> {
        control.composes_size().then(|| control.size(self.skin))
    }
}

/// Computes a node's intrinsic size from its override, children, or control specification.
#[must_use]
pub(crate) fn compute_size(
    node: &ExpandedNode,
    skin: &SkinDoc,
    snapshot: &dyn Snapshot,
) -> SizeSpec {
    let override_size = match node {
        ExpandedNode::Object { .. }
        | ExpandedNode::Optional { .. }
        | ExpandedNode::Placed { .. }
        | ExpandedNode::Popover { .. }
        | ExpandedNode::Pressable { .. }
        | ExpandedNode::Reveal { .. } => None,
        ExpandedNode::Adaptive { size, .. }
        | ExpandedNode::Scroll { size, .. }
        | ExpandedNode::Row { size, .. }
        | ExpandedNode::Column { size, .. }
        | ExpandedNode::Stage { size, .. }
        | ExpandedNode::Slot { size, .. }
        | ExpandedNode::Control { size, .. } => *size,
    };
    if let Some(size) = override_size {
        return size;
    }

    match node {
        ExpandedNode::Adaptive {
            measure,
            base,
            steps,
            ..
        } => compute_size(branch(measure, base, steps, snapshot), skin, snapshot),
        ExpandedNode::Object { child, .. }
        | ExpandedNode::Optional { child, .. }
        | ExpandedNode::Placed { child, .. }
        | ExpandedNode::Pressable { child, .. }
        | ExpandedNode::Reveal { child, .. }
        | ExpandedNode::Scroll { child, .. } => compute_size(child, skin, snapshot),
        ExpandedNode::Popover { anchor, .. } => compute_size(anchor, skin, snapshot),
        ExpandedNode::Row {
            children,
            gap,
            pad,
            pad_x,
            pad_y,
            ..
        } => {
            let laid_out: Vec<_> = visible(children, snapshot).collect();
            inset(
                combine_horizontal(
                    laid_out
                        .iter()
                        .map(|child| compute_size(child, skin, snapshot)),
                ),
                gap_total(gap.unwrap_or(skin.layout.grid_gap), laid_out.len()),
                0.0,
                Pad::new(*pad, *pad_x, *pad_y, skin.layout.grid_pad),
            )
        }
        ExpandedNode::Column {
            children,
            gap,
            pad,
            pad_x,
            pad_y,
            ..
        } => {
            let laid_out: Vec<_> = visible(children, snapshot).collect();
            inset(
                combine_vertical(
                    laid_out
                        .iter()
                        .map(|child| compute_size(child, skin, snapshot)),
                ),
                0.0,
                gap_total(gap.unwrap_or(skin.layout.grid_gap), laid_out.len()),
                Pad::new(*pad, *pad_x, *pad_y, skin.layout.grid_pad),
            )
        }
        ExpandedNode::Stage { children, .. } => visible(children, snapshot)
            .next()
            .map_or(SizeSpec::FILL, |first| compute_size(first, skin, snapshot)),
        ExpandedNode::Slot { children, .. } => {
            let laid_out: Vec<_> = visible(children, snapshot).collect();
            if laid_out.is_empty() {
                SizeSpec::FILL
            } else {
                combine_vertical(
                    laid_out
                        .iter()
                        .map(|child| compute_size(child, skin, snapshot)),
                )
            }
        }
        ExpandedNode::Control { spec, .. } => control_size(spec, skin),
    }
}

#[must_use]
pub(crate) fn min_size(node: &ExpandedNode, skin: &SkinDoc) -> SizeSpec {
    match node {
        ExpandedNode::Control { .. } => needs(compute_size(node, skin, DEFAULTS)),
        ExpandedNode::Scroll { size, child, .. } => {
            size.map_or_else(|| min_size(child, skin), needs)
        }
        ExpandedNode::Object { child, .. }
        | ExpandedNode::Optional { child, .. }
        | ExpandedNode::Placed { child, .. }
        | ExpandedNode::Pressable { child, .. }
        | ExpandedNode::Reveal { child, .. } => min_size(child, skin),
        ExpandedNode::Popover { anchor, .. } => min_size(anchor, skin),
        ExpandedNode::Adaptive { size, base, .. } => at_least(*size, min_size(base, skin)),
        ExpandedNode::Stage { size, children, .. } => at_least(
            *size,
            children
                .first()
                .map_or(NOTHING, |first| min_size(first, skin)),
        ),
        ExpandedNode::Slot { size, children, .. } => at_least(
            *size,
            combine_vertical(children.iter().map(|child| min_size(child, skin))),
        ),
        ExpandedNode::Row { size, measure, .. } | ExpandedNode::Column { size, measure, .. } => {
            at_least(*size, settled(node, *measure, skin))
        }
    }
}

pub(crate) const NOTHING: SizeSpec = SizeSpec::new(Dim::Fixed(0.0), Dim::Fixed(0.0));

pub(crate) fn settled(
    node: &ExpandedNode,
    measure: Option<MeasureAxis>,
    skin: &SkinDoc,
) -> SizeSpec {
    Cells::of(node, skin).map_or(NOTHING, |cells| cells.settled(measure))
}

#[must_use]
pub(crate) fn rooms(node: &ExpandedNode, axis: MeasureAxis, skin: &SkinDoc) -> Vec<(f32, f32)> {
    Cells::of(node, skin).map_or_else(Vec::new, |cells| {
        cells.rooms(axis, axis_min(min_size(node, skin), axis))
    })
}

pub(crate) fn axis_dim(size: SizeSpec, axis: MeasureAxis) -> Dim {
    match axis {
        MeasureAxis::Width => size.w,
        MeasureAxis::Height => size.h,
    }
}

pub(crate) fn axis_min(size: SizeSpec, axis: MeasureAxis) -> f32 {
    axis_dim(size, axis).min()
}

pub(crate) struct Cell {
    until: Option<f32>,
    min: SizeSpec,
    from: f32,
}

impl Cell {
    pub(crate) const fn new(from: f32, until: Option<f32>, min: SizeSpec) -> Self {
        Self { until, min, from }
    }
}

#[must_use]
pub(crate) fn stands(from: f32, until: Option<f32>, room: f32) -> bool {
    from <= room && until.is_none_or(|until| room < until)
}

pub(crate) struct Cells {
    along: Axis,
    pad: Pad,
    cells: Vec<Cell>,
    gap: f32,
}

impl Cells {
    pub(crate) const fn new(along: Axis, cells: Vec<Cell>) -> Self {
        Self {
            along,
            cells,
            gap: 0.0,
            pad: Pad::NONE,
        }
    }

    fn need(&self, room: Option<f32>) -> SizeSpec {
        let standing: Vec<_> = self
            .cells
            .iter()
            .filter(|cell| room.is_none_or(|room| stands(cell.from, cell.until, room)))
            .map(|cell| cell.min)
            .collect();
        let gaps = gap_total(self.gap, standing.len());
        match self.along {
            Axis::Horizontal => inset(combine_horizontal(standing), gaps, 0.0, self.pad),
            Axis::Vertical => inset(combine_vertical(standing), 0.0, gaps, self.pad),
        }
    }

    fn of(node: &ExpandedNode, skin: &SkinDoc) -> Option<Self> {
        let (along, children, gap, pad, pad_x, pad_y) = match node {
            ExpandedNode::Row {
                children,
                gap,
                pad,
                pad_x,
                pad_y,
                ..
            } => (Axis::Horizontal, children, gap, pad, pad_x, pad_y),
            ExpandedNode::Column {
                children,
                gap,
                pad,
                pad_x,
                pad_y,
                ..
            } => (Axis::Vertical, children, gap, pad, pad_x, pad_y),
            _ => return None,
        };
        let cells = children
            .iter()
            .map(|child| {
                let (from, until) = match child {
                    ExpandedNode::Reveal { from, until, .. } => (*from, *until),
                    _ => (0.0, None),
                };
                Cell::new(from, until, min_size(child, skin))
            })
            .collect();
        Some(Self {
            along,
            cells,
            gap: gap.unwrap_or(skin.layout.grid_gap),
            pad: Pad::new(*pad, *pad_x, *pad_y, skin.layout.grid_pad),
        })
    }

    pub(crate) fn rooms(&self, axis: MeasureAxis, least: f32) -> Vec<(f32, f32)> {
        std::iter::once(least)
            .chain(
                self.cells
                    .iter()
                    .map(|cell| cell.from)
                    .filter(|from| *from > least),
            )
            .map(|room| (room, axis_min(self.need(Some(room)), axis)))
            .collect()
    }

    /// Each round only adds cells and covers the last, so the climb terminates at the first size
    /// that asks for no more room than it already occupies — the widest set the container holds.
    pub(crate) fn settled(&self, measure: Option<MeasureAxis>) -> SizeSpec {
        let Some(axis) = measure else {
            return self.need(None);
        };
        let mut size = self.need(Some(0.0));
        loop {
            let covered = covering(size, self.need(Some(axis_min(size, axis))));
            if covered == size {
                return size;
            }
            size = covered;
        }
    }
}

fn covering(left: SizeSpec, right: SizeSpec) -> SizeSpec {
    SizeSpec::new(
        Dim::from(Bounds::from(left.w).max(right.w)),
        Dim::from(Bounds::from(left.h).max(right.h)),
    )
}

fn needs(size: SizeSpec) -> SizeSpec {
    SizeSpec::new(Dim::Fixed(size.w.min()), Dim::Fixed(size.h.min()))
}

pub(crate) fn at_least(declared: Option<SizeSpec>, composed: SizeSpec) -> SizeSpec {
    declared.map_or(composed, |declared| {
        SizeSpec::new(
            Dim::Fixed(declared.w.min().max(composed.w.min())),
            Dim::Fixed(declared.h.min().max(composed.h.min())),
        )
    })
}

pub(crate) fn with_module_chrome(size: SizeSpec, chrome: ChromeStyle, skin: &SkinDoc) -> SizeSpec {
    if chrome != ChromeStyle::Full {
        return size;
    }
    let lines = skin.chrome.inner_line_width * 2.0;
    let height = skin.chrome.header_height + skin.chrome.footer_height + lines;
    SizeSpec::new(size.w, grow(size.h, height))
}

fn gap_total(gap: f32, child_count: usize) -> f32 {
    let gaps = u16::try_from(child_count.saturating_sub(1)).unwrap_or(u16::MAX);
    gap * f32::from(gaps)
}

/// Padding a container adds on each axis, mirroring what the renderer applies:
/// the per-axis override wins over `pad`, which wins over the skin default.
#[derive(Clone, Copy)]
struct Pad {
    x: f32,
    y: f32,
}

impl Pad {
    const NONE: Self = Self { x: 0.0, y: 0.0 };

    fn new(pad: Option<f32>, pad_x: Option<f32>, pad_y: Option<f32>, default: f32) -> Self {
        let base = pad.unwrap_or(default);
        Self {
            x: pad_x.unwrap_or(base),
            y: pad_y.unwrap_or(base),
        }
    }
}

fn inset(size: SizeSpec, extra_w: f32, extra_h: f32, pad: Pad) -> SizeSpec {
    SizeSpec::new(
        grow(size.w, extra_w + pad.x * 2.0),
        grow(size.h, extra_h + pad.y * 2.0),
    )
}

pub(super) fn grow(dim: Dim, delta: f32) -> Dim {
    if delta <= 0.0 {
        return dim;
    }
    match dim {
        Dim::Fixed(value) => Dim::Fixed(value + delta),
        Dim::Range { min, max } => Dim::Range {
            min: min + delta,
            max: max.map(|max| max + delta),
        },
        Dim::Fill | Dim::Shrink => dim,
    }
}
