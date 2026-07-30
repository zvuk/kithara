#[path = "common/app_menu_spec.rs"]
mod app_menu_spec;

use std::collections::BTreeSet;

use app_menu_spec::{BLOCKS, BOXES, Block, Box, Edges, MARKS, Mark, RUNS, Run};
use kithara_test_utils::kithara;
use kithara_ui::{
    builtin,
    compile::{CompiledNode, CompiledUi},
    error::UiDocError,
    expand::{Binding, ControlSpec, ExpandedNode},
    ids::InternId,
    layout::FrameSides,
    size::SizeSpec,
    skin::{ColorRole, SkinDoc},
    source::SourceResolver,
};

/// Instance prefix every path in the mounted document carries.
const PREFIX: &str = "app-menu/";

/// The four tables of the plan's §5.3, plus every endpoint the document asks a
/// host to answer, in document order.
#[derive(Default)]
struct Spec<'a> {
    boxes: Vec<Box<'a>>,
    runs: Vec<Run<'a>>,
    marks: Vec<Mark<'a>>,
    blocks: Vec<Block<'a>>,
    reads: BTreeSet<&'a str>,
}

/// The container fields a `Row` and a `Column` share, so one walker arm serves
/// both. A `Column` declares none of the active-tone fields.
struct Container<'n> {
    id: Option<InternId>,
    size: Option<SizeSpec>,
    gap: Option<f32>,
    pad: Option<f32>,
    pad_x: Option<f32>,
    pad_y: Option<f32>,
    frame: Option<FrameSides>,
    frame_color: Option<ColorRole>,
    active_frame_color: Option<ColorRole>,
    background: Option<ColorRole>,
    active_background: Option<ColorRole>,
    active: Option<&'n Binding>,
    children: &'n [ExpandedNode],
}

fn container(node: &ExpandedNode) -> Option<Container<'_>> {
    match node {
        ExpandedNode::Row {
            id,
            size,
            gap,
            pad,
            pad_x,
            pad_y,
            frame,
            background,
            active,
            active_background,
            frame_color,
            active_frame_color,
            children,
            ..
        } => Some(Container {
            id: *id,
            size: *size,
            gap: *gap,
            pad: *pad,
            pad_x: *pad_x,
            pad_y: *pad_y,
            frame: *frame,
            frame_color: *frame_color,
            active_frame_color: *active_frame_color,
            background: *background,
            active_background: *active_background,
            active: active.as_ref(),
            children,
        }),
        ExpandedNode::Column {
            id,
            size,
            gap,
            pad,
            pad_x,
            pad_y,
            frame,
            background,
            children,
            ..
        } => Some(Container {
            id: *id,
            size: *size,
            gap: *gap,
            pad: *pad,
            pad_x: *pad_x,
            pad_y: *pad_y,
            frame: *frame,
            frame_color: None,
            active_frame_color: None,
            background: *background,
            active_background: None,
            active: None,
            children,
        }),
        _ => None,
    }
}

fn short(ui: &CompiledUi, id: InternId) -> &str {
    let path = ui.resolve(id);
    path.strip_prefix(PREFIX).unwrap_or(path)
}

fn edges(sides: FrameSides) -> Edges {
    Edges {
        top: sides.top,
        right: sides.right,
        bottom: sides.bottom,
        left: sides.left,
    }
}

/// Resolves `gap` and the paddings the way `render/tree/node.rs` does, so an
/// omitted `gap` reads `skin.layout.grid_gap` and never zero.
fn box_entry<'a>(
    id: &'a str,
    inner: &Container<'_>,
    ui: &'a CompiledUi,
    skin: &SkinDoc,
) -> Box<'a> {
    let base = inner.pad.unwrap_or(skin.layout.grid_pad);
    Box {
        id,
        size: inner.size,
        gap: Some(inner.gap.unwrap_or(skin.layout.grid_gap)),
        pad_x: Some(inner.pad_x.unwrap_or(base)),
        pad_y: Some(inner.pad_y.unwrap_or(base)),
        frame: inner.frame.map(edges),
        frame_color: inner.frame_color,
        active_frame_color: inner.active_frame_color,
        background: inner.background,
        active_background: inner.active_background,
        press: None,
        active: inner.active.map(|binding| ui.resolve(binding.key())),
    }
}

fn node_id<'a>(node: &ExpandedNode, ui: &'a CompiledUi) -> &'a str {
    match node {
        ExpandedNode::Popover { path, .. } | ExpandedNode::Pressable { path, .. } => {
            short(ui, *path)
        }
        ExpandedNode::Control { id, .. } | ExpandedNode::Slot { id, .. } => ui.resolve(*id),
        node => {
            let Some(id) = container(node).and_then(|inner| inner.id) else {
                panic!("a wrapped node carries no id: {node:?}")
            };
            ui.resolve(id)
        }
    }
}

/// `ExpandedNode` is `#[non_exhaustive]`, so a walker outside the crate cannot
/// match it exhaustively. The trailing arm takes containers and panics on
/// anything else rather than skipping it.
fn walk<'a>(node: &'a ExpandedNode, ui: &'a CompiledUi, skin: &SkinDoc, spec: &mut Spec<'a>) {
    match node {
        ExpandedNode::Popover {
            path,
            open,
            anchor,
            content,
            ..
        } => {
            let key = ui.resolve(open.key());
            spec.reads.insert(key);
            spec.boxes.push(Box::bare(short(ui, *path)).active(key));
            walk(anchor, ui, skin, spec);
            walk(content, ui, skin, spec);
        }
        ExpandedNode::Pressable { path, press, child } => {
            let id = short(ui, *path);
            let key = ui.resolve(press.key());
            if let Some(inner) = container(child) {
                let entry = box_entry(id, &inner, ui, skin).press(key);
                spec.reads.extend(entry.active);
                spec.boxes.push(entry);
                for grandchild in inner.children {
                    walk(grandchild, ui, skin, spec);
                }
            } else {
                spec.boxes.push(Box::bare(id).press(key));
                walk(child, ui, skin, spec);
            }
        }
        ExpandedNode::Optional { block, child } => {
            let hidden = ui.resolve(block.hidden.key());
            spec.reads.insert(hidden);
            spec.blocks.push(Block {
                id: short(ui, block.path),
                wraps: node_id(child, ui),
                hidden,
            });
            walk(child, ui, skin, spec);
        }
        ExpandedNode::Control {
            id,
            spec: control,
            size,
            read,
            ..
        } => {
            let read = read.as_ref().map(|binding| ui.resolve(binding.key()));
            spec.reads.extend(read);
            match control {
                ControlSpec::Text {
                    style,
                    label,
                    active,
                    align,
                } => {
                    let active = active.as_ref().map(|binding| ui.resolve(binding.key()));
                    spec.reads.extend(active);
                    spec.runs.push(Run {
                        id: ui.resolve(*id),
                        style: *style,
                        align: *align,
                        size: *size,
                        label: label.map(|label| ui.resolve(label)),
                        read,
                        active,
                    });
                }
                ControlSpec::Glyph {
                    icon,
                    active_icon,
                    style,
                    color,
                    active_color,
                    active,
                } => {
                    let active = active.as_ref().map(|binding| ui.resolve(binding.key()));
                    spec.reads.extend(active);
                    spec.marks.push(Mark {
                        id: ui.resolve(*id),
                        icon: *icon,
                        active_icon: *active_icon,
                        style: *style,
                        color: *color,
                        active_color: *active_color,
                        active,
                    });
                }
                control => panic!("the App Menu declares no {control:?}"),
            }
        }
        node => {
            let Some(inner) = container(node) else {
                panic!("the App Menu spec walker does not know {node:?}")
            };
            if let Some(id) = inner.id {
                let entry = box_entry(ui.resolve(id), &inner, ui, skin);
                spec.reads.extend(entry.active);
                spec.boxes.push(entry);
            }
            for child in inner.children {
                walk(child, ui, skin, spec);
            }
        }
    }
}

fn spec(ui: &CompiledUi) -> Spec<'_> {
    let CompiledNode::Module { root, .. } = &ui.root else {
        panic!("the App Menu mounts as a module");
    };
    let skin = builtin::skin_doc();
    let mut spec = Spec::default();
    walk(root, ui, skin, &mut spec);
    spec
}

#[kithara::test]
fn app_menu_compiles_against_the_endpoints_it_names() {
    drop(app_menu_spec::menu());
}

#[kithara::test]
fn the_app_menu_is_a_shipped_asset_and_not_builtin_preset_surface() {
    let error = builtin::resolver()
        .load(None, "modules/app-menu.kmodule.ron")
        .unwrap_err();
    assert!(
        matches!(error, UiDocError::NotFound { ref rel, .. } if rel == "modules/app-menu.kmodule.ron"),
        "{error}"
    );
}

#[kithara::test]
fn every_box_carries_the_geometry_the_design_pins() {
    let ui = app_menu_spec::menu();

    assert_eq!(spec(&ui).boxes, BOXES);
}

#[kithara::test]
fn every_run_carries_the_role_content_and_size_the_design_pins() {
    let ui = app_menu_spec::menu();

    assert_eq!(spec(&ui).runs, RUNS);
}

#[kithara::test]
fn every_glyph_carries_the_icon_size_and_tone_the_design_pins() {
    let ui = app_menu_spec::menu();

    assert_eq!(spec(&ui).marks, MARKS);
}

#[kithara::test]
fn every_optional_wraps_the_node_the_design_names() {
    let ui = app_menu_spec::menu();

    assert_eq!(spec(&ui).blocks, BLOCKS);
}
